//! Session transcripts (spec C18 §8.10.7): find the session logs of Claude Code, Codex,
//! Gemini CLI, Cursor, GitHub Copilot and Aider, and of a file the person names, parse
//! each with its adapter in `sparkles_memory_import::transcripts`, render each session as
//! Markdown with one `## user` or `## assistant` heading per turn, redact it, and register
//! it as the source of the session's graph with the `transcript` profile, which starts a
//! chunk at every heading.
//!
//! A transcript becomes an episode, not facts: the graph holds the session's description
//! (`mem:Session` with its id, branch, model, harness version and times) and the link
//! from each memory file the session wrote with `Write` or `Edit` to the session. Those
//! links are written in the session's graph, so a sync of the memory file never has to
//! know about them.
//!
//! By default a rendition keeps the text of the user's and the assistant's messages on
//! the main chain. Thinking and reasoning, tool calls and results, attachments, system
//! and bookkeeping lines are left out. `--transcript-content tools` adds tool calls and
//! the first 2,000 characters of each tool result. A file that no adapter claims is read
//! by the generic adapter. A session longer than a source holds is split at turn boundaries into at most [`MAX_PARTS`] parts, `<graph>/part-N` after
//! the first.

use super::conn::{CmdError, Conn, val};
use super::sync::{self, Cache, CacheEntry, FileReport, RegisterText, SyncOpts, count, normalize};
use serde_json::Value;
use sparkles_memory_import::redact::{self, Pattern};
use sparkles_memory_import::transcripts::{self, Format, Session, render, split};
use sparkles_memory_import::vocab::{self, XSD_DATETIME, XSD_INTEGER, mem};
use sparkles_memory_import::{Ctx, Fact, FileImport, FileKind, Harness, Obj, Project, Roots};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The most parts of one session.
pub const MAX_PARTS: usize = 8;
/// The most bytes of one part: a source's limit less room for the note of the last part.
const PART_BYTES: usize = (2 << 20) - 4096;
/// A session whose last line is younger than this is in progress.
const IN_PROGRESS: Duration = Duration::from_secs(600);
/// The format that makes every heading start a chunk.
pub const FORMAT: &str = "text/markdown;profile=transcript";

/// What a transcript import reads.
#[derive(Clone, Debug)]
pub struct Opts {
    pub tools: bool,
    pub subagents: bool,
    pub since: Duration,
    pub max_bytes: u64,
    /// the transcript a session end hook named, imported even when it is recent
    pub named: Option<PathBuf>,
}

/// `30d`, `12h`, `90m` or `45s`.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = n.parse().ok()?;
    let secs = match unit {
        "d" => n * 86_400,
        "h" => n * 3600,
        "m" => n * 60,
        "s" => n,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

/// One transcript file found for a project.
#[derive(Clone, Debug)]
pub struct Found {
    pub harness: Harness,
    pub path: PathBuf,
    /// a subagent's transcript, with its parent session's id
    pub parent: Option<String>,
}

/// The transcripts of a project: Claude Code's under the project's directory, with
/// subagents when asked, Codex's sessions whose `cwd` lies in the project, and the logs
/// that each adapter of `sparkles_memory_import::transcripts` finds for the project.
pub fn find(
    roots: &Roots,
    project: &Project,
    harnesses: &[Harness],
    subagents: bool,
) -> Vec<Found> {
    let mut out = Vec::new();
    if harnesses.contains(&Harness::ClaudeCode)
        && let Some(dir) = roots.claude_project_dir(project)
        && let Ok(rd) = std::fs::read_dir(&dir)
    {
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        files.sort();
        for p in files {
            if subagents {
                let stem = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if let Ok(rd) = std::fs::read_dir(dir.join(&stem).join("subagents")) {
                    let mut subs: Vec<PathBuf> = rd
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                        .collect();
                    subs.sort();
                    out.extend(subs.into_iter().map(|path| Found {
                        harness: Harness::ClaudeCode,
                        path,
                        parent: Some(stem.clone()),
                    }));
                }
            }
            out.push(Found {
                harness: Harness::ClaudeCode,
                path: p,
                parent: None,
            });
        }
    }
    if harnesses.contains(&Harness::Codex) {
        let mut stack = vec![roots.codex.join("sessions")];
        let mut files = Vec::new();
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                match e.file_type() {
                    Ok(t) if t.is_dir() => stack.push(p),
                    Ok(t) if t.is_file() && p.extension().is_some_and(|x| x == "jsonl") => {
                        files.push(p)
                    }
                    _ => {}
                }
            }
        }
        files.sort();
        for p in files {
            if codex_cwd(&p).is_some_and(|c| Path::new(&c).starts_with(&project.root)) {
                out.push(Found {
                    harness: Harness::Codex,
                    path: p,
                    parent: None,
                });
            }
        }
    }
    for (harness, locate) in [
        (Harness::GeminiCli, transcripts::gemini::locate as Locate),
        (Harness::Cursor, transcripts::cursor::locate),
        (Harness::Copilot, transcripts::copilot::locate),
        (Harness::Aider, transcripts::aider::locate),
    ] {
        if harnesses.contains(&harness) {
            out.extend(locate(roots, project).into_iter().map(|path| Found {
                harness,
                path,
                parent: None,
            }));
        }
    }
    out
}

type Locate = fn(&Roots, &Project) -> Vec<PathBuf>;

/// The `cwd` of a Codex session's first lines.
fn codex_cwd(p: &Path) -> Option<String> {
    use std::io::BufRead;
    let f = std::fs::File::open(p).ok()?;
    for line in std::io::BufReader::new(f).lines().take(5) {
        let j: Value = serde_json::from_str(&line.ok()?).ok()?;
        if j["type"] == "session_meta" {
            return j["payload"]["cwd"].as_str().map(str::to_string);
        }
    }
    None
}

/// The Codex transcript of a session id, for a hook that names only the id.
pub fn codex_by_id(roots: &Roots, id: &str) -> Option<PathBuf> {
    let mut stack = vec![roots.codex.join("sessions")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains(id))
            {
                return Some(p);
            }
        }
    }
    None
}

fn age(p: &Path, last: Option<&str>) -> Option<Duration> {
    let t = last
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&chrono::Utc))
        .or_else(|| {
            std::fs::metadata(p)
                .ok()?
                .modified()
                .ok()
                .map(chrono::DateTime::<chrono::Utc>::from)
        })?;
    (chrono::Utc::now() - t)
        .to_std()
        .ok()
        .or(Some(Duration::ZERO))
}

fn time_obj(t: &str) -> Obj {
    match chrono::DateTime::parse_from_rfc3339(t) {
        Ok(d) => Obj::typed(
            d.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            XSD_DATETIME,
        ),
        Err(_) => Obj::lit(t),
    }
}

fn fact(s: &str, p: &str, o: Obj) -> Fact {
    Fact {
        s: s.to_string(),
        p: p.to_string(),
        o,
        quote: None,
    }
}

/// Everything one transcript sync needs.
pub struct Run<'a> {
    pub conn: &'a Conn,
    pub ctx: &'a Ctx,
    pub roots: &'a Roots,
    pub project: &'a Project,
    pub harnesses: Vec<Harness>,
    pub patterns: &'a [Pattern],
    /// the graph of each scanned memory or instruction file, by path
    pub file_graphs: HashMap<PathBuf, String>,
    pub opts: Opts,
    pub sync: &'a SyncOpts,
}

/// The digests of every source of the principal's session graphs, by graph and source.
fn session_digests(
    conn: &Conn,
    prefix: &str,
) -> Result<HashMap<String, BTreeMap<String, String>>, CmdError> {
    let q = format!(
        "SELECT ?g ?src ?d WHERE {{ GRAPH ?g {{ ?src <{}> ?d }} \
         FILTER(STRSTARTS(STR(?g), {}) && CONTAINS(STR(?g), \"/sessions/\")) \
         FILTER NOT EXISTS {{ GRAPH ?g {{ ?g <{}> ?inv }} }} }}",
        vocab::SPK_CONTENT_DIGEST,
        sparkles_memory_import::quoted(prefix),
        vocab::PROV_INVALIDATED_AT,
    );
    let mut out: HashMap<String, BTreeMap<String, String>> = HashMap::new();
    for r in conn.select(&q)?.rows {
        if let (Some(g), Some(s), Some(d)) = (val(&r, "g"), val(&r, "src"), val(&r, "d")) {
            out.entry(g).or_default().insert(s, d);
        }
    }
    Ok(out)
}

/// Import the project's transcripts. Returns a report per transcript.
pub fn sync(run: &Run, cache: &mut Cache) -> Result<Vec<FileReport>, CmdError> {
    let mut found = find(run.roots, run.project, &run.harnesses, run.opts.subagents);
    let named = run
        .opts
        .named
        .as_ref()
        .map(|p| sparkles_memory_import::project::absolute(p));
    // a file the person names that no adapter located is read by its detected format,
    // and by the generic adapter when none claims it
    if let Some(n) = &named
        && n.is_file()
        && !found.iter().any(|f| f.path == *n)
    {
        found.push(Found {
            harness: Harness::Generic,
            path: n.clone(),
            parent: None,
        });
    }
    let digests = session_digests(run.conn, &run.sync.prefix)?;
    let mut reports = Vec::new();
    let mut budget = run.opts.max_bytes;
    let seg = run.project.segment();
    let project_iri = run.ctx.project_iri(&run.project.key);
    for f in found {
        let path_key = f.path.display().to_string();
        let mut rep = FileReport {
            path: path_key.clone(),
            harness: f.harness.segment().into(),
            kind: "transcript".into(),
            ..Default::default()
        };
        let is_named = named.as_ref().is_some_and(|n| *n == f.path);
        let stamp = sync::stamp(&f.path);
        if !is_named
            && let Some(e) = cache.files.get(&path_key)
            && stamp.is_some_and(|(m, l)| m == e.mtime && l == e.len)
            && digests.contains_key(&e.graph)
        {
            rep.graph = e.graph.clone();
            rep.status = "unchanged".into();
            reports.push(rep);
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&f.path) else {
            rep.status = "skipped".into();
            rep.reason = Some("unreadable".into());
            reports.push(rep);
            continue;
        };
        let format = if f.harness == Harness::Generic {
            transcripts::detect(&f.path, &raw)
        } else {
            Format::of(f.harness)
        };
        let f = Found {
            harness: format.harness(),
            ..f
        };
        let mut s = format.parse(
            &raw,
            &transcripts::Opts {
                tools: run.opts.tools,
                sidechain: f.parent.is_some(),
            },
        );
        drop(raw);
        let stem = f
            .path
            .file_stem()
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_default();
        let id = match (&f.parent, &s.id) {
            (Some(parent), _) => format!("{parent}.{stem}"),
            (None, Some(id)) => id.clone(),
            (None, None) => stem.clone(),
        };
        s.id = Some(id.clone());
        let graph = run.ctx.graph(
            f.harness.segment(),
            &seg,
            &format!("sessions/{}", sparkles_memory_import::ids::segment(&id)),
        );
        rep.graph = graph.clone();
        let old = age(&f.path, s.ended.as_deref());
        if old.is_some_and(|a| a > run.opts.since) {
            rep.status = "skipped".into();
            rep.reason = Some("older than --since".into());
            reports.push(rep);
            continue;
        }
        if !is_named && old.is_some_and(|a| a < IN_PROGRESS) {
            rep.status = "skipped".into();
            rep.reason = Some("in progress (last line under 10 minutes old)".into());
            reports.push(rep);
            continue;
        }
        if s.turns.is_empty() {
            rep.status = "skipped".into();
            rep.reason = Some("no messages".into());
            reports.push(rep);
            continue;
        }
        // redaction over the whole rendering, then the parts
        let turns = render(&s);
        let mut names = Vec::new();
        let turns: Vec<String> = turns
            .into_iter()
            .map(|t| {
                let r = redact::redact(&t, run.patterns);
                names.extend(r.names);
                normalize(&r.text)
            })
            .collect();
        rep.redactions = names.clone();
        let parts = split(&turns, PART_BYTES, MAX_PARTS);
        let total: u64 = parts.iter().map(|p| p.len() as u64).sum();
        let iri_of = |k: usize| {
            if k == 0 {
                graph.clone()
            } else {
                format!("{graph}/part-{}", k + 1)
            }
        };
        let expected: Vec<String> = parts
            .iter()
            .map(|p| sparkles_memory_import::ids::digest(p.as_bytes()))
            .collect();
        let server = digests.get(&graph);
        let same = server.is_some_and(|m| {
            expected
                .iter()
                .enumerate()
                .all(|(k, d)| m.get(&iri_of(k)) == Some(d))
        });
        let entry = CacheEntry {
            digest: expected.first().cloned().unwrap_or_default(),
            body_digest: String::new(),
            graph: graph.clone(),
            mtime: stamp.map_or(0, |s| s.0),
            len: stamp.map_or(0, |s| s.1),
        };
        if same {
            rep.status = "unchanged".into();
            if !run.sync.dry_run {
                cache.files.insert(path_key, entry);
            }
            reports.push(rep);
            continue;
        }
        if total > budget {
            rep.status = "skipped".into();
            rep.reason =
                Some("over --max-transcript-bytes for this sync; the next sync imports it".into());
            reports.push(rep);
            continue;
        }
        budget -= total;
        let message = format!(
            "sparkles memory sync: session {id} ({})",
            f.harness.segment()
        );
        let result = (|| -> Result<(), CmdError> {
            for (k, part) in parts.iter().enumerate() {
                let iri = iri_of(k);
                if server.and_then(|m| m.get(&iri)) == Some(&expected[k]) {
                    continue;
                }
                let title = if parts.len() == 1 {
                    format!("Session {id}")
                } else {
                    format!("Session {id}, part {} of {}", k + 1, parts.len())
                };
                let out = sync::register_text(
                    run.conn,
                    RegisterText {
                        graph: &graph,
                        iri: &iri,
                        title: &title,
                        format: FORMAT,
                        text: part,
                        original: None,
                    },
                    false,
                    None,
                    &message,
                    run.sync,
                )?;
                rep.retracted += count(&out, "retracted");
            }
            let facts = session_facts(
                run,
                &s,
                &graph,
                &project_iri,
                f.harness,
                &f.path,
                names.len(),
            );
            let fi = FileImport {
                adapter: sparkles_memory_import::Adapter::Generic,
                harness: f.harness,
                kind: FileKind::Transcript,
                scope: None,
                path: f.path.clone(),
                rel_path: f
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                key: id.clone(),
                graph: graph.clone(),
                entity: Some(run.ctx.session_iri(f.harness.segment(), &id)),
                digest: expected[0].clone(),
                body_digest: String::new(),
                title: format!("Session {id}"),
                redactions: names.clone(),
                facts,
                text: parts[0].clone(),
            };
            let current = if server.is_some() {
                sync::current_facts(run.conn, &graph)?
            } else {
                Default::default()
            };
            let ch = sync::diff(&fi, &current, false, None);
            rep.added = ch.adds.len() - ch.replaced;
            rep.replaced = ch.replaced;
            rep.retracted += ch.retracts.len();
            rep.commit = sync::write(
                run.conn,
                &graph,
                ch.adds,
                ch.retracts,
                &message,
                (
                    server
                        .and_then(|m| m.get(&graph))
                        .map_or("", String::as_str),
                    &expected.join(","),
                    None,
                ),
                f.harness.segment(),
                run.sync,
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                rep.status = if server.is_some() { "edited" } else { "new" }.into();
                if !run.sync.dry_run {
                    cache.files.insert(path_key, entry);
                }
            }
            Err(e) if e.exit == super::EXIT_UNREACHABLE => return Err(e),
            Err(e) => {
                rep.status = "failed".into();
                rep.error = Some(super::conn::err_json(&e));
            }
        }
        reports.push(rep);
    }
    Ok(reports)
}

/// The session's description, and the links from the memory files it wrote.
fn session_facts(
    run: &Run,
    s: &Session,
    graph: &str,
    project_iri: &str,
    harness: Harness,
    path: &Path,
    redactions: usize,
) -> Vec<Fact> {
    let id = s.id.clone().unwrap_or_default();
    let sess = run.ctx.session_iri(harness.segment(), &id);
    let mut f = vec![
        fact(graph, &mem("harness"), Obj::Iri(harness.iri())),
        fact(graph, &mem("project"), Obj::Iri(project_iri.to_string())),
        fact(
            graph,
            &mem("filePath"),
            Obj::lit(
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
        ),
        fact(
            graph,
            &mem("redactions"),
            Obj::typed(redactions.to_string(), XSD_INTEGER),
        ),
        fact(graph, vocab::PROV_GENERATED_BY, Obj::Iri(sess.clone())),
        fact(project_iri, vocab::RDF_TYPE, Obj::Iri(mem("Project"))),
        fact(
            project_iri,
            vocab::RDFS_LABEL,
            Obj::lit(run.project.key.clone()),
        ),
        fact(&sess, vocab::RDF_TYPE, Obj::Iri(mem("Session"))),
        fact(&sess, &mem("sessionId"), Obj::lit(id.clone())),
        fact(&sess, &mem("harness"), Obj::Iri(harness.iri())),
        fact(&sess, &mem("project"), Obj::Iri(project_iri.to_string())),
        fact(&sess, vocab::RDFS_LABEL, Obj::lit(format!("Session {id}"))),
    ];
    for (p, v) in [
        ("gitBranch", &s.branch),
        ("model", &s.model),
        ("harnessVersion", &s.version),
    ] {
        if let Some(v) = v {
            f.push(fact(&sess, &mem(p), Obj::lit(v.clone())));
        }
    }
    if let Some(t) = &s.started {
        f.push(fact(&sess, vocab::PROV_STARTED_AT, time_obj(t)));
    }
    if let Some(t) = &s.ended {
        f.push(fact(&sess, vocab::PROV_ENDED_AT, time_obj(t)));
    }
    // §8.10.7: the memory files the session wrote
    let mut linked = std::collections::BTreeSet::new();
    for w in &s.writes {
        let p = PathBuf::from(w);
        let p = if p.is_absolute() {
            p
        } else {
            s.cwd
                .as_deref()
                .map_or(p.clone(), |c| Path::new(c).join(&p))
        };
        let p = sparkles_memory_import::project::normalize(&p);
        if let Some(g) = run.file_graphs.get(&p)
            && linked.insert(g.clone())
        {
            f.push(fact(g, vocab::PROV_GENERATED_BY, Obj::Iri(sess.clone())));
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(
            parse_duration("30d"),
            Some(Duration::from_secs(30 * 86_400))
        );
        assert_eq!(parse_duration("90m"), Some(Duration::from_secs(5400)));
        assert_eq!(parse_duration("x"), None);
    }
}
