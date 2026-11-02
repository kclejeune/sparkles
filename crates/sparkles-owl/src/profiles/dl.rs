//! OWL 2 DL: the typing constraints (structural specification §5.8.1), the
//! restrictions on the reserved vocabulary (§5.1–§5.6) and the global restrictions on
//! the axiom closure (§11), with the triples a strict read cannot map.

use super::{Broken, Violation, broken, datatypes, individuals};
use crate::datatypes::Literals;
use crate::model::*;
use crate::read::{Ontology, Typing};
use crate::vocab;
use std::collections::{BTreeMap, BTreeSet};

const TYPING: &str = "OWL 2 Structural Specification §5.8.1";
const RESERVED: &str = "OWL 2 Structural Specification §5";
const GLOBAL: &str = "OWL 2 Structural Specification §11.2";
const HEADER: &str = "OWL 2 Mapping to RDF Graphs §3.1.2";
const MAPPING: &str = "OWL 2 Mapping to RDF Graphs §3";
const LITERALS: &str = "OWL 2 Structural Specification §5.7";
const RESTRICTIONS: &str = "OWL 2 Structural Specification §7.5";
const DEFINITIONS: &str = "OWL 2 Structural Specification §9.4";

/// The most unmapped triples listed one by one; the rest are counted in one violation.
const MAX_UNMAPPED: usize = 100;

type Ope = ObjectPropertyExpression;

pub(super) struct Dl {
    typing: Typing,
    /// the relation → of §11.1
    arrow: BTreeMap<Ope, BTreeSet<Ope>>,
    non_simple: BTreeSet<Ope>,
    /// chain axioms that no strict order of §11.2 can satisfy
    irregular: BTreeSet<Axiom>,
    /// how many datatype definitions each datatype has, and those defined in a cycle
    definitions: BTreeMap<String, usize>,
    cyclic: BTreeSet<String>,
    /// object property assertions between anonymous individuals, and the number of
    /// assertions linking each anonymous individual to a named one
    anon_edges: Vec<(String, String, Option<Axiom>)>,
    anon_named: BTreeMap<String, usize>,
    global: Vec<Violation>,
}

fn named_ope(p: &ObjectProperty) -> Ope {
    Ope::Property(p.clone())
}

impl Dl {
    pub(super) fn new(o: &Ontology) -> Dl {
        let mut dl = Dl {
            typing: o.typing.clone(),
            arrow: BTreeMap::new(),
            non_simple: BTreeSet::new(),
            irregular: BTreeSet::new(),
            definitions: BTreeMap::new(),
            cyclic: BTreeSet::new(),
            anon_edges: Vec::new(),
            anon_named: BTreeMap::new(),
            global: Vec::new(),
        };
        let mut composite: BTreeSet<Ope> = [
            named_ope(&ObjectProperty::top()),
            named_ope(&ObjectProperty::bottom()),
        ]
        .into_iter()
        .collect();
        let mut chains: Vec<(&Axiom, &Vec<Ope>, &Ope)> = Vec::new();
        let mut defs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for ax in o.axiom_set() {
            match &ax.kind {
                AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(a), b) => {
                    dl.arrow_add(a, b)
                }
                AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(ps), b) => {
                    if ps.len() > 1 {
                        composite.insert(b.clone());
                        composite.insert(b.inverse());
                    }
                    chains.push((ax, ps, b));
                }
                AxiomKind::EquivalentObjectProperties(ps) => {
                    for a in ps {
                        for b in ps {
                            if a != b {
                                dl.arrow_add(a, b);
                            }
                        }
                    }
                }
                AxiomKind::InverseObjectProperties(a, b) => {
                    dl.arrow_add(a, &b.inverse());
                    dl.arrow_add(&b.inverse(), a);
                }
                AxiomKind::SymmetricObjectProperty(a) => dl.arrow_add(a, &a.inverse()),
                AxiomKind::TransitiveObjectProperty(a) => {
                    composite.insert(a.clone());
                    composite.insert(a.inverse());
                }
                AxiomKind::DatatypeDefinition(d, r) => {
                    *dl.definitions.entry(d.as_str().to_string()).or_default() += 1;
                    // the datatypes of the definition's range, `d` itself included
                    let mut sig = crate::model::Signature::default();
                    sig.dr(r);
                    let used: BTreeSet<String> = sig
                        .0
                        .into_iter()
                        .filter_map(|e| match e {
                            crate::model::Entity::Datatype(x) => Some(x.as_str().to_string()),
                            _ => None,
                        })
                        .collect();
                    defs.entry(d.as_str().to_string()).or_default().extend(used);
                }
                _ => {}
            }
        }
        // §11.1: an expression is simple when no composite expression reaches it
        let mut stack: Vec<Ope> = composite.into_iter().collect();
        while let Some(x) = stack.pop() {
            if dl.non_simple.insert(x.clone())
                && let Some(next) = dl.arrow.get(&x)
            {
                stack.extend(next.iter().cloned());
            }
        }
        dl.regularity(&chains);
        // datatype definitions must be acyclic
        for d in defs.keys() {
            let mut seen = BTreeSet::new();
            let mut stack: Vec<String> = defs[d].iter().cloned().collect();
            while let Some(x) = stack.pop() {
                if &x == d {
                    dl.cyclic.insert(d.clone());
                    break;
                }
                if seen.insert(x.clone())
                    && let Some(n) = defs.get(&x)
                {
                    stack.extend(n.iter().cloned());
                }
            }
        }
        // typing constraints between declarations
        let mut declared: Vec<(&String, crate::read::Kinds)> =
            o.typing.declared.iter().map(|(k, v)| (k, *v)).collect();
        declared.sort_by(|a, b| a.0.cmp(b.0));
        for (iri, kinds) in declared {
            let props: Vec<&str> = [
                EntityKind::ObjectProperty,
                EntityKind::DataProperty,
                EntityKind::AnnotationProperty,
            ]
            .into_iter()
            .filter(|k| kinds.has(*k) || crate::read::builtin_kind(iri) == Some(*k))
            .map(EntityKind::name)
            .collect();
            if props.len() > 1 {
                dl.global.push(Violation {
                    axiom: None,
                    graph: None,
                    rule: TYPING.into(),
                    message: format!("<{iri}> is declared as {}", props.join(" and ")),
                });
            }
            let class = kinds.has(EntityKind::Class)
                || crate::read::builtin_kind(iri) == Some(EntityKind::Class);
            let datatype = kinds.has(EntityKind::Datatype)
                || crate::read::builtin_kind(iri) == Some(EntityKind::Datatype);
            if class && datatype {
                dl.global.push(Violation {
                    axiom: None,
                    graph: None,
                    rule: TYPING.into(),
                    message: format!("<{iri}> is declared as Class and Datatype"),
                });
            }
        }
        // what a strict read refuses
        for (n, i) in o.unmapped.iter().enumerate() {
            if n == MAX_UNMAPPED {
                dl.global.push(Violation {
                    axiom: None,
                    graph: None,
                    rule: MAPPING.into(),
                    message: format!(
                        "{} more triples map to no axiom",
                        o.unmapped.len() - MAX_UNMAPPED
                    ),
                });
                break;
            }
            dl.global.push(Violation {
                axiom: None,
                graph: o.graph_of.get(*i).copied(),
                rule: MAPPING.into(),
                message: format!("the triple {} maps to no axiom", o.triples[*i]),
            });
        }
        // §3.1.2: exactly one ontology header must match
        if o.header_graph.is_none() {
            dl.global.push(Violation {
                axiom: None,
                graph: None,
                rule: HEADER.into(),
                message: "the graph has no ontology header (no node of rdf:type owl:Ontology)"
                    .into(),
            });
        }
        for d in &o.diagnostics {
            if d.code == "header" {
                dl.global.push(Violation {
                    axiom: None,
                    graph: None,
                    rule: HEADER.into(),
                    message: d.message.clone(),
                });
            }
            if d.code == "untyped" {
                dl.global.push(Violation {
                    axiom: None,
                    graph: d.triples.first().and_then(|i| o.graph_of.get(*i).copied()),
                    rule: MAPPING.into(),
                    message: d.message.clone(),
                });
            }
        }
        // the literals of the ontology's own annotations
        let mut header = Vec::new();
        dl.literals(
            &Literals::of_annotations(&o.header.annotations),
            &mut header,
        );
        dl.global.extend(header.into_iter().map(|b| Violation {
            axiom: None,
            graph: o.header_graph,
            rule: b.rule.into(),
            message: b.message,
        }));
        dl
    }

    fn arrow_add(&mut self, a: &Ope, b: &Ope) {
        self.arrow.entry(a.clone()).or_default().insert(b.clone());
        self.arrow
            .entry(a.inverse())
            .or_default()
            .insert(b.inverse());
    }

    /// The expressions `x` reaches through →*, `x` included.
    fn reach(&self, x: &Ope) -> BTreeSet<Ope> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![x.clone()];
        while let Some(y) = stack.pop() {
            if seen.insert(y.clone())
                && let Some(n) = self.arrow.get(&y)
            {
                stack.extend(n.iter().cloned());
            }
        }
        seen
    }

    /// The restriction on the property hierarchy of §11.2: find the chain axioms that
    /// no strict order can satisfy.
    fn regularity(&mut self, chains: &[(&Axiom, &Vec<Ope>, &Ope)]) {
        let top = named_ope(&ObjectProperty::top());
        // required x < y, keyed by the name of x (x < y iff INV(x) < y)
        let mut req: BTreeMap<ObjectProperty, BTreeSet<Ope>> = BTreeMap::new();
        let mut needs: Vec<(&Axiom, Vec<(Ope, Ope)>)> = Vec::new();
        for (ax, ps, p) in chains {
            if **p == top || ps.len() < 2 {
                continue;
            }
            let n = ps.len();
            let pairs: Vec<(Ope, Ope)> = if n == 2 && ps[0] == **p && ps[1] == **p {
                Vec::new()
            } else if ps[0] == **p {
                ps[1..].iter().map(|x| (x.clone(), (*p).clone())).collect()
            } else if ps[n - 1] == **p {
                ps[..n - 1]
                    .iter()
                    .map(|x| (x.clone(), (*p).clone()))
                    .collect()
            } else {
                ps.iter().map(|x| (x.clone(), (*p).clone())).collect()
            };
            for (x, y) in &pairs {
                req.entry(x.named().clone()).or_default().insert(y.clone());
            }
            needs.push((ax, pairs));
        }
        let up = |name: &ObjectProperty| -> BTreeSet<Ope> {
            let mut seen = BTreeSet::new();
            let mut stack: Vec<Ope> = req.get(name).into_iter().flatten().cloned().collect();
            while let Some(y) = stack.pop() {
                if seen.insert(y.clone())
                    && let Some(n) = req.get(y.named())
                {
                    stack.extend(n.iter().cloned());
                }
            }
            seen
        };
        for (ax, pairs) in needs {
            let bad = pairs.iter().any(|(x, _)| {
                let a = x.named();
                let ups = up(a);
                ups.iter().any(|z| {
                    z.named() == a || {
                        let r = self.reach(z);
                        r.contains(&named_ope(a)) || r.contains(&Ope::InverseOf(a.clone()))
                    }
                })
            });
            if bad {
                self.irregular.insert(ax.clone());
            }
        }
    }

    fn simple(&self, p: &Ope, what: &str, out: &mut Vec<Broken>) {
        if self.non_simple.contains(p) {
            out.push(broken(
                GLOBAL,
                format!(
                    "{} is not a simple property and cannot be used in {what}",
                    crate::ofn::Renderer::default().ope(p)
                ),
            ));
        }
    }

    fn simple_ce(&self, c: &ClassExpression, out: &mut Vec<Broken>) {
        use ClassExpression as C;
        match c {
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
                cs.iter().for_each(|c| self.simple_ce(c, out))
            }
            C::ObjectComplementOf(c)
            | C::ObjectSomeValuesFrom(_, c)
            | C::ObjectAllValuesFrom(_, c) => self.simple_ce(c, out),
            C::ObjectMinCardinality(_, p, f)
            | C::ObjectMaxCardinality(_, p, f)
            | C::ObjectExactCardinality(_, p, f) => {
                self.simple(p, c.type_name(), out);
                if let Some(f) = f {
                    self.simple_ce(f, out);
                }
            }
            C::ObjectHasSelf(p) => self.simple(p, "ObjectHasSelf", out),
            _ => {}
        }
    }

    fn class_expressions(ax: &AxiomKind) -> Vec<&ClassExpression> {
        use AxiomKind as K;
        match ax {
            K::SubClassOf(a, b) => vec![a, b],
            K::EquivalentClasses(cs) | K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => {
                cs.iter().collect()
            }
            K::ObjectPropertyDomain(_, c)
            | K::ObjectPropertyRange(_, c)
            | K::DataPropertyDomain(_, c)
            | K::HasKey(c, _, _)
            | K::ClassAssertion(c, _) => vec![c],
            _ => Vec::new(),
        }
    }

    fn anon_in_value_positions(c: &ClassExpression) -> bool {
        use ClassExpression as C;
        match c {
            C::ObjectOneOf(is) => is.iter().any(|i| matches!(i, Individual::Anonymous(_))),
            C::ObjectHasValue(_, Individual::Anonymous(_)) => true,
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
                cs.iter().any(Self::anon_in_value_positions)
            }
            C::ObjectComplementOf(c)
            | C::ObjectSomeValuesFrom(_, c)
            | C::ObjectAllValuesFrom(_, c) => Self::anon_in_value_positions(c),
            C::ObjectMinCardinality(_, _, Some(c))
            | C::ObjectMaxCardinality(_, _, Some(c))
            | C::ObjectExactCardinality(_, _, Some(c)) => Self::anon_in_value_positions(c),
            _ => false,
        }
    }

    /// The lexical forms of literals (§5.7), the facet pairs of datatype restrictions
    /// (§7.5) and the use of defined datatypes in either (§9.4). A literal whose
    /// datatype is outside the OWL 2 datatype map and is not defined has no known lexical
    /// space; the datatype check reports that datatype instead.
    fn literals(&self, lits: &Literals<'_>, out: &mut Vec<Broken>) {
        let r = crate::ofn::Renderer::new(Vec::<(String, String)>::new());
        for (l, _annotation) in &lits.literals {
            let dt = l.datatype();
            if self.definitions.contains_key(dt) && !vocab::is_builtin_datatype(dt) {
                out.push(broken(
                    DEFINITIONS,
                    format!(
                        "the literal {} has the datatype <{dt}>, which a datatype definition defines and which therefore has no lexical forms",
                        r.literal(l)
                    ),
                ));
            } else if crate::datatypes::lexical(dt, l.value()) == Some(false) {
                out.push(broken(
                    LITERALS,
                    format!(
                        "the lexical form of {} is not in the lexical space of <{dt}>",
                        r.literal(l)
                    ),
                ));
            }
        }
        for (d, f) in &lits.restrictions {
            let d = d.as_str();
            if self.definitions.contains_key(d) && !vocab::is_builtin_datatype(d) {
                out.push(broken(
                    DEFINITIONS,
                    format!("the datatype <{d}> is defined by a datatype definition, so it supports no facets and cannot be restricted"),
                ));
            } else if let Some(m) = crate::datatypes::facet_problem(d, f.facet.as_str(), &f.value) {
                out.push(broken(RESTRICTIONS, m));
            }
        }
    }

    pub(super) fn axiom(&mut self, ax: &Axiom) -> Vec<Broken> {
        use AxiomKind as K;
        let mut out = Vec::new();
        // typing constraints and the reserved vocabulary
        for e in ax.signature() {
            let i = e.iri().as_str();
            let (declared, reserved_ok) = match &e {
                Entity::Class(_) => (
                    self.typing.is_declared(i, EntityKind::Class),
                    i == vocab::OWL_THING || i == vocab::OWL_NOTHING,
                ),
                Entity::Datatype(_) => (
                    self.typing.is_declared(i, EntityKind::Datatype),
                    vocab::is_builtin_datatype(i),
                ),
                Entity::ObjectProperty(_) => (
                    self.typing.is_declared(i, EntityKind::ObjectProperty),
                    i == vocab::OWL_TOP_OBJECT_PROPERTY || i == vocab::OWL_BOTTOM_OBJECT_PROPERTY,
                ),
                Entity::DataProperty(_) => (
                    self.typing.is_declared(i, EntityKind::DataProperty),
                    i == vocab::OWL_TOP_DATA_PROPERTY || i == vocab::OWL_BOTTOM_DATA_PROPERTY,
                ),
                Entity::AnnotationProperty(_) => (
                    self.typing.is_declared(i, EntityKind::AnnotationProperty),
                    vocab::is_builtin_annotation_property(i),
                ),
                Entity::NamedIndividual(_) => (true, false),
            };
            if !declared {
                out.push(broken(
                    TYPING,
                    format!("<{i}> is used as {} without a declaration", e.kind().name()),
                ));
            }
            if vocab::is_reserved(i) && !reserved_ok {
                out.push(broken(
                    RESERVED,
                    format!(
                        "<{i}> is in the reserved vocabulary and cannot identify a {}",
                        e.kind().name()
                    ),
                ));
            }
        }
        // owl:topDataProperty only as the superproperty of SubDataPropertyOf
        let top_data = DataProperty::top();
        let sig = ax.signature();
        if sig.contains(&Entity::DataProperty(top_data.clone()))
            && !matches!(&ax.kind, K::SubDataPropertyOf(a, b) if *b == top_data && *a != top_data)
            && !matches!(&ax.kind, K::Declaration(_))
        {
            out.push(broken(
                GLOBAL,
                "owl:topDataProperty may occur only as the superproperty of SubDataPropertyOf",
            ));
        }
        // datatypes: built in, or defined by exactly one definition, without cycles
        for d in datatypes(&ax.kind) {
            let n = self.definitions.get(&d).copied().unwrap_or(0);
            if vocab::is_builtin_datatype(&d) {
                if n > 0 && matches!(&ax.kind, K::DatatypeDefinition(x, _) if x.as_str() == d) {
                    out.push(broken(
                        GLOBAL,
                        format!("the datatype definition redefines the built-in <{d}>"),
                    ));
                }
            } else if n != 1 {
                out.push(broken(
                    GLOBAL,
                    if n == 0 {
                        format!("the datatype <{d}> is neither built in nor defined by a datatype definition")
                    } else {
                        format!("the datatype <{d}> has {n} datatype definitions")
                    },
                ));
            }
        }
        if let K::DatatypeDefinition(d, _) = &ax.kind
            && self.cyclic.contains(d.as_str())
        {
            out.push(broken(
                GLOBAL,
                format!("the definition of <{}> is cyclic", d.as_str()),
            ));
        }
        // literals and facets
        self.literals(&Literals::of(ax), &mut out);
        // simple properties
        for c in Self::class_expressions(&ax.kind) {
            self.simple_ce(c, &mut out);
        }
        match &ax.kind {
            K::FunctionalObjectProperty(p) => self.simple(p, "FunctionalObjectProperty", &mut out),
            K::InverseFunctionalObjectProperty(p) => {
                self.simple(p, "InverseFunctionalObjectProperty", &mut out)
            }
            K::IrreflexiveObjectProperty(p) => {
                self.simple(p, "IrreflexiveObjectProperty", &mut out)
            }
            K::AsymmetricObjectProperty(p) => self.simple(p, "AsymmetricObjectProperty", &mut out),
            K::DisjointObjectProperties(ps) => {
                for p in ps {
                    self.simple(p, "DisjointObjectProperties", &mut out);
                }
            }
            _ => {}
        }
        if self.irregular.contains(ax) {
            out.push(broken(
                GLOBAL,
                "the property chain makes the property hierarchy irregular: no strict order of the properties satisfies it",
            ));
        }
        // anonymous individuals
        let anonymous = individuals(&ax.kind)
            .iter()
            .any(|i| matches!(i, Individual::Anonymous(_)));
        if anonymous
            && matches!(
                &ax.kind,
                K::SameIndividual(_)
                    | K::DifferentIndividuals(_)
                    | K::NegativeObjectPropertyAssertion(..)
                    | K::NegativeDataPropertyAssertion(..)
            )
        {
            out.push(broken(
                GLOBAL,
                format!(
                    "anonymous individuals cannot occur in {}",
                    ax.axiom_type().name()
                ),
            ));
        }
        if Self::class_expressions(&ax.kind)
            .into_iter()
            .any(Self::anon_in_value_positions)
        {
            out.push(broken(
                GLOBAL,
                "anonymous individuals cannot occur in ObjectOneOf or ObjectHasValue",
            ));
        }
        if let K::ObjectPropertyAssertion(_, a, b) = &ax.kind {
            match (a, b) {
                (Individual::Anonymous(x), Individual::Anonymous(y)) => {
                    let (x, y) = (x.label().to_string(), y.label().to_string());
                    self.anon_named.entry(x.clone()).or_default();
                    self.anon_named.entry(y.clone()).or_default();
                    self.anon_edges.push((x, y, Some(ax.clone())));
                }
                (Individual::Anonymous(x), Individual::Named(_))
                | (Individual::Named(_), Individual::Anonymous(x)) => {
                    *self.anon_named.entry(x.label().to_string()).or_default() += 1;
                }
                _ => {}
            }
        }
        out
    }

    /// The restrictions of the whole ontology, once every axiom was seen.
    pub(super) fn finish(&mut self) -> Vec<Violation> {
        let mut out = std::mem::take(&mut self.global);
        // the anonymous individual graph must be a forest of trees with one root each
        let nodes: Vec<String> = self.anon_named.keys().cloned().collect();
        let index: BTreeMap<&str, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();
        let mut parent: Vec<usize> = (0..nodes.len()).collect();
        fn find(p: &mut [usize], mut x: usize) -> usize {
            while p[x] != x {
                p[x] = p[p[x]];
                x = p[x];
            }
            x
        }
        for (x, y, ax) in &self.anon_edges {
            let (a, b) = (
                find(&mut parent, index[x.as_str()]),
                find(&mut parent, index[y.as_str()]),
            );
            if a == b {
                out.push(Violation {
                    axiom: ax.clone(),
                    graph: None,
                    rule: GLOBAL.into(),
                    message: format!(
                        "the assertions between anonymous individuals _:{x} and _:{y} do not form a tree"
                    ),
                });
            } else {
                parent[a] = b;
            }
        }
        let mut roots: BTreeMap<usize, bool> = BTreeMap::new();
        for (n, name) in nodes.iter().enumerate() {
            let r = find(&mut parent, n);
            let ok = self.anon_named.get(name).copied().unwrap_or(0) <= 1;
            *roots.entry(r).or_default() |= ok;
        }
        for (r, ok) in roots {
            if !ok {
                out.push(Violation {
                    axiom: None,
                    graph: None,
                    rule: GLOBAL.into(),
                    message: format!(
                        "every anonymous individual connected to _:{} is linked to more than one named individual",
                        nodes[r]
                    ),
                });
            }
        }
        out
    }
}
