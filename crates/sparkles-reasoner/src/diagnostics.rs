//! Inconsistency diagnostics: a fixed, sound subset of the OWL 2 RL rules whose
//! conclusion is `false` (OWL 2 Profiles §4.3), each checked by one SPARQL SELECT over
//! the query default graph, optionally extended by the materialized inferences. The
//! checks can also run over the merge of chosen graphs ([`DiagnoseOptions::graphs`]).
//!
//! A finding is a genuine inconsistency of the checked graph (the OWL 2 RL/RDF rules are
//! sound). The converse does not hold: finding nothing does **not** establish OWL
//! consistency, and the report never says so.
//!
//! With inferences included, every finding is re-checked with the same bindings over the
//! asserted data alone, so a finding that depends on (possibly outdated) inferences is
//! marked [`Basis::UsesInferences`].

use crate::INFERRED_GRAPH;
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{BlankNode, Literal, NamedNode, Term, Triple};
use serde_json::{Value as J, json};
use sparkles_core::Error;
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::results::term_json;
use sparkles_core::store::{Generation, Snapshot};
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
        id: LITERAL_NOT_IN_DATATYPE,
        rules: &["dt-not-type", "dt-type2", "prp-rng"],
        severity: Severity::Inconsistency,
        query: include_str!("../diagnostics/datatype-range.rq"),
    },
    Check {
        id: "unsatisfiable-class",
        rules: &["lint"],
        severity: Severity::Warning,
        query: include_str!("../diagnostics/unsatisfiable-class.rq"),
    },
];

/// The id of the check of literals outside the value space of their datatype.
pub const LITERAL_NOT_IN_DATATYPE: &str = "literal-not-in-datatype";

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
    /// extend the checked graph by [`INFERRED_GRAPH`]
    pub inferences: bool,
    /// The graphs whose merge is checked: graph IRIs, or [`DEFAULT_GRAPH`] for the
    /// default graph. Empty checks the query default graph.
    pub graphs: Vec<String>,
    pub closure: Closure,
    /// for the whole report
    pub timeout: Option<Duration>,
    /// prefixes for the messages (rdf, rdfs, owl and xsd are always known)
    pub prefixes: Vec<(String, String)>,
    /// the query memory grant the checks' queries charge (`None`: they are not counted
    /// in a pool)
    pub grant: Option<Arc<sparkles_core::sparql::pool::PoolGrant>>,
}

impl Default for DiagnoseOptions {
    fn default() -> Self {
        DiagnoseOptions {
            checks: Vec::new(),
            limit: 100,
            inferences: false,
            graphs: Vec::new(),
            closure: Closure::Subclass,
            timeout: None,
            prefixes: Vec::new(),
            grant: None,
        }
    }
}

/// The name of the default graph among [`DiagnoseOptions::graphs`].
pub const DEFAULT_GRAPH: &str = "default";

/// Check the graph names of [`DiagnoseOptions::graphs`]: `default` or absolute IRIs,
/// without duplicates. Returns the bad name.
pub fn parse_graphs<S: AsRef<str>>(names: &[S]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let n = n.as_ref().trim();
        if n != DEFAULT_GRAPH && NamedNode::new(n).is_err() {
            return Err(n.to_string());
        }
        if !out.iter().any(|g| g == n) {
            out.push(n.to_string());
        }
    }
    Ok(out)
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
    /// the checked graphs ([`DiagnoseOptions::graphs`]); empty for the query default graph
    pub graphs: Vec<String>,
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
        let mut scope = json!({
            "graph": "default",
            "inferences": { "included": self.inferences },
            "closure": self.closure.name(),
        });
        if !self.graphs.is_empty() {
            scope["graph"] = "graphs".into();
            scope["graphs"] = self.graphs.clone().into();
        }
        json!({
            "diagnosticsFormat": 1,
            "commit": self.commit,
            "computedAt": self.computed_at,
            "scope": scope,
            "status": self.status.name(),
            "note": NOTE,
            "checks": checks,
            "findings": findings,
        })
    }

    /// The report as RDF: one `spk:DiagnosticsReport` (`spk:` is `urn:x-sparkles:`)
    /// whose findings use the SHACL result properties (`sh:focusNode`,
    /// `sh:resultSeverity`, `sh:resultMessage`, `sh:sourceConstraintComponent`). There is
    /// no `sh:conforms`: finding nothing does not establish consistency.
    pub fn to_rdf(&self, ctx: &ReportContext<'_>) -> Vec<Triple> {
        let mut out = Vec::new();
        let mut add = |s: &BlankNode, p: NamedNode, o: Term| out.push(Triple::new(s.clone(), p, o));
        let report = BlankNode::default();
        let s = |v: &str| Term::Literal(Literal::new_simple_literal(v));
        let int = |v: u64| Term::Literal(Literal::new_typed_literal(v.to_string(), xsd::INTEGER));
        let bool_ = |v: bool| Term::Literal(Literal::from(v));
        add(
            &report,
            rdf::TYPE.into_owned(),
            spk("DiagnosticsReport").into(),
        );
        if let Some(d) = ctx.dataset {
            add(&report, spk("dataset"), s(d));
        }
        add(&report, spk("commit"), int(self.commit));
        add(
            &report,
            spk("computedAt"),
            Literal::new_typed_literal(&self.computed_at, xsd::DATE_TIME).into(),
        );
        add(&report, spk("status"), s(self.status.name()));
        add(&report, spk("note"), s(NOTE));
        add(&report, spk("closure"), s(self.closure.name()));
        for g in &self.graphs {
            let o = match NamedNode::new(g.as_str()) {
                Ok(n) if g != DEFAULT_GRAPH => n.into(),
                _ => s(g),
            };
            add(&report, spk("graph"), o);
        }
        add(&report, spk("inferencesIncluded"), bool_(self.inferences));
        if self.inferences {
            if let Some(p) = ctx.profile {
                add(&report, spk("inferencesProfile"), s(p));
            }
            if let Some(stale) = ctx.stale {
                add(&report, spk("inferencesStale"), bool_(stale));
            }
            if let Some(n) = ctx.commits_since {
                add(&report, spk("commitsSince"), int(n));
            }
        }
        for c in &self.checks {
            let node = BlankNode::default();
            add(&report, spk("check"), node.clone().into());
            add(&node, spk("id"), s(c.id));
            for r in c.rules {
                add(&node, spk("rule"), s(r));
            }
            add(&node, spk("severity"), s(c.severity.name()));
            add(&node, spk("status"), s(c.status.name()));
            add(&node, spk("findings"), int(c.findings as u64));
            add(&node, spk("millis"), int(c.millis));
            if let Some(e) = &c.error {
                add(&node, spk("error"), s(e));
            }
        }
        let mut lists = Vec::new();
        for f in &self.findings {
            let node = BlankNode::default();
            add(&report, sh("result"), node.clone().into());
            add(&node, rdf::TYPE.into_owned(), spk("Finding").into());
            add(&node, sh("focusNode"), f.focus.clone());
            let severity = match f.severity {
                Severity::Inconsistency => "Violation",
                Severity::Warning => "Warning",
            };
            add(&node, sh("resultSeverity"), sh(severity).into());
            add(&node, sh("resultMessage"), s(&f.message));
            add(
                &node,
                sh("sourceConstraintComponent"),
                spk(&format!("check:{}", f.check)).into(),
            );
            add(&node, spk("rule"), s(f.rule));
            add(&node, spk("basis"), s(f.basis.name()));
            for (k, v) in &f.evidence {
                let o = match v {
                    Evidence::One(t) => t.clone(),
                    Evidence::Many(ts) if ts.is_empty() => rdf::NIL.into_owned().into(),
                    Evidence::Many(ts) => {
                        let head = BlankNode::default();
                        lists.push((head.clone(), ts.clone()));
                        head.into()
                    }
                };
                add(&node, spk(k), o);
            }
        }
        // the evidence lists, as RDF collections
        for (head, items) in lists {
            let mut cur = head;
            for (i, t) in items.iter().enumerate() {
                add(&cur, rdf::FIRST.into_owned(), t.clone());
                if i + 1 == items.len() {
                    add(&cur, rdf::REST.into_owned(), rdf::NIL.into_owned().into());
                } else {
                    let next = BlankNode::default();
                    add(&cur, rdf::REST.into_owned(), next.clone().into());
                    cur = next;
                }
            }
        }
        out
    }

    /// Turtle serialization of [`to_rdf`](Self::to_rdf), with the `sh:`, `spk:`, `rdf:`
    /// and `xsd:` prefixes and the context's prefixes.
    pub fn to_turtle(&self, ctx: &ReportContext<'_>) -> String {
        let mut ser = oxttl::TurtleSerializer::new();
        let fixed = [
            ("sh", SH),
            ("spk", SPK),
            ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
            ("xsd", "http://www.w3.org/2001/XMLSchema#"),
        ];
        for (p, ns) in fixed {
            ser = ser.with_prefix(p, ns).expect("valid prefix");
        }
        for (p, ns) in ctx.prefixes {
            if fixed.iter().any(|(f, _)| f == p) {
                continue;
            }
            // a prefix the serializer rejects is left out
            if let Ok(s) = ser.clone().with_prefix(p, ns) {
                ser = s;
            }
        }
        let mut w = ser.for_writer(Vec::new());
        for t in self.to_rdf(ctx) {
            w.serialize_triple(&t)
                .expect("writing to a Vec cannot fail");
        }
        String::from_utf8(w.finish().expect("writing to a Vec cannot fail"))
            .expect("Turtle is UTF-8")
    }
}

/// What the caller knows beyond the report itself, for its RDF form.
#[derive(Clone, Debug, Default)]
pub struct ReportContext<'a> {
    pub dataset: Option<&'a str>,
    /// the profile of the included inferences
    pub profile: Option<&'a str>,
    /// their freshness (`None` when unknown)
    pub stale: Option<bool>,
    pub commits_since: Option<u64>,
    /// prefixes for the Turtle output
    pub prefixes: &'a [(String, String)],
}

/// Namespace of the report vocabulary.
pub const SPK: &str = "urn:x-sparkles:";
const SH: &str = "http://www.w3.org/ns/shacl#";

fn spk(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{SPK}{local}"))
}

fn sh(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{SH}{local}"))
}

/// Literals that a materialization's generalized facts type with a datatype outside of
/// whose value space they are: the facts `lit rdf:type DT`, with `DT` in the OWL 2 RL
/// datatype map, which RDF cannot state and the inferred graph therefore lacks. See
/// [`crate::literal_typings`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiteralTypings {
    /// the commit of the materialization
    pub commit: u64,
    /// the literal and the datatype, each literal outside the datatype's value space
    pub facts: Vec<(Literal, NamedNode)>,
}

impl LiteralTypings {
    /// The typings among `facts` that the check reports: a literal subject, `rdf:type`,
    /// and a datatype the literal's value is decided to be outside of.
    pub fn violation(s: &Term, p: &Term, o: &Term) -> Option<(Literal, NamedNode)> {
        match (s, p, o) {
            (Term::Literal(l), Term::NamedNode(p), Term::NamedNode(dt))
                if p.as_str() == rdf::TYPE.as_str()
                    && in_value_space(l, dt.as_str()) == Some(false) =>
            {
                Some((l.clone(), dt.clone()))
            }
            _ => None,
        }
    }
}

/// Run the checks on one snapshot. Unknown check ids are an error; a failing or timed
/// out check is reported in its outcome instead.
pub fn diagnose(snap: Arc<Snapshot>, opts: &DiagnoseOptions) -> anyhow::Result<DiagnosticsReport> {
    diagnose_with(snap, opts, None)
}

/// [`diagnose`], with the literal typings of a materialization's generalized facts for
/// `literal-not-in-datatype` when the checks read the inferences. Their findings are
/// marked [`Basis::UsesInferences`].
pub fn diagnose_with(
    snap: Arc<Snapshot>,
    opts: &DiagnoseOptions,
    typings: Option<&LiteralTypings>,
) -> anyhow::Result<DiagnosticsReport> {
    diagnose_controlled(snap, opts, typings, &DiagnosticControls::default())
}

/// Additional controls shared by entailment diffs and justification oracle calls.
#[derive(Clone, Debug, Default)]
pub struct DiagnosticControls {
    pub deadline: Option<Instant>,
    pub cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Each query and the decoded rows it retains are held to this estimate.
    pub max_memory: Option<u64>,
}

fn check_control(
    deadline: Option<Instant>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> sparkles_core::Result<()> {
    if cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
        return Err(Error::Cancelled);
    }
    if deadline.is_some_and(|limit| Instant::now() >= limit) {
        return Err(Error::Timeout);
    }
    Ok(())
}

/// Run diagnostics within the parent analysis's absolute deadline and memory grant.
pub fn diagnose_controlled(
    snap: Arc<Snapshot>,
    opts: &DiagnoseOptions,
    typings: Option<&LiteralTypings>,
    controls: &DiagnosticControls,
) -> anyhow::Result<DiagnosticsReport> {
    check_control(controls.deadline, controls.cancel.as_deref())?;
    let checks = select_checks(&opts.checks)
        .map_err(|bad| anyhow::anyhow!("unknown diagnostics check '{bad}'"))?;
    let limit = opts.limit.clamp(1, MAX_LIMIT);
    let relative = opts
        .timeout
        .and_then(|timeout| Instant::now().checked_add(timeout));
    let deadline = match (controls.deadline, relative) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let mut prefixes: BTreeMap<String, String> = sparkles_core::io::standard_prefixes();
    prefixes.extend(opts.prefixes.iter().cloned());
    let names = Names::new(&prefixes);
    let mut outcomes = Vec::new();
    let mut findings = Vec::new();
    for check in checks {
        if controls
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
        {
            return Err(Error::Cancelled.into());
        }
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
            graphs: &opts.graphs,
            names: &names,
            typings: typings.filter(|_| opts.inferences),
            grant: opts.grant.as_ref(),
            cancel: controls.cancel.as_ref(),
            max_memory: controls.max_memory,
        };
        let found = if check.id == LITERAL_NOT_IN_DATATYPE {
            run.paged_findings(limit)
        } else {
            run.findings(limit)
        };
        match found {
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
    if controls
        .cancel
        .as_ref()
        .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
    {
        return Err(Error::Cancelled.into());
    }
    let status = if outcomes
        .iter()
        .any(|c| c.severity == Severity::Inconsistency && c.findings > 0)
    {
        ReportStatus::ViolationsFound
    } else if outcomes.iter().any(|c| {
        matches!(c.status, CheckStatus::Timeout | CheckStatus::Error)
            // a check that stopped before it found anything did not finish
            || (c.status == CheckStatus::Truncated && c.findings == 0)
    }) {
        ReportStatus::Incomplete
    } else {
        ReportStatus::NoneFound
    };
    Ok(DiagnosticsReport {
        commit: snap.commit,
        computed_at: sparkles_core::builder::now_rfc3339(),
        inferences: opts.inferences,
        graphs: opts.graphs.clone(),
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
    graphs: &'a [String],
    names: &'a Names,
    /// the literal typings of the inferences, when the check reads them
    typings: Option<&'a LiteralTypings>,
    grant: Option<&'a Arc<sparkles_core::sparql::pool::PoolGrant>>,
    cancel: Option<&'a Arc<std::sync::atomic::AtomicBool>>,
    max_memory: Option<u64>,
}

type Row = BTreeMap<String, Term>;

/// The first and the largest page of candidate rows that [`Run::paged_findings`] reads.
/// Pages double in size from the first to the largest.
const PAGE_FIRST: usize = 256;
const PAGE_MAX: usize = 16_384;
/// The candidate rows [`Run::paged_findings`] reads at most before it stops and reports
/// the check truncated, and the ill-typed literals it looks up at most.
const CANDIDATES_MAX: usize = 1 << 18;
/// The ill-typed literals [`Run::ill_typed_into`] looks up with one query.
const ILL_TYPED_BATCH: usize = 256;
/// The positions of the delta vocabulary [`ill_typed`] reads under one hold of its lock.
const DELTA_CHUNK: u64 = 4_096;

/// The literal of a vocabulary key when the table decides it is ill-typed for its own
/// datatype: a typed literal of an XSD datatype the check implements, outside that
/// datatype's value space. Simple and language-tagged literals are well typed, and so are
/// the literals inlined in their ids, which take only canonical, valid lexical forms.
fn ill_typed_key(key: &[u8]) -> Option<Literal> {
    if key.first() != Some(&b'"') {
        return None;
    }
    let sep = key.iter().rposition(|&b| b == sparkles_core::id::KEY_SEP)?;
    let dt = key[sep + 1..].strip_prefix(b"^")?;
    let local = std::str::from_utf8(dt.strip_prefix(XSD.as_bytes())?).ok()?;
    family(local)?;
    let lex = std::str::from_utf8(&key[1..sep]).ok()?;
    let dt = std::str::from_utf8(dt).ok()?;
    let lit = Literal::new_typed_literal(lex, NamedNode::new_unchecked(dt));
    (in_value_space(&lit, dt) == Some(false)).then_some(lit)
}

/// The ids of the literals of a snapshot's vocabulary that are ill-typed for their own
/// datatype (see [`ill_typed_key`]).
struct IllTyped {
    /// those of the base vocabulary, in id order, as the generation remembers them
    base: Arc<[u64]>,
    /// the positions of those of the delta vocabulary
    delta: Vec<u64>,
}

impl IllTyped {
    /// The ids, those of the base vocabulary first.
    fn ids(&self) -> impl Iterator<Item = sparkles_core::id::Id> + '_ {
        let base = self.base.iter().map(|&i| sparkles_core::id::Id::vocab(i));
        base.chain(self.delta.iter().map(|&i| sparkles_core::id::Id::delta(i)))
    }
    fn len(&self) -> usize {
        self.base.len() + self.delta.len()
    }
}

/// The ids of the literals of `snap`'s vocabulary that are ill-typed for their own
/// datatype, at most `max`, and a `max`th one more when there are more. The base
/// vocabulary of a generation is read once and its ids are remembered with the generation
/// ([`Generation::ill_typed_literals`]), so they are freed with it. The read holds the
/// generation's lock, so a call that comes meanwhile waits for it, until its deadline,
/// instead of reading the vocabulary again. The delta vocabulary, which grows with each
/// commit, is read each time up to the snapshot's end, a chunk of positions to a hold of
/// its lock, so a writer that adds terms waits for one chunk at most. A deadline that
/// passes during a read is a timeout, and nothing is remembered.
fn ill_typed(
    snap: &Snapshot,
    max: usize,
    deadline: Option<Instant>,
) -> sparkles_core::Result<IllTyped> {
    ill_typed_controlled(snap, max, deadline, None)
}

fn ill_typed_controlled(
    snap: &Snapshot,
    max: usize,
    deadline: Option<Instant>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> sparkles_core::Result<IllTyped> {
    check_control(deadline, cancel)?;
    let generation = &snap.generation;
    let base = {
        let mut slot = if deadline.is_none() && cancel.is_none() {
            generation.ill_typed_literals.lock()
        } else {
            loop {
                check_control(deadline, cancel)?;
                let slice = deadline.map_or(Duration::from_millis(5), |limit| {
                    limit
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(5))
                });
                if let Some(locked) = generation.ill_typed_literals.try_lock_for(slice) {
                    break locked;
                }
            }
        };
        match &*slot {
            // a list that stopped early serves only a cap below its end
            Some((ids, complete)) if *complete || ids.len() > max => ids.clone(),
            _ => {
                let (ids, complete) = scan_base(generation, max + 1, deadline, cancel)?;
                let ids: Arc<[u64]> = ids.into();
                *slot = Some((ids.clone(), complete));
                ids
            }
        }
    };
    let room = (max + 1).saturating_sub(base.len());
    let mut delta = Vec::new();
    let mut at = 0;
    while at < snap.dvocab_len && delta.len() < room {
        check_control(deadline, cancel)?;
        let end = snap.dvocab_len.min(at + DELTA_CHUNK);
        generation.dvocab.with(|v| {
            for i in at..end {
                if delta.len() < room && v.get(i).and_then(ill_typed_key).is_some() {
                    delta.push(i);
                }
            }
        });
        #[cfg(test)]
        DELTA_CHUNK_HOOK.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f(generation);
            }
        });
        at = end;
    }
    Ok(IllTyped { base, delta })
}

/// The ids of the literals of `generation`'s base vocabulary that are ill-typed for their
/// own datatype, in id order, at most `max`, and whether they are all of them.
fn scan_base(
    generation: &Generation,
    max: usize,
    deadline: Option<Instant>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> sparkles_core::Result<(Vec<u64>, bool)> {
    #[cfg(test)]
    scan_seam(generation);
    let vocab = &generation.vocab;
    let (lo, hi) = vocab.prefix_range(b"\"");
    let mut found: Vec<u64> = Vec::new();
    let mut ids: Vec<u64> = Vec::new();
    let mut at = lo;
    while at < hi {
        check_control(deadline, cancel)?;
        let end = hi.min(at + (1 << 16));
        ids.clear();
        ids.extend(at..end);
        vocab.get_sorted(&ids, |id, key| {
            if found.len() < max && ill_typed_key(key).is_some() {
                found.push(id);
            }
        });
        if found.len() >= max {
            return Ok((found, false));
        }
        at = end;
    }
    Ok((found, true))
}

/// The ill-typed literals of `ids`, a batch of [`IllTyped::ids`], in its order. Each is
/// judged again, so an id whose key is not an ill-typed literal is left out.
fn ill_typed_literals(snap: &Snapshot, ids: &[sparkles_core::id::Id]) -> Vec<Literal> {
    use sparkles_core::id::Tag;
    let mut out = Vec::with_capacity(ids.len());
    let base: Vec<u64> = ids
        .iter()
        .filter(|i| i.tag() == Tag::Vocab)
        .map(|i| i.payload())
        .collect();
    snap.generation
        .vocab
        .get_sorted(&base, |_, key| out.extend(ill_typed_key(key)));
    for i in ids.iter().filter(|i| i.tag() == Tag::Delta) {
        out.extend(snap.key(*i).and_then(|k| ill_typed_key(&k)));
    }
    out
}

/// Tests only: what [`ill_typed`] calls after each chunk of the delta vocabulary.
#[cfg(test)]
type ChunkHook = Box<dyn FnMut(&Generation)>;

#[cfg(test)]
thread_local! {
    /// Tests only: the page of [`Run::paged_findings`] (counted from 0) that fails with a
    /// timeout before it runs its query.
    static PAGE_TIMEOUT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    /// Tests only: replaces [`CANDIDATES_MAX`].
    static CANDIDATES_CAP: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    /// Tests only: called after each chunk of the delta vocabulary that [`ill_typed`]
    /// reads, with the lock of the delta vocabulary released.
    static DELTA_CHUNK_HOOK: std::cell::RefCell<Option<ChunkHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Tests only: the generations whose base vocabulary [`scan_base`] read, once per read,
/// and the generation whose read sleeps first.
#[cfg(test)]
static SCANNED: std::sync::Mutex<(Vec<u64>, Option<u64>)> =
    std::sync::Mutex::new((Vec::new(), None));

#[cfg(test)]
fn scan_seam(generation: &Generation) {
    let slow = {
        let mut s = SCANNED.lock().unwrap();
        s.0.push(generation.uid);
        s.1 == Some(generation.uid)
    };
    if slow {
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Tests only: the reads of `generation`'s base vocabulary.
#[cfg(test)]
fn scans_of(generation: &Generation) -> usize {
    let s = SCANNED.lock().unwrap();
    s.0.iter().filter(|&&u| u == generation.uid).count()
}

/// [`CANDIDATES_MAX`], or the cap a test set.
fn candidates_max() -> usize {
    #[cfg(test)]
    if let Some(n) = CANDIDATES_CAP.with(std::cell::Cell::get) {
        return n;
    }
    CANDIDATES_MAX
}
/// A total order of the rows of `datatype-range.rq`. Every key is an IRI, a blank node
/// or a simple literal, which compare by their strings, and together the keys identify a
/// row of the DISTINCT projection. Pages of this order therefore neither skip nor repeat
/// a row, and the rows of one finding are adjacent.
const CANDIDATE_ORDER: &str = "ORDER BY ?x ?p ?range STR(?lit) DATATYPE(?lit) LANG(?lit) ?via";

impl Run<'_> {
    fn check(&self) -> sparkles_core::Result<()> {
        check_control(self.deadline, self.cancel.map(Arc::as_ref))
    }

    fn options(
        &self,
        inferences: bool,
        bindings: Vec<(String, Term)>,
    ) -> sparkles_core::Result<QueryOptions> {
        self.check()?;
        let timeout = match self.deadline {
            Some(d) => Some(
                d.checked_duration_since(Instant::now())
                    .filter(|t| !t.is_zero())
                    .ok_or(Error::Timeout)?,
            ),
            None => None,
        };
        // chosen graphs replace the default graph, as a protocol dataset does
        let mut chosen: Vec<String> = self
            .graphs
            .iter()
            .map(|g| match g.as_str() {
                DEFAULT_GRAPH => "urn:x-arq:DefaultGraph".to_string(),
                g => g.to_string(),
            })
            .collect();
        let mut extra = Vec::new();
        if inferences {
            if chosen.is_empty() {
                extra.push(INFERRED_GRAPH.to_string());
            } else {
                chosen.push(INFERRED_GRAPH.to_string());
            }
        }
        Ok(QueryOptions {
            timeout,
            no_cache: true,
            default_graph_uris: chosen,
            default_graph_extra: extra,
            initial_bindings: bindings,
            grant: self.grant.cloned(),
            deadline: self.deadline,
            cancel: self.cancel.cloned(),
            max_memory_bytes: self.max_memory,
            ..Default::default()
        })
    }

    fn select(
        &self,
        text: &str,
        inferences: bool,
        bindings: Vec<(String, Term)>,
    ) -> sparkles_core::Result<Vec<Row>> {
        let opts = self.options(inferences, bindings)?;
        let r = sparkles_core::sparql::query(self.snap.clone(), text, &opts)?;
        let mut rows = Vec::new();
        let mut bytes = 0u64;
        for i in 0..r.table.len() {
            self.check()?;
            let mut row = Row::new();
            for (name, column) in r.vars.iter().zip(&r.table.cols) {
                if let Some(term) = r.term(column[i]) {
                    bytes = bytes.saturating_add(
                        (64 + name.len() + sparkles_core::explain::term_bytes(&term)) as u64,
                    );
                    if let Some(limit) = self.max_memory
                        && bytes > limit
                    {
                        return Err(Error::BudgetExceeded(sparkles_core::Budget {
                            kind: sparkles_core::BudgetKind::Memory,
                            limit,
                            requested: bytes,
                        }));
                    }
                    row.insert(name.clone(), term);
                }
            }
            rows.push(row);
        }
        Ok(rows)
    }

    /// The deduplicated findings (at most `limit`) and whether more remain.
    fn findings(&self, limit: usize) -> sparkles_core::Result<(Vec<Finding>, bool)> {
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

    /// The findings of `literal-not-in-datatype`, at most `limit`, and whether more may
    /// exist. They come from three sources, in this order, and a finding of a later one
    /// that an earlier one reported is left out:
    ///
    /// 1. the literals that are ill-typed for their own datatype, found by a scan of the
    ///    vocabulary ([`ill_typed`]) and then looked up in the checked graphs;
    /// 2. the candidates of the range query, judged one by one ([`Run::paged_into`]);
    /// 3. the literal typings of the inferences' generalized facts, when the check reads
    ///    the inferences and the caller has them.
    ///
    /// A deadline after at least one finding returns the findings found, truncated.
    fn paged_findings(&self, limit: usize) -> sparkles_core::Result<(Vec<Finding>, bool)> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let r = (|| {
            if self.ill_typed_into(limit, &mut out, &mut seen)? {
                return Ok(true);
            }
            if self.paged_into(limit, &mut out, &mut seen)? {
                return Ok(true);
            }
            self.typings_into(limit, &mut out)
        })();
        match r {
            Ok(truncated) => Ok((out, truncated)),
            Err(Error::Timeout) if !out.is_empty() => Ok((out, true)),
            Err(e) => Err(e),
        }
    }

    /// The findings of the literals that are ill-typed for their own datatype
    /// (`dt-type2`): the literals of the vocabulary that the table decides are outside
    /// their datatype's value space, each with the subjects and properties that have it as
    /// a value in the checked graphs. The literals are decoded and looked up in batches, in
    /// the order of their ids, those of the base vocabulary first, at most
    /// [`CANDIDATES_MAX`] of them. Returns whether the check is truncated.
    fn ill_typed_into(
        &self,
        limit: usize,
        out: &mut Vec<Finding>,
        seen: &mut HashSet<String>,
    ) -> sparkles_core::Result<bool> {
        let max = candidates_max();
        let found = match self.cancel {
            Some(cancel) => {
                ill_typed_controlled(self.snap, max, self.deadline, Some(cancel.as_ref()))?
            }
            None => ill_typed(self.snap, max, self.deadline)?,
        };
        let truncated = found.len() > max;
        let ids: Vec<sparkles_core::id::Id> = found.ids().take(max).collect();
        let via = Term::Literal(Literal::new_simple_literal("datatype"));
        for batch in ids.chunks(ILL_TYPED_BATCH) {
            let values: Vec<String> = ill_typed_literals(self.snap, batch)
                .iter()
                .map(Literal::to_string)
                .collect();
            if values.is_empty() {
                continue;
            }
            let text = format!(
                "SELECT DISTINCT ?x ?p ?lit WHERE {{ VALUES ?lit {{ {} }} ?x ?p ?lit . }}\n\
                 ORDER BY ?x ?p STR(?lit) DATATYPE(?lit) LANG(?lit)\nLIMIT {}",
                values.join(" "),
                limit - out.len() + 1
            );
            for mut row in self.select(&text, self.inferences, Vec::new())? {
                let Some(Term::Literal(l)) = row.get("lit") else {
                    continue;
                };
                let range = Term::NamedNode(l.datatype().into_owned());
                row.insert("range".into(), range);
                row.insert("via".into(), via.clone());
                let Some(key) = self.key(&row) else { continue };
                if seen.contains(&key) {
                    continue;
                }
                let Some(mut f) = self.finding(std::slice::from_ref(&row), Basis::Asserted)? else {
                    continue;
                };
                if out.len() == limit {
                    return Ok(true);
                }
                if self.inferences && !self.occurs_asserted(&row)? {
                    f.basis = Basis::UsesInferences;
                }
                seen.insert(key);
                out.push(f);
            }
        }
        Ok(truncated)
    }

    /// Whether the asserted data has the row's literal as the value of its subject and
    /// property.
    fn occurs_asserted(&self, row: &Row) -> sparkles_core::Result<bool> {
        let bindings = ["x", "p", "lit"]
            .iter()
            .filter_map(|v| Some((v.to_string(), row.get(*v)?.clone())))
            .collect();
        let rows = self.select("SELECT * WHERE { ?x ?p ?lit } LIMIT 1", false, bindings)?;
        Ok(!rows.is_empty())
    }

    /// The findings of the literal typings of the inferences that no finding before
    /// names with the same literal and range: each literal with the datatype that types it
    /// and whose value space it is outside of, marked as using the inferences. Only the
    /// literals that the checked graphs hold are reported, looked up
    /// [`ILL_TYPED_BATCH`] at a time, so a typing of a literal in a graph the options leave
    /// out, or deleted since the materialization, is not. Returns whether the check is
    /// truncated.
    fn typings_into(&self, limit: usize, out: &mut Vec<Finding>) -> sparkles_core::Result<bool> {
        let Some(typings) = self.typings else {
            return Ok(false);
        };
        let one = |f: &Finding, name: &str| {
            f.evidence.iter().find_map(|(n, e)| match e {
                Evidence::One(t) if *n == name => Some(t.to_string()),
                _ => None,
            })
        };
        let named: HashSet<(String, String)> = out
            .iter()
            .filter_map(|f| Some((one(f, "literal")?, one(f, "range")?)))
            .collect();
        let mut facts: Vec<(String, &Literal, &NamedNode)> = typings
            .facts
            .iter()
            .map(|(l, dt)| (l.to_string(), l, dt))
            .collect();
        facts.sort_by(|a, b| (&a.0, a.2.as_str()).cmp(&(&b.0, b.2.as_str())));
        facts.dedup_by(|a, b| a.0 == b.0 && a.2 == b.2);
        facts.retain(|(text, lit, dt)| {
            !named.contains(&(text.clone(), Term::NamedNode((*dt).clone()).to_string()))
                && in_value_space(lit, dt.as_str()) == Some(false)
        });
        // only the literals that the checked graphs hold, as for the other sources
        for batch in facts.chunks(ILL_TYPED_BATCH) {
            let mut values: Vec<&str> = batch.iter().map(|f| f.0.as_str()).collect();
            values.dedup();
            let text = format!(
                "SELECT DISTINCT ?lit WHERE {{ VALUES ?lit {{ {} }} ?x ?p ?lit . }}",
                values.join(" ")
            );
            let present: HashSet<String> = self
                .select(&text, self.inferences, Vec::new())?
                .into_iter()
                .filter_map(|row| row.get("lit").map(Term::to_string))
                .collect();
            for (text, lit, dt) in batch {
                if !present.contains(text) {
                    continue;
                }
                if out.len() == limit {
                    return Ok(true);
                }
                let (l, d) = (
                    Term::Literal((*lit).clone()),
                    Term::NamedNode((*dt).clone()),
                );
                let n = |t: &Term| self.names.show(t);
                out.push(Finding {
                    check: self.check.id,
                    rule: self.check.rules[0],
                    severity: self.check.severity,
                    message: format!(
                        "{}: the value is not in the value space of {}, which the inferences give as its type",
                        n(&l),
                        n(&d)
                    ),
                    focus: l.clone(),
                    evidence: vec![("range", Evidence::One(d)), ("literal", Evidence::One(l))],
                    basis: Basis::UsesInferences,
                });
            }
        }
        Ok(false)
    }

    /// The candidates of the range query, at most `limit` findings in `out` with those
    /// found before, leaving out the findings whose keys are in `seen`. Each candidate is
    /// judged before it counts toward the limit. The candidates are read in pages of one
    /// total order ([`CANDIDATE_ORDER`]). Each page holds its rows, and the query's sort
    /// keeps the rows up to the end of the page, at most 278,528. Every page runs the
    /// query and its sort again, which trades repeated query work for that bound. The
    /// pages stop at the first finding past the limit, when the candidates run out, at
    /// the deadline, or after [`CANDIDATES_MAX`] candidate rows. Returns whether the
    /// check is truncated, which it is in the first and the last case.
    fn paged_into(
        &self,
        limit: usize,
        out: &mut Vec<Finding>,
        seen: &mut HashSet<String>,
    ) -> sparkles_core::Result<bool> {
        let mut offset = 0usize;
        let mut page = (2 * limit + 2).clamp(PAGE_FIRST, PAGE_MAX);
        #[cfg(test)]
        let mut pages = 0usize;
        loop {
            #[cfg(test)]
            {
                if PAGE_TIMEOUT.with(std::cell::Cell::get) == Some(pages) {
                    return Err(Error::Timeout);
                }
                pages += 1;
            }
            let text = format!(
                "{}\n{CANDIDATE_ORDER}\nLIMIT {page} OFFSET {offset}",
                self.text
            );
            let rows = self.select(&text, self.inferences, Vec::new())?;
            let full = rows.len() >= page;
            let n = rows.len();
            // the rows of one finding are adjacent in the order
            // (key, position of the first row in the page, rows)
            let mut groups: Vec<(String, usize, Vec<Row>)> = Vec::new();
            for (i, row) in rows.into_iter().enumerate() {
                let Some(key) = self.key(&row) else { continue };
                match groups.last_mut() {
                    Some((k, _, g)) if *k == key => g.push(row),
                    _ => groups.push((key, i, vec![row])),
                }
            }
            // The last group of a full page may continue on the next page, so it is
            // read again there, unless it starts the page.
            let mut consumed = n;
            if full && let Some(&(_, start, _)) = groups.last() {
                if start > 0 {
                    consumed = start;
                    groups.pop();
                } else if page < PAGE_MAX {
                    page = (page * 2).min(PAGE_MAX);
                    continue;
                }
            }
            for (key, _, rows) in groups {
                if seen.contains(&key) {
                    continue;
                }
                let Some(mut f) = self.finding(&rows, Basis::Asserted)? else {
                    continue;
                };
                if out.len() == limit {
                    return Ok(true);
                }
                if self.inferences && !self.holds_asserted(&rows[0])? {
                    f.basis = Basis::UsesInferences;
                }
                seen.insert(key);
                out.push(f);
            }
            if !full {
                return Ok(false);
            }
            offset += consumed;
            if offset >= candidates_max() {
                return Ok(true);
            }
            page = (page * 2).min(PAGE_MAX);
        }
    }

    /// The same query with the row's bindings, over the asserted data only.
    fn holds_asserted(&self, row: &Row) -> sparkles_core::Result<bool> {
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
            LITERAL_NOT_IN_DATATYPE => {
                format!("{} {} {} {}", t("x")?, t("p")?, t("lit")?, t("range")?)
            }
            _ => return None,
        })
    }

    fn finding(&self, rows: &[Row], basis: Basis) -> sparkles_core::Result<Option<Finding>> {
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
            LITERAL_NOT_IN_DATATYPE => {
                let (Some(x), Some(p), Some(range), Some(Term::Literal(lit))) =
                    (get("x"), get("p"), get("range"), get("lit"))
                else {
                    return Ok(None);
                };
                let Term::NamedNode(r) = &range else {
                    return Ok(None);
                };
                // only a literal the table decides is outside is reported
                if in_value_space(&lit, r.as_str()) != Some(false) {
                    return Ok(None);
                }
                let by_range = matches!(get("via"), Some(Term::Literal(v)) if v.value() == "range");
                let l = Term::Literal(lit);
                let msg = if by_range {
                    format!(
                        "{} {} {}: the value is not in the value space of {}, a range of {}",
                        n(&x),
                        n(&p),
                        n(&l),
                        n(&range),
                        n(&p)
                    )
                } else {
                    format!(
                        "{} {} {}: the literal is ill-typed for its datatype {}",
                        n(&x),
                        n(&p),
                        n(&l),
                        n(&range)
                    )
                };
                (
                    x,
                    vec![
                        ("property", Evidence::One(p)),
                        ("range", Evidence::One(range)),
                        ("literal", Evidence::One(l)),
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
    fn path_to_nothing(&self, c: &Term) -> sparkles_core::Result<Vec<Term>> {
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

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";
const RDF_PLAIN_LITERAL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";

/// The bounds of xsd:integer and its derived types, `None` for no bound.
fn integer_bounds(local: &str) -> Option<(Option<i128>, Option<i128>)> {
    Some(match local {
        "integer" => (None, None),
        "long" => (Some(i64::MIN as i128), Some(i64::MAX as i128)),
        "int" => (Some(i32::MIN as i128), Some(i32::MAX as i128)),
        "short" => (Some(i16::MIN as i128), Some(i16::MAX as i128)),
        "byte" => (Some(i8::MIN as i128), Some(i8::MAX as i128)),
        "nonNegativeInteger" => (Some(0), None),
        "nonPositiveInteger" => (None, Some(0)),
        "positiveInteger" => (Some(1), None),
        "negativeInteger" => (None, Some(-1)),
        "unsignedLong" => (Some(0), Some(u64::MAX as i128)),
        "unsignedInt" => (Some(0), Some(u32::MAX as i128)),
        "unsignedShort" => (Some(0), Some(u16::MAX as i128)),
        "unsignedByte" => (Some(0), Some(u8::MAX as i128)),
        _ => return None,
    })
}

const STRING_TYPES: &[&str] = &[
    "string",
    "normalizedString",
    "token",
    "language",
    "Name",
    "NCName",
    "NMTOKEN",
];

/// An exact number of the xsd:decimal value space, read from a lexical form: its sign,
/// the digits of its integer part without leading zeros (empty for zero), and whether
/// its fractional part is nonzero. Zero is never negative.
struct Exact<'a> {
    neg: bool,
    int: &'a str,
    frac: bool,
}

impl Exact<'_> {
    /// The XSD 1.1 lexical space of xsd:decimal, `(\+|-)?([0-9]+(\.[0-9]*)?|\.[0-9]+)`, or
    /// with `integer` that of xsd:integer, `[\-+]?[0-9]+`. Any number of digits is read
    /// exactly.
    fn parse(lex: &str, integer: bool) -> Option<Exact<'_>> {
        let (neg, body) = match lex.as_bytes().first() {
            Some(b'-') => (true, &lex[1..]),
            Some(b'+') => (false, &lex[1..]),
            _ => (false, lex),
        };
        let (int, frac) = match body.split_once('.') {
            Some(_) if integer => return None,
            Some((i, f)) => (i, f),
            None => (body, ""),
        };
        if (int.is_empty() && frac.is_empty())
            || !int.bytes().all(|b| b.is_ascii_digit())
            || !frac.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let int = int.trim_start_matches('0');
        let frac = frac.bytes().any(|b| b != b'0');
        Some(Exact {
            neg: neg && (!int.is_empty() || frac),
            int,
            frac,
        })
    }

    /// The order of an integral number and an integer bound, compared by sign and then
    /// by the length and the digits of the magnitudes.
    fn cmp_bound(&self, bound: i128) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let mag = bound.unsigned_abs().to_string();
        let mag = mag.trim_start_matches('0');
        let by_mag = |a: &str, b: &str| a.len().cmp(&b.len()).then_with(|| a.cmp(b));
        match (self.neg, bound < 0) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => by_mag(self.int, mag),
            (true, true) => by_mag(mag, self.int),
        }
    }

    /// Whether the number is an integer within the bounds of an xsd:integer type.
    fn in_integer_type(&self, local: &str) -> bool {
        let Some((lo, hi)) = integer_bounds(local) else {
            return false;
        };
        !self.frac
            && lo.is_none_or(|lo| self.cmp_bound(lo).is_ge())
            && hi.is_none_or(|hi| self.cmp_bound(hi).is_le())
    }
}

/// Whether `c` matches the XML 1.0 (fifth edition) production NameStartChar.
fn name_start_char(c: char) -> bool {
    matches!(c,
        ':' | 'A'..='Z' | '_' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

/// Whether `c` matches the XML 1.0 (fifth edition) production NameChar.
fn name_char(c: char) -> bool {
    name_start_char(c)
        || matches!(c,
            '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

/// Whether a string lies in the value space of a string type: xsd:string or one derived
/// from it by facets. The lexical form is used as it is, without the XML whitespace
/// processing, since RDF maps lexical forms directly.
fn string_in(v: &str, local: &str) -> bool {
    let normalized = !v.contains(['\r', '\n', '\t']);
    let token = normalized && !v.starts_with(' ') && !v.ends_with(' ') && !v.contains("  ");
    match local {
        "string" => true,
        "normalizedString" => normalized,
        "token" => token,
        "language" => {
            let mut parts = v.split('-');
            let first = parts.next().unwrap_or("");
            (1..=8).contains(&first.len())
                && first.bytes().all(|b| b.is_ascii_alphabetic())
                && parts.all(|p| {
                    (1..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric())
                })
        }
        "NMTOKEN" => !v.is_empty() && v.chars().all(name_char),
        "Name" | "NCName" => {
            let mut cs = v.chars();
            cs.next().is_some_and(name_start_char)
                && cs.all(name_char)
                && (local == "Name" || !v.contains(':'))
        }
        _ => false,
    }
}

/// Whether an xsd:dateTime lexical form ends with a timezone, `Z` or `±hh:mm`.
fn has_timezone(lex: &str) -> bool {
    let b = lex.as_bytes();
    lex.ends_with('Z')
        || (b.len() > 6 && matches!(b[b.len() - 6], b'+' | b'-') && b[b.len() - 3] == b':')
}

/// Whether a literal of `local` (an XSD local name) is well typed, for the datatypes of
/// the last row of the check's table, or `None` for the others.
fn well_typed_other(lex: &str, local: &str) -> Option<bool> {
    let lex_t = lex.trim();
    Some(match local {
        "boolean" => matches!(lex_t, "true" | "false" | "1" | "0"),
        "dateTime" => matches!(
            sparkles_core::sparql::value::Value::from_typed(lex_t, &format!("{XSD}dateTime")),
            sparkles_core::sparql::value::Value::DateTime(_)
        ),
        "dateTimeStamp" => well_typed_other(lex_t, "dateTime")? && has_timezone(lex_t),
        "anyURI" => true,
        "hexBinary" => {
            lex_t.len().is_multiple_of(2) && lex_t.bytes().all(|b| b.is_ascii_hexdigit())
        }
        "base64Binary" => {
            let s: Vec<u8> = lex_t.bytes().filter(|b| *b != b' ').collect();
            let body = s.iter().take_while(|b| **b != b'=').count();
            s.len().is_multiple_of(4)
                && s.len() - body <= 2
                && s[..body]
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
                && s[body..].iter().all(|b| *b == b'=')
        }
        _ => return None,
    })
}

/// The families of value spaces the check implements. Under the OWL 2 datatype map the
/// value spaces of different families are disjoint: owl:real (holding xsd:decimal and
/// the integer types), xsd:double and xsd:float are pairwise disjoint, strings are
/// neither IRIs nor language-tagged strings, and booleans, date-times, xsd:hexBinary
/// values and xsd:base64Binary values are values of their own (OWL 2 section 4.5 makes
/// the two binary value spaces disjoint). Within a family the value spaces overlap, and
/// [`in_value_space`] decides membership from the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    /// language-tagged strings (rdf:langString)
    Lang,
    /// xsd:decimal, xsd:integer and the types derived from xsd:integer
    Exact,
    /// xsd:double and xsd:float, which the check treats as one family
    Float,
    /// xsd:string and the types derived from it
    Str,
    Boolean,
    /// xsd:dateTime and xsd:dateTimeStamp
    DateTime,
    AnyUri,
    /// xsd:hexBinary
    Hex,
    /// xsd:base64Binary
    Base64,
}

/// The family of an XSD datatype whose value space the check implements.
fn family(local: &str) -> Option<Family> {
    Some(match local {
        "decimal" => Family::Exact,
        l if integer_bounds(l).is_some() => Family::Exact,
        "double" | "float" => Family::Float,
        l if STRING_TYPES.contains(&l) => Family::Str,
        "boolean" => Family::Boolean,
        "dateTime" | "dateTimeStamp" => Family::DateTime,
        "anyURI" => Family::AnyUri,
        "hexBinary" => Family::Hex,
        "base64Binary" => Family::Base64,
        _ => return None,
    })
}

/// Whether [`in_value_space`] can decide some literal for the datatype `range`: the
/// ranges of the table.
pub(crate) fn decidable_range(range: &str) -> bool {
    matches!(range, RDF_LANG_STRING | RDF_PLAIN_LITERAL)
        || range.strip_prefix(XSD).is_some_and(|l| family(l).is_some())
}

/// Whether `lit` is in the value space of the datatype `range` (OWL 2 RL `dt-not-type`),
/// or `None` when the check does not decide it. It is undecided for a range outside the
/// table and for a literal whose own datatype is outside it, such as xsd:date or a
/// datatype outside the XSD namespace. A supported literal that is ill-typed for its
/// own datatype denotes no value and is outside every supported range.
pub fn in_value_space(lit: &Literal, range: &str) -> Option<bool> {
    let lex = lit.value();
    let dt = lit.datatype();
    let dt = dt.as_str();
    // the literal's own datatype and family; any other datatype leaves the answer open
    let (local, src) = if lit.language().is_some() {
        ("", Family::Lang)
    } else {
        let local = dt.strip_prefix(XSD)?;
        (local, family(local)?)
    };
    // the range's family
    enum Target<'r> {
        LangString,
        PlainLiteral,
        Xsd(&'r str, Family),
    }
    let target = match range {
        RDF_LANG_STRING => Target::LangString,
        RDF_PLAIN_LITERAL => Target::PlainLiteral,
        r => {
            let r = r.strip_prefix(XSD)?;
            Target::Xsd(r, family(r)?)
        }
    };
    // the literal's own well-typedness
    let exact = match src {
        Family::Exact => match Exact::parse(lex.trim(), local != "decimal") {
            Some(v) if local == "decimal" || v.in_integer_type(local) => Some(v),
            _ => return Some(false),
        },
        _ => None,
    };
    let well_typed = match src {
        Family::Lang | Family::Exact => true,
        Family::Float => sparkles_core::sparql::value::Value::from_typed(lex, dt).is_numeric(),
        Family::Str => string_in(lex, local),
        _ => well_typed_other(lex, local)?,
    };
    if !well_typed {
        return Some(false);
    }
    // membership of the value in the range
    Some(match target {
        Target::LangString => src == Family::Lang,
        Target::PlainLiteral => matches!(src, Family::Lang | Family::Str),
        Target::Xsd(_, f) if f != src => false,
        Target::Xsd(r, Family::Exact) => {
            r == "decimal" || exact.is_some_and(|v| v.in_integer_type(r))
        }
        Target::Xsd(r, Family::Str) => string_in(lex, r),
        Target::Xsd("dateTimeStamp", _) => local == "dateTimeStamp" || has_timezone(lex.trim()),
        // the other families hold every well-typed value of every datatype in them
        Target::Xsd(..) => true,
    })
}

#[cfg(test)]
mod value_space_tests {
    use super::in_value_space;
    use oxrdf::{Literal, NamedNode};

    fn typed(lex: &str, dt: &str) -> Literal {
        Literal::new_typed_literal(
            lex,
            NamedNode::new_unchecked(format!("http://www.w3.org/2001/XMLSchema#{dt}")),
        )
    }

    fn xsd(local: &str) -> String {
        format!("http://www.w3.org/2001/XMLSchema#{local}")
    }

    #[test]
    fn the_table_decides_value_space_membership() {
        let yes =
            |l: Literal, r: &str| assert_eq!(in_value_space(&l, &xsd(r)), Some(true), "{l} {r}");
        let no =
            |l: Literal, r: &str| assert_eq!(in_value_space(&l, &xsd(r)), Some(false), "{l} {r}");
        yes(typed("1.0", "decimal"), "integer");
        no(typed("1.5", "decimal"), "integer");
        yes(typed("1.5", "decimal"), "decimal");
        no(typed("abc", "integer"), "integer");
        no(typed("300", "integer"), "byte");
        yes(typed("-128", "integer"), "byte");
        yes(
            typed("18446744073709551615", "unsignedLong"),
            "unsignedLong",
        );
        no(typed("-1", "integer"), "nonNegativeInteger");
        no(typed("1", "integer"), "double");
        yes(typed("1", "float"), "double");
        no(typed("NaNx", "double"), "double");
        no(typed("1", "integer"), "string");
        yes(typed("en-US", "string"), "language");
        no(typed("not a token ", "string"), "token");
        no(typed("1abc", "string"), "NCName");
        no(typed("a:b", "string"), "NCName");
        yes(typed("a:b", "string"), "Name");
        no(typed("yes", "boolean"), "boolean");
        yes(typed("0", "boolean"), "boolean");
        yes(typed("2020-01-01T00:00:00Z", "dateTimeStamp"), "dateTime");
        no(typed("2020-01-01T00:00:00", "dateTime"), "dateTimeStamp");
        no(typed("2020-13-01T00:00:00", "dateTime"), "dateTime");
        no(typed("abc", "hexBinary"), "hexBinary");
        yes(typed("0FAB", "hexBinary"), "hexBinary");
        yes(typed("aGk=", "base64Binary"), "base64Binary");
        no(typed("true", "boolean"), "dateTime");
        let tagged = Literal::new_language_tagged_literal_unchecked("x", "en");
        no(tagged.clone(), "string");
        let lang = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";
        let plain = "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral";
        assert_eq!(in_value_space(&tagged, lang), Some(true));
        assert_eq!(in_value_space(&tagged, plain), Some(true));
        assert_eq!(
            in_value_space(&Literal::new_simple_literal("x"), lang),
            Some(false)
        );
        assert_eq!(
            in_value_space(&Literal::new_simple_literal("x"), plain),
            Some(true)
        );
        // what the table leaves open
        let custom = Literal::new_typed_literal("x", NamedNode::new_unchecked("http://ex.org/dt"));
        assert_eq!(in_value_space(&custom, &xsd("integer")), None);
        assert_eq!(in_value_space(&typed("1", "integer"), &xsd("date")), None);
        assert_eq!(
            in_value_space(
                &typed("1", "integer"),
                "http://www.w3.org/2000/01/rdf-schema#Literal"
            ),
            None
        );
    }

    fn check(l: &Literal, r: &str, want: Option<bool>) {
        assert_eq!(in_value_space(l, &xsd(r)), want, "{l} in xsd:{r}");
    }

    fn rdf(local: &str) -> String {
        format!("http://www.w3.org/1999/02/22-rdf-syntax-ns#{local}")
    }

    #[test]
    fn f11_integers_are_compared_exactly() {
        let yes = |lex: &str, dt: &str, r: &str| check(&typed(lex, dt), r, Some(true));
        let no = |lex: &str, dt: &str, r: &str| check(&typed(lex, dt), r, Some(false));
        let two127 = "170141183460469231731687303715884105728";
        let max127 = "170141183460469231731687303715884105727";
        let neg_two127 = "-170141183460469231731687303715884105728";
        let neg_max127 = "-170141183460469231731687303715884105727";
        for v in [two127, max127, neg_two127, neg_max127] {
            yes(v, "integer", "integer");
            yes(v, "integer", "decimal");
            no(v, "integer", "long");
            no(v, "integer", "byte");
        }
        // a huge integer is in no bounded type
        let huge = "100000000000000000000000000000000000000000000000000";
        no(huge, "integer", "byte");
        no(huge, "integer", "unsignedLong");
        yes(huge, "integer", "positiveInteger");
        yes(huge, "integer", "nonNegativeInteger");
        no(huge, "integer", "nonPositiveInteger");
        no(huge, "integer", "negativeInteger");
        yes(two127, "positiveInteger", "positiveInteger");
        yes(two127, "positiveInteger", "integer");
        no(two127, "positiveInteger", "nonPositiveInteger");
        let neg_huge = "-100000000000000000000000000000000000000000000000000";
        yes(neg_huge, "integer", "negativeInteger");
        yes(neg_huge, "negativeInteger", "nonPositiveInteger");
        no(neg_huge, "integer", "nonNegativeInteger");
        no(neg_huge, "integer", "positiveInteger");
        // the bounds of xsd:unsignedLong
        yes("18446744073709551615", "integer", "unsignedLong");
        no("18446744073709551616", "integer", "unsignedLong");
        no("18446744073709551616", "unsignedLong", "integer");
        yes("-9223372036854775808", "integer", "long");
        no("-9223372036854775809", "integer", "long");
        // leading zeros and signed zeros
        yes("000000000000000000000000000000000000000001", "byte", "byte");
        yes(
            "-000000000000000000000000000000000000000128",
            "integer",
            "byte",
        );
        no(
            "-000000000000000000000000000000000000000129",
            "integer",
            "byte",
        );
        for z in ["-0", "+0", "-000", "0"] {
            yes(z, "integer", "nonNegativeInteger");
            yes(z, "integer", "nonPositiveInteger");
            no(z, "integer", "positiveInteger");
            no(z, "integer", "negativeInteger");
        }
        yes("-0.0", "decimal", "nonNegativeInteger");
        // integral decimals beyond i128 are integers
        let big_dec = "170141183460469231731687303715884105728.000";
        yes(big_dec, "decimal", "integer");
        yes(big_dec, "decimal", "decimal");
        no(big_dec, "decimal", "long");
        let frac = "170141183460469231731687303715884105728.5";
        no(frac, "decimal", "integer");
        yes(frac, "decimal", "decimal");
        // the lexical spaces: XSD 1.1 decimals allow "1." and ".5", integers do not
        yes("1.", "decimal", "decimal");
        yes("1.", "decimal", "integer");
        yes(".5", "decimal", "decimal");
        no(".5", "decimal", "integer");
        no("1.", "integer", "integer");
        no(".", "decimal", "decimal");
        no("1e3", "integer", "integer");
        no("1e3", "decimal", "decimal");
        no("+-1", "integer", "integer");
        no("+-1", "decimal", "decimal");
        no("", "integer", "integer");
        no("", "decimal", "decimal");
        no("-", "integer", "integer");
        yes("+7", "integer", "byte");
    }

    #[test]
    fn f12_xml_names_follow_the_xml_productions() {
        let s = |v: &str| typed(v, "string");
        let yes = |v: &str, r: &str| check(&s(v), r, Some(true));
        let no = |v: &str, r: &str| check(&s(v), r, Some(false));
        // a combining mark is a NameChar but not a NameStartChar
        yes("a\u{0301}", "NCName");
        yes("a\u{0301}", "NMTOKEN");
        yes("\u{0301}", "NMTOKEN");
        no("\u{0301}a", "NCName");
        yes("a:b", "Name");
        no("a:b", "NCName");
        yes(":a", "Name");
        // the start character ranges and their endpoints
        for c in [
            '\u{10000}',
            '\u{EFFFF}',
            '\u{2070}',
            '\u{C0}',
            '\u{37F}',
            '\u{FFFD}',
        ] {
            yes(&format!("{c}x"), "NCName");
        }
        for c in [
            '\u{F0000}',
            '\u{37E}',
            '\u{D7}',
            '\u{F7}',
            '\u{2000}',
            '\u{FFFE}',
        ] {
            no(&format!("{c}x"), "NCName");
            no(&format!("x{c}"), "NCName");
            no(&format!("x{c}"), "NMTOKEN");
        }
        // NameChars that cannot start a name
        for c in [
            '\u{B7}', '\u{300}', '\u{36F}', '\u{203F}', '\u{2040}', '-', '.', '1',
        ] {
            no(&format!("{c}a"), "NCName");
            no(&format!("{c}a"), "Name");
            yes(&format!("a{c}"), "NCName");
            yes(&format!("{c}a"), "NMTOKEN");
        }
        no("", "NMTOKEN");
        no("", "Name");
        // no whitespace processing of the raw lexical form
        check(&typed(" a\t b ", "token"), "token", Some(false));
        check(
            &typed("\t", "normalizedString"),
            "normalizedString",
            Some(false),
        );
        check(&typed(" a", "NCName"), "string", Some(false));
    }

    #[test]
    fn f13_overlapping_value_spaces() {
        let yes = |lex: &str, dt: &str, r: &str| check(&typed(lex, dt), r, Some(true));
        let no = |lex: &str, dt: &str, r: &str| check(&typed(lex, dt), r, Some(false));
        // xsd:hexBinary and xsd:base64Binary are disjoint (OWL 2 section 4.5)
        no("", "hexBinary", "base64Binary");
        no("", "base64Binary", "hexBinary");
        no("0FAB", "hexBinary", "base64Binary");
        no("D6s=", "base64Binary", "hexBinary");
        yes("0FAB", "hexBinary", "hexBinary");
        yes("D6s=", "base64Binary", "base64Binary");
        no("zz", "hexBinary", "base64Binary");
        no("abc", "base64Binary", "hexBinary");
        // a dateTime with a timezone is a dateTimeStamp
        for v in [
            "2020-01-01T00:00:00Z",
            "2020-01-01T00:00:00+05:30",
            "2020-01-01T00:00:00-08:00",
        ] {
            yes(v, "dateTime", "dateTimeStamp");
            yes(v, "dateTimeStamp", "dateTime");
        }
        no("2020-01-01T00:00:00", "dateTime", "dateTimeStamp");
        no("2020-13-01T00:00:00Z", "dateTime", "dateTimeStamp");
        // known disjoint pairs stay decided
        no("true", "boolean", "dateTime");
        no("http://ex.org/", "anyURI", "boolean");
        no("1", "integer", "boolean");
        no("0FAB", "hexBinary", "string");
    }

    #[test]
    fn f14_plain_literals_include_the_derived_strings() {
        let plain = rdf("PlainLiteral");
        let in_plain = |l: Literal| in_value_space(&l, &plain);
        for (lex, dt) in [
            ("abc", "token"),
            ("a b", "normalizedString"),
            ("a:b", "Name"),
            ("ab", "NCName"),
            ("-1", "NMTOKEN"),
            ("en-US", "language"),
            ("a  b\t", "string"),
        ] {
            assert_eq!(in_plain(typed(lex, dt)), Some(true), "{lex} {dt}");
        }
        assert_eq!(in_plain(Literal::new_simple_literal("x")), Some(true));
        assert_eq!(
            in_plain(Literal::new_language_tagged_literal_unchecked("x", "en")),
            Some(true)
        );
        // ill-typed sources and non-strings are outside
        for (lex, dt) in [
            ("a  b", "token"),
            ("a\tb", "normalizedString"),
            ("1a", "NCName"),
            ("a:b", "NCName"),
            ("", "NMTOKEN"),
            ("1", "integer"),
            ("true", "boolean"),
        ] {
            assert_eq!(in_plain(typed(lex, dt)), Some(false), "{lex} {dt}");
        }
    }

    #[test]
    fn f15_unsupported_source_datatypes_are_undecided() {
        let custom = typed("1", "custom");
        let external =
            Literal::new_typed_literal("1", NamedNode::new_unchecked("http://ex.org/dt"));
        let date = typed("2020-01-01", "date");
        for l in [&custom, &external, &date, &typed("P1D", "duration")] {
            for r in [
                "integer",
                "string",
                "decimal",
                "dateTime",
                "boolean",
                "hexBinary",
            ] {
                check(l, r, None);
            }
            assert_eq!(in_value_space(l, &rdf("PlainLiteral")), None, "{l}");
            assert_eq!(in_value_space(l, &rdf("langString")), None, "{l}");
        }
        check(&typed("2020", "gYear"), "integer", None);
        // the known disjoint pairs are still decided
        check(&typed("1", "integer"), "string", Some(false));
        check(&typed("true", "boolean"), "decimal", Some(false));
        check(&typed("1.5", "double"), "decimal", Some(false));
        assert_eq!(
            in_value_space(&typed("1", "integer"), &rdf("langString")),
            Some(false)
        );
    }
}

#[cfg(test)]
mod paging_tests {
    use super::*;
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::store::{Store, StoreOptions};

    const ID: &str = LITERAL_NOT_IN_DATATYPE;

    /// `count` integer literals under a range of non-negative integers, those at the
    /// positions of `negative` negative.
    fn store(count: usize, negative: &[usize]) -> Store {
        let mut ttl = String::from(
            "@prefix ex: <http://ex.org/> .
             @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
             @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
             ex:n rdfs:range xsd:nonNegativeInteger .\n",
        );
        for i in 0..count {
            let v = if negative.contains(&i) {
                -(i as i64) - 1
            } else {
                i as i64
            };
            ttl.push_str(&format!("ex:i{i} ex:n \"{v}\"^^xsd:integer .\n"));
        }
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            ttl.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        s
    }

    fn run(s: &Store, limit: usize) -> (CheckStatus, usize, ReportStatus) {
        let r = diagnose(
            s.snapshot(),
            &DiagnoseOptions {
                checks: vec![ID.into()],
                limit,
                ..Default::default()
            },
        )
        .unwrap();
        let c = r.checks.iter().find(|c| c.id == ID).unwrap();
        (c.status, c.findings, r.status)
    }

    /// The ill-typed literals remembered for `g`.
    fn remembered(g: &Arc<Generation>) -> usize {
        g.ill_typed_literals
            .lock()
            .as_ref()
            .map_or(0, |e| e.0.len())
    }

    /// Clears the test seams when it drops.
    struct Seams;

    impl Drop for Seams {
        fn drop(&mut self) {
            PAGE_TIMEOUT.with(|c| c.set(None));
            CANDIDATES_CAP.with(|c| c.set(None));
            DELTA_CHUNK_HOOK.with(|h| *h.borrow_mut() = None);
        }
    }

    /// A deadline after the first page keeps the violations that page held, and the
    /// check is truncated. A deadline before the first finding is a timeout.
    #[test]
    fn a_timeout_after_findings_truncates() {
        let _seams = Seams;
        // in candidate order (by subject name) the first page of 256 rows holds
        // i0, i100 and i200, and the later ones none
        let s = store(600, &[0, 100, 200]);
        assert_eq!(
            run(&s, 10),
            (CheckStatus::Violations, 3, ReportStatus::ViolationsFound)
        );
        PAGE_TIMEOUT.with(|c| c.set(Some(1)));
        assert_eq!(
            run(&s, 10),
            (CheckStatus::Truncated, 3, ReportStatus::ViolationsFound)
        );
        PAGE_TIMEOUT.with(|c| c.set(Some(0)));
        assert_eq!(
            run(&s, 10),
            (CheckStatus::Timeout, 0, ReportStatus::Incomplete)
        );
    }

    /// The check stops after the candidate cap and reports itself truncated, also
    /// when it found nothing before.
    #[test]
    fn the_candidate_cap_truncates() {
        let _seams = Seams;
        // the only violation has the last name in candidate order ("i999")
        let s = store(1_200, &[999]);
        assert_eq!(
            run(&s, 10),
            (CheckStatus::Violations, 1, ReportStatus::ViolationsFound)
        );
        CANDIDATES_CAP.with(|c| c.set(Some(600)));
        assert_eq!(
            run(&s, 10),
            (CheckStatus::Truncated, 0, ReportStatus::Incomplete)
        );
    }

    /// The foci of the check's findings over `graphs`, sorted.
    fn foci(s: &Store, graphs: &[&str]) -> Vec<String> {
        let r = diagnose(
            s.snapshot(),
            &DiagnoseOptions {
                checks: vec![ID.into()],
                graphs: graphs.iter().map(|g| g.to_string()).collect(),
                ..Default::default()
            },
        )
        .unwrap();
        let mut f: Vec<String> = r.findings.iter().map(|f| f.focus.to_string()).collect();
        f.sort();
        f
    }

    /// A store with `n` ill-typed `xsd:int` literals in the default graph, and one valid.
    fn ill_typed_store(n: usize) -> Store {
        let xsd = "http://www.w3.org/2001/XMLSchema#";
        let s = Store::in_memory(StoreOptions::default());
        let mut nq = format!("<http://ex.org/k> <http://ex.org/v> \"7\"^^<{xsd}int> .\n");
        for i in 0..n {
            nq.push_str(&format!(
                "<http://ex.org/s{i}> <http://ex.org/v> \"bad{i}\"^^<{xsd}int> .\n"
            ));
        }
        s.load(&[Source::from_bytes(nq.into_bytes(), RdfFormat::NQuads, None)])
            .unwrap();
        s
    }

    /// P2-1 of the follow-up's review: the ill-typed literals remembered for a generation
    /// stop at the candidate cap, so a dataset full of them does not pin them all, and
    /// the check reports itself truncated.
    #[test]
    fn f16_ill_typed_memory_is_bounded() {
        let _seams = Seams;
        let s = ill_typed_store(20);
        CANDIDATES_CAP.with(|c| c.set(Some(5)));
        assert_eq!(
            run(&s, 100),
            (CheckStatus::Truncated, 5, ReportStatus::ViolationsFound)
        );
        let held = remembered(&s.snapshot().generation);
        assert!(held <= 6, "{held} ids remembered");
    }

    /// P3-3 of the follow-up's review: two first checks of one generation at once read its
    /// base vocabulary once, the second waiting for the first.
    #[test]
    fn f16_concurrent_first_scans_share_one_read() {
        let s = ill_typed_store(3);
        let generation = s.snapshot().generation.clone();
        SCANNED.lock().unwrap().1 = Some(generation.uid);
        let want = (CheckStatus::Violations, 3, ReportStatus::ViolationsFound);
        std::thread::scope(|t| {
            let a = t.spawn(|| run(&s, 10));
            let b = t.spawn(|| run(&s, 10));
            assert_eq!(a.join().unwrap(), want);
            assert_eq!(b.join().unwrap(), want);
        });
        SCANNED.lock().unwrap().1 = None;
        assert_eq!(scans_of(&generation), 1);
    }

    /// P3-4 of the follow-up's review: the delta vocabulary is read in chunks, and its lock
    /// is free between them, so a writer that adds a term waits for one chunk at most. The
    /// hook adds a term on the reading thread, which would deadlock if the read still held
    /// the lock.
    #[test]
    fn f16_delta_scan_releases_the_lock() {
        let xsd = "http://www.w3.org/2001/XMLSchema#";
        let s = ill_typed_store(0);
        let mut u = String::from("INSERT DATA {");
        for i in 0..5_000 {
            u.push_str(&format!(
                " <http://ex.org/d{i}> <http://ex.org/v> \"bad{i}\"^^<{xsd}int> ."
            ));
        }
        u.push('}');
        sparkles_core::sparql::update::update(&s, &u, &QueryOptions::default()).unwrap();
        assert!(s.snapshot().dvocab_len > 2 * DELTA_CHUNK);
        let (tx, rx) = std::sync::mpsc::channel();
        let snap = s.snapshot();
        std::thread::spawn(move || {
            let _seams = Seams;
            let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let n = ran.clone();
            DELTA_CHUNK_HOOK.with(|h| {
                *h.borrow_mut() = Some(Box::new(move |g: &Generation| {
                    let i = n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let key = sparkles_core::id::iri_key(&format!("http://ex.org/w{i}"));
                    g.dvocab.insert(&key).unwrap();
                }))
            });
            let r = diagnose(
                snap,
                &DiagnoseOptions {
                    checks: vec![ID.into()],
                    limit: 10,
                    ..Default::default()
                },
            )
            .unwrap();
            let c = r.checks.iter().find(|c| c.id == ID).unwrap();
            let ran = ran.load(std::sync::atomic::Ordering::Relaxed) > 0;
            let _ = tx.send((c.status, c.findings, ran));
        });
        let Ok(got) = rx.recv_timeout(Duration::from_secs(60)) else {
            // the reading thread is stuck holding the lock, which dropping the store
            // would wait for
            std::mem::forget(s);
            panic!("the hook could not add a term while the delta vocabulary was read");
        };
        // the hook ran, on the reading thread
        assert_eq!(got, (CheckStatus::Truncated, 10, true));
    }

    /// The key of an ill-typed `xsd:int` literal.
    fn ill_typed_literal_key(lex: &str) -> Vec<u8> {
        sparkles_core::id::term_key(&Term::Literal(Literal::new_typed_literal(lex, xsd::INT)))
    }

    /// P3-3 of the follow-up's re-review: the delta vocabulary is read up to the
    /// snapshot's end, so ill-typed literals interned after it, by a later commit or by a
    /// transaction in flight, are not counted for it.
    #[test]
    fn f16_delta_scan_stops_at_the_snapshot() {
        let s = ill_typed_store(0);
        let insert = |from: usize, n: usize| {
            let mut u = String::from("INSERT DATA {");
            for i in from..from + n {
                u.push_str(&format!(
                    " <http://ex.org/d{i}> <http://ex.org/v> \"bad{i}\"^^<{}> .",
                    xsd::INT.as_str()
                ));
            }
            u.push('}');
            sparkles_core::sparql::update::update(&s, &u, &QueryOptions::default()).unwrap();
        };
        insert(0, 3);
        let snap = s.snapshot();
        assert_eq!(ill_typed(&snap, 100, None).unwrap().delta.len(), 3);
        insert(3, 4);
        snap.generation
            .dvocab
            .insert(&ill_typed_literal_key("in flight"))
            .unwrap();
        assert_eq!(ill_typed(&snap, 100, None).unwrap().delta.len(), 3);
        // the snapshot of the later commit holds its literals, and the store hands it
        // out until the next commit
        assert_eq!(ill_typed(&s.snapshot(), 100, None).unwrap().delta.len(), 7);
    }

    /// P3-3 of the follow-up's re-review: a call whose deadline passes while another holds
    /// the generation's list times out, and a list that stopped early serves a smaller
    /// cap and is read again for a larger one.
    #[test]
    fn f16_ill_typed_list_waits_and_grows() {
        let s = ill_typed_store(5);
        let snap = s.snapshot();
        let generation = snap.generation.clone();
        {
            let _held = generation.ill_typed_literals.lock();
            let soon = Instant::now() + Duration::from_millis(50);
            assert!(matches!(
                ill_typed(&snap, 10, Some(soon)),
                Err(Error::Timeout)
            ));
        }
        assert_eq!(scans_of(&generation), 0);
        // the list stops after 3, one past the cap
        assert_eq!(ill_typed(&snap, 2, None).unwrap().len(), 3);
        assert_eq!(remembered(&generation), 3);
        assert_eq!(scans_of(&generation), 1);
        // a smaller cap than the list is served from it
        assert_eq!(ill_typed(&snap, 1, None).unwrap().len(), 3);
        assert_eq!(scans_of(&generation), 1);
        // a larger one reads the vocabulary again, and the whole list serves any cap
        assert_eq!(ill_typed(&snap, 10, None).unwrap().len(), 5);
        assert_eq!(scans_of(&generation), 2);
        assert_eq!(ill_typed(&snap, 2, None).unwrap().len(), 5);
        assert_eq!(scans_of(&generation), 2);
    }

    /// An ill-typed literal that only the delta vocabulary holds is found in the named
    /// graph that holds it, and only there.
    #[test]
    fn f16_delta_literal_in_a_named_graph() {
        let xsd = "http://www.w3.org/2001/XMLSchema#";
        let s = ill_typed_store(0);
        let u = format!(
            "INSERT DATA {{ GRAPH <http://ex.org/g> {{ <http://ex.org/e> <http://ex.org/v> \"no\"^^<{xsd}int> }} }}"
        );
        sparkles_core::sparql::update::update(&s, &u, &QueryOptions::default()).unwrap();
        assert_eq!(foci(&s, &["http://ex.org/g"]), ["<http://ex.org/e>"]);
        assert!(foci(&s, &[]).is_empty());
        assert!(foci(&s, &["default"]).is_empty());
    }

    /// F16 §8: the literals that are ill-typed for their own datatype come from a scan of
    /// the vocabulary, which is read once per generation, and the delta vocabulary is
    /// read each time. Only literals that the checked graphs hold are reported.
    #[test]
    fn f16_ill_typed_literals_come_from_the_vocabulary() {
        let xsd = "http://www.w3.org/2001/XMLSchema#";
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            format!(
                "<http://ex.org/a> <http://ex.org/v> \"x\"^^<{xsd}int> .
                 <http://ex.org/k> <http://ex.org/v> \"7\"^^<{xsd}int> .
                 <http://ex.org/b> <http://ex.org/v> \"y\"^^<{xsd}short> <http://ex.org/g> .\n"
            )
            .into_bytes(),
            RdfFormat::NQuads,
            None,
        )])
        .unwrap();
        let generation = s.snapshot().generation.clone();
        assert_eq!(foci(&s, &[]), ["<http://ex.org/a>"]);
        assert_eq!(foci(&s, &[]), ["<http://ex.org/a>"]);
        assert_eq!(foci(&s, &["http://ex.org/g"]), ["<http://ex.org/b>"]);
        assert_eq!(scans_of(&generation), 1);
        // a literal that no graph holds any more is not reported, and one the delta
        // vocabulary holds is
        let u = format!(
            "DELETE DATA {{ <http://ex.org/a> <http://ex.org/v> \"x\"^^<{xsd}int> }} ;
             INSERT DATA {{ <http://ex.org/c> <http://ex.org/v> \"300\"^^<{xsd}unsignedByte> }}"
        );
        sparkles_core::sparql::update::update(&s, &u, &QueryOptions::default()).unwrap();
        assert!(Arc::ptr_eq(&s.snapshot().generation, &generation));
        assert_eq!(foci(&s, &[]), ["<http://ex.org/c>"]);
        assert_eq!(scans_of(&generation), 1);
    }
}
