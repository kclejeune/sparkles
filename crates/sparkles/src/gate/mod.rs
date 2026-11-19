//! Gate suites (spec C31, roadmap item 1136): checks of a dataset state described in RDF,
//! each with a severity, and the runner that evaluates a suite on a merge candidate.
//!
//! A suite is one `chk:Suite` with its checks, in the `urn:x-sparkles:checks#` vocabulary.
//! Severities are SHACL's `sh:Violation`, `sh:Warning` and `sh:Info`. A check of severity
//! `sh:Violation` that does not pass fails the suite, and the other checks are reported
//! only.
//!
//! ```turtle
//! @prefix chk: <urn:x-sparkles:checks#> .
//! @prefix sh:  <http://www.w3.org/ns/shacl#> .
//!
//! <urn:example:release> a chk:Suite ;
//!   chk:check <urn:example:release#shapes>, <urn:example:release#orphans> .
//! <urn:example:release#shapes> a chk:ShaclCheck ; chk:shapesGraph <urn:example:shapes> .
//! <urn:example:release#orphans> a chk:QueryCheck ; chk:severity sh:Warning ;
//!   chk:query "SELECT ?s WHERE { ?s a <urn:example:Orphan> }" .
//! ```
//!
//! The check types are `chk:ShaclCheck` (`chk:shapesGraph` or inline `chk:shapes` Turtle,
//! `chk:dataGraph` and `chk:inferences`), `chk:ShexCheck` (`chk:schema` ShExC and
//! `chk:shapeMap`), `chk:DiagnosticsCheck` (OWL RL diagnostics, `chk:diagnostic` ids, none
//! for all), `chk:UnsatisfiableClassesCheck` (the OWL 2 EL classifier on the asserted
//! terminology of the `chk:graph` graphs, the default graph without one, and their imports,
//! with `chk:requireComplete`) and
//! `chk:QueryCheck`, a query that must return nothing (`chk:query` text, or
//! `chk:storedQuery` with `chk:storedQueryVersion` and `chk:storedQueryDigest`, the
//! `sha256:` digest of its text). A stored query without its digest parses, so that a store
//! of suites can fill it in, but it does not run: its versions restart when it is deleted and
//! created again, and only the digest pins its text. The SHACL and diagnostics checks take
//! `chk:inferences true` to read the inferred graph too.
//!
//! A suite has no blank nodes, not even inside triple terms, so that its checks can be named
//! in results and overrides and its canonical N-Triples, whose digest is its revision, are
//! deterministic. A repeated triple counts once, and both the document and its canonical
//! N-Triples are at most [`MAX_SUITE_BYTES`]. An unknown
//! check type, an unknown term of the vocabulary, a missing or repeated value, or a check
//! the suite does not link is refused, never ignored.
//!
//! A SHACL check selects its data graph as the SHACL write guard does, so that a gate and
//! the guard judge the same data. Without `chk:dataGraph` it reads the store's default
//! graph, which is the union of all graphs only when the store uses a union default graph,
//! so on another store it does not see data in named graphs. A check that must see every
//! graph names `chk:dataGraph chk:UnionGraph` or runs on a store with a union default
//! graph. Graph IRIs, with `chk:DefaultGraph` for the default graph, read those graphs, each
//! of which must hold data in the candidate. The shapes graph, the run records and, unless
//! `chk:inferences true`, the materialized inferences are never data. A shapes graph is an
//! ordinary named graph: `chk:shapesGraph` refuses the ARQ aliases and the reserved graphs,
//! and shapes with no active shape that has a target are refused rather than validating
//! nothing. A shape that `sh:deactivated true` turns off is not active.
//!
//! An unsatisfiable-classes check classifies the terminology and leaves out assertions
//! about individuals in every form the graphs hold them, so an assertion can neither fail
//! the check nor change its answer, and the check avoids a full assertion scan. The
//! structural triples it reads can still include those of assertion-only structures, whose
//! number grows with the data. A contradiction among assertions, such as an individual of
//! two disjoint classes, is found by a `chk:DiagnosticsCheck` when it matches a
//! contradiction pattern the diagnostics support, or by a SHACL check whose shapes describe
//! it. The check never reads the materialized inferences, also through an import. An index
//! error, a quad it cannot decode, an error of the OWL reader, a terminology statement the
//! reader cannot map to its axiom, an import that resolves to no graph and graphs with no
//! terminology are errors. A terminology statement is the main triple of a terminology
//! axiom, such as an `rdfs:subClassOf` triple or a named class's `owl:intersectionOf`, and
//! one whose list is malformed, whose predicate is typed as an annotation property or whose
//! operands collapse, as in `owl:members ( A A )`, is refused rather than left out. What the answer
//! may depend on but the check leaves out, the axioms outside what the classifier reasons
//! with and the assertions about individuals that the terminology names, is a note of
//! severity `sh:Warning` in the result, and `chk:requireComplete true` makes it keep the
//! check from passing with the code `check-incomplete`.
//!
//! [`run`] reads the inputs of a check, a shapes graph or a stored query, from the state
//! the merge starts from and never from the candidate, so a merge cannot weaken the checks
//! it is gated by.

mod run;
#[cfg(test)]
mod tests;

pub use run::{CheckResult, Inputs, Note, Status, SuiteReport, run};

use oxrdf::{NamedNode, Term, Triple};
use std::collections::{BTreeMap, BTreeSet};

/// The namespace of the vocabulary.
pub const NS: &str = "urn:x-sparkles:checks#";

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const SH: &str = "http://www.w3.org/ns/shacl#";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";

/// The most checks one suite holds.
pub const MAX_CHECKS: usize = 256;

/// The largest suite document, in bytes.
pub const MAX_SUITE_BYTES: usize = 1 << 20;

/// How much a failed check matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// `sh:Violation`: the suite fails.
    Violation,
    /// `sh:Warning`: reported only.
    Warning,
    /// `sh:Info`: reported only.
    Info,
}

impl Severity {
    /// The SHACL IRI of the severity.
    pub fn iri(self) -> &'static str {
        match self {
            Severity::Violation => "http://www.w3.org/ns/shacl#Violation",
            Severity::Warning => "http://www.w3.org/ns/shacl#Warning",
            Severity::Info => "http://www.w3.org/ns/shacl#Info",
        }
    }

    /// `violation`, `warning` or `info`.
    pub fn name(self) -> &'static str {
        match self {
            Severity::Violation => "violation",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }

    fn from_iri(iri: &str) -> Option<Severity> {
        [Severity::Violation, Severity::Warning, Severity::Info]
            .into_iter()
            .find(|s| s.iri() == iri)
    }
}

/// Where a SHACL check's shapes come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shapes {
    /// a graph of the state the merge starts from
    Graph(NamedNode),
    /// Turtle text in the suite
    Inline(String),
}

/// The data graph of a SHACL check, selected as the SHACL write guard selects it
/// ([`sparkles_core::guard::config::DataGraphSel`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataGraph {
    /// no `chk:dataGraph`: the store's default graph, which is the union of all graphs when
    /// the store uses a union default graph, as for the write guard's `"default"`
    Default,
    /// `chk:dataGraph chk:UnionGraph`: every graph
    Union,
    /// the listed graphs, sorted, `chk:DefaultGraph` among them as
    /// `urn:x-arq:DefaultGraph`. Each must hold data in the candidate.
    Graphs(Vec<String>),
}

impl DataGraph {
    /// The write guard's selection of the same graphs.
    pub fn selection(&self) -> sparkles_core::guard::config::DataGraphSel {
        use sparkles_core::guard::config::DataGraphSel;
        match self {
            DataGraph::Default => DataGraphSel::Named("default".into()),
            DataGraph::Union => DataGraphSel::Named("union".into()),
            DataGraph::Graphs(gs) => DataGraphSel::Graphs(gs.clone()),
        }
    }
}

/// The graph that will hold the gate's run records, which no check reads.
pub const RUNS_GRAPH: &str = "urn:x-sparkles:checks:runs";

/// The query of a must-be-empty check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Query {
    /// SPARQL text in the suite
    Inline(String),
    /// a version of a stored query of the dataset, and the digest of its query text when
    /// the suite pins it. A stored query's versions start again when it is deleted and
    /// created anew, so the digest is what pins its content.
    Stored {
        name: String,
        version: u64,
        digest: Option<String>,
    },
}

/// What a check does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckKind {
    Shacl {
        shapes: Shapes,
        data_graph: DataGraph,
        /// also read the materialized inferences (`urn:x-sparkles:inferred`), which the data
        /// graph leaves out otherwise
        inferences: bool,
    },
    Shex {
        schema: String,
        shape_map: String,
    },
    Diagnostics {
        /// diagnostic ids (empty: all)
        diagnostics: Vec<String>,
        inferences: bool,
    },
    /// the classes the OWL 2 EL classifier finds unsatisfiable in the asserted
    /// terminology of `graphs` (empty: the default graph) and their imports. Axioms the
    /// answer may depend on but the classifier leaves out are a `sh:Warning` note, and
    /// with `require_complete` (`chk:requireComplete true`) they keep the check from
    /// passing.
    UnsatisfiableClasses {
        graphs: Vec<NamedNode>,
        require_complete: bool,
    },
    /// a query that must return no solutions, `false` or no triples
    Query(Query),
}

impl CheckKind {
    /// The local name of the check type in the vocabulary.
    pub fn type_name(&self) -> &'static str {
        match self {
            CheckKind::Shacl { .. } => "ShaclCheck",
            CheckKind::Shex { .. } => "ShexCheck",
            CheckKind::Diagnostics { .. } => "DiagnosticsCheck",
            CheckKind::UnsatisfiableClasses { .. } => "UnsatisfiableClassesCheck",
            CheckKind::Query(_) => "QueryCheck",
        }
    }

    /// Whether this build can run the check: the cargo feature it needs is compiled in.
    pub fn available(&self) -> bool {
        match self {
            CheckKind::Shacl { .. } => cfg!(feature = "shacl"),
            CheckKind::Shex { .. } => cfg!(feature = "shex"),
            CheckKind::Diagnostics { .. } => cfg!(feature = "reasoning"),
            CheckKind::UnsatisfiableClasses { .. } => cfg!(feature = "ontology"),
            CheckKind::Query(_) => true,
        }
    }
}

/// One check of a suite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub iri: NamedNode,
    pub severity: Severity,
    pub label: Option<String>,
    pub kind: CheckKind,
}

/// A gate suite: its checks in IRI order, and its canonical N-Triples.
#[derive(Clone, Debug, PartialEq, Eq)]
///
/// Its fields are private, so the checks that run are always the ones its revision names.
/// A changed suite is parsed again.
///
/// ```compile_fail
/// fn weaken(s: &mut sparkles::gate::Suite) {
///     s.checks.clear();
/// }
/// ```
pub struct Suite {
    iri: NamedNode,
    label: Option<String>,
    checks: Vec<Check>,
    canonical: String,
}

/// Why a suite was refused: the code `invalid-suite` and the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuiteError(pub String);

impl std::fmt::Display for SuiteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid suite: {}", self.0)
    }
}

impl std::error::Error for SuiteError {}

impl SuiteError {
    /// The stable code of the error.
    pub const CODE: &'static str = "invalid-suite";
}

fn bad(msg: impl Into<String>) -> SuiteError {
    SuiteError(msg.into())
}

/// The properties each check type takes, besides `rdf:type`, `chk:severity` and labels.
fn allowed(kind: &str) -> &'static [&'static str] {
    match kind {
        "ShaclCheck" => &["shapesGraph", "shapes", "dataGraph", "inferences"],
        "ShexCheck" => &["schema", "shapeMap"],
        "DiagnosticsCheck" => &["diagnostic", "inferences"],
        "UnsatisfiableClassesCheck" => &["graph", "requireComplete"],
        "QueryCheck" => &[
            "query",
            "storedQuery",
            "storedQueryVersion",
            "storedQueryDigest",
        ],
        _ => &[],
    }
}

const CHECK_TYPES: [&str; 5] = [
    "ShaclCheck",
    "ShexCheck",
    "DiagnosticsCheck",
    "UnsatisfiableClassesCheck",
    "QueryCheck",
];

const LABELS: [&str; 3] = [
    "http://www.w3.org/2000/01/rdf-schema#label",
    "http://www.w3.org/2000/01/rdf-schema#comment",
    "http://purl.org/dc/terms/description",
];

/// The values of each subject's predicates.
type Props = BTreeMap<String, Vec<Term>>;

impl Suite {
    /// Parse a suite from Turtle or N-Triples text.
    pub fn parse(text: &str, format: oxrdfio::RdfFormat) -> Result<Suite, SuiteError> {
        if text.len() > MAX_SUITE_BYTES {
            return Err(bad(format!(
                "the document is larger than {MAX_SUITE_BYTES} bytes"
            )));
        }
        if !matches!(
            format,
            oxrdfio::RdfFormat::Turtle | oxrdfio::RdfFormat::NTriples
        ) {
            return Err(bad("a suite is Turtle or N-Triples"));
        }
        Suite::accumulate(
            oxrdfio::RdfParser::from_format(format)
                .for_slice(text.as_bytes())
                .map(|q| {
                    q.map(|q| Triple::new(q.subject, q.predicate, q.object))
                        .map_err(|e| bad(format!("syntax error: {e}")))
                }),
        )
    }

    /// Read a suite from its triples.
    pub fn from_triples(triples: impl IntoIterator<Item = Triple>) -> Result<Suite, SuiteError> {
        Suite::accumulate(triples.into_iter().map(Ok))
    }

    /// Read a suite from triples as they arrive, so that no more than its canonical
    /// N-Triples is held: the first unique triple past [`MAX_SUITE_BYTES`] ends the read
    /// before the next is asked for.
    fn accumulate(
        triples: impl Iterator<Item = Result<Triple, SuiteError>>,
    ) -> Result<Suite, SuiteError> {
        let mut subjects: BTreeMap<String, Props> = BTreeMap::new();
        let mut lines = BTreeSet::new();
        let mut canonical_bytes = 0usize;
        for t in triples {
            let t = t?;
            let s = match &t.subject {
                oxrdf::NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
                _ => {
                    return Err(bad(
                        "a suite has no blank nodes; name every node with an IRI",
                    ));
                }
            };
            if has_blank_node(&t.object) {
                return Err(bad(
                    "a suite has no blank nodes; name every node with an IRI",
                ));
            }
            if let Term::Literal(l) = &t.object
                && l.language().is_some()
                && t.predicate.as_str().starts_with(NS)
            {
                return Err(bad(format!(
                    "<{}>: a value of the vocabulary has no language tag",
                    t.predicate.as_str()
                )));
            }
            let line = format!("{t} .");
            let len = line.len() + 1;
            if !lines.insert(line) {
                // a repeated triple is the same triple
                continue;
            }
            canonical_bytes = canonical_bytes.saturating_add(len);
            if canonical_bytes > MAX_SUITE_BYTES {
                return Err(bad(format!(
                    "the suite is larger than {MAX_SUITE_BYTES} bytes as N-Triples"
                )));
            }
            subjects
                .entry(s)
                .or_default()
                .entry(t.predicate.as_str().to_string())
                .or_default()
                .push(t.object);
        }
        let typed = |props: &Props, local: &str| {
            props
                .get(RDF_TYPE)
                .is_some_and(|ts| ts.iter().any(|t| is_iri(t, &format!("{NS}{local}"))))
        };
        let suites: Vec<&String> = subjects
            .iter()
            .filter(|(_, p)| typed(p, "Suite"))
            .map(|(s, _)| s)
            .collect();
        let suite_iri = match suites.as_slice() {
            [s] => (*s).clone(),
            [] => return Err(bad("the document has no chk:Suite")),
            _ => return Err(bad("the document has more than one chk:Suite")),
        };
        let suite_props = &subjects[&suite_iri];
        let vocab_types = suite_props
            .get(RDF_TYPE)
            .map(|v| v.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|t| matches!(t, Term::NamedNode(n) if n.as_str().starts_with(NS)))
            .count();
        if vocab_types != 1 {
            return Err(bad(format!(
                "<{suite_iri}>: a suite has no type of the vocabulary besides chk:Suite"
            )));
        }
        for p in suite_props.keys() {
            if p.starts_with(NS) && p != &format!("{NS}check") {
                return Err(bad(format!("<{suite_iri}>: a suite does not take <{p}>")));
            }
        }
        let label = text_value(suite_props, LABELS[0], &suite_iri)?;
        let linked: BTreeSet<String> = suite_props
            .get(&format!("{NS}check"))
            .map(|v| v.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|t| match t {
                Term::NamedNode(n) => Ok(n.as_str().to_string()),
                _ => Err(bad(format!("<{suite_iri}>: chk:check takes IRIs"))),
            })
            .collect::<Result<_, _>>()?;
        if linked.len() > MAX_CHECKS {
            return Err(bad(format!("a suite has at most {MAX_CHECKS} checks")));
        }
        let mut checks = Vec::new();
        for (s, props) in &subjects {
            if s == &suite_iri {
                continue;
            }
            let types: Vec<&str> = props
                .get(RDF_TYPE)
                .map(|v| v.as_slice())
                .unwrap_or_default()
                .iter()
                .filter_map(|t| match t {
                    Term::NamedNode(n) => n.as_str().strip_prefix(NS),
                    _ => None,
                })
                .collect();
            let in_vocab = props.keys().any(|p| p.starts_with(NS));
            if types.is_empty() && !in_vocab {
                // a node the suite only describes, such as a label's subject
                if linked.contains(s) {
                    return Err(bad(format!("<{s}>: a check needs a check type")));
                }
                continue;
            }
            let kind = match types.as_slice() {
                [k] if CHECK_TYPES.contains(k) => *k,
                [k] => return Err(bad(format!("<{s}>: unknown check type chk:{k}"))),
                [] => return Err(bad(format!("<{s}>: a check needs a check type"))),
                _ => return Err(bad(format!("<{s}>: a check has one check type"))),
            };
            if !linked.contains(s) {
                return Err(bad(format!(
                    "<{s}>: a check that <{suite_iri}> does not link with chk:check"
                )));
            }
            checks.push(check(s, kind, props)?);
        }
        if let Some(missing) = linked
            .iter()
            .find(|l| !checks.iter().any(|c: &Check| c.iri.as_str() == l.as_str()))
        {
            return Err(bad(format!("<{missing}>: a linked check is not described")));
        }
        Ok(Suite {
            iri: NamedNode::new_unchecked(suite_iri),
            label,
            checks,
            canonical: lines.into_iter().collect::<Vec<_>>().join("\n") + "\n",
        })
    }

    /// The suite's IRI.
    pub fn iri(&self) -> &NamedNode {
        &self.iri
    }

    /// The suite's `rdfs:label`.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// The checks, in IRI order.
    pub fn checks(&self) -> &[Check] {
        &self.checks
    }

    /// The suite as sorted N-Triples, one triple per line.
    pub fn canonical(&self) -> &str {
        &self.canonical
    }

    /// `sha256:` and the hex SHA-256 digest of [`canonical`](Self::canonical).
    pub fn revision(&self) -> String {
        sparkles_core::managed::settings_digest(self.canonical.as_bytes())
    }

    /// The checks this build cannot run, for a store of suites to refuse.
    pub fn unavailable(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| !c.kind.available()).collect()
    }
}

/// Whether `t` is a blank node or a triple term with one at any depth.
fn has_blank_node(t: &Term) -> bool {
    let mut stack = vec![t];
    while let Some(t) = stack.pop() {
        match t {
            Term::BlankNode(_) => return true,
            Term::Triple(tt) => {
                if matches!(tt.subject, oxrdf::NamedOrBlankNode::BlankNode(_)) {
                    return true;
                }
                stack.push(&tt.object);
            }
            _ => {}
        }
    }
    false
}

/// Whether `d` has the form of [`sparkles_core::managed::settings_digest`]: `sha256:` and
/// 64 lowercase hexadecimal digits.
fn is_digest(d: &str) -> bool {
    d.strip_prefix("sha256:").is_some_and(|h| {
        h.len() == 64
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn is_iri(t: &Term, iri: &str) -> bool {
    matches!(t, Term::NamedNode(n) if n.as_str() == iri)
}

fn one<'a>(props: &'a Props, p: &str, s: &str) -> Result<Option<&'a Term>, SuiteError> {
    match props.get(p).map(|v| v.as_slice()) {
        None | Some([]) => Ok(None),
        Some([v]) => Ok(Some(v)),
        Some(_) => Err(bad(format!("<{s}>: <{p}> has one value"))),
    }
}

fn text_value(props: &Props, p: &str, s: &str) -> Result<Option<String>, SuiteError> {
    match one(props, p, s)? {
        None => Ok(None),
        Some(Term::Literal(l)) if l.datatype().as_str() == XSD_STRING || l.language().is_some() => {
            Ok(Some(l.value().to_string()))
        }
        Some(_) => Err(bad(format!("<{s}>: <{p}> takes a string"))),
    }
}

/// Graph IRIs that are not data a check may select: the ARQ aliases, which the vocabulary
/// names as `chk:DefaultGraph` and `chk:UnionGraph`, the materialized inferences, which
/// `chk:inferences` selects, and the run records.
fn reserved_graph(iri: &str) -> bool {
    matches!(
        iri,
        "urn:x-arq:DefaultGraph" | "urn:x-arq:UnionGraph" | RUNS_GRAPH
    ) || iri == sparkles_core::INFERRED_GRAPH
        || iri.starts_with(NS)
}

/// The `chk:dataGraph` selection of a SHACL check.
fn data_graph(props: &Props, s: &str, shapes: &Shapes) -> Result<DataGraph, SuiteError> {
    let values = props
        .get(&format!("{NS}dataGraph"))
        .map(|v| v.as_slice())
        .unwrap_or_default();
    let union = format!("{NS}UnionGraph");
    let default = format!("{NS}DefaultGraph");
    let mut graphs = Vec::new();
    let mut has_union = false;
    for t in values {
        let Term::NamedNode(n) = t else {
            return Err(bad(format!("<{s}>: chk:dataGraph takes IRIs")));
        };
        let iri = n.as_str();
        if iri == union {
            has_union = true;
        } else if iri == default {
            graphs.push("urn:x-arq:DefaultGraph".to_string());
        } else if reserved_graph(iri) {
            return Err(bad(format!(
                "<{s}>: chk:dataGraph does not take <{iri}>; use chk:DefaultGraph, \
                 chk:UnionGraph or chk:inferences"
            )));
        } else if matches!(shapes, Shapes::Graph(g) if g.as_str() == iri) {
            return Err(bad(format!(
                "<{s}>: the shapes graph <{iri}> is not data of the same check"
            )));
        } else {
            graphs.push(iri.to_string());
        }
    }
    graphs.sort();
    match (has_union, graphs.is_empty()) {
        (true, true) => Ok(DataGraph::Union),
        (true, false) => Err(bad(format!(
            "<{s}>: chk:UnionGraph is every graph, so it takes no other chk:dataGraph"
        ))),
        (false, true) => Ok(DataGraph::Default),
        (false, false) => Ok(DataGraph::Graphs(graphs)),
    }
}

fn check(s: &str, kind: &str, props: &Props) -> Result<Check, SuiteError> {
    let allowed = allowed(kind);
    for p in props.keys() {
        let Some(local) = p.strip_prefix(NS) else {
            continue;
        };
        if local != "severity" && !allowed.contains(&local) {
            return Err(bad(format!("<{s}>: chk:{kind} does not take chk:{local}")));
        }
    }
    let ns = |local: &str| format!("{NS}{local}");
    let severity = match one(props, &ns("severity"), s)? {
        None => Severity::Violation,
        Some(Term::NamedNode(n)) => Severity::from_iri(n.as_str()).ok_or_else(|| {
            bad(format!(
                "<{s}>: chk:severity is sh:Violation, sh:Warning or sh:Info, not <{}>",
                n.as_str()
            ))
        })?,
        Some(_) => return Err(bad(format!("<{s}>: chk:severity takes an IRI of {SH}"))),
    };
    let label = text_value(props, LABELS[0], s)?;
    let string = |local: &str| -> Result<Option<String>, SuiteError> {
        match one(props, &ns(local), s)? {
            None => Ok(None),
            Some(Term::Literal(l)) if l.datatype().as_str() == XSD_STRING => {
                Ok(Some(l.value().to_string()))
            }
            Some(_) => Err(bad(format!("<{s}>: chk:{local} takes a string"))),
        }
    };
    let required = |local: &str| -> Result<String, SuiteError> {
        string(local)?.ok_or_else(|| bad(format!("<{s}>: chk:{kind} needs chk:{local}")))
    };
    let iri = |local: &str| -> Result<Option<NamedNode>, SuiteError> {
        match one(props, &ns(local), s)? {
            None => Ok(None),
            Some(Term::NamedNode(n)) => Ok(Some(n.clone())),
            Some(_) => Err(bad(format!("<{s}>: chk:{local} takes an IRI"))),
        }
    };
    let boolean = |local: &str| -> Result<bool, SuiteError> {
        match one(props, &ns(local), s)? {
            None => Ok(false),
            Some(Term::Literal(l)) if l.datatype().as_str() == XSD_BOOLEAN => match l.value() {
                "true" | "1" => Ok(true),
                "false" | "0" => Ok(false),
                _ => Err(bad(format!("<{s}>: chk:{local} takes a boolean"))),
            },
            Some(_) => Err(bad(format!("<{s}>: chk:{local} takes a boolean"))),
        }
    };
    let inferences = || boolean("inferences");
    let kind = match kind {
        "ShaclCheck" => {
            let shapes = match (iri("shapesGraph")?, string("shapes")?) {
                (Some(g), None) if reserved_graph(g.as_str()) => {
                    return Err(bad(format!(
                        "<{s}>: chk:shapesGraph names a graph that holds the shapes, not <{}>; \
                         the shapes come from a named graph or from chk:shapes",
                        g.as_str()
                    )));
                }
                (Some(g), None) => Shapes::Graph(g),
                (None, Some(t)) => Shapes::Inline(t),
                _ => {
                    return Err(bad(format!(
                        "<{s}>: chk:ShaclCheck needs one of chk:shapesGraph and chk:shapes"
                    )));
                }
            };
            let data_graph = data_graph(props, s, &shapes)?;
            let inferences = inferences()?;
            CheckKind::Shacl {
                shapes,
                data_graph,
                inferences,
            }
        }
        "ShexCheck" => CheckKind::Shex {
            schema: required("schema")?,
            shape_map: required("shapeMap")?,
        },
        "DiagnosticsCheck" => {
            let mut diagnostics = Vec::new();
            for t in props
                .get(&ns("diagnostic"))
                .map(|v| v.as_slice())
                .unwrap_or_default()
            {
                match t {
                    Term::Literal(l) if l.datatype().as_str() == XSD_STRING => {
                        diagnostics.push(l.value().to_string())
                    }
                    _ => return Err(bad(format!("<{s}>: chk:diagnostic takes strings"))),
                }
            }
            diagnostics.sort();
            #[cfg(feature = "reasoning")]
            if let Some(d) = diagnostics.iter().find(|d| {
                !sparkles_reasoner::diagnostics::CHECKS
                    .iter()
                    .any(|c| c.id == d.as_str())
            }) {
                return Err(bad(format!("<{s}>: unknown diagnostic {d:?}")));
            }
            CheckKind::Diagnostics {
                diagnostics,
                inferences: inferences()?,
            }
        }
        "UnsatisfiableClassesCheck" => {
            let mut graphs = Vec::new();
            for t in props
                .get(&ns("graph"))
                .map(|v| v.as_slice())
                .unwrap_or_default()
            {
                match t {
                    Term::NamedNode(n) if reserved_graph(n.as_str()) => {
                        return Err(bad(format!(
                            "<{s}>: chk:graph names an ontology graph, not <{}>",
                            n.as_str()
                        )));
                    }
                    Term::NamedNode(n) => graphs.push(n.clone()),
                    _ => return Err(bad(format!("<{s}>: chk:graph takes IRIs"))),
                }
            }
            graphs.sort();
            CheckKind::UnsatisfiableClasses {
                graphs,
                require_complete: boolean("requireComplete")?,
            }
        }
        "QueryCheck" => {
            let version = match one(props, &ns("storedQueryVersion"), s)? {
                None => None,
                Some(Term::Literal(l)) => Some(
                    l.value()
                        .parse::<u64>()
                        .ok()
                        .filter(|v| *v > 0 && l.datatype().as_str() == XSD_INTEGER)
                        .ok_or_else(|| {
                            bad(format!(
                                "<{s}>: chk:storedQueryVersion takes a positive integer"
                            ))
                        })?,
                ),
                Some(_) => {
                    return Err(bad(format!(
                        "<{s}>: chk:storedQueryVersion takes a positive integer"
                    )));
                }
            };
            if string("query")?.is_some() && string("storedQueryDigest")?.is_some() {
                return Err(bad(format!(
                    "<{s}>: chk:storedQueryDigest pins a chk:storedQuery"
                )));
            }
            if let Some(d) = string("storedQueryDigest")?
                && !is_digest(&d)
            {
                return Err(bad(format!(
                    "<{s}>: chk:storedQueryDigest takes \"sha256:\" and 64 lowercase hexadecimal digits"
                )));
            }
            match (string("query")?, string("storedQuery")?, version) {
                (Some(q), None, None) => CheckKind::Query(Query::Inline(q)),
                (None, Some(name), Some(version)) => CheckKind::Query(Query::Stored {
                    name,
                    version,
                    digest: string("storedQueryDigest")?,
                }),
                (None, Some(_), None) => {
                    return Err(bad(format!(
                        "<{s}>: chk:storedQuery needs chk:storedQueryVersion, since stored \
                         queries are not versioned with the branches"
                    )));
                }
                _ => {
                    return Err(bad(format!(
                        "<{s}>: chk:QueryCheck needs one of chk:query and chk:storedQuery"
                    )));
                }
            }
        }
        _ => unreachable!("a known check type"),
    };
    Ok(Check {
        iri: NamedNode::new_unchecked(s),
        severity,
        label,
        kind,
    })
}
