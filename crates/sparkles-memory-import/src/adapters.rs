//! The adapters of spec C18 §8.10.1 and §8.10.4: which files each reads, and the
//! structural facts each file gives.

use crate::frontmatter;
use crate::ids::{self, Ctx};
use crate::project::{self, Project};
use crate::redact::{self, Pattern};
use crate::vocab::*;
use crate::{Adapter, Fact, FileImport, FileKind, Harness, MAX_FILE_BYTES, Obj, Scope};
use regex::Regex;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// The most `@path` hops followed from one instruction file, as Claude Code allows.
const MAX_IMPORT_DEPTH: usize = 5;
/// Quotes are cut to this many characters (the limit of `assert_facts`).
const QUOTE_CHARS: usize = 1000;

/// Where the harnesses keep their files.
#[derive(Clone, Debug)]
pub struct Roots {
    pub home: PathBuf,
    /// `CLAUDE_CONFIG_DIR`, else `~/.claude`
    pub claude: PathBuf,
    /// `CODEX_HOME`, else `~/.codex`
    pub codex: PathBuf,
    /// `$GEMINI_CLI_HOME/.gemini`, else `~/.gemini`
    pub gemini: PathBuf,
    /// `~/.cursor`
    pub cursor: PathBuf,
    /// `COPILOT_HOME`, else `~/.copilot`
    pub copilot: PathBuf,
    /// the directory of Claude Code's managed `CLAUDE.md`
    pub managed: PathBuf,
}

impl Roots {
    /// The roots from the environment: `HOME`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
    /// `GEMINI_CLI_HOME` and `COPILOT_HOME`.
    pub fn from_env() -> Roots {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let managed = var("SPARKLES_CLAUDE_MANAGED_DIR").unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                PathBuf::from("/Library/Application Support/ClaudeCode")
            } else {
                PathBuf::from("/etc/claude-code")
            }
        });
        Roots {
            claude: var("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude")),
            codex: var("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
            gemini: var("GEMINI_CLI_HOME")
                .map(|h| h.join(".gemini"))
                .unwrap_or_else(|| home.join(".gemini")),
            cursor: home.join(".cursor"),
            copilot: var("COPILOT_HOME").unwrap_or_else(|| home.join(".copilot")),
            managed,
            home,
        }
    }

    /// Claude Code's directory for a project: the path with every character other than a
    /// letter or a digit turned into `-`. The name is computed from the path and never
    /// decoded. When no such directory exists, a transcript whose `cwd` is the project
    /// names it.
    pub fn claude_project_dir(&self, p: &Project) -> Option<PathBuf> {
        let projects = self.claude.join("projects");
        for d in [&p.dir, &p.root] {
            let name: String = d
                .to_string_lossy()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            let cand = projects.join(&name);
            if cand.is_dir() {
                return Some(cand);
            }
        }
        let want: Vec<String> = [&p.dir, &p.root]
            .iter()
            .map(|d| d.to_string_lossy().into_owned())
            .collect();
        for e in std::fs::read_dir(&projects).ok()?.flatten() {
            let dir = e.path();
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            for f in files.flatten().take(50) {
                let fp = f.path();
                if fp.extension().is_some_and(|x| x == "jsonl")
                    && transcript_cwd(&fp).is_some_and(|c| want.contains(&c))
                {
                    return Some(dir);
                }
            }
        }
        None
    }

    /// Whether a path is in one of Claude Code's memory directories.
    pub fn in_claude_memory(&self, path: &Path) -> bool {
        let projects = self.claude.join("projects");
        path.strip_prefix(&projects).is_ok_and(|rest| {
            let mut c = rest.components();
            c.next().is_some() && c.next().is_some_and(|m| m.as_os_str() == "memory")
        })
    }
}

/// The `cwd` of the first lines of a transcript.
fn transcript_cwd(p: &Path) -> Option<String> {
    use std::io::BufRead;
    let f = std::fs::File::open(p).ok()?;
    for line in std::io::BufReader::new(f).lines().take(20) {
        let line = line.ok()?;
        if let Some(i) = line.find("\"cwd\":\"") {
            let rest = &line[i + 7..];
            let end = rest.find('"')?;
            return Some(rest[..end].replace("\\\\", "\\"));
        }
    }
    None
}

/// What to scan.
pub struct Request<'a> {
    pub ctx: &'a Ctx,
    pub roots: &'a Roots,
    pub adapters: Vec<Adapter>,
    pub project: Option<Project>,
    /// the user-scope files: `~/.claude/CLAUDE.md`, user rules, `~/.codex/AGENTS.md`,
    /// Codex's memories, the user's `GEMINI.md` and the managed `CLAUDE.md`
    pub user_scope: bool,
    /// Markdown files for the generic adapter
    pub paths: Vec<PathBuf>,
    /// leave out memory files and the index
    pub instructions_only: bool,
    pub patterns: &'a [Pattern],
}

/// A file the scan did not import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub path: PathBuf,
    /// Why the file was skipped, including when redaction refused a linked file.
    pub reason: String,
}

/// The files of one scan.
#[derive(Clone, Debug, Default)]
pub struct Scan {
    pub files: Vec<FileImport>,
    pub skipped: Vec<Skipped>,
    /// graph prefixes this scan listed in full: a source under one of them without a
    /// file was deleted
    pub areas: Vec<String>,
    /// Prefixes where a redaction refusal prevented the scan from establishing absence.
    pub incomplete_areas: HashSet<String>,
    /// Refused files, keyed by graph area using the same relative paths as readable files.
    pub refused_paths: HashMap<String, HashSet<String>>,
    /// Local lookup for formerly unmasked stored paths under the current redaction policy.
    masked_refusal_digests: HashMap<String, HashSet<String>>,
    /// Instruction areas reachable from a parent whose imports could not be read.
    hidden_import_areas: HashSet<String>,
}

impl Scan {
    /// Whether a refusal could have hidden the owner of this graph at this path.
    pub fn has_refused_path(&self, graph: &str, rel_path: &str) -> bool {
        self.refused_paths
            .iter()
            .any(|(area, paths)| graph.starts_with(area) && paths.contains(rel_path))
    }

    fn may_hide_imports(&self, graph: &str) -> bool {
        self.hidden_import_areas
            .iter()
            .any(|area| graph.starts_with(area))
    }

    /// Whether an unmasked stored path matches a file refused by this status scan.
    pub fn refused_local_path(&self, graph: &str, path: &str) -> bool {
        self.has_refused_path(graph, path)
            || self.masked_refusal_digests.iter().any(|(area, digests)| {
                graph.starts_with(area) && digests.contains(&ids::digest(path.as_bytes()))
            })
    }

    /// Without a readable candidate, a refused parent may have hidden even a unique path.
    pub fn absence_uncertain(&self, graph: &str, stored_path: Option<&str>) -> bool {
        self.may_hide_imports(graph)
            || self.incomplete_areas.contains(graph)
            || match stored_path.filter(|p| !p.contains("[redacted:")) {
                Some(path) => self.refused_local_path(graph, path),
                None => self
                    .incomplete_areas
                    .iter()
                    .any(|area| graph.starts_with(area)),
            }
    }

    /// A hidden import may have a distinct OS path with the same normalized spelling.
    pub fn hidden_path_collision(&self, graph: &str, path: &str) -> bool {
        (path.contains(char::REPLACEMENT_CHARACTER)
            || (!std::path::is_separator('\\') && path.contains('/')))
            && self.may_hide_imports(graph)
    }
}

/// Run the adapters of `req`.
pub fn scan(req: &Request) -> Scan {
    scan_with_policy(req, &redact::Policy::from_patterns(req.patterns.to_vec()))
}

/// Run the adapters of `req` with a full redaction [`redact::Policy`] instead of
/// `req.patterns`.
pub fn scan_with_policy(req: &Request, policy: &redact::Policy) -> Scan {
    scan_with_project_key(req, policy, None)
}

/// Scan with an optional override for the published identity of the requested project.
pub fn scan_with_project_key(
    req: &Request,
    policy: &redact::Policy,
    project_key: Option<String>,
) -> Scan {
    collect_files(req, policy, project_key, false)
}

/// Keep every distinct file that could own a source when comparing disk with the server.
pub fn scan_for_status(req: &Request, policy: &redact::Policy, project_key: String) -> Scan {
    collect_files(req, policy, Some(project_key), true)
}

fn collect_files(
    req: &Request,
    policy: &redact::Policy,
    project_key: Option<String>,
    keep_collisions: bool,
) -> Scan {
    let mut s = Scanner {
        keep_collisions,
        req,
        policy,
        project_key: req
            .project
            .as_ref()
            .map(|p| project_key.unwrap_or_else(|| p.redacted_key(policy).into_owned())),
        out: Scan::default(),
        seen: HashMap::new(),
        attempted_instructions: HashMap::new(),
    };
    for a in &req.adapters {
        match a {
            Adapter::ClaudeCode => s.claude_code(),
            Adapter::Codex => s.codex(),
            Adapter::Generic => s.generic(),
            // these read transcripts only, which the command line handles
            Adapter::GeminiCli | Adapter::Cursor | Adapter::Copilot | Adapter::Aider => {}
        }
    }
    let mut areas: Vec<String> = std::mem::take(&mut s.out.areas);
    areas.sort();
    areas.dedup();
    s.out.areas = areas;
    s.out
}

/// A file read and redacted.
struct Read {
    bytes: Vec<u8>,
    text: String,
    redactions: Vec<String>,
    edits: Vec<redact::Edit>,
    modified: String,
}

struct Scanner<'a> {
    req: &'a Request<'a>,
    policy: &'a redact::Policy,
    project_key: Option<String>,
    out: Scan,
    /// Files already produced for each graph. Imports retain only the first owner.
    seen: HashMap<String, HashSet<PathBuf>>,
    /// Includes refused and discarded paths, which never enter `seen`.
    attempted_instructions: HashMap<String, HashSet<PathBuf>>,
    keep_collisions: bool,
}

fn rel(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// A path relative to its root as a key: `/` turned into `.`.
fn path_key(rel: &str) -> String {
    rel.replace('/', ".")
}

/// A memory name as Claude Code names files: lower case, spaces turned into `-`.
pub fn norm_name(n: &str) -> String {
    n.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

fn cut(s: &str) -> String {
    if s.chars().count() <= QUOTE_CHARS {
        s.to_string()
    } else {
        s.chars().take(QUOTE_CHARS).collect()
    }
}

fn fact(s: &str, p: &str, o: Obj, quote: Option<&str>) -> Fact {
    Fact {
        s: s.to_string(),
        p: p.to_string(),
        o,
        quote: quote.map(cut).filter(|q| !q.trim().is_empty()),
    }
}

/// Markdown files below `dir`, sorted, without following symbolic links.
fn markdown_files(dir: &Path, ext: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file()
                && p.extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| ext.contains(&x))
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// `[[name]]` links outside code fences, with their lines.
fn wiki_links(body: &str) -> Vec<(String, String)> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[\[([^\]\|#\n]+)(?:[#|][^\]\n]*)?\]\]").expect("regex"));
    let mut out = Vec::new();
    let mut fenced = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        for c in RE.captures_iter(line) {
            out.push((c[1].trim().to_string(), line.trim().to_string()));
        }
    }
    out
}

/// `@path` imports outside code fences, with their lines.
fn at_imports(body: &str) -> Vec<(String, String)> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:^|\s)@([~./A-Za-z0-9_][^\s`'\x22)]*)").expect("regex"));
    let mut out = Vec::new();
    let mut fenced = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        for c in RE.captures_iter(line) {
            let raw = c[1].trim_end_matches(['.', ',', ';', ':']).to_string();
            if raw.contains('/') || raw.contains('.') || raw.starts_with('~') {
                out.push((raw, line.trim().to_string()));
            }
        }
    }
    out
}

fn mtime(meta: &std::fs::Metadata) -> String {
    let t: chrono::DateTime<chrono::Utc> = meta
        .modified()
        .map(Into::into)
        .unwrap_or_else(|_| chrono::Utc::now());
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A frontmatter time as `xsd:dateTime` when it is one, else as a plain literal.
fn time_obj(s: &str) -> Obj {
    match chrono::DateTime::parse_from_rfc3339(s.trim()) {
        Ok(t) => Obj::typed(
            t.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            XSD_DATETIME,
        ),
        Err(_) => match chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d") {
            Ok(d) => Obj::typed(format!("{d}T00:00:00Z"), XSD_DATETIME),
            Err(_) => Obj::lit(s.trim()),
        },
    }
}

impl Scanner<'_> {
    fn ctx(&self) -> &Ctx {
        self.req.ctx
    }

    fn project_key(&self) -> Option<&str> {
        self.project_key.as_deref()
    }

    fn project_segment(&self) -> String {
        self.project_key()
            .map_or_else(|| "user".to_string(), project::segment_of)
    }

    /// The part of a harness directory included in this listing.
    fn list_area(&mut self, harness: Harness, project_segment: &str) {
        let tail = if self.req.instructions_only {
            "instructions/"
        } else {
            ""
        };
        self.out
            .areas
            .push(self.ctx().graph(harness.segment(), project_segment, tail));
    }

    /// Read a file: the size limit, UTF-8, the generated marker, then redaction.
    fn read(&mut self, path: &Path, rel_path: &str, graph_prefix: &str) -> Option<Read> {
        let skip = |me: &mut Self, reason: &str| {
            me.out.skipped.push(Skipped {
                path: path.to_path_buf(),
                reason: reason.into(),
            });
            None
        };
        let Ok(meta) = std::fs::metadata(path) else {
            return skip(self, "unreadable");
        };
        if meta.len() > MAX_FILE_BYTES {
            return skip(self, "too-large");
        }
        let Ok(bytes) = std::fs::read(path) else {
            return skip(self, "unreadable");
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return skip(self, "not-utf8");
        };
        if crate::is_generated(text) {
            return skip(self, "generated");
        }
        let Ok((r, edits)) = self.policy.redact_bounded(
            text,
            MAX_FILE_BYTES as usize,
            || Ok(()),
            || "redaction-too-large",
        ) else {
            let published = self.redact(rel_path).text;
            if self.keep_collisions && published != rel_path {
                self.out
                    .masked_refusal_digests
                    .entry(graph_prefix.to_owned())
                    .or_default()
                    .insert(ids::digest(rel_path.as_bytes()));
            }
            self.out.incomplete_areas.insert(graph_prefix.to_owned());
            self.out
                .refused_paths
                .entry(graph_prefix.to_owned())
                .or_default()
                .insert(published);
            return skip(self, "redaction-too-large");
        };
        Some(Read {
            modified: mtime(&meta),
            text: r.text,
            redactions: r.names,
            edits,
            bytes,
        })
    }

    fn redact(&self, value: &str) -> redact::Redacted {
        self.policy.redact(value)
    }

    fn path_key(&self, path: &str) -> String {
        if self.policy.first_match(path).is_none() {
            path_key(path)
        } else {
            // Keep distinct files distinct without embedding the matched path in their IRIs.
            ids::digest(path.as_bytes())
                .trim_start_matches("sha256:")
                .to_string()
        }
    }

    /// The source's description and the project's facts, which every file's graph has.
    #[allow(clippy::too_many_arguments)]
    fn source_facts(
        &self,
        graph: &str,
        harness: Harness,
        path: &Path,
        rel_path: &str,
        read: &Read,
        with_project: bool,
        facts: &mut Vec<Fact>,
    ) {
        let title = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        facts.push(fact(graph, RDF_TYPE, Obj::Iri(PROV_ENTITY.into()), None));
        facts.push(fact(
            graph,
            DCT_TITLE,
            Obj::lit(self.redact(&title).text),
            None,
        ));
        facts.push(fact(graph, DCT_FORMAT, Obj::lit("text/markdown"), None));
        facts.push(fact(
            graph,
            SPK_CONTENT_DIGEST,
            Obj::lit(ids::digest(&read.bytes)),
            None,
        ));
        facts.push(fact(graph, &mem("harness"), Obj::Iri(harness.iri()), None));
        facts.push(fact(
            graph,
            &mem("filePath"),
            Obj::lit(self.redact(rel_path).text),
            None,
        ));
        facts.push(fact(
            graph,
            DCT_MODIFIED,
            Obj::typed(read.modified.clone(), XSD_DATETIME),
            None,
        ));
        facts.push(fact(
            graph,
            &mem("redactions"),
            Obj::typed(read.redactions.len().to_string(), XSD_INTEGER),
            None,
        ));
        if with_project && let Some(key) = self.project_key() {
            let p = self.ctx().project_iri(key);
            facts.push(fact(graph, &mem("project"), Obj::Iri(p.clone()), None));
            facts.push(fact(&p, RDF_TYPE, Obj::Iri(mem("Project")), None));
            facts.push(fact(&p, RDFS_LABEL, Obj::lit(key), None));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        adapter: Adapter,
        harness: Harness,
        kind: FileKind,
        scope: Option<Scope>,
        path: &Path,
        rel_path: String,
        key: String,
        graph: String,
        entity: Option<String>,
        read: Read,
        body_offset: usize,
        facts: Vec<Fact>,
    ) {
        let paths = self.seen.entry(graph.clone()).or_default();
        if (!self.keep_collisions && !paths.is_empty()) || !paths.insert(path.to_owned()) {
            return;
        }
        let title = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let body = read.bytes.get(body_offset..).unwrap_or(&[]);
        let mut facts = facts;
        for (iri, line) in crate::copies_of(&read.text) {
            facts.push(fact(&graph, &mem("copyOf"), Obj::Iri(iri), Some(&line)));
        }
        let title = self.redact(&title).text;
        let published_path = self.redact(&rel_path).text;
        let local_path_digest = (self.keep_collisions && published_path != rel_path)
            .then(|| ids::digest(rel_path.as_bytes()));
        self.out.files.push(FileImport {
            adapter,
            harness,
            kind,
            scope,
            path: path.to_path_buf(),
            rel_path: published_path,
            local_path_digest,
            key,
            digest: ids::digest(&read.bytes),
            body_digest: ids::digest(body),
            graph,
            entity,
            title,
            redactions: read.redactions,
            facts,
            text: read.text,
        });
    }

    /// The offset of the body in the original bytes: the frontmatter's length, which
    /// redaction may have changed in the text but not in the bytes.
    fn body_offset(bytes: &[u8]) -> usize {
        let Ok(t) = std::str::from_utf8(bytes) else {
            return 0;
        };
        let d = frontmatter::parse(t);
        t.len() - d.body.len()
    }

    // ------------------------------------------------------------- memories ------

    /// A memory file: Claude Code's, Codex's generated ones, or a generic one.
    #[allow(clippy::too_many_arguments)]
    fn memory(
        &mut self,
        adapter: Adapter,
        harness: Harness,
        path: &Path,
        rel_path: String,
        project_key: &str,
        project_segment: &str,
        generated: bool,
    ) {
        let area = self
            .ctx()
            .graph(harness.segment(), project_segment, "memory/");
        let Some(read) = self.read(path, &rel_path, &area) else {
            return;
        };
        let doc = frontmatter::parse(&read.text);
        let name = doc
            .scalar("name")
            .or_else(|| doc.scalar("title"))
            .map(|(v, l)| (v.to_string(), l.to_string()));
        let key = match &name {
            Some((n, _)) if !norm_name(n).is_empty() => norm_name(n),
            _ => self.path_key(&rel_path),
        };
        let graph = format!("{area}{}", ids::segment(&key));
        let m = self.ctx().memory_iri(harness.segment(), project_key, &key);
        let mut facts = Vec::new();
        facts.push(fact(&m, RDF_TYPE, Obj::Iri(mem("Memory")), None));
        let kind = doc
            .get_nested("metadata", "type")
            .or_else(|| doc.get("type"))
            .and_then(|e| match &e.value {
                frontmatter::Value::Scalar(s) if !s.is_empty() => Some((s.clone(), e.line.clone())),
                _ => None,
            });
        match &kind {
            Some((k, line)) => {
                let class = match k.as_str() {
                    "user" => Some("UserMemory"),
                    "feedback" => Some("FeedbackMemory"),
                    "project" => Some("ProjectMemory"),
                    "reference" => Some("ReferenceMemory"),
                    "generated" => Some("GeneratedMemory"),
                    _ => None,
                };
                if let Some(c) = class {
                    facts.push(fact(&m, RDF_TYPE, Obj::Iri(mem(c)), Some(line)));
                }
                facts.push(fact(&m, &mem("kind"), Obj::lit(k.clone()), Some(line)));
            }
            None if generated => {
                facts.push(fact(&m, RDF_TYPE, Obj::Iri(mem("GeneratedMemory")), None));
                facts.push(fact(&m, &mem("kind"), Obj::lit("generated"), None));
            }
            None => {}
        }
        if generated && kind.is_some() {
            facts.push(fact(&m, RDF_TYPE, Obj::Iri(mem("GeneratedMemory")), None));
        }
        let label = match &name {
            Some((n, l)) => (n.clone(), Some(l.clone())),
            None => (heading_or_name(doc.body, path, self.policy), None),
        };
        facts.push(fact(&m, RDFS_LABEL, Obj::lit(label.0), label.1.as_deref()));
        if let Some((d, l)) = doc.scalar("description") {
            facts.push(fact(&m, SCHEMA_DESCRIPTION, Obj::lit(d), Some(l)));
        }
        match doc.scalar("modified") {
            Some((t, l)) => facts.push(fact(&m, DCT_MODIFIED, time_obj(t), Some(l))),
            None => facts.push(fact(
                &m,
                DCT_MODIFIED,
                Obj::typed(read.modified.clone(), XSD_DATETIME),
                None,
            )),
        }
        if let Some((globs, l)) = doc.list("globs").or_else(|| doc.list("paths")) {
            for g in globs {
                facts.push(fact(&m, &mem("appliesTo"), Obj::lit(g), Some(l)));
            }
        }
        facts.push(fact(&m, &mem("harness"), Obj::Iri(harness.iri()), None));
        if project_key != "user" {
            facts.push(fact(
                &m,
                &mem("project"),
                Obj::Iri(self.ctx().project_iri(project_key)),
                None,
            ));
        }
        facts.push(fact(
            &m,
            &mem("filePath"),
            Obj::lit(self.redact(&rel_path).text),
            None,
        ));
        facts.push(fact(&m, &mem("file"), Obj::Iri(graph.clone()), None));
        let mut linked = BTreeSet::new();
        for (target, line) in wiki_links(doc.body) {
            let t = self
                .ctx()
                .memory_iri(harness.segment(), project_key, &norm_name(&target));
            if t != m && linked.insert(t.clone()) {
                facts.push(fact(&m, DCT_REFERENCES, Obj::Iri(t), Some(&line)));
            }
        }
        self.source_facts(
            &graph,
            harness,
            path,
            &rel_path,
            &read,
            project_key != "user",
            &mut facts,
        );
        let off = Self::body_offset(&read.bytes);
        self.push(
            adapter,
            harness,
            FileKind::Memory,
            None,
            path,
            rel_path,
            key,
            graph,
            Some(m),
            read,
            off,
            facts,
        );
    }

    /// `MEMORY.md`: each line that links to a memory file gives that memory its position
    /// and the line's text.
    fn index(&mut self, path: &Path, memdir: &Path, project_key: &str, project_segment: &str) {
        static LINK: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\(([^)\s]+)\)").expect("regex"));
        let harness = Harness::ClaudeCode;
        let graph = self
            .ctx()
            .graph(harness.segment(), project_segment, "index");
        let rel_path = rel(path, memdir);
        let Some(read) = self.read(path, &rel_path, &graph) else {
            return;
        };
        let mut facts = Vec::new();
        let mut position = 0usize;
        let mut done = HashSet::new();
        let keys: std::collections::HashMap<_, _> = self
            .out
            .files
            .iter()
            .filter(|f| f.kind == FileKind::Memory && f.path.starts_with(memdir))
            .map(|f| (f.path.as_path(), f.key.as_str()))
            .collect();
        let original = std::str::from_utf8(&read.bytes).expect("read checked UTF-8");
        let offset = |at| redact::remap_byte_offset(&read.edits, at);
        let mut end = 0;
        for line in original.split_inclusive('\n') {
            let start = end;
            end += line.len();
            let Some(c) = LINK.captures(line) else {
                continue;
            };
            let target = c[2].trim();
            if target.contains("://") || !target.ends_with(".md") {
                continue;
            }
            let file = project::normalize(&memdir.join(target));
            if !file.starts_with(memdir) {
                continue;
            }
            if self
                .out
                .skipped
                .iter()
                .any(|s| s.reason == "redaction-too-large" && s.path == file)
            {
                // A refused target might have had a different imported key. Keep
                // the prior index until the target can be read again.
                self.out.skipped.push(Skipped {
                    path: path.to_path_buf(),
                    reason: "linked-redaction-too-large".into(),
                });
                self.out.incomplete_areas.insert(graph);
                return;
            }
            position += 1;
            let fallback;
            let key = match keys.get(file.as_path()) {
                Some(&key) => key,
                None => {
                    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
                    fallback = if self.policy.first_match(&stem).is_some() {
                        self.path_key(&rel(&file, memdir))
                    } else {
                        norm_name(&stem)
                    };
                    &fallback
                }
            };
            let m = self.ctx().memory_iri(harness.segment(), project_key, key);
            if !done.insert(m.clone()) {
                continue;
            }
            let m0 = c.get(0).expect("group 0");
            let quote = read.text[offset(start)..offset(end)].trim();
            let text = read.text[offset(start + m0.end())..offset(end)]
                .trim()
                .trim_start_matches(['—', '–', '-', ':'])
                .trim();
            facts.push(fact(
                &m,
                &mem("indexPosition"),
                Obj::typed(position.to_string(), XSD_INTEGER),
                Some(quote),
            ));
            if !text.is_empty() {
                facts.push(fact(&m, &mem("indexText"), Obj::lit(text), Some(quote)));
            }
        }
        self.source_facts(&graph, harness, path, &rel_path, &read, true, &mut facts);
        self.push(
            Adapter::ClaudeCode,
            harness,
            FileKind::Index,
            None,
            path,
            rel_path,
            "index".into(),
            graph,
            None,
            read,
            0,
            facts,
        );
    }

    // --------------------------------------------------------- instructions ------

    /// An instruction file, and with `at_imports` the files its `@path` lines import,
    /// breadth first up to [`MAX_IMPORT_DEPTH`] hops.
    #[allow(clippy::too_many_arguments)]
    fn instructions(
        &mut self,
        adapter: Adapter,
        harness: Harness,
        scope: Scope,
        user_area: bool,
        path: &Path,
        root: &Path,
        key_prefix: &str,
        follow_imports: bool,
    ) {
        let user = user_area;
        let mut queue: VecDeque<(PathBuf, Scope, bool, PathBuf, String, usize)> = VecDeque::new();
        queue.push_back((
            path.to_path_buf(),
            scope,
            user,
            root.to_path_buf(),
            key_prefix.to_string(),
            0,
        ));
        while let Some((path, scope, user, root, key_prefix, depth)) = queue.pop_front() {
            let project_key = if user {
                "user".to_string()
            } else {
                self.project_key().unwrap_or("user").to_string()
            };
            let project_segment = if user {
                "user".to_string()
            } else {
                self.project_segment()
            };
            let rel_path = rel(&path, &root);
            let key = format!("{key_prefix}{}", self.path_key(&rel_path));
            let area = self
                .ctx()
                .graph(harness.segment(), &project_segment, "instructions/");
            let graph = format!("{area}{}", ids::segment(&key));
            if !self
                .attempted_instructions
                .entry(graph.clone())
                .or_default()
                .insert(path.clone())
            {
                continue;
            }
            let Some(read) = self.read(&path, &rel_path, &area) else {
                // A refused file can hide imports in either instruction scope.
                if self
                    .out
                    .skipped
                    .last()
                    .is_some_and(|s| s.reason == "redaction-too-large")
                {
                    // The instruction key is known even when its content is unavailable.
                    self.out.incomplete_areas.insert(graph);
                    if follow_imports {
                        let other = if user {
                            self.project_segment()
                        } else {
                            "user".to_string()
                        };
                        let other_area =
                            self.ctx().graph(harness.segment(), &other, "instructions/");
                        self.out.incomplete_areas.insert(other_area.clone());
                        self.out.hidden_import_areas.extend([area, other_area]);
                    }
                }
                continue;
            };
            // Read distinct paths even when sync will keep only the first graph.
            // A refusal on a colliding path affects whether the first can be synced.
            if !self.keep_collisions && self.seen.contains_key(&graph) {
                continue;
            }
            let doc = frontmatter::parse(&read.text);
            let i = self
                .ctx()
                .instructions_iri(harness.segment(), &project_key, &key);
            let mut facts = vec![
                fact(&i, RDF_TYPE, Obj::Iri(mem("Instructions")), None),
                fact(&i, RDFS_LABEL, Obj::lit(self.redact(&rel_path).text), None),
                fact(&i, &mem("scope"), Obj::Iri(scope.iri()), None),
                fact(&i, &mem("harness"), Obj::Iri(harness.iri()), None),
                fact(
                    &i,
                    &mem("filePath"),
                    Obj::lit(self.redact(&rel_path).text),
                    None,
                ),
                fact(&i, &mem("file"), Obj::Iri(graph.clone()), None),
            ];
            if !user && let Some(k) = self.project_key() {
                facts.push(fact(
                    &i,
                    &mem("project"),
                    Obj::Iri(self.ctx().project_iri(k)),
                    None,
                ));
            }
            if let Some((d, l)) = doc.scalar("description") {
                facts.push(fact(&i, SCHEMA_DESCRIPTION, Obj::lit(d), Some(l)));
            }
            if let Some((globs, l)) = doc.list("paths").or_else(|| doc.list("globs")) {
                for g in globs {
                    facts.push(fact(&i, &mem("appliesTo"), Obj::lit(g), Some(l)));
                }
            }
            if let Some((v, l)) = doc.scalar("alwaysApply") {
                let b = matches!(v.to_ascii_lowercase().as_str(), "true" | "yes");
                facts.push(fact(
                    &i,
                    &mem("alwaysApply"),
                    Obj::typed(b.to_string(), XSD_BOOLEAN),
                    Some(l),
                ));
            }
            if follow_imports {
                let base_dir = path.parent().unwrap_or(&root).to_path_buf();
                for (raw, line) in at_imports(doc.body) {
                    let target = self.resolve_import(&raw, &base_dir);
                    let iri = match target {
                        Some((t, tscope, tuser, troot, tprefix)) => {
                            let tkey = format!("{tprefix}{}", self.path_key(&rel(&t, &troot)));
                            let pk = if tuser {
                                "user".to_string()
                            } else {
                                self.project_key().unwrap_or("user").to_string()
                            };
                            let iri = self.ctx().instructions_iri(harness.segment(), &pk, &tkey);
                            if t.is_file() && depth < MAX_IMPORT_DEPTH {
                                queue.push_back((t, tscope, tuser, troot, tprefix, depth + 1));
                            }
                            iri
                        }
                        // not followed: outside the project and the user directory
                        None => self
                            .ctx()
                            .dangling_iri(harness.segment(), &project_key, &raw),
                    };
                    if iri != i {
                        facts.push(fact(&i, &mem("imports"), Obj::Iri(iri), Some(&line)));
                    }
                }
            }
            self.source_facts(&graph, harness, &path, &rel_path, &read, !user, &mut facts);
            let off = Self::body_offset(&read.bytes);
            self.push(
                adapter,
                harness,
                FileKind::Instructions,
                Some(scope),
                &path,
                rel_path,
                key,
                graph,
                Some(i),
                read,
                off,
                facts,
            );
        }
    }

    /// Where an `@path` import leads, when it is inside the project or the harness's user
    /// directory: the file, its scope, the root its key is relative to and the key's
    /// prefix. The check is lexical and, for a file that exists, also on its real path,
    /// so a symbolic link cannot lead outside.
    fn resolve_import(
        &self,
        raw: &str,
        base: &Path,
    ) -> Option<(PathBuf, Scope, bool, PathBuf, String)> {
        let roots = self.req.roots;
        let p = if let Some(r) = raw.strip_prefix("~/") {
            roots.home.join(r)
        } else if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            base.join(raw)
        };
        let p = project::normalize(&p);
        let real = std::fs::canonicalize(&p).ok();
        let inside = |root: &Path| {
            let root = project::normalize(root);
            let real_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
            p.starts_with(&root) && real.as_ref().is_none_or(|r| r.starts_with(&real_root))
        };
        if let Some(pr) = &self.req.project
            && inside(&pr.root)
        {
            return Some((p, Scope::Project, false, pr.root.clone(), String::new()));
        }
        if inside(&roots.claude) {
            return Some((p, Scope::User, true, roots.claude.clone(), String::new()));
        }
        None
    }

    // ------------------------------------------------------------- adapters ------

    fn claude_code(&mut self) {
        let roots = self.req.roots.clone();
        if let Some(project) = self.req.project.clone() {
            let key = self.project_key().expect("project key").to_string();
            let seg = self.project_segment();
            self.list_area(Harness::ClaudeCode, &seg);
            if !self.req.instructions_only
                && let Some(pdir) = roots.claude_project_dir(&project)
            {
                let memdir = pdir.join("memory");
                let mut index = None;
                for f in markdown_files(&memdir, &["md"]) {
                    let r = rel(&f, &memdir);
                    if r == "MEMORY.md" {
                        index = Some(f);
                        continue;
                    }
                    self.memory(
                        Adapter::ClaudeCode,
                        Harness::ClaudeCode,
                        &f,
                        r,
                        &key,
                        &seg,
                        false,
                    );
                }
                if let Some(index) = index {
                    self.index(&index, &memdir, &key, &seg);
                }
            }
            let root = project.root.clone();
            let files: Vec<(PathBuf, Scope)> = [
                (root.join("CLAUDE.md"), Scope::Project),
                (root.join(".claude/CLAUDE.md"), Scope::Project),
                (root.join("CLAUDE.local.md"), Scope::Local),
            ]
            .into_iter()
            .chain(
                markdown_files(&root.join(".claude/rules"), &["md"])
                    .into_iter()
                    .map(|f| (f, Scope::Rule)),
            )
            .collect();
            for (f, scope) in files {
                if f.is_file() {
                    self.instructions(
                        Adapter::ClaudeCode,
                        Harness::ClaudeCode,
                        scope,
                        false,
                        &f,
                        &root,
                        "",
                        true,
                    );
                }
            }
        }
        if self.req.user_scope {
            self.list_area(Harness::ClaudeCode, "user");
            let user: Vec<(PathBuf, Scope, PathBuf, &str)> = [(
                roots.claude.join("CLAUDE.md"),
                Scope::User,
                roots.claude.clone(),
                "",
            )]
            .into_iter()
            .chain(
                markdown_files(&roots.claude.join("rules"), &["md"])
                    .into_iter()
                    .map(|f| (f, Scope::Rule, roots.claude.clone(), "")),
            )
            .chain([(
                roots.managed.join("CLAUDE.md"),
                Scope::Managed,
                roots.managed.clone(),
                "managed.",
            )])
            .collect();
            for (f, scope, root, prefix) in user {
                if f.is_file() {
                    self.instructions(
                        Adapter::ClaudeCode,
                        Harness::ClaudeCode,
                        scope,
                        true,
                        &f,
                        &root,
                        prefix,
                        true,
                    );
                }
            }
        }
    }

    fn codex(&mut self) {
        let roots = self.req.roots.clone();
        if let Some(project) = self.req.project.clone() {
            self.out.areas.push(format!(
                "{}instructions/",
                self.ctx()
                    .area(Harness::Codex.segment(), &self.project_segment())
            ));
            // one file per directory, from the repository's root down to the project's
            let mut dirs = vec![project.dir.clone()];
            let mut d = project.dir.as_path();
            while d != project.root {
                match d.parent() {
                    Some(p) if p.starts_with(&project.root) => {
                        dirs.push(p.to_path_buf());
                        d = p;
                    }
                    _ => break,
                }
            }
            dirs.reverse();
            for dir in dirs {
                let f = [dir.join("AGENTS.override.md"), dir.join("AGENTS.md")]
                    .into_iter()
                    .find(|f| f.is_file());
                if let Some(f) = f {
                    self.instructions(
                        Adapter::Codex,
                        Harness::Codex,
                        Scope::Project,
                        false,
                        &f,
                        &project.root,
                        "",
                        false,
                    );
                }
            }
        }
        if self.req.user_scope {
            self.list_area(Harness::Codex, "user");
            if let Some(f) = [
                roots.codex.join("AGENTS.override.md"),
                roots.codex.join("AGENTS.md"),
            ]
            .into_iter()
            .find(|f| f.is_file())
            {
                self.instructions(
                    Adapter::Codex,
                    Harness::Codex,
                    Scope::User,
                    true,
                    &f,
                    &roots.codex,
                    "",
                    false,
                );
            }
            // Codex's generated memories: read-only, through the generic reading
            if !self.req.instructions_only {
                let memdir = roots.codex.join("memories");
                for f in markdown_files(&memdir, &["md", "txt"]) {
                    let r = rel(&f, &memdir);
                    self.memory(
                        Adapter::Generic,
                        Harness::Codex,
                        &f,
                        r,
                        "user",
                        "user",
                        true,
                    );
                }
            }
        }
    }

    fn generic(&mut self) {
        let roots = self.req.roots.clone();
        if let Some(project) = self.req.project.as_ref() {
            let root = project.root.clone();
            for h in [Harness::GeminiCli, Harness::Cursor, Harness::Generic] {
                self.out.areas.push(format!(
                    "{}instructions/",
                    self.ctx().area(h.segment(), &self.project_segment())
                ));
            }
            let mut files: Vec<(PathBuf, Harness, Scope)> = vec![
                (root.join("GEMINI.md"), Harness::GeminiCli, Scope::Project),
                (root.join(".cursorrules"), Harness::Cursor, Scope::Project),
                (
                    root.join(".github/copilot-instructions.md"),
                    Harness::Generic,
                    Scope::Project,
                ),
            ];
            for f in markdown_files(&root.join(".cursor/rules"), &["mdc", "md"]) {
                files.push((f, Harness::Cursor, Scope::Rule));
            }
            for (f, h, scope) in files {
                if f.is_file() {
                    self.instructions(Adapter::Generic, h, scope, false, &f, &root, "", false);
                }
            }
        }
        if self.req.user_scope {
            self.out.areas.push(format!(
                "{}instructions/",
                self.ctx().area(Harness::GeminiCli.segment(), "user")
            ));
            let f = roots.gemini.join("GEMINI.md");
            if f.is_file() {
                self.instructions(
                    Adapter::Generic,
                    Harness::GeminiCli,
                    Scope::User,
                    true,
                    &f,
                    &roots.gemini,
                    "",
                    false,
                );
            }
        }
        // files named on the command line
        for p in self.req.paths.clone() {
            let p = project::absolute(&p);
            let (root, pk, seg) = match &self.req.project {
                Some(pr) if p.starts_with(&pr.root) => (
                    pr.root.clone(),
                    self.project_key().expect("project key").to_string(),
                    self.project_segment(),
                ),
                _ => (
                    p.parent().map(Path::to_path_buf).unwrap_or_default(),
                    "user".to_string(),
                    "user".to_string(),
                ),
            };
            if !p.is_file() {
                self.out.skipped.push(Skipped {
                    path: p,
                    reason: "unreadable".into(),
                });
                continue;
            }
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let known = matches!(
                name.as_str(),
                "GEMINI.md"
                    | "AGENTS.md"
                    | "CLAUDE.md"
                    | ".cursorrules"
                    | "copilot-instructions.md"
            ) || name.ends_with(".mdc");
            if known {
                let scope = if pk == "user" {
                    Scope::User
                } else {
                    Scope::Project
                };
                self.instructions(
                    Adapter::Generic,
                    Harness::Generic,
                    scope,
                    pk == "user",
                    &p,
                    &root,
                    "",
                    false,
                );
            } else {
                let r = rel(&p, &root);
                self.memory(Adapter::Generic, Harness::Generic, &p, r, &pk, &seg, false);
            }
        }
    }
}

/// A file's label when its frontmatter has no name: its first heading, else its name.
fn heading_or_name(body: &str, path: &Path, policy: &redact::Policy) -> String {
    body.lines()
        .find_map(|l| {
            let t = l.trim_start();
            t.starts_with('#')
                .then(|| t.trim_start_matches('#').trim().to_string())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| {
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            policy.redact(&stem).text
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_imports() {
        let l =
            wiki_links("see [[deploy-checklist]] and [[Deploy Checklist|x]]\n```\n[[no]]\n```\n");
        assert_eq!(
            l.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(),
            vec!["deploy-checklist", "Deploy Checklist"]
        );
        assert_eq!(norm_name("Deploy  Checklist"), "deploy-checklist");
        let i =
            at_imports("@docs/testing.md\nmail a@b.org\nSee @README.md.\n@~/../../etc/passwd\n");
        assert_eq!(
            i.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(),
            vec!["docs/testing.md", "README.md", "~/../../etc/passwd"]
        );
    }
}
