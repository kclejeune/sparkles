//! The disclosure guard of reports graphs (spec C22 §6).
//!
//! Every graph under [`GUARDED_PREFIX`] is guarded. A build records a [`Footprint`] of
//! what it read and commits it to [`DISCLOSURE_GRAPH`] together with its first report,
//! and each report links to its footprint. [`apply`] leaves a restricted view's guarded
//! graphs undecided, and binding the view to the snapshot a request reads admits those
//! that the snapshot's footprints prove for the reader, so a decision never misses a
//! footprint committed after the view was resolved. A path that does not bind sees no
//! guarded graph.
//!
//! Coverage compares definitions and caller attributes, never data:
//!
//! 1. a reader with no view, or one that reads every graph with no protection in force
//!    for reads, is proven;
//! 2. otherwise every graph of the footprint must be in the reader's read graphs, and on
//!    each, every protection in force for the reader must be in force in the footprint
//!    with an identical definition, with the footprint's values of every caller
//!    variable its pattern uses.
//!
//! The second rule includes the identical resolved view. Everything else is unproven
//! and fails closed. A guarded graph with no valid footprint is proven only by rule 1.

use super::settings::GUARDED_PREFIX;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sparkles::access::{Caller, Decider, GraphAccess, Graphs, GuardDecision, Protection, Rule};
use sparkles::id::Id;
use sparkles::index::Perm;
use sparkles::store::Snapshot;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// The footprint format this server reads and writes.
pub const FOOTPRINT_VERSION: u32 = 1;
/// The pattern that names every guarded graph.
pub fn guarded_pattern() -> String {
    format!("{GUARDED_PREFIX}*")
}
/// A graph set as the footprint keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase", tag = "kind")]
pub enum FpGraphs {
    All,
    Only {
        #[serde(default)]
        default_graph: bool,
        #[serde(default)]
        iris: Vec<String>,
        #[serde(default)]
        patterns: Vec<String>,
        #[serde(default)]
        protected: Vec<String>,
        #[serde(default)]
        blank_nodes: bool,
        #[serde(default)]
        excluded: Vec<String>,
    },
}

impl FpGraphs {
    /// The graph set, or `None` when it holds a guard and so is not a protection's own
    /// scope as configured.
    fn of(g: &Graphs) -> Option<FpGraphs> {
        Some(match g {
            Graphs::All => FpGraphs::All,
            Graphs::Only(r) => {
                if !r.guards.is_empty() {
                    return None;
                }
                FpGraphs::Only {
                    default_graph: r.default_graph,
                    iris: r.iris.clone(),
                    patterns: r.patterns.clone(),
                    protected: r.protected.clone(),
                    blank_nodes: r.blank_nodes,
                    excluded: r.excluded.clone(),
                }
            }
        })
    }
}

/// A protection's full definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FpProtection {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicates: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classes: Option<Vec<String>>,
    pub subclasses: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphs: Option<FpGraphs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default)]
    pub prefixes: BTreeMap<String, String>,
    pub hide_inferences: bool,
}

impl FpProtection {
    fn of(p: &Protection) -> Option<FpProtection> {
        Some(FpProtection {
            name: p.name.clone(),
            predicates: p.predicates.clone(),
            classes: p.classes.clone(),
            subclasses: p.subclasses,
            graphs: match &p.graphs {
                None => None,
                Some(g) => Some(FpGraphs::of(g)?),
            },
            pattern: p.pattern.clone(),
            prefixes: p.prefixes.clone(),
            hide_inferences: p.hide_inferences,
        })
    }
}

/// The caller attributes a pattern may bind.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FpCaller {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub groups: Vec<String>,
}

impl FpCaller {
    fn of(c: &Caller) -> FpCaller {
        let mut roles = c.roles.clone();
        roles.sort();
        roles.dedup();
        let mut groups = c.groups.clone();
        groups.sort();
        groups.dedup();
        FpCaller {
            user: c.user.clone(),
            roles,
            groups,
        }
    }
}

/// A graph the build read, with the protections in force on it for the principal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FpGraph {
    /// an IRI, or `default` for the default graph
    pub graph: String,
    #[serde(default)]
    pub protections: Vec<FpProtection>,
}

/// What one build read (§6.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Footprint {
    pub version: u32,
    pub dataset: String,
    pub branch: String,
    pub source_commit: u64,
    pub principal: String,
    pub caller: FpCaller,
    pub endpoint: String,
    pub graphs: Vec<FpGraph>,
    #[serde(default)]
    pub reports: Vec<String>,
}

/// Why a footprint cannot be captured.
#[derive(Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// the principal cannot read a graph the build would read
    Unreadable(String),
    /// a graph or a protection cannot be written in the footprint's form
    Unrepresentable(String),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Unreadable(g) => write!(f, "the principal cannot read the graph {g}"),
            CaptureError::Unrepresentable(m) => {
                write!(f, "the disclosure footprint cannot describe {m}")
            }
        }
    }
}

impl std::error::Error for CaptureError {}

fn term_of(graph: &str) -> Option<oxrdf::Term> {
    if graph == "default" {
        None
    } else {
        Some(oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            graph,
        )))
    }
}

/// The rules in force for reads on `graph` for a view.
fn in_force_on<'a>(view: &'a GraphAccess, graph: &str) -> Vec<&'a Rule> {
    let term = term_of(graph);
    view.triples.as_ref().map_or_else(Vec::new, |t| {
        t.rules
            .iter()
            .filter(|r| r.protection.covers_graph(term.as_ref()) && !r.read.allows(term.as_ref()))
            .collect()
    })
}

/// Whether a view reads `graph`.
fn reads(view: &GraphAccess, graph: &str) -> bool {
    view.read.allows(term_of(graph).as_ref())
}

pub struct CaptureInput<'a> {
    pub dataset: &'a str,
    pub branch: &'a str,
    pub source_commit: u64,
    pub principal: &'a str,
    pub endpoint: &'a str,
    /// the principal's view at that endpoint, `None` when it is unrestricted
    pub view: Option<&'a GraphAccess>,
    /// the caller attributes of the principal
    pub caller: &'a Caller,
    /// the concrete graphs the build reads: IRIs, or `default`
    pub graphs: &'a [String],
    pub reports: &'a [String],
}

/// Capture the footprint of a build.
pub fn capture(i: &CaptureInput<'_>) -> Result<Footprint, CaptureError> {
    let mut graphs = Vec::with_capacity(i.graphs.len());
    let mut names: Vec<&String> = i.graphs.iter().collect();
    names.sort();
    names.dedup();
    for g in names {
        if g.starts_with("_:") || g.is_empty() {
            return Err(CaptureError::Unrepresentable(format!("the graph {g:?}")));
        }
        if super::settings::is_guarded(g) {
            return Err(CaptureError::Unrepresentable(format!(
                "a build that reads the guarded graph {g}"
            )));
        }
        let mut protections = Vec::new();
        if let Some(v) = i.view {
            if !reads(v, g) {
                return Err(CaptureError::Unreadable(g.clone()));
            }
            for r in in_force_on(v, g) {
                protections.push(FpProtection::of(&r.protection).ok_or_else(|| {
                    CaptureError::Unrepresentable(format!("the protection {}", r.protection.name))
                })?);
            }
        }
        graphs.push(FpGraph {
            graph: g.clone(),
            protections,
        });
    }
    Ok(Footprint {
        version: FOOTPRINT_VERSION,
        dataset: i.dataset.to_string(),
        branch: i.branch.to_string(),
        source_commit: i.source_commit,
        principal: i.principal.to_string(),
        caller: FpCaller::of(i.caller),
        endpoint: i.endpoint.to_string(),
        graphs,
        reports: i.reports.to_vec(),
    })
}

/// A coverage decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Proven,
    /// the reader cannot read a graph of the footprint
    Denied,
    /// no admitted rule proves coverage
    Unproven,
}

/// Whether a view reads everything with no protection in force for reads (rule 1).
pub fn unrestricted(view: Option<&GraphAccess>) -> bool {
    match view {
        None => true,
        Some(v) => v.read.is_all() && v.triples.as_ref().is_none_or(|t| !t.hides()),
    }
}

/// The caller variables a pattern uses: `?user`, `?role` and `?group`, or the same
/// names with `$`.
fn uses_var(pattern: &str, var: &str) -> bool {
    let b = pattern.as_bytes();
    let n = var.len();
    (0..b.len()).any(|i| {
        (b[i] == b'?' || b[i] == b'$')
            && pattern.get(i + 1..i + 1 + n) == Some(var)
            && b.get(i + 1 + n)
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_' || *c >= 0x80))
    })
}

/// Decide whether a reader's view covers a footprint.
pub fn covers(reader: Option<&GraphAccess>, fp: &Footprint) -> Decision {
    if unrestricted(reader) {
        return Decision::Proven;
    }
    let Some(v) = reader else {
        return Decision::Proven;
    };
    if fp.version != FOOTPRINT_VERSION {
        return Decision::Unproven;
    }
    let caller = v
        .triples
        .as_ref()
        .map(|t| FpCaller::of(&t.caller))
        .unwrap_or_default();
    for g in &fp.graphs {
        if !reads(v, &g.graph) {
            return Decision::Denied;
        }
        for r in in_force_on(v, &g.graph) {
            let Some(p) = FpProtection::of(&r.protection) else {
                return Decision::Unproven;
            };
            if !g.protections.contains(&p) {
                return Decision::Unproven;
            }
            if let Some(pat) = &p.pattern
                && (uses_var(pat, "user") && caller.user != fp.caller.user
                    || uses_var(pat, "role") && caller.roles != fp.caller.roles
                    || uses_var(pat, "group") && caller.groups != fp.caller.groups)
            {
                return Decision::Unproven;
            }
        }
    }
    Decision::Proven
}

/// The guarded graph that holds every footprint of a dataset (§6.2). No footprint
/// proves it, so only unrestricted readers see it.
pub const DISCLOSURE_GRAPH: &str = "urn:x-sparkles:graphrag:disclosure";
/// The vocabulary of reports and footprints.
pub const GR: &str = "urn:x-sparkles:graphrag#";
/// The prefix of a footprint's IRI, followed by the SHA-256 of its JSON in hex.
pub const FOOTPRINT_PREFIX: &str = "urn:x-sparkles:graphrag:footprint:";
/// The largest footprint literal read, in bytes.
pub const MAX_FOOTPRINT_BYTES: usize = 4 << 20;

/// An IRI of the vocabulary.
pub fn gr(name: &str) -> String {
    format!("{GR}{name}")
}

impl Footprint {
    /// The footprint's JSON, as it is stored.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("serializable")
    }

    /// The IRI that names the footprint: the SHA-256 of its JSON.
    pub fn iri(&self) -> String {
        iri_of(&self.to_json())
    }

    /// Parse a stored footprint, refusing a version this server does not read before
    /// any other member, and a JSON longer than [`MAX_FOOTPRINT_BYTES`].
    pub fn parse(json: &str) -> Result<Footprint, String> {
        #[derive(Deserialize)]
        struct Probe {
            version: Option<serde_json::Value>,
        }
        if json.len() > MAX_FOOTPRINT_BYTES {
            return Err("the footprint is too large".into());
        }
        let p: Probe = serde_json::from_str(json).map_err(|e| e.to_string())?;
        match p.version.as_ref().and_then(|v| v.as_u64()) {
            Some(v) if v == u64::from(FOOTPRINT_VERSION) => {}
            Some(v) => {
                return Err(format!(
                    "the footprint has format version {v}, and this server reads version {FOOTPRINT_VERSION}"
                ));
            }
            None => return Err("the footprint has no version".into()),
        }
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    /// The quads that store the footprint for `reports_graph` in [`DISCLOSURE_GRAPH`].
    pub fn quads(&self, reports_graph: &str) -> Vec<oxrdf::Quad> {
        let json = self.to_json();
        let s = oxrdf::NamedNode::new_unchecked(iri_of(&json));
        let g = oxrdf::GraphName::NamedNode(oxrdf::NamedNode::new_unchecked(DISCLOSURE_GRAPH));
        vec![
            oxrdf::Quad::new(
                s.clone(),
                oxrdf::NamedNode::new_unchecked(gr("footprint")),
                oxrdf::Literal::new_simple_literal(json),
                g.clone(),
            ),
            oxrdf::Quad::new(
                s,
                oxrdf::NamedNode::new_unchecked(gr("covers")),
                oxrdf::NamedNode::new_unchecked(reports_graph),
                g,
            ),
        ]
    }
}

fn iri_of(json: &str) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(json.as_bytes());
    let mut out = String::with_capacity(FOOTPRINT_PREFIX.len() + 64);
    out.push_str(FOOTPRINT_PREFIX);
    for b in d {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// What one snapshot holds for one guarded graph.
#[derive(Clone, Debug, Default)]
pub struct GraphFootprints {
    /// the footprints that cover the graph, `None` for one that is invalid
    pub footprints: Vec<Option<Arc<Footprint>>>,
    /// a report in the graph links to a footprint that does not cover it
    pub missing: bool,
}

/// The footprints a snapshot holds, by guarded graph.
#[derive(Clone, Debug, Default)]
pub struct Index {
    pub graphs: HashMap<String, GraphFootprints>,
}

impl Index {
    /// The guarded graphs that `view` may read at this snapshot (§6.3).
    pub fn proven(&self, view: &GraphAccess) -> Vec<String> {
        self.graphs
            .iter()
            .filter(|(g, f)| {
                g.as_str() != DISCLOSURE_GRAPH
                    && !f.missing
                    && !f.footprints.is_empty()
                    && f.footprints.iter().all(|fp| {
                        fp.as_ref()
                            .is_some_and(|fp| covers(Some(view), fp) == Decision::Proven)
                    })
            })
            .map(|(g, _)| g.clone())
            .collect()
    }
}

/// The IRI of a term, when it is one.
fn iri(t: Option<oxrdf::Term>) -> Option<String> {
    match t {
        Some(oxrdf::Term::NamedNode(n)) => Some(n.into_string()),
        _ => None,
    }
}

/// The `[s, p, o, g]` quads with predicate `p`.
fn with_predicate(snap: &Snapshot, p: &str) -> sparkles::Result<Vec<[Id; 4]>> {
    let Some(pid) = snap.lookup_iri(p) else {
        return Ok(Vec::new());
    };
    Ok(snap
        .scan_keys(Perm::Pso, &[pid.0])?
        .iter()
        .map(|k| Perm::Pso.to_quad(k))
        .collect())
}

/// Read the footprints of `snap`. `parse` turns a footprint's IRI and JSON into the
/// footprint, `None` when it is invalid. A snapshot that cannot be read gives an index
/// in which every graph that links to a footprint is missing one.
fn read_index(
    snap: &Snapshot,
    parse: &dyn Fn(&str, &str) -> Option<Arc<Footprint>>,
) -> sparkles::Result<Index> {
    let mut index = Index::default();
    let dg = snap.lookup_iri(DISCLOSURE_GRAPH);
    // footprint IRI -> the graphs it covers, and its JSON literals
    let mut covers: HashMap<Id, Vec<String>> = HashMap::new();
    let mut json: HashMap<Id, Vec<String>> = HashMap::new();
    if let Some(dg) = dg {
        for [s, _, o, g] in with_predicate(snap, &gr("covers"))? {
            if g == dg
                && let Some(o) = iri(snap.term(o)).filter(|o| super::settings::is_guarded(o))
            {
                covers.entry(s).or_default().push(o);
            }
        }
        for [s, _, o, g] in with_predicate(snap, &gr("footprint"))? {
            if g == dg {
                let lit = match snap.term(o) {
                    Some(oxrdf::Term::Literal(l)) => l.value().to_string(),
                    _ => String::new(),
                };
                json.entry(s).or_default().push(lit);
            }
        }
    }
    let mut covered: std::collections::HashSet<(String, String)> = Default::default();
    for (s, graphs) in &covers {
        let fp = match (iri(snap.term(*s)), json.get(s).map(Vec::as_slice)) {
            (Some(name), Some([one])) => {
                let fp = parse(&name, one);
                if fp.is_some() {
                    for g in graphs {
                        covered.insert((name.clone(), g.clone()));
                    }
                }
                fp
            }
            // no literal, or several
            _ => None,
        };
        for g in graphs {
            index
                .graphs
                .entry(g.clone())
                .or_default()
                .footprints
                .push(fp.clone());
        }
    }
    for [_, _, o, g] in with_predicate(snap, &gr("disclosure"))? {
        if Some(g) == dg {
            continue;
        }
        let Some(graph) = iri(snap.term(g)).filter(|g| super::settings::is_guarded(g)) else {
            continue;
        };
        let linked = iri(snap.term(o)).unwrap_or_default();
        if !covered.contains(&(linked, graph.clone())) {
            index.graphs.entry(graph).or_default().missing = true;
        }
    }
    Ok(index)
}

/// What identifies a committed snapshot's content, for the index cache: its store's
/// result cache, which each store owns, and the generation and commit that key that
/// cache. The entry holds the result cache, so its address is not reused while cached.
struct CacheKey {
    store: Arc<sparkles::sparql::cache::ResultCache>,
    generation: u64,
    commit: u64,
}

impl CacheKey {
    fn of(snap: &Snapshot) -> Option<CacheKey> {
        (snap.cacheable && snap.mask.is_none()).then(|| CacheKey {
            store: snap.results.clone(),
            generation: snap.generation.uid,
            commit: snap.commit,
        })
    }

    fn same(&self, o: &CacheKey) -> bool {
        Arc::ptr_eq(&self.store, &o.store)
            && self.generation == o.generation
            && self.commit == o.commit
    }
}

/// The indexes kept.
const CACHED_INDEXES: usize = 16;
/// The parsed footprints kept.
const CACHED_FOOTPRINTS: usize = 4096;

/// The server's disclosure guard: the footprints of recent snapshots and parsed
/// footprints, shared by every request's decision.
#[derive(Default)]
pub struct Guard {
    indexes: Mutex<Vec<(CacheKey, Arc<Index>)>>,
    parsed: Mutex<HashMap<String, Option<Arc<Footprint>>>>,
}

impl std::fmt::Debug for Guard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Guard").finish_non_exhaustive()
    }
}

impl Guard {
    pub fn new() -> Guard {
        Guard::default()
    }

    /// A footprint named `name` with JSON `json`, `None` when it is invalid. Parsed
    /// footprints are kept by the digest of their JSON, so a name is checked against
    /// the JSON every time.
    fn parse(&self, name: &str, json: &str) -> Option<Arc<Footprint>> {
        let digest = iri_of(json);
        if digest != name {
            tracing::warn!(target: "sparkles::graphrag", "the footprint {name} does not match its digest");
            return None;
        }
        if let Some(fp) = self.parsed.lock().get(&digest) {
            return fp.clone();
        }
        let fp = match Footprint::parse(json) {
            Ok(fp) => Some(Arc::new(fp)),
            Err(e) => {
                tracing::warn!(target: "sparkles::graphrag", "the footprint {name} is invalid: {e}");
                None
            }
        };
        let mut parsed = self.parsed.lock();
        if parsed.len() >= CACHED_FOOTPRINTS {
            parsed.clear();
        }
        parsed.insert(digest, fp.clone());
        fp
    }

    /// The footprints of `snap`.
    pub fn index(&self, snap: &Snapshot) -> Arc<Index> {
        let key = CacheKey::of(snap);
        if let Some(k) = &key
            && let Some((_, i)) = self.indexes.lock().iter().find(|(c, _)| c.same(k))
        {
            return i.clone();
        }
        let index = match read_index(snap, &|n, j| self.parse(n, j)) {
            Ok(i) => Arc::new(i),
            Err(e) => {
                // nothing is proven from a snapshot that cannot be read
                tracing::warn!(target: "sparkles::graphrag", "the footprints cannot be read: {e}");
                return Arc::new(Index::default());
            }
        };
        if let Some(k) = key {
            let mut c = self.indexes.lock();
            if c.len() >= CACHED_INDEXES {
                c.remove(0);
            }
            c.push((k, index.clone()));
        }
        index
    }
}

/// One reader's decision of the guarded graphs, made against the snapshot it reads.
struct Decide {
    guard: Arc<Guard>,
    view: GraphAccess,
}

impl GuardDecision for Decide {
    fn exempt(&self, snap: &Arc<Snapshot>) -> Vec<String> {
        self.guard.index(snap).proven(&self.view)
    }
}

/// Guard a restricted view (§6.4). Its guarded graphs stay hidden until the view is
/// bound to the snapshot a request reads, and are then proven against that snapshot's
/// footprints. Writes never reach a guarded graph through a restricted view. An
/// unrestricted view is returned as it is, and without a guard nothing is proven.
pub fn apply(view: GraphAccess, guard: Option<&Arc<Guard>>) -> GraphAccess {
    if unrestricted(Some(&view)) {
        return view;
    }
    let pattern = guarded_pattern();
    let read = match guard {
        Some(g) => view.read.guarded_by(
            &pattern,
            Decider(Arc::new(Decide {
                guard: g.clone(),
                view: view.clone(),
            })),
        ),
        None => view.read.guarded(&pattern, &[]),
    };
    GraphAccess {
        read,
        write: view.write.guarded(&pattern, &[]),
        triples: view.triples,
    }
}

#[cfg(test)]
mod tests;
