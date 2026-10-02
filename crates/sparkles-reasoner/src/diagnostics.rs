//! Inconsistency diagnostics: a fixed, sound subset of the OWL 2 RL rules whose
//! conclusion is `false` (OWL 2 Profiles §4.3), each checked by one SPARQL SELECT over
//! the query default graph, optionally extended by the materialized inferences.
//!
//! A finding is a genuine inconsistency of the checked graph (the OWL 2 RL/RDF rules are
//! sound). The converse does not hold: finding nothing does **not** establish OWL
//! consistency, and the report never says so.
//!
//! With inferences included, every finding is re-checked with the same bindings over the
//! asserted data alone, so a finding that depends on (possibly outdated) inferences is
//! marked [`Basis::UsesInferences`].

use crate::INFERRED_GRAPH;
use oxrdf::{NamedNode, Term, Triple};
use serde_json::{Value as J, json};
use sparkles::Error;
use sparkles::sparql::QueryOptions;
use sparkles::sparql::results::term_json;
use sparkles::store::Snapshot;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_MEMBERS: &str = "http://www.w3.org/2002/07/owl#members";
const OWL_TARGET_INDIVIDUAL: &str = "http://www.w3.org/2002/07/owl#targetIndividual";

/// The note every report carries.
pub const NOTE: &str = "Checks a fixed subset of OWL 2 RL inconsistency rules; 'none-found' does not establish OWL consistency.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Inconsistency,
    Warning,
}

impl Severity {
    pub fn name(self) -> &'static str {
        match self {
            Severity::Inconsistency => "inconsistency",
            Severity::Warning => "warning",
        }
    }
}

/// How class membership `T(x, C)` is tested.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Closure {
    /// `?x rdf:type/rdfs:subClassOf* ?C` (sound under RDFS `rdfs9`/`rdfs11`, OWL 2 RL
    /// `cax-sco`)
    #[default]
    Subclass,
    /// only stated types: `?x rdf:type ?C`
    None,
}

impl Closure {
    pub fn name(self) -> &'static str {
        match self {
            Closure::Subclass => "subclass",
            Closure::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Closure> {
        match s {
            "subclass" => Some(Closure::Subclass),
            "none" => Some(Closure::None),
            _ => None,
        }
    }

    fn type_path(self) -> &'static str {
        match self {
            Closure::Subclass => "rdf:type/rdfs:subClassOf*",
            Closure::None => "rdf:type",
        }
    }
}

/// One check: an id, the rules it implements, and its query (`diagnostics/{id}.rq`).
pub struct Check {
    pub id: &'static str,
    pub rules: &'static [&'static str],
    pub severity: Severity,
    pub query: &'static str,
}

/// The checks, in report order.
pub const CHECKS: &[Check] = &[
    Check {
        id: "nothing-member",
        rules: &["cls-nothing2", "cax-sco"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/nothing-member.rq"),
    },
    Check {
        id: "disjoint-classes",
        rules: &["cax-dw"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/disjoint-classes.rq"),
    },
    Check {
        id: "all-disjoint-classes",
        rules: &["cax-adc"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/all-disjoint-classes.rq"),
    },
    Check {
        id: "complement-classes",
        rules: &["cls-com", "cax-sco"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/complement-classes.rq"),
    },
    Check {
        id: "max-cardinality-zero",
        rules: &["cls-maxc1", "cax-sco"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/max-cardinality-zero.rq"),
    },
    Check {
        id: "max-qualified-cardinality-zero",
        rules: &["cls-maxqc1", "cls-maxqc2", "cax-sco"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/max-qualified-cardinality-zero.rq"),
    },
    Check {
        id: "same-different",
        rules: &["eq-diff1", "eq-ref", "eq-sym", "eq-trans"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/same-different.rq"),
    },
    Check {
        id: "all-different",
        rules: &["eq-diff2", "eq-diff3", "eq-ref", "eq-sym", "eq-trans"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/all-different.rq"),
    },
    Check {
        id: "functional-literal-conflict",
        rules: &["prp-fp", "dt-diff", "eq-diff1"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/functional-literal-conflict.rq"),
    },
    Check {
        id: "irreflexive-property",
        rules: &["prp-irp"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/irreflexive-property.rq"),
    },
    Check {
        id: "asymmetric-property",
        rules: &["prp-asyp"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/asymmetric-property.rq"),
    },
    Check {
        id: "disjoint-properties",
        rules: &["prp-pdw"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/disjoint-properties.rq"),
    },
    Check {
        id: "all-disjoint-properties",
        rules: &["prp-adp"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/all-disjoint-properties.rq"),
    },
    Check {
        id: "negative-property-assertion",
        rules: &["prp-npa1", "prp-npa2"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/negative-property-assertion.rq"),
    },
    Check {
        id: "thing-empty",
        rules: &["thing-nonempty"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/thing-empty.rq"),
    },
    Check {
        id: "unsatisfiable-class",
        rules: &["lint"],
        severity: Severity::Warning,
        query: include_str!("../diagnostics/unsatisfiable-class.rq"),
    },
];

/// The checks named by `ids` (all of them when empty), in report order, or the first
/// unknown id.
pub fn select_checks(ids: &[String]) -> Result<Vec<&'static Check>, String> {
    if let Some(bad) = ids
        .iter()
        .find(|i| !CHECKS.iter().any(|c| c.id == i.as_str()))
    {
        return Err(bad.clone());
    }
    Ok(CHECKS
        .iter()
        .filter(|c| ids.is_empty() || ids.iter().any(|i| i == c.id))
        .collect())
}

/// Largest accepted `limit`.
pub const MAX_LIMIT: usize = 10_000;

#[derive(Clone, Debug)]
pub struct DiagnoseOptions {
    /// check ids; empty = all
    pub checks: Vec<String>,
    /// findings per check (at most [`MAX_LIMIT`])
    pub limit: usize,
    /// extend the default graph by [`INFERRED_GRAPH`]
    pub inferences: bool,
    pub closure: Closure,
    /// for the whole report
    pub timeout: Option<Duration>,
    /// prefixes for the messages (rdf, rdfs, owl and xsd are always known)
    pub prefixes: Vec<(String, String)>,
}

impl Default for DiagnoseOptions {
    fn default() -> Self {
        DiagnoseOptions {
            checks: Vec::new(),
            limit: 100,
            inferences: false,
            closure: Closure::Subclass,
            timeout: None,
            prefixes: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckStatus {
    Violations,
    None,
    Truncated,
    Timeout,
    Error,
}

impl CheckStatus {
    pub fn name(self) -> &'static str {
        match self {
            CheckStatus::Violations => "violations",
            CheckStatus::None => "none",
            CheckStatus::Truncated => "truncated",
            CheckStatus::Timeout => "timeout",
            CheckStatus::Error => "error",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CheckOutcome {
    pub id: &'static str,
    pub rules: &'static [&'static str],
    pub severity: Severity,
    pub status: CheckStatus,
    /// findings reported (at most `limit`)
    pub findings: usize,
    pub millis: u64,
    pub error: Option<String>,
}

/// Whether a finding also holds over the asserted data alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    Asserted,
    UsesInferences,
}

impl Basis {
    pub fn name(self) -> &'static str {
        match self {
            Basis::Asserted => "asserted",
            Basis::UsesInferences => "uses-inferences",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Evidence {
    One(Term),
    Many(Vec<Term>),
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub check: &'static str,
    pub rule: &'static str,
    pub severity: Severity,
    pub focus: Term,
    pub evidence: Vec<(&'static str, Evidence)>,
    pub basis: Basis,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportStatus {
    ViolationsFound,
    NoneFound,
    Incomplete,
}

impl ReportStatus {
    pub fn name(self) -> &'static str {
        match self {
            ReportStatus::ViolationsFound => "violations-found",
            ReportStatus::NoneFound => "none-found",
            ReportStatus::Incomplete => "incomplete",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DiagnosticsReport {
    /// the snapshot's commit (`seq`)
    pub commit: u64,
    pub computed_at: String,
    pub inferences: bool,
    pub closure: Closure,
    pub status: ReportStatus,
    pub checks: Vec<CheckOutcome>,
    pub findings: Vec<Finding>,
}

impl DiagnosticsReport {
    /// The JSON report (`diagnosticsFormat: 1`), without the `dataset` member and with
    /// only `included` in `scope.inferences`: the caller adds what it knows.
    pub fn to_json(&self) -> J {
        let checks: Vec<J> = self
            .checks
            .iter()
            .map(|c| {
                let mut o = json!({
                    "id": c.id,
                    "rules": c.rules,
                    "severity": c.severity.name(),
                    "status": c.status.name(),
                    "findings": c.findings,
                    "millis": c.millis,
                });
                if let Some(e) = &c.error {
                    o["error"] = e.clone().into();
                }
                o
            })
            .collect();
        let findings: Vec<J> = self
            .findings
            .iter()
            .map(|f| {
                let evidence: serde_json::Map<String, J> = f
                    .evidence
                    .iter()
                    .map(|(k, v)| {
                        let v = match v {
                            Evidence::One(t) => term_json(t),
                            Evidence::Many(ts) => J::Array(ts.iter().map(term_json).collect()),
                        };
                        (k.to_string(), v)
                    })
                    .collect();
                json!({
                    "check": f.check,
                    "rule": f.rule,
                    "severity": f.severity.name(),
                    "focus": term_json(&f.focus),
                    "evidence": evidence,
                    "basis": f.basis.name(),
                    "message": f.message,
                })
            })
            .collect();
        json!({
            "diagnosticsFormat": 1,
            "commit": self.commit,
            "computedAt": self.computed_at,
            "scope": {
                "graph": "default",
                "inferences": { "included": self.inferences },
                "closure": self.closure.name(),
            },
            "status": self.status.name(),
            "note": NOTE,
            "checks": checks,
            "findings": findings,
        })
    }
}

/// Run the checks on one snapshot. Unknown check ids are an error; a failing or timed
/// out check is reported in its outcome instead.
pub fn diagnose(snap: Arc<Snapshot>, opts: &DiagnoseOptions) -> anyhow::Result<DiagnosticsReport> {
    let checks = select_checks(&opts.checks)
        .map_err(|bad| anyhow::anyhow!("unknown diagnostics check '{bad}'"))?;
    let limit = opts.limit.clamp(1, MAX_LIMIT);
    let deadline = opts.timeout.map(|t| Instant::now() + t);
    let mut prefixes: BTreeMap<String, String> = sparkles::io::standard_prefixes();
    prefixes.extend(opts.prefixes.iter().cloned());
    let names = Names::new(&prefixes);
    let mut outcomes = Vec::new();
    let mut findings = Vec::new();
    for check in checks {
        let t0 = Instant::now();
        let mut out = CheckOutcome {
            id: check.id,
            rules: check.rules,
            severity: check.severity,
            status: CheckStatus::None,
            findings: 0,
            millis: 0,
            error: None,
        };
        if deadline.is_some_and(|d| Instant::now() >= d) {
            out.status = CheckStatus::Timeout;
            outcomes.push(out);
            continue;
        }
        let run = Run {
            snap: &snap,
            check,
            text: check.query.replace("{TYPE}", opts.closure.type_path()),
            deadline,
            inferences: opts.inferences,
            names: &names,
        };
        match run.findings(limit) {
            Ok((found, truncated)) => {
                out.findings = found.len();
                out.status = if truncated {
                    CheckStatus::Truncated
                } else if found.is_empty() {
                    CheckStatus::None
                } else {
                    CheckStatus::Violations
                };
                findings.extend(found);
            }
            Err(Error::Timeout) => out.status = CheckStatus::Timeout,
            Err(e) => {
                out.status = CheckStatus::Error;
                out.error = Some(e.to_string());
            }
        }
        out.millis = t0.elapsed().as_millis() as u64;
        outcomes.push(out);
    }
    let status = if outcomes
        .iter()
        .any(|c| c.severity == Severity::Inconsistency && c.findings > 0)
    {
        ReportStatus::ViolationsFound
    } else if outcomes
        .iter()
        .any(|c| matches!(c.status, CheckStatus::Timeout | CheckStatus::Error))
    {
        ReportStatus::Incomplete
    } else {
        ReportStatus::NoneFound
    };
    Ok(DiagnosticsReport {
        commit: snap.commit,
        computed_at: sparkles::builder::now_rfc3339(),
        inferences: opts.inferences,
        closure: opts.closure,
        status,
        checks: outcomes,
        findings,
    })
}

/// One check being run.
struct Run<'a> {
    snap: &'a Arc<Snapshot>,
    check: &'static Check,
    text: String,
    deadline: Option<Instant>,
    inferences: bool,
    names: &'a Names,
}

type Row = BTreeMap<String, Term>;

impl Run<'_> {
    fn options(
        &self,
        inferences: bool,
        bindings: Vec<(String, Term)>,
    ) -> sparkles::Result<QueryOptions> {
        let timeout = match self.deadline {
            Some(d) => Some(
                d.checked_duration_since(Instant::now())
                    .filter(|t| !t.is_zero())
                    .ok_or(Error::Timeout)?,
            ),
            None => None,
        };
        Ok(QueryOptions {
            timeout,
            no_cache: true,
            default_graph_extra: if inferences {
                vec![INFERRED_GRAPH.to_string()]
            } else {
                Vec::new()
            },
            initial_bindings: bindings,
            ..Default::default()
        })
    }

    fn select(
        &self,
        text: &str,
        inferences: bool,
        bindings: Vec<(String, Term)>,
    ) -> sparkles::Result<Vec<Row>> {
        let opts = self.options(inferences, bindings)?;
        let r = sparkles::sparql::query(self.snap.clone(), text, &opts)?;
        let vars = r.vars.clone();
        Ok(r.rows()
            .into_iter()
            .map(|row| {
                vars.iter()
                    .zip(row)
                    .filter_map(|(v, t)| Some((v.clone(), t?)))
                    .collect()
            })
            .collect())
    }

    /// The deduplicated findings (at most `limit`) and whether more remain.
    fn findings(&self, limit: usize) -> sparkles::Result<(Vec<Finding>, bool)> {
        let text = format!("{}\nLIMIT {}", self.text, 2 * limit + 2);
        let rows = self.select(&text, self.inferences, Vec::new())?;
        let full = rows.len() >= 2 * limit + 2;
        // group rows by the finding they belong to, in first-seen order
        let mut keys: Vec<String> = Vec::new();
        let mut groups: std::collections::HashMap<String, Vec<Row>> = Default::default();
        for row in rows {
            let Some(key) = self.key(&row) else { continue };
            groups
                .entry(key.clone())
                .or_insert_with(|| {
                    keys.push(key);
                    Vec::new()
                })
                .push(row);
        }
        // a full answer may hide more findings
        let truncated = keys.len() > limit || full;
        keys.truncate(limit);
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            let rows = &groups[&key];
            let basis = if self.inferences && !self.holds_asserted(&rows[0])? {
                Basis::UsesInferences
            } else {
                Basis::Asserted
            };
            if let Some(f) = self.finding(rows, basis)? {
                out.push(f);
            }
        }
        Ok((out, truncated))
    }

    /// The same query with the row's bindings, over the asserted data only.
    fn holds_asserted(&self, row: &Row) -> sparkles::Result<bool> {
        let text = format!("{}\nLIMIT 1", self.text);
        let bindings = row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Ok(!self.select(&text, false, bindings)?.is_empty())
    }

    /// Identity of the finding a row belongs to (unordered pairs are one finding).
    fn key(&self, row: &Row) -> Option<String> {
        let t = |v: &str| row.get(v).map(Term::to_string);
        let pair = |a: &str, b: &str| {
            let (a, b) = (t(a)?, t(b)?);
            Some(if a <= b {
                format!("{a} {b}")
            } else {
                format!("{b} {a}")
            })
        };
        Some(match self.check.id {
            "nothing-member" => t("x")?,
            "disjoint-classes" => format!("{} {}", t("x")?, pair("c1", "c2")?),
            "all-disjoint-classes" => format!("{} {} {}", t("d")?, t("x")?, pair("c1", "c2")?),
            "same-different" => pair("x", "y")?,
            "functional-literal-conflict" => {
                format!("{} {} {}", t("x")?, t("p")?, pair("v1", "v2")?)
            }
            "thing-empty" => format!("{} {} {}", t("s")?, t("p")?, t("o")?),
            "unsatisfiable-class" => t("c")?,
            "complement-classes" => format!("{} {}", t("x")?, pair("c1", "c2")?),
            "max-cardinality-zero" | "max-qualified-cardinality-zero" => {
                format!("{} {}", t("r")?, t("x")?)
            }
            "all-different" => format!("{} {}", t("d")?, pair("x", "y")?),
            "irreflexive-property" => format!("{} {}", t("x")?, t("p")?),
            "asymmetric-property" => format!("{} {}", t("p")?, pair("x", "y")?),
            "disjoint-properties" => {
                format!("{} {} {}", t("x")?, t("y")?, pair("p1", "p2")?)
            }
            "all-disjoint-properties" => {
                format!("{} {} {} {}", t("d")?, t("x")?, t("y")?, pair("p1", "p2")?)
            }
            "negative-property-assertion" => t("a")?,
            _ => return None,
        })
    }

    fn finding(&self, rows: &[Row], basis: Basis) -> sparkles::Result<Option<Finding>> {
        let row = &rows[0];
        let get = |v: &str| row.get(v).cloned();
        let sorted = |a: Term, b: Term| {
            if a.to_string() <= b.to_string() {
                vec![a, b]
            } else {
                vec![b, a]
            }
        };
        let n = |t: &Term| self.names.show(t);
        let is = |t: &Term, iri: &str| matches!(t, Term::NamedNode(x) if x.as_str() == iri);
        let mut rule = self.check.rules[0];
        let (focus, evidence, message) = match self.check.id {
            "nothing-member" => {
                let (Some(x), Some(_)) = (get("x"), get("type")) else {
                    return Ok(None);
                };
                let mut types: Vec<Term> = Vec::new();
                for r in rows {
                    if let Some(t) = r.get("type")
                        && !types.contains(t)
                    {
                        types.push(t.clone());
                    }
                }
                // the most specific stated type first: owl:Nothing itself last
                types.sort_by_key(|t| matches!(t, Term::NamedNode(n) if n.as_str() == OWL_NOTHING));
                let msg = match &types[0] {
                    Term::NamedNode(t) if t.as_str() == OWL_NOTHING => {
                        format!("{} is an instance of owl:Nothing", n(&x))
                    }
                    t => format!(
                        "{} is an instance of {}, a subclass of owl:Nothing",
                        n(&x),
                        n(t)
                    ),
                };
                let ev = if types.len() == 1 {
                    Evidence::One(types.remove(0))
                } else {
                    Evidence::Many(types)
                };
                (x, vec![("type", ev)], msg)
            }
            "disjoint-classes" => {
                let (Some(x), Some(c1), Some(c2)) = (get("x"), get("c1"), get("c2")) else {
                    return Ok(None);
                };
                let msg = if c1 == c2 {
                    format!(
                        "{} is an instance of {}, which is declared disjoint with itself",
                        n(&x),
                        n(&c1)
                    )
                } else {
                    let c = sorted(c1.clone(), c2.clone());
                    format!(
                        "{} is an instance of the disjoint classes {} and {}",
                        n(&x),
                        n(&c[0]),
                        n(&c[1])
                    )
                };
                (x, vec![("classes", Evidence::Many(sorted(c1, c2)))], msg)
            }
            "all-disjoint-classes" => {
                let (Some(d), Some(x), Some(c1), Some(c2)) =
                    (get("d"), get("x"), get("c1"), get("c2"))
                else {
                    return Ok(None);
                };
                let c = sorted(c1, c2);
                let msg = format!(
                    "{} is an instance of {} and {}, which {} declares pairwise disjoint",
                    n(&x),
                    n(&c[0]),
                    n(&c[1]),
                    n(&d)
                );
                (
                    x,
                    vec![("axiom", Evidence::One(d)), ("classes", Evidence::Many(c))],
                    msg,
                )
            }
            "same-different" => {
                let (Some(x), Some(y)) = (get("x"), get("y")) else {
                    return Ok(None);
                };
                let msg = if x == y {
                    format!("{} is declared different from itself", n(&x))
                } else {
                    format!(
                        "{} and {} are declared different but are the same individual (owl:sameAs)",
                        n(&x),
                        n(&y)
                    )
                };
                (x, vec![("other", Evidence::One(y))], msg)
            }
            "functional-literal-conflict" => {
                let (Some(x), Some(p), Some(v1), Some(v2)) =
                    (get("x"), get("p"), get("v1"), get("v2"))
                else {
                    return Ok(None);
                };
                let v = sorted(v1, v2);
                let msg = format!(
                    "{} has two different values for the functional property {}: {} and {}",
                    n(&x),
                    n(&p),
                    n(&v[0]),
                    n(&v[1])
                );
                (
                    x,
                    vec![
                        ("property", Evidence::One(p)),
                        ("values", Evidence::Many(v)),
                    ],
                    msg,
                )
            }
            "thing-empty" => {
                let (Some(Term::NamedNode(s)), Some(Term::NamedNode(p)), Some(o)) =
                    (get("s"), get("p"), get("o"))
                else {
                    return Ok(None);
                };
                let axiom = Term::Triple(Box::new(Triple::new(s.clone(), p.clone(), o.clone())));
                let msg = format!(
                    "the axiom {} {} {} makes owl:Thing empty",
                    n(&s.into()),
                    n(&p.into()),
                    n(&o)
                );
                (
                    Term::NamedNode(NamedNode::new_unchecked(OWL_THING)),
                    vec![("axiom", Evidence::One(axiom))],
                    msg,
                )
            }
            "unsatisfiable-class" => {
                let Some(c) = get("c") else { return Ok(None) };
                let path = self.path_to_nothing(&c)?;
                let msg = format!(
                    "{} is a subclass of owl:Nothing, so it can have no members",
                    n(&c)
                );
                (c, vec![("path", Evidence::Many(path))], msg)
            }
            "complement-classes" => {
                let (Some(x), Some(c1), Some(c2)) = (get("x"), get("c1"), get("c2")) else {
                    return Ok(None);
                };
                let msg = if c1 == c2 {
                    format!(
                        "{} is an instance of {}, which is declared the complement of itself",
                        n(&x),
                        n(&c1)
                    )
                } else {
                    format!(
                        "{} is an instance of both {} and its complement {}",
                        n(&x),
                        n(&c1),
                        n(&c2)
                    )
                };
                (x, vec![("classes", Evidence::Many(sorted(c1, c2)))], msg)
            }
            "max-cardinality-zero" | "max-qualified-cardinality-zero" => {
                let (Some(r), Some(x), Some(p), Some(y)) = (get("r"), get("x"), get("p"), get("y"))
                else {
                    return Ok(None);
                };
                let mut evidence = vec![
                    ("restriction", Evidence::One(r.clone())),
                    ("property", Evidence::One(p.clone())),
                ];
                let msg = match get("c") {
                    Some(c) => {
                        let thing = is(&c, OWL_THING);
                        if thing {
                            rule = "cls-maxqc2";
                        }
                        let what = if thing {
                            String::new()
                        } else {
                            format!(" of class {}", n(&c))
                        };
                        evidence.push(("class", Evidence::One(c)));
                        format!(
                            "{} has the value {}{what} for {}, but its type {} allows none (owl:maxQualifiedCardinality 0)",
                            n(&x),
                            n(&y),
                            n(&p),
                            n(&r)
                        )
                    }
                    None => format!(
                        "{} has the value {} for {}, but its type {} allows none (owl:maxCardinality 0)",
                        n(&x),
                        n(&y),
                        n(&p),
                        n(&r)
                    ),
                };
                evidence.push(("value", Evidence::One(y)));
                (x, evidence, msg)
            }
            "all-different" => {
                let (Some(d), Some(m), Some(x), Some(y)) = (get("d"), get("m"), get("x"), get("y"))
                else {
                    return Ok(None);
                };
                if !is(&m, OWL_MEMBERS) {
                    rule = "eq-diff3";
                }
                let msg = if x == y {
                    format!(
                        "{} is listed twice in the owl:AllDifferent axiom {}",
                        n(&x),
                        n(&d)
                    )
                } else {
                    format!(
                        "{} and {} are declared different by {} but are the same individual (owl:sameAs)",
                        n(&x),
                        n(&y),
                        n(&d)
                    )
                };
                let both = sorted(x, y);
                (
                    both[0].clone(),
                    vec![
                        ("axiom", Evidence::One(d)),
                        ("individuals", Evidence::Many(both)),
                    ],
                    msg,
                )
            }
            "irreflexive-property" => {
                let (Some(x), Some(p)) = (get("x"), get("p")) else {
                    return Ok(None);
                };
                let msg = format!(
                    "{} is related to itself by the irreflexive property {}",
                    n(&x),
                    n(&p)
                );
                (x, vec![("property", Evidence::One(p))], msg)
            }
            "asymmetric-property" => {
                let (Some(x), Some(p), Some(y)) = (get("x"), get("p"), get("y")) else {
                    return Ok(None);
                };
                let both = sorted(x, y);
                let (x, y) = (both[0].clone(), both[1].clone());
                let msg = if x == y {
                    format!(
                        "{} is related to itself by the asymmetric property {}",
                        n(&x),
                        n(&p)
                    )
                } else {
                    format!(
                        "{} and {} are related in both directions by the asymmetric property {}",
                        n(&x),
                        n(&y),
                        n(&p)
                    )
                };
                (
                    x,
                    vec![("property", Evidence::One(p)), ("other", Evidence::One(y))],
                    msg,
                )
            }
            "disjoint-properties" | "all-disjoint-properties" => {
                let (Some(x), Some(y), Some(p1), Some(p2)) =
                    (get("x"), get("y"), get("p1"), get("p2"))
                else {
                    return Ok(None);
                };
                let axiom = get("d");
                let declared = match &axiom {
                    Some(d) => format!("{} declares pairwise disjoint", n(d)),
                    None => "are declared disjoint".to_string(),
                };
                let msg = if p1 == p2 {
                    let why = match &axiom {
                        Some(d) => format!("{} lists twice as pairwise disjoint", n(d)),
                        None => "is declared disjoint with itself".to_string(),
                    };
                    format!(
                        "{} is related to {} by {}, which {why}",
                        n(&x),
                        n(&y),
                        n(&p1)
                    )
                } else {
                    let p = sorted(p1.clone(), p2.clone());
                    format!(
                        "{} is related to {} by both {} and {}, which {declared}",
                        n(&x),
                        n(&y),
                        n(&p[0]),
                        n(&p[1])
                    )
                };
                let mut evidence = Vec::new();
                if let Some(d) = axiom {
                    evidence.push(("axiom", Evidence::One(d)));
                }
                evidence.push(("properties", Evidence::Many(sorted(p1, p2))));
                evidence.push(("value", Evidence::One(y)));
                (x, evidence, msg)
            }
            "negative-property-assertion" => {
                let (Some(a), Some(x), Some(p), Some(t), Some(y)) =
                    (get("a"), get("x"), get("p"), get("t"), get("y"))
                else {
                    return Ok(None);
                };
                if !is(&t, OWL_TARGET_INDIVIDUAL) {
                    rule = "prp-npa2";
                }
                let msg = format!(
                    "{} {} {} is stated, but the negative property assertion {} denies it",
                    n(&x),
                    n(&p),
                    n(&y),
                    n(&a)
                );
                (
                    x,
                    vec![
                        ("axiom", Evidence::One(a)),
                        ("property", Evidence::One(p)),
                        ("target", Evidence::One(y)),
                    ],
                    msg,
                )
            }
            _ => return Ok(None),
        };
        Ok(Some(Finding {
            check: self.check.id,
            rule,
            severity: self.check.severity,
            focus,
            evidence,
            basis,
            message,
        }))
    }

    /// A chain of `rdfs:subClassOf` steps from `c` to owl:Nothing (at most 32 steps).
    fn path_to_nothing(&self, c: &Term) -> sparkles::Result<Vec<Term>> {
        const STEP: &str = "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
SELECT ?m WHERE { ?c rdfs:subClassOf ?m . ?m rdfs:subClassOf* owl:Nothing . FILTER(?m != ?c) }";
        let nothing = Term::NamedNode(NamedNode::new_unchecked(OWL_NOTHING));
        let mut path = vec![c.clone()];
        let mut seen: HashSet<Term> = HashSet::from([c.clone()]);
        let mut cur = c.clone();
        while cur != nothing && path.len() <= 32 {
            let rows = self.select(STEP, self.inferences, vec![("c".into(), cur.clone())])?;
            // a direct step to owl:Nothing if there is one, else any unvisited step
            let next = rows
                .iter()
                .filter_map(|r| r.get("m"))
                .filter(|m| !seen.contains(*m))
                .min_by_key(|m| **m != nothing)
                .cloned();
            let Some(next) = next else { break };
            seen.insert(next.clone());
            path.push(next.clone());
            cur = next;
        }
        Ok(path)
    }
}

/// Short names for messages: prefixed names where the local part is simple.
struct Names(Vec<(String, String)>);

impl Names {
    fn new(prefixes: &BTreeMap<String, String>) -> Names {
        let mut v: Vec<(String, String)> = prefixes
            .iter()
            .filter(|(_, ns)| !ns.is_empty())
            .map(|(p, ns)| (p.clone(), ns.clone()))
            .collect();
        // longest namespace first
        v.sort_by_key(|(_, ns)| std::cmp::Reverse(ns.len()));
        Names(v)
    }

    fn iri(&self, iri: &str) -> String {
        for (p, ns) in &self.0 {
            if let Some(l) = iri.strip_prefix(ns.as_str())
                && !l.is_empty()
                && l.chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            {
                return format!("{p}:{l}");
            }
        }
        format!("<{iri}>")
    }

    fn show(&self, t: &Term) -> String {
        match t {
            Term::NamedNode(n) => self.iri(n.as_str()),
            Term::Literal(l) => {
                let dt = l.datatype().as_str();
                const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
                match dt.strip_prefix(XSD) {
                    Some("integer" | "decimal" | "double" | "boolean") => l.value().to_string(),
                    Some("string") => format!("{:?}", l.value()),
                    _ if l.language().is_some() => t.to_string(),
                    _ => format!("{:?}^^{}", l.value(), self.iri(dt)),
                }
            }
            t => t.to_string(),
        }
    }
}
