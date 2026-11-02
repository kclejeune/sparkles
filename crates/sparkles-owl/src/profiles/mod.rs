//! The OWL 2 profile checks: the EL, QL and RL grammars of the OWL 2 Profiles
//! recommendation and the OWL 2 DL restrictions of the structural specification
//! (typing constraints §5.8.1, reserved vocabulary §5.1–§5.6 and the global
//! restrictions of §11).
//!
//! Every profile is a subset of OWL 2 DL, so the report of EL, QL or RL also lists the
//! DL violations. A violation names the axiom, when there is one, and the section of
//! the rule it breaks.

mod dl;
mod el;
mod ql;
mod rl;

use crate::model::*;
use crate::read::Ontology;
use std::fmt;

/// An OWL 2 profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Profile {
    El,
    Ql,
    Rl,
    Dl,
}

impl Profile {
    pub const ALL: [Profile; 4] = [Profile::El, Profile::Ql, Profile::Rl, Profile::Dl];

    /// `el`, `ql`, `rl` or `dl`.
    pub fn parse(s: &str) -> Option<Profile> {
        match s.to_ascii_lowercase().as_str() {
            "el" => Some(Profile::El),
            "ql" => Some(Profile::Ql),
            "rl" => Some(Profile::Rl),
            "dl" => Some(Profile::Dl),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Profile::El => "el",
            Profile::Ql => "ql",
            Profile::Rl => "rl",
            Profile::Dl => "dl",
        }
    }

    /// `OWL 2 EL`, …
    pub fn title(self) -> &'static str {
        match self {
            Profile::El => "OWL 2 EL",
            Profile::Ql => "OWL 2 QL",
            Profile::Rl => "OWL 2 RL",
            Profile::Dl => "OWL 2 DL",
        }
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.title())
    }
}

/// One axiom outside a profile, or a restriction of the whole ontology it breaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub axiom: Option<Axiom>,
    pub graph: Option<u32>,
    /// the document and section of the rule, such as `OWL 2 Profiles §2.2.3`
    pub rule: String,
    pub message: String,
}

/// The answer of a check for one profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub profile: Profile,
    pub conforms: bool,
    /// the axioms checked
    pub axioms: usize,
    pub checked: usize,
    pub violations: Vec<Violation>,
    /// the violations found, listed or not
    pub total: usize,
    pub truncated: bool,
}

/// The options of a check.
#[derive(Clone, Copy, Debug)]
pub struct CheckOptions {
    /// the most violations a report lists
    pub max_violations: usize,
}

impl Default for CheckOptions {
    fn default() -> CheckOptions {
        CheckOptions {
            max_violations: 1000,
        }
    }
}

/// A rule broken by part of an axiom: its section and message.
pub(crate) struct Broken {
    pub rule: &'static str,
    pub message: String,
}

pub(crate) fn broken(rule: &'static str, message: impl Into<String>) -> Broken {
    Broken {
        rule,
        message: message.into(),
    }
}

/// The rules of OWL 2 EL that one axiom breaks, without the global restriction on the
/// ranges of property chains.
pub(crate) fn el_axiom(ax: &AxiomKind) -> Vec<Broken> {
    el::axiom_kind(ax)
}

/// Checks axioms one at a time against several profiles. The ontology given to
/// [`new`](Checker::new) supplies the typing, the property hierarchy and the datatype
/// definitions that the global restrictions need; the axioms passed to
/// [`axiom`](Checker::axiom) can come from it or be streamed from a store.
pub struct Checker {
    profiles: Vec<Profile>,
    opts: CheckOptions,
    dl: dl::Dl,
    el: el::El,
    reports: Vec<Report>,
    /// violations of the whole ontology for DL, added to every report at the end
    dl_global: Vec<Violation>,
    checked: usize,
}

impl Checker {
    pub fn new(ontology: &Ontology, profiles: &[Profile], opts: CheckOptions) -> Checker {
        let mut profiles: Vec<Profile> = profiles.to_vec();
        profiles.sort();
        profiles.dedup();
        let dl = dl::Dl::new(ontology);
        let el = el::El::new(ontology.axiom_set());
        let reports = profiles
            .iter()
            .map(|p| Report {
                profile: *p,
                conforms: true,
                axioms: 0,
                checked: 0,
                violations: Vec::new(),
                total: 0,
                truncated: false,
            })
            .collect();
        Checker {
            profiles,
            opts,
            dl,
            el,
            reports,
            dl_global: Vec::new(),
            checked: 0,
        }
    }

    fn push(&mut self, n: usize, v: Violation) {
        let max = self.opts.max_violations;
        let r = &mut self.reports[n];
        r.conforms = false;
        r.total += 1;
        if r.violations.len() < max {
            r.violations.push(v);
        } else {
            r.truncated = true;
        }
    }

    /// Check one axiom; `graph` is where it was read from.
    pub fn axiom(&mut self, ax: &Axiom, graph: Option<u32>) {
        self.checked += 1;
        let dl: Vec<Broken> = self.dl.axiom(ax);
        for n in 0..self.profiles.len() {
            let own = match self.profiles[n] {
                Profile::El => {
                    let mut b = el::axiom(ax);
                    b.extend(self.el.chain_ranges(ax));
                    b
                }
                Profile::Ql => ql::axiom(ax),
                Profile::Rl => rl::axiom(ax),
                Profile::Dl => Vec::new(),
            };
            let title = self.profiles[n].title();
            for b in own.into_iter().chain(dl.iter().map(|b| Broken {
                rule: b.rule,
                message: b.message.clone(),
            })) {
                let message = if b.rule.starts_with("OWL 2 Profiles") {
                    format!("{} in {title}", b.message)
                } else {
                    b.message
                };
                self.push(
                    n,
                    Violation {
                        axiom: Some(ax.clone()),
                        graph,
                        rule: b.rule.to_string(),
                        message,
                    },
                );
            }
        }
    }

    /// Note a triple that maps to no axiom, such as an assertion triple streamed from a
    /// store: it keeps the ontology out of OWL 2 DL, and so out of every profile.
    pub fn unmapped(&mut self, t: &oxrdf::Triple, graph: Option<u32>) {
        let v = Violation {
            axiom: None,
            graph,
            rule: "OWL 2 Mapping to RDF Graphs §3".into(),
            message: format!("the triple {t} maps to no axiom"),
        };
        for n in 0..self.profiles.len() {
            self.push(n, v.clone());
        }
    }

    /// Finish the check: the restrictions of the whole ontology, and the reports.
    pub fn finish(mut self) -> Vec<Report> {
        let global = self.dl.finish();
        self.dl_global.extend(global);
        let global = std::mem::take(&mut self.dl_global);
        for n in 0..self.profiles.len() {
            for v in &global {
                self.push(n, v.clone());
            }
            self.reports[n].axioms = self.checked;
            self.reports[n].checked = self.checked;
        }
        self.reports
    }
}

/// The axioms of an ontology split by one profile's own grammar (§2 to §4 of OWL 2
/// Profiles), or for OWL 2 DL by the restrictions that apply to one axiom at a time:
/// those inside it and the violations of those outside, one per broken rule. The DL
/// restrictions over the whole ontology, such as the typing of entities, are left to a
/// check.
#[derive(Clone, Debug, Default)]
pub struct Partition {
    /// the axioms of the terminology the profile's grammar allows, in order
    pub inside: Vec<Axiom>,
    /// the violations of the other axioms, each naming its axiom
    pub outside: Vec<Violation>,
}

impl Partition {
    /// The axioms outside the profile, each once.
    pub fn outside_axioms(&self) -> std::collections::BTreeSet<&Axiom> {
        self.outside
            .iter()
            .filter_map(|v| v.axiom.as_ref())
            .collect()
    }
}

/// Split the axioms of `ontology` by the grammar of `profile`. Unlike [`check`], an
/// EL, QL or RL partition applies the profile's own rules only, without the OWL 2 DL
/// global restrictions or the unmapped triples, since it serves a consumer that uses
/// the axioms a profile allows and sets the others aside: query rewriting under OWL 2
/// QL (spec F17 §7) reads `inside`, and a materialization under OWL 2 RL warns about
/// `outside` (spec F21 §7).
pub fn partition(ontology: &Ontology, profile: Profile) -> Partition {
    let el = (profile == Profile::El).then(|| el::El::new(ontology.axiom_set()));
    let mut dl = (profile == Profile::Dl).then(|| dl::Dl::new(ontology));
    let title = profile.title();
    let mut out = Partition::default();
    for (ax, origins) in &ontology.axioms {
        let broken = match (profile, &el, &mut dl) {
            (Profile::El, Some(el), _) => {
                let mut b = el::axiom(ax);
                b.extend(el.chain_ranges(ax));
                b
            }
            (Profile::Ql, _, _) => ql::axiom(ax),
            (Profile::Rl, _, _) => rl::axiom(ax),
            (Profile::Dl, _, Some(dl)) => dl.axiom(ax),
            _ => Vec::new(),
        };
        if broken.is_empty() {
            out.inside.push(ax.clone());
            continue;
        }
        let graph = origins.first().map(|o| o.graph);
        for b in broken {
            let message = if b.rule.starts_with("OWL 2 Profiles") {
                format!("{} in {title}", b.message)
            } else {
                b.message
            };
            out.outside.push(Violation {
                axiom: Some(ax.clone()),
                graph,
                rule: b.rule.to_string(),
                message,
            });
        }
    }
    out
}

/// Check every axiom of an ontology against `profiles`.
pub fn check(ontology: &Ontology, profiles: &[Profile], opts: CheckOptions) -> Vec<Report> {
    let mut c = Checker::new(ontology, profiles, opts);
    for (ax, origins) in &ontology.axioms {
        c.axiom(ax, origins.first().map(|o| o.graph));
    }
    c.finish()
}

/// The datatypes that OWL 2 EL and OWL 2 QL forbid (Profiles §2.2.1 and §3.2.1).
pub(crate) const EL_QL_FORBIDDEN_DATATYPES: [&str; 15] = [
    "http://www.w3.org/2001/XMLSchema#double",
    "http://www.w3.org/2001/XMLSchema#float",
    "http://www.w3.org/2001/XMLSchema#nonPositiveInteger",
    "http://www.w3.org/2001/XMLSchema#positiveInteger",
    "http://www.w3.org/2001/XMLSchema#negativeInteger",
    "http://www.w3.org/2001/XMLSchema#long",
    "http://www.w3.org/2001/XMLSchema#int",
    "http://www.w3.org/2001/XMLSchema#short",
    "http://www.w3.org/2001/XMLSchema#byte",
    "http://www.w3.org/2001/XMLSchema#unsignedLong",
    "http://www.w3.org/2001/XMLSchema#unsignedInt",
    "http://www.w3.org/2001/XMLSchema#unsignedShort",
    "http://www.w3.org/2001/XMLSchema#unsignedByte",
    "http://www.w3.org/2001/XMLSchema#language",
    "http://www.w3.org/2001/XMLSchema#boolean",
];

/// The individuals of an axiom outside its annotations.
pub(crate) fn individuals(ax: &AxiomKind) -> Vec<&Individual> {
    fn ce<'a>(c: &'a ClassExpression, out: &mut Vec<&'a Individual>) {
        use ClassExpression as C;
        match c {
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
                cs.iter().for_each(|c| ce(c, out))
            }
            C::ObjectComplementOf(c)
            | C::ObjectSomeValuesFrom(_, c)
            | C::ObjectAllValuesFrom(_, c) => ce(c, out),
            C::ObjectMinCardinality(_, _, Some(c))
            | C::ObjectMaxCardinality(_, _, Some(c))
            | C::ObjectExactCardinality(_, _, Some(c)) => ce(c, out),
            C::ObjectOneOf(is) => out.extend(is.iter()),
            C::ObjectHasValue(_, i) => out.push(i),
            _ => {}
        }
    }
    use AxiomKind as K;
    let mut out = Vec::new();
    match ax {
        K::SubClassOf(a, b) => {
            ce(a, &mut out);
            ce(b, &mut out);
        }
        K::EquivalentClasses(cs) | K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => {
            cs.iter().for_each(|c| ce(c, &mut out))
        }
        K::ObjectPropertyDomain(_, c)
        | K::ObjectPropertyRange(_, c)
        | K::DataPropertyDomain(_, c)
        | K::HasKey(c, _, _) => ce(c, &mut out),
        K::ClassAssertion(c, i) => {
            ce(c, &mut out);
            out.push(i);
        }
        K::SameIndividual(is) | K::DifferentIndividuals(is) => out.extend(is.iter()),
        K::ObjectPropertyAssertion(_, a, b) | K::NegativeObjectPropertyAssertion(_, a, b) => {
            out.push(a);
            out.push(b);
        }
        K::DataPropertyAssertion(_, a, _) | K::NegativeDataPropertyAssertion(_, a, _) => {
            out.push(a)
        }
        _ => {}
    }
    out
}

/// The datatypes of an axiom outside its annotations, from its data ranges and
/// literals.
pub(crate) fn datatypes(ax: &AxiomKind) -> Vec<String> {
    let mut s = crate::model::Signature::default();
    s.axiom(ax);
    let mut out: Vec<String> =
        s.0.into_iter()
            .filter_map(|e| match e {
                Entity::Datatype(d) => Some(d.as_str().to_string()),
                _ => None,
            })
            .collect();
    // declarations of datatypes name them but do not use them
    if matches!(ax, AxiomKind::Declaration(_)) {
        out.clear();
    }
    out
}
