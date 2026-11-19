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
//! and an optional `chk:dataGraph`), `chk:ShexCheck` (`chk:schema` ShExC and
//! `chk:shapeMap`), `chk:DiagnosticsCheck` (OWL RL diagnostics, `chk:diagnostic` ids, none
//! for all), `chk:UnsatisfiableClassesCheck` (the `unsatisfiable-class` diagnostic) and
//! `chk:QueryCheck`, a query that must return nothing (`chk:query` text, or
//! `chk:storedQuery` with `chk:storedQueryVersion` and optionally `chk:storedQueryDigest`,
//! the `sha256:` digest of its text). The diagnostics checks take
//! `chk:inferences true` to read the inferred graph too.
//!
//! A suite has no blank nodes, so that its checks can be named in results and overrides
//! and its canonical N-Triples, whose digest is its revision, are deterministic. An unknown
//! check type, an unknown term of the vocabulary, a missing or repeated value, or a check
//! the suite does not link is refused, never ignored.
//!
//! [`run`] reads the inputs of a check, a shapes graph or a stored query, from the state
//! the merge starts from and never from the candidate, so a merge cannot weaken the checks
//! it is gated by.

mod run;
#[cfg(test)]
mod tests;

pub use run::{CheckResult, Inputs, Status, SuiteReport, run};

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
        /// the data graph (`None`: the store's default graph)
        data_graph: Option<NamedNode>,
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
    UnsatisfiableClasses {
        inferences: bool,
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
            CheckKind::Diagnostics { .. } | CheckKind::UnsatisfiableClasses { .. } => {
                cfg!(feature = "reasoning")
            }
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
pub struct Suite {
    pub iri: NamedNode,
    pub label: Option<String>,
    pub checks: Vec<Check>,
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
        "ShaclCheck" => &["shapesGraph", "shapes", "dataGraph"],
        "ShexCheck" => &["schema", "shapeMap"],
        "DiagnosticsCheck" => &["diagnostic", "inferences"],
        "UnsatisfiableClassesCheck" => &["inferences"],
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
        let mut triples = Vec::new();
        for q in oxrdfio::RdfParser::from_format(format).for_slice(text.as_bytes()) {
            let q = q.map_err(|e| bad(format!("syntax error: {e}")))?;
            triples.push(Triple::new(q.subject, q.predicate, q.object));
        }
        Suite::from_triples(triples)
    }

    /// Read a suite from its triples.
    pub fn from_triples(triples: impl IntoIterator<Item = Triple>) -> Result<Suite, SuiteError> {
        let mut subjects: BTreeMap<String, Props> = BTreeMap::new();
        let mut lines = BTreeSet::new();
        for t in triples {
            let s = match &t.subject {
                oxrdf::NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
                _ => {
                    return Err(bad(
                        "a suite has no blank nodes; name every node with an IRI",
                    ));
                }
            };
            if matches!(t.object, Term::BlankNode(_)) {
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
            lines.insert(format!("{t} ."));
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
    let inferences = || -> Result<bool, SuiteError> {
        match one(props, &ns("inferences"), s)? {
            None => Ok(false),
            Some(Term::Literal(l)) if l.datatype().as_str() == XSD_BOOLEAN => match l.value() {
                "true" | "1" => Ok(true),
                "false" | "0" => Ok(false),
                _ => Err(bad(format!("<{s}>: chk:inferences takes a boolean"))),
            },
            Some(_) => Err(bad(format!("<{s}>: chk:inferences takes a boolean"))),
        }
    };
    let kind = match kind {
        "ShaclCheck" => {
            let shapes = match (iri("shapesGraph")?, string("shapes")?) {
                (Some(g), None) => Shapes::Graph(g),
                (None, Some(t)) => Shapes::Inline(t),
                _ => {
                    return Err(bad(format!(
                        "<{s}>: chk:ShaclCheck needs one of chk:shapesGraph and chk:shapes"
                    )));
                }
            };
            CheckKind::Shacl {
                shapes,
                data_graph: iri("dataGraph")?,
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
        "UnsatisfiableClassesCheck" => CheckKind::UnsatisfiableClasses {
            inferences: inferences()?,
        },
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
