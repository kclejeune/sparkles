//! `import` and `sync` (spec C18 §8.10.6 and §10.2): compare the scanned files with the
//! sources on the server and write the difference with `POST /{ds}/facts`.
//!
//! The server holds the record. A file's graph is also its source's IRI, and the
//! source's `spk:contentDigest` says what was imported. A sync lists the principal's
//! import graphs with their digests in one query, and then per file:
//!
//! - a graph that does not exist yet gets every structural fact, with its quote;
//! - a graph with another digest is diffed against the structural facts it holds now:
//!   single-valued predicates are replaced (the old value is superseded), new values of
//!   other predicates are added, and values the file no longer gives are retracted;
//! - a file whose graph is gone but whose bytes match a source that lost its file is a
//!   rename: the new graph is written with `dcterms:replaces` the old one, and the old
//!   one is treated as deleted;
//! - a source under a fully scanned area without a file is deleted: its facts are
//!   retracted and the source description stays, with `prov:invalidatedAtTime`.
//!
//! Facts an agent extracted from a file are left alone, because the diff reads back only
//! the import's structural predicates. The local cache only skips the facts query of a
//! file whose digest and modification time are unchanged.

use super::conn::{CmdError, Conn, obj, val};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sparkles_memory_import::vocab::{self, MEM, XSD_DATETIME};
use sparkles_memory_import::{Fact, FileImport, Obj, Scan};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
const PROV: &str = "http://www.w3.org/ns/prov#";
/// What `assert_facts` takes in one call.
const MAX_PER_CALL: usize = 400;

/// The local state of one server and dataset: each file's last imported digest and
/// modification time.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cache {
    #[serde(default)]
    pub files: BTreeMap<String, CacheEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheEntry {
    pub digest: String,
    pub body_digest: String,
    pub graph: String,
    pub mtime: i128,
    pub len: u64,
}

/// The state directory: `$XDG_STATE_HOME/sparkles/memory`, else
/// `~/.local/state/sparkles/memory`.
pub fn state_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".local/state")
        });
    base.join("sparkles").join("memory")
}

/// The file stem of the state of one server, dataset and principal.
pub fn state_key(label: &str, dataset: &str, principal: &str) -> String {
    let h = Sha256::digest(format!("{label}\0{dataset}\0{principal}").as_bytes());
    h.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

impl Cache {
    pub fn load(path: &Path) -> Cache {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap_or_default())?;
        std::fs::rename(tmp, path)
    }
}

/// A file's modification time in nanoseconds, and its length.
pub fn stamp(p: &Path) -> Option<(i128, u64)> {
    let m = std::fs::metadata(p).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos() as i128;
    Some((t, m.len()))
}

/// One source on the server.
#[derive(Clone, Debug)]
pub struct Listed {
    pub digest: String,
    pub file_path: Option<String>,
    pub deleted: bool,
}

/// The import graphs under `prefix` with their digests, and the head they were read at.
pub fn list_sources(
    conn: &Conn,
    prefix: &str,
) -> Result<(BTreeMap<String, Listed>, Option<u64>), CmdError> {
    let q = format!(
        "SELECT ?g ?d ?fp ?inv WHERE {{ GRAPH ?g {{ ?g <{}> ?d \
         OPTIONAL {{ ?g <{MEM}filePath> ?fp }} OPTIONAL {{ ?g <{PROV}invalidatedAtTime> ?inv }} }} \
         FILTER(STRSTARTS(STR(?g), {})) }}",
        vocab::SPK_CONTENT_DIGEST,
        sparkles_memory_import::quoted(prefix)
    );
    let sel = conn.select(&q)?;
    let mut out = BTreeMap::new();
    for row in &sel.rows {
        let (Some(g), Some(d)) = (val(row, "g"), val(row, "d")) else {
            continue;
        };
        out.insert(
            g,
            Listed {
                digest: d,
                file_path: val(row, "fp"),
                deleted: val(row, "inv").is_some(),
            },
        );
    }
    Ok((out, sel.head))
}

/// The structural facts a graph holds now: no reifier, activity or agent.
fn current_facts(conn: &Conn, graph: &str) -> Result<BTreeSet<(String, String, Obj)>, CmdError> {
    let q = format!(
        "SELECT ?s ?p ?o WHERE {{ GRAPH <{graph}> {{ ?s ?p ?o }} \
         FILTER NOT EXISTS {{ GRAPH <{graph}> {{ ?s <{RDF_REIFIES}> ?x }} }} \
         FILTER NOT EXISTS {{ GRAPH <{graph}> {{ ?s a ?k VALUES ?k {{ <{PROV}Activity> <{PROV}SoftwareAgent> }} }} }} }}"
    );
    let sel = conn.select(&q)?;
    let mut out = BTreeSet::new();
    for row in &sel.rows {
        let (Some(s), Some(p), Some(o)) = (val(row, "s"), val(row, "p"), obj(row, "o")) else {
            continue;
        };
        if row["s"]["type"] != "uri" || !vocab::structural(&p) {
            continue;
        }
        out.insert((s, p, o));
    }
    Ok(out)
}

/// What one sync did to one file.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReport {
    pub path: String,
    pub graph: String,
    pub harness: String,
    pub kind: String,
    /// `new`, `edited`, `renamed`, `deleted`, `unchanged`, `skipped` or `failed`
    pub status: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub added: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub replaced: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub retracted: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<u64>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl FileReport {
    /// One line for people, such as `staging-db.md: edited, 1 fact replaced, 1 added`.
    pub fn line(&self) -> String {
        let mut parts = vec![self.status.clone()];
        let n = |k: usize, what: &str| format!("{k} fact{} {what}", if k == 1 { "" } else { "s" });
        if self.added > 0 {
            parts.push(n(self.added, "added"));
        }
        if self.replaced > 0 {
            parts.push(n(self.replaced, "replaced"));
        }
        if self.retracted > 0 {
            parts.push(n(self.retracted, "retracted"));
        }
        if !self.redactions.is_empty() {
            parts.push(format!("{} redacted", self.redactions.len()));
        }
        if let Some(r) = &self.renamed_from {
            parts.push(format!("from {r}"));
        }
        if let Some(r) = &self.reason {
            parts.push(r.clone());
        }
        if let Some(e) = &self.error {
            parts.push(e["error"].as_str().unwrap_or("failed").to_string());
        }
        format!("{}: {}", self.path, parts.join(", "))
    }
}

/// The options of one sync.
pub struct SyncOpts {
    pub branch: Option<String>,
    pub dry_run: bool,
    /// only this file (a hook's)
    pub only: Option<PathBuf>,
    /// deletions only under `…/instructions/`
    pub instructions_only: bool,
    /// the principal's prefix: `<base><principal>/`
    pub prefix: String,
}

struct Change {
    adds: Vec<Value>,
    replaced: usize,
    retracts: Vec<Value>,
}

fn fact_json(f: &Fact, mode: Option<&str>) -> Value {
    let mut j = json!({ "s": format!("<{}>", f.s), "p": format!("<{}>", f.p), "o": f.o.sparql() });
    if let Some(m) = mode {
        j["mode"] = m.into();
    }
    if let Some(q) = &f.quote
        && !q.trim().is_empty()
    {
        j["quote"] = q.clone().into();
    }
    j
}

fn retract_json(s: &str, p: &str, o: &Obj, graph: &str) -> Value {
    json!({ "s": format!("<{s}>"), "p": format!("<{p}>"), "o": o.sparql(), "graph": graph })
}

/// The changes that turn `current` into the file's facts.
fn diff(file: &FileImport, current: &BTreeSet<(String, String, Obj)>) -> Change {
    let mut wanted: BTreeMap<(String, String, Obj), &Fact> = BTreeMap::new();
    for f in &file.facts {
        wanted
            .entry((f.s.clone(), f.p.clone(), f.o.clone()))
            .or_insert(f);
    }
    let mut adds = Vec::new();
    let mut replaced = 0;
    let mut replaced_sp: BTreeSet<(String, String)> = BTreeSet::new();
    for (k, f) in &wanted {
        if current.contains(k) {
            continue;
        }
        let single =
            vocab::single_valued(&f.p) && current.iter().any(|(s, p, _)| s == &f.s && p == &f.p);
        if single {
            replaced += 1;
            replaced_sp.insert((f.s.clone(), f.p.clone()));
            adds.push(fact_json(f, Some("replace")));
        } else {
            adds.push(fact_json(f, None));
        }
    }
    let mut retracts = Vec::new();
    for k in current {
        // a rename's link to the old source is the sync's own, not the file's
        if wanted.contains_key(k)
            || replaced_sp.contains(&(k.0.clone(), k.1.clone()))
            || k.1 == vocab::DCT_REPLACES
        {
            continue;
        }
        retracts.push(retract_json(&k.0, &k.1, &k.2, &file.graph));
    }
    Change {
        adds,
        replaced,
        retracts,
    }
}

/// The predicates of a deleted source's description, which stay.
fn kept_on_delete(p: &str) -> bool {
    [
        vocab::RDF_TYPE,
        vocab::DCT_TITLE,
        vocab::DCT_FORMAT,
        vocab::SPK_CONTENT_DIGEST,
        vocab::DCT_MODIFIED,
    ]
    .contains(&p)
        || p.strip_prefix(MEM)
            .is_some_and(|l| matches!(l, "harness" | "filePath" | "redactions" | "project"))
}

fn idem_key(graph: &str, old: &str, new: &str, head: Option<u64>, part: usize) -> String {
    let h = Sha256::digest(format!("{graph}\0{old}\0{new}\0{head:?}\0{part}").as_bytes());
    let hex: String = h.iter().take(24).map(|b| format!("{b:02x}")).collect();
    format!("import:{hex}")
}

/// Send the changes of one graph in calls of at most [`MAX_PER_CALL`] items. Returns the
/// last commit.
#[allow(clippy::too_many_arguments)]
fn write(
    conn: &Conn,
    graph: &str,
    adds: Vec<Value>,
    retracts: Vec<Value>,
    message: &str,
    keys: (&str, &str, Option<u64>),
    opts: &SyncOpts,
) -> Result<Option<u64>, CmdError> {
    let mut commit = None;
    let mut part = 0;
    let mut adds = adds.into_iter().peekable();
    let mut retracts = retracts.into_iter().peekable();
    while adds.peek().is_some() || retracts.peek().is_some() || part == 0 {
        let a: Vec<Value> = adds.by_ref().take(MAX_PER_CALL).collect();
        let r: Vec<Value> = if a.is_empty() {
            retracts.by_ref().take(MAX_PER_CALL).collect()
        } else {
            Vec::new()
        };
        if a.is_empty() && r.is_empty() {
            break;
        }
        let mut args = json!({
            "graph": graph,
            "source": { "iri": graph },
            "facts": a,
            "message": message,
            "idempotencyKey": idem_key(graph, keys.0, keys.1, keys.2, part),
            "allowUnknownIris": true,
        });
        if !r.is_empty() {
            args["retract"] = r.into();
        }
        if opts.dry_run {
            args["dryRun"] = true.into();
        }
        if let Some(b) = &opts.branch {
            args["branch"] = b.clone().into();
        }
        let out = conn.tool("facts", &args)?;
        if let Some(c) = out["commit"].as_u64() {
            commit = Some(c);
        }
        part += 1;
    }
    Ok(commit)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Run one sync. Returns a report per file, in path order.
pub fn sync(
    conn: &Conn,
    scan: &Scan,
    cache: &mut Cache,
    opts: &SyncOpts,
) -> Result<Vec<FileReport>, CmdError> {
    let (listed, head) = list_sources(conn, &opts.prefix)?;
    let mut reports = Vec::new();
    let only = opts
        .only
        .as_ref()
        .map(|p| sparkles_memory_import::project::absolute(p));
    let scanned: HashMap<&str, &FileImport> =
        scan.files.iter().map(|f| (f.graph.as_str(), f)).collect();
    // sources that lost their file: under a scanned area and not in the scan
    let mut gone: BTreeSet<String> = BTreeSet::new();
    if only.is_none() {
        for (g, l) in &listed {
            if l.deleted || scanned.contains_key(g.as_str()) {
                continue;
            }
            let in_area = scan.areas.iter().any(|a| g.starts_with(a.as_str()));
            if !in_area || (opts.instructions_only && !g.contains("/instructions/")) {
                continue;
            }
            gone.insert(g.clone());
        }
    }
    // the digests of the sources that lost their file, for renames
    let mut by_digest: HashMap<&str, &str> = HashMap::new();
    for g in &gone {
        by_digest.insert(listed[g].digest.as_str(), g.as_str());
    }
    let mut by_body: HashMap<String, String> = HashMap::new();
    for (path, e) in &cache.files {
        if gone.contains(&e.graph) && !Path::new(path).exists() {
            by_body.insert(e.body_digest.clone(), e.graph.clone());
        }
    }
    let mut renamed_away: BTreeSet<String> = BTreeSet::new();
    for s in scan.skipped.iter().filter(|_| only.is_none()) {
        reports.push(FileReport {
            path: s.path.display().to_string(),
            status: "skipped".into(),
            reason: Some(s.reason.clone()),
            ..Default::default()
        });
    }
    let mut files: Vec<&FileImport> = scan.files.iter().collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    for f in files {
        if let Some(o) = &only
            && &f.path != o
        {
            continue;
        }
        let path_key = f.path.display().to_string();
        let mut rep = FileReport {
            path: path_key.clone(),
            graph: f.graph.clone(),
            harness: f.harness.segment().into(),
            kind: f.kind.name().into(),
            redactions: f.redactions.clone(),
            ..Default::default()
        };
        let stamp = stamp(&f.path);
        let server = listed.get(&f.graph);
        let cached = cache.files.get(&path_key);
        // unchanged: the server has this digest, and the source was not deleted
        if let Some(l) = server
            && !l.deleted
            && l.digest == f.digest
            && l.file_path.as_deref() == Some(f.rel_path.as_str())
        {
            rep.status = "unchanged".into();
            if !opts.dry_run {
                cache.files.insert(path_key, entry(f, stamp));
            }
            reports.push(rep);
            continue;
        }
        let _ = cached;
        let message = format!(
            "sparkles memory sync: {} ({})",
            f.rel_path,
            f.harness.segment()
        );
        let result = (|| -> Result<(), CmdError> {
            match server {
                Some(l) => {
                    // edited, or recreated after a deletion
                    let current = current_facts(conn, &f.graph)?;
                    let ch = diff(f, &current);
                    rep.status = if l.deleted { "new" } else { "edited" }.into();
                    rep.added = ch.adds.len() - ch.replaced;
                    rep.replaced = ch.replaced;
                    rep.retracted = ch.retracts.len();
                    rep.commit = write(
                        conn,
                        &f.graph,
                        ch.adds,
                        ch.retracts,
                        &message,
                        (&l.digest, &f.digest, head),
                        opts,
                    )?;
                }
                None => {
                    let from = by_digest
                        .get(f.digest.as_str())
                        .map(|g| g.to_string())
                        .or_else(|| by_body.get(&f.body_digest).cloned())
                        .filter(|g| !renamed_away.contains(g));
                    let mut adds: Vec<Value> = f.facts.iter().map(|x| fact_json(x, None)).collect();
                    rep.status = "new".into();
                    if let Some(old) = &from {
                        adds.push(json!({ "s": format!("<{}>", f.graph), "p": format!("<{}>", vocab::DCT_REPLACES), "o": format!("<{old}>") }));
                        rep.status = "renamed".into();
                        rep.renamed_from = listed
                            .get(old)
                            .and_then(|l| l.file_path.clone())
                            .or_else(|| Some(old.clone()));
                        renamed_away.insert(old.clone());
                    }
                    rep.added = adds.len();
                    rep.commit = write(
                        conn,
                        &f.graph,
                        adds,
                        Vec::new(),
                        &message,
                        ("", &f.digest, head),
                        opts,
                    )?;
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                if !opts.dry_run {
                    cache.files.insert(path_key, entry(f, stamp));
                }
            }
            Err(e) if e.exit == super::EXIT_UNREACHABLE => return Err(e),
            Err(e) => {
                rep.status = "failed".into();
                rep.error = Some(super::conn::err_json(&e));
                cache.files.remove(&path_key);
            }
        }
        reports.push(rep);
    }
    // deletions, and the old graphs of renames
    for g in &gone {
        let l = &listed[g];
        let mut rep = FileReport {
            path: l.file_path.clone().unwrap_or_else(|| g.clone()),
            graph: g.clone(),
            status: "deleted".into(),
            ..Default::default()
        };
        if renamed_away.contains(g) {
            rep.reason = Some("renamed".into());
        }
        let result = (|| -> Result<(), CmdError> {
            let current = current_facts(conn, g)?;
            let retracts: Vec<Value> = current
                .iter()
                .filter(|(s, p, _)| !(s == g && kept_on_delete(p)))
                .map(|(s, p, o)| retract_json(s, p, o, g))
                .collect();
            rep.retracted = retracts.len();
            let at = json!({
                "s": format!("<{g}>"),
                "p": format!("<{}>", vocab::PROV_INVALIDATED_AT),
                "o": Obj::typed(now_rfc3339(), XSD_DATETIME).sparql(),
            });
            // the invalidation first, then the retractions (one call per kind)
            rep.commit = write(
                conn,
                g,
                vec![at],
                Vec::new(),
                &format!("sparkles memory sync: {} deleted", rep.path),
                (&l.digest, "deleted", head),
                opts,
            )?;
            if !retracts.is_empty() {
                rep.commit = write(
                    conn,
                    g,
                    Vec::new(),
                    retracts,
                    &format!("sparkles memory sync: {} deleted", rep.path),
                    (&l.digest, "deleted-facts", head),
                    opts,
                )?
                .or(rep.commit);
            }
            Ok(())
        })();
        if let Err(e) = result {
            if e.exit == super::EXIT_UNREACHABLE {
                return Err(e);
            }
            rep.status = "failed".into();
            rep.error = Some(super::conn::err_json(&e));
        } else if !opts.dry_run {
            cache.files.retain(|_, e| &e.graph != g);
        }
        reports.push(rep);
    }
    if !opts.dry_run {
        cache.last_sync = Some(now_rfc3339());
    }
    Ok(reports)
}

fn entry(f: &FileImport, stamp: Option<(i128, u64)>) -> CacheEntry {
    let (mtime, len) = stamp.unwrap_or((0, 0));
    CacheEntry {
        digest: f.digest.clone(),
        body_digest: f.body_digest.clone(),
        graph: f.graph.clone(),
        mtime,
        len,
    }
}

/// Whether the cache says no scanned file changed since the last sync, so a sync may
/// skip the server entirely (files only; deletions are found by the listing).
pub fn unchanged_by_cache(scan: &Scan, cache: &Cache) -> bool {
    scan.files.iter().all(|f| {
        cache
            .files
            .get(&f.path.display().to_string())
            .is_some_and(|e| e.digest == f.digest && Some((e.mtime, e.len)) == stamp(&f.path))
    }) && cache.files.keys().all(|p| Path::new(p).exists())
}

/// The lock of a sync: a held lock marks `again` and returns `None`.
pub struct Lock {
    _file: std::fs::File,
    again: PathBuf,
}

impl Lock {
    pub fn take(dir: &Path, key: &str) -> Result<Option<Lock>, CmdError> {
        std::fs::create_dir_all(dir)?;
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join(format!("{key}.lock")))?;
        let again = dir.join(format!("{key}.again"));
        match f.try_lock() {
            Ok(()) => {
                let _ = std::fs::remove_file(&again);
                Ok(Some(Lock { _file: f, again }))
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                std::fs::write(&again, b"")?;
                Ok(None)
            }
            Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }

    /// Whether another sync asked for one more run while this one held the lock; the
    /// mark is cleared.
    pub fn again(&self) -> bool {
        std::fs::remove_file(&self.again).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fi(facts: Vec<Fact>) -> FileImport {
        FileImport {
            adapter: sparkles_memory_import::Adapter::ClaudeCode,
            harness: sparkles_memory_import::Harness::ClaudeCode,
            kind: sparkles_memory_import::FileKind::Memory,
            scope: None,
            path: "/x/a.md".into(),
            rel_path: "a.md".into(),
            key: "a".into(),
            graph: "urn:g".into(),
            entity: None,
            digest: "d".into(),
            body_digest: "b".into(),
            title: "a.md".into(),
            redactions: vec![],
            facts,
        }
    }

    fn f(s: &str, p: &str, o: Obj) -> Fact {
        Fact {
            s: s.into(),
            p: p.into(),
            o,
            quote: None,
        }
    }

    #[test]
    fn diff_replaces_single_values_and_retracts_the_rest() {
        let desc = vocab::SCHEMA_DESCRIPTION;
        let refs = vocab::DCT_REFERENCES;
        let file = fi(vec![
            f("urn:m", desc, Obj::lit("new")),
            f("urn:m", refs, Obj::Iri("urn:b".into())),
            f("urn:m", refs, Obj::Iri("urn:c".into())),
        ]);
        let current: BTreeSet<_> = [
            ("urn:m".to_string(), desc.to_string(), Obj::lit("old")),
            (
                "urn:m".to_string(),
                refs.to_string(),
                Obj::Iri("urn:a".into()),
            ),
            (
                "urn:m".to_string(),
                refs.to_string(),
                Obj::Iri("urn:b".into()),
            ),
        ]
        .into_iter()
        .collect();
        let ch = diff(&file, &current);
        assert_eq!(ch.replaced, 1);
        assert_eq!(ch.adds.len(), 2);
        assert!(
            ch.adds
                .iter()
                .any(|a| a["mode"] == "replace" && a["o"] == "\"new\"")
        );
        assert_eq!(ch.retracts.len(), 1);
        assert_eq!(ch.retracts[0]["o"], "<urn:a>");
        // nothing to do when equal
        let same: BTreeSet<_> = file
            .facts
            .iter()
            .map(|x| (x.s.clone(), x.p.clone(), x.o.clone()))
            .collect();
        let ch = diff(&file, &same);
        assert!(ch.adds.is_empty() && ch.retracts.is_empty());
    }

    #[test]
    fn idempotency_keys_fit() {
        let k = idem_key(&"x".repeat(5000), "a", "b", Some(7), 3);
        assert!(k.len() <= 128);
        assert_ne!(k, idem_key("x", "a", "b", Some(7), 3));
    }

    #[test]
    fn a_held_lock_marks_again() {
        let d = tempfile::tempdir().unwrap();
        let l = Lock::take(d.path(), "k").unwrap().unwrap();
        assert!(Lock::take(d.path(), "k").unwrap().is_none());
        assert!(l.again());
        assert!(!l.again());
        drop(l);
        assert!(Lock::take(d.path(), "k").unwrap().is_some());
    }
}
