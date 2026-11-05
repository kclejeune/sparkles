//! On-demand ontology metrics, streaming instance assertions without retaining them.
use super::*;
use sparkles_owl::{
    ClassExpression as C, ObjectPropertyExpression as O, SubObjectPropertyExpression as S,
};

/// Limit the additional signature memory and work of a metrics request.
const MAX_ITEMS: usize = 1_000_000;

#[derive(Default)]
struct Features {
    alc: bool,
    transitive: bool,
    hierarchy: bool,
    roles: bool,
    inverse: bool,
    nominal: bool,
    functional: bool,
    number: bool,
    qualified: bool,
    data: bool,
    keys: bool,
}

impl Features {
    fn property(&mut self, p: &O) {
        self.inverse |= matches!(p, O::InverseOf(_));
        self.roles |= p.named() == &sparkles_owl::ObjectProperty::top()
            || p.named() == &sparkles_owl::ObjectProperty::bottom();
    }

    fn expression(&mut self, c: &C) {
        match c {
            C::Class(c) => self.alc |= c == &sparkles_owl::Class::nothing(),
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
                self.alc |= matches!(c, C::ObjectUnionOf(_));
                for c in cs {
                    self.expression(c);
                }
            }
            C::ObjectComplementOf(c) => {
                self.alc = true;
                self.expression(c);
            }
            C::ObjectOneOf(_) => self.nominal = true,
            C::ObjectSomeValuesFrom(p, c) => {
                self.property(p);
                self.expression(c);
            }
            C::ObjectAllValuesFrom(p, c) => {
                self.alc = true;
                self.property(p);
                self.expression(c);
            }
            C::ObjectHasValue(p, _) => {
                self.property(p);
                self.nominal = true;
            }
            C::ObjectHasSelf(p) => {
                self.property(p);
                self.roles = true;
            }
            C::ObjectMinCardinality(_, p, c)
            | C::ObjectMaxCardinality(_, p, c)
            | C::ObjectExactCardinality(_, p, c) => {
                self.property(p);
                if let Some(c) = c {
                    self.qualified = true;
                    self.expression(c);
                } else {
                    self.number = true;
                }
            }
            C::DataSomeValuesFrom(..)
            | C::DataAllValuesFrom(..)
            | C::DataHasValue(..)
            | C::DataMinCardinality(..)
            | C::DataMaxCardinality(..)
            | C::DataExactCardinality(..) => {
                self.data = true;
                self.qualified |= matches!(
                    c,
                    C::DataMinCardinality(..)
                        | C::DataMaxCardinality(..)
                        | C::DataExactCardinality(..)
                );
            }
        }
    }

    fn axiom(&mut self, a: &Axiom) {
        use AxiomKind as K;
        match &a.kind {
            K::SubClassOf(a, b) => {
                self.expression(a);
                self.expression(b);
            }
            K::EquivalentClasses(cs) | K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => {
                self.alc |= matches!(&a.kind, K::DisjointClasses(_) | K::DisjointUnion(..));
                for c in cs {
                    self.expression(c);
                }
            }
            K::SubObjectPropertyOf(s, p) => {
                self.hierarchy = true;
                self.property(p);
                match s {
                    S::Expression(p) => self.property(p),
                    S::Chain(ps) => {
                        self.roles = true;
                        for p in ps {
                            self.property(p);
                        }
                    }
                }
            }
            K::EquivalentObjectProperties(ps) | K::DisjointObjectProperties(ps) => {
                self.hierarchy = true;
                self.roles |= matches!(&a.kind, K::DisjointObjectProperties(_));
                for p in ps {
                    self.property(p);
                }
            }
            K::InverseObjectProperties(a, b) => {
                self.property(a);
                self.property(b);
                self.inverse = true;
            }
            K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => {
                self.alc |= matches!(&a.kind, K::ObjectPropertyRange(..));
                self.property(p);
                self.expression(c);
            }
            K::FunctionalObjectProperty(p) | K::InverseFunctionalObjectProperty(p) => {
                self.property(p);
                self.functional = true;
                self.inverse |= matches!(&a.kind, K::InverseFunctionalObjectProperty(_));
            }
            K::TransitiveObjectProperty(p) => {
                self.property(p);
                self.transitive = true;
            }
            K::SymmetricObjectProperty(p) => {
                self.property(p);
                self.inverse = true;
            }
            K::ReflexiveObjectProperty(p)
            | K::IrreflexiveObjectProperty(p)
            | K::AsymmetricObjectProperty(p) => {
                self.property(p);
                self.roles = true;
            }
            K::DataPropertyDomain(_, c) => {
                self.expression(c);
                self.data = true;
            }
            K::SubDataPropertyOf(..)
            | K::EquivalentDataProperties(..)
            | K::DisjointDataProperties(..)
            | K::DataPropertyRange(..)
            | K::FunctionalDataProperty(..)
            | K::DatatypeDefinition(..)
            | K::DataPropertyAssertion(..)
            | K::NegativeDataPropertyAssertion(..) => {
                self.data = true;
                self.functional |= matches!(&a.kind, K::FunctionalDataProperty(_));
                self.roles |= matches!(
                    &a.kind,
                    K::SubDataPropertyOf(..)
                        | K::EquivalentDataProperties(_)
                        | K::DisjointDataProperties(_)
                );
            }
            K::HasKey(c, ps, ds) => {
                self.keys = true;
                self.expression(c);
                for p in ps {
                    self.property(p);
                }
                self.data |= !ds.is_empty();
            }
            K::SameIndividual(_) | K::DifferentIndividuals(_) => self.nominal = true,
            K::ClassAssertion(c, _) => self.expression(c),
            K::ObjectPropertyAssertion(p, ..) => self.property(p),
            K::NegativeObjectPropertyAssertion(p, ..) => {
                self.property(p);
                self.nominal = true;
            }
            K::Declaration(_)
            | K::AnnotationAssertion(..)
            | K::SubAnnotationPropertyOf(..)
            | K::AnnotationPropertyDomain(..)
            | K::AnnotationPropertyRange(..) => {}
        }
    }

    /// A containing logic, deliberately not the least expressive possible logic.
    fn bound(&self) -> String {
        if !self.alc
            && !self.transitive
            && !self.hierarchy
            && !self.roles
            && !self.inverse
            && !self.nominal
            && !self.functional
            && !self.number
            && !self.qualified
            && !self.data
        {
            return "EL".into();
        }
        let mut s = if self.roles || self.transitive {
            "S"
        } else {
            "ALC"
        }
        .to_string();
        if self.roles {
            s.push('R');
        } else if self.hierarchy {
            s.push('H');
        }
        if self.nominal {
            s.push('O');
        }
        if self.inverse {
            s.push('I');
        }
        // R includes the role machinery, but not number restrictions.
        if self.qualified {
            s.push('Q');
        } else if self.number {
            s.push('N');
        } else if self.functional {
            s.push('F');
        }
        if self.data {
            s.push_str("(D)");
        }
        s
    }
}

/// Counts and reports cover the mapped terminology and streamed assertion occurrences.
/// A failed limit or deadline returns an error, never a partial conformance result.
pub fn body(
    name: &str,
    o: &Opened,
    req: &OntologyRequest,
    r: &sparkles_owl::ofn::Renderer,
    opts: &QueryOptions,
) -> Result<serde_json::Value> {
    body_with_limit(name, o, req, r, opts, MAX_ITEMS)
}

fn body_with_limit(
    name: &str,
    o: &Opened,
    req: &OntologyRequest,
    r: &sparkles_owl::ofn::Renderer,
    opts: &QueryOptions,
    max_items: usize,
) -> Result<serde_json::Value> {
    check(opts)?;
    if o.ontology.axioms.len() > max_items {
        return Err(Error::invalid(
            "ontology metrics exceeds 1000000 mapped axioms",
        ));
    }
    let mut signature = BTreeSet::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut features = Features::default();
    let mut checker = Checker::new(
        &o.ontology,
        &Profile::ALL,
        CheckOptions {
            max_violations: 100,
        },
    );
    for (n, (a, origins)) in o.ontology.axioms.iter().enumerate() {
        if n.is_multiple_of(256) {
            check(opts)?;
        }
        if n >= max_items {
            return Err(Error::invalid(
                "ontology metrics exceeds 1000000 mapped axioms",
            ));
        }
        *counts.entry(a.axiom_type().name().into()).or_default() += 1;
        signature.extend(a.signature());
        if signature.len() > max_items {
            return Err(Error::invalid(
                "ontology metrics exceeds 1000000 signature entities",
            ));
        }
        features.axiom(a);
        checker.axiom(a, origins.first().map(|x| x.graph));
    }
    let terminology = counts.clone();
    let mut assertion_counts = BTreeMap::<String, usize>::new();
    let mut unmapped_assertions = 0usize;
    let mut scanned = counts.values().sum::<usize>();
    o.assertions(opts, |ax, t, g| {
        scanned += 1;
        if scanned > max_items {
            return Err(Error::invalid(
                "ontology metrics exceeds 1000000 mapped axioms and assertion triples",
            ));
        }
        if scanned.is_multiple_of(256) {
            check(opts)?;
        }
        match ax {
            Some(ax) => {
                *counts.entry(ax.axiom_type().name().into()).or_default() += 1;
                *assertion_counts
                    .entry(ax.axiom_type().name().into())
                    .or_default() += 1;
                signature.extend(ax.signature());
                if signature.len() > max_items {
                    return Err(Error::invalid(
                        "ontology metrics exceeds 1000000 signature entities",
                    ));
                }
                features.axiom(&ax);
                checker.axiom(&ax, Some(g));
            }
            None => {
                unmapped_assertions += 1;
                checker.unmapped(t, Some(g));
            }
        }
        Ok(true)
    })?;
    check(opts)?;
    let reports = checker.finish();
    let mut reasons = Vec::new();
    if reports
        .iter()
        .any(|r| r.profile == Profile::Dl && !r.conforms)
    {
        reasons.push("The mapped ontology does not conform to OWL 2 DL.");
    }
    if unmapped_assertions > 0
        || !o.ontology.unmapped.is_empty()
        || o.ontology.diagnostics_dropped > 0
        || o.ontology
            .diagnostics
            .iter()
            .any(|d| d.severity == read::Severity::Error)
    {
        reasons.push("The RDF mapping is incomplete or has errors.");
    }
    if o.imports.iter().any(|i| i.graph.is_none()) {
        reasons.push("An import could not be resolved to a graph of this dataset.");
    }
    if features.keys {
        reasons.push("Keys are outside the supported DL notation.");
    }
    if o.inferences
        .as_ref()
        .is_some_and(|i| i.stale != Some(false))
    {
        reasons.push("The selected materialized inferences are stale or have unknown freshness.");
    }
    let entities: BTreeMap<_, _> = EntityKind::ALL
        .into_iter()
        .map(|k| (k.name(), signature.iter().filter(|e| e.kind() == k).count()))
        .collect();
    let scope_complete = unmapped_assertions == 0
        && o.ontology.unmapped.is_empty()
        && o.ontology.diagnostics_dropped == 0
        && !o
            .ontology
            .diagnostics
            .iter()
            .any(|d| d.severity == read::Severity::Error)
        && o.imports.iter().all(|i| i.graph.is_some())
        && !o
            .inferences
            .as_ref()
            .is_some_and(|i| i.stale != Some(false));
    let bound = features.bound();
    let mut j = wire::reports_body(name, &o.view(), req, r, &reports, true);
    j["scope"] = serde_json::json!("ontology");
    j["scopeComplete"] = serde_json::json!(scope_complete);
    if !scope_complete && reports.iter().all(|r| r.conforms) {
        j["conforms"] = serde_json::Value::Null;
    }
    j["terminologyAxioms"] = serde_json::json!(terminology);
    j["assertionOccurrences"] = serde_json::json!(assertion_counts);
    j["unmappedAssertions"] = serde_json::json!(unmapped_assertions);
    j["counting"] = serde_json::json!(
        "distinct terminology axioms plus streamed assertion occurrences; signature entities are distinct by kind and IRI"
    );
    j["axioms"] = serde_json::json!(counts);
    j["axiomCount"] = serde_json::json!(counts.values().sum::<usize>());
    j["entities"] = serde_json::json!(entities);
    j["entityCount"] = serde_json::json!(signature.len());
    j["expressivity"] = serde_json::json!({
        "notation": if reasons.is_empty() { Some(&bound) } else { None },
        "observedBound": if features.keys { None } else { Some(&bound) },
        "meaning": "conservative-containing-logic",
        "reasons": reasons,
    });
    Ok(j)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dataset(body: &str) -> Dataset {
        let ds = Dataset::memory();
        ds.update(&format!("PREFIX : <http://ex.org/> PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> INSERT DATA {{ :o a owl:Ontology . {body} }}")).unwrap();
        ds
    }
    fn metrics(ds: &Dataset, args: serde_json::Value) -> serde_json::Value {
        wire::call(ds, "summary", &args, &QueryOptions::default()).unwrap()["metrics"].clone()
    }
    #[test]
    fn empty_scope_and_opt_in() {
        let ds = dataset("");
        assert!(
            wire::call(&ds, "summary", &json!({}), &QueryOptions::default())
                .unwrap()
                .get("metrics")
                .is_none()
        );
        let m = metrics(&ds, json!({"metrics":true}));
        assert_eq!(m["axiomCount"], 0);
        assert_eq!(m["entityCount"], 0);
        assert_eq!(m["scopeComplete"], true);
        assert_eq!(m["expressivity"]["notation"], "EL");
        assert_eq!(m["reports"].as_array().unwrap().len(), 4);
        assert!(
            m["reports"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["conforms"] == true)
        );
    }
    #[test]
    fn streams_assertions_and_counts_undeclared_and_punned_entities() {
        let ds = dataset(
            ":A a owl:Class . :A rdfs:subClassOf :B . :p a owl:ObjectProperty . :A a :B . :A :p :b . :d a owl:DatatypeProperty . :b :d 3 .",
        );
        let m = metrics(&ds, json!({"metrics":true}));
        assert_eq!(m["entities"]["Class"], 2);
        assert_eq!(m["entities"]["NamedIndividual"], 2);
        for kind in [
            "ClassAssertion",
            "ObjectPropertyAssertion",
            "DataPropertyAssertion",
        ] {
            assert_eq!(m["axioms"][kind], 1);
            assert_eq!(m["assertionOccurrences"][kind], 1);
        }
        assert!(
            m["terminologyAxioms"]
                .get("DataPropertyAssertion")
                .is_none()
        );
        assert_eq!(m["expressivity"]["observedBound"], "ALC(D)");
        assert!(
            m["reports"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["checked"] == m["axiomCount"])
        );
    }
    #[test]
    fn imports_scope_and_assertion_occurrences() {
        let ds = dataset(
            ":o a owl:Ontology ; owl:imports <http://ex.org/g> . :A a owl:Class . :a a :A . GRAPH :g { :A a owl:Class . :a a :A . :b a :A . }",
        );
        let m = metrics(&ds, json!({"metrics":true}));
        assert_eq!(m["graphs"], json!(["default", "http://ex.org/g"]));
        assert_eq!(m["axioms"]["Declaration"], 1);
        assert_eq!(m["assertionOccurrences"]["ClassAssertion"], 3);
        assert_eq!(m["entities"]["NamedIndividual"], 2);
        let local = metrics(&ds, json!({"metrics":true,"imports":false}));
        assert_eq!(local["assertionOccurrences"]["ClassAssertion"], 1);
        assert_eq!(local["entities"]["NamedIndividual"], 1);
    }
    #[test]
    fn unresolved_import_and_bad_mapping_have_unknown_expressivity() {
        let ds = dataset(":o a owl:Ontology ; owl:imports <http://missing/> . :A a owl:Class .");
        let m = metrics(&ds, json!({"metrics":true}));
        assert!(m["expressivity"]["notation"].is_null());
        assert_eq!(m["expressivity"]["observedBound"], "EL");
        assert_eq!(m["scopeComplete"], false);
        assert!(m["conforms"].is_null());
        assert!(!m["expressivity"]["reasons"].as_array().unwrap().is_empty());
        assert_eq!(
            metrics(&ds, json!({"metrics":true,"imports":false}))["expressivity"]["notation"],
            "EL"
        );
        let bad = dataset(":A a owl:Class . :a :unknown :b .");
        let m = metrics(&bad, json!({"metrics":true,"mode":"strict"}));
        assert!(m["expressivity"]["notation"].is_null());
        assert_eq!(m["unmappedAssertions"], 1);
    }
    #[test]
    fn containing_logic_tracks_nested_constructors_and_role_features() {
        let cases = [
            (
                "SubClassOf(<http://e/A> ObjectSomeValuesFrom(<http://e/p> <http://e/B>))",
                "EL",
            ),
            (
                "SubClassOf(<http://e/A> ObjectAllValuesFrom(<http://e/p> ObjectUnionOf(<http://e/B> <http://e/C>)))",
                "ALC",
            ),
            (
                "SubClassOf(<http://e/A> ObjectMinCardinality(2 <http://e/p>))",
                "ALCN",
            ),
            (
                "SubClassOf(<http://e/A> ObjectMaxCardinality(2 ObjectInverseOf(<http://e/p>) ObjectOneOf(<http://e/a>)))",
                "ALCOIQ",
            ),
            ("FunctionalObjectProperty(<http://e/p>)", "ALCF"),
            ("InverseFunctionalObjectProperty(<http://e/p>)", "ALCIF"),
            ("SubObjectPropertyOf(<http://e/p> <http://e/q>)", "ALCH"),
            ("TransitiveObjectProperty(<http://e/p>)", "S"),
            (
                "SubObjectPropertyOf(ObjectPropertyChain(<http://e/p> <http://e/q>) <http://e/r>)",
                "SR",
            ),
            ("Declaration(DataProperty(<http://e/d>))", "EL"),
            (
                "SubClassOf(<http://e/A> DataMinCardinality(2 <http://e/d>))",
                "ALCQ(D)",
            ),
        ];
        for (axiom, bound) in cases {
            let mut f = Features::default();
            f.axiom(&sparkles_owl::ofn_read::parse_axiom(axiom, &[]).unwrap());
            assert_eq!(f.bound(), bound, "{axiom}");
        }
    }
    #[test]
    fn profiles_are_bounded_but_check_the_whole_scope() {
        let mut triples = String::from(":p a owl:ObjectProperty . :B a owl:Class . ");
        for n in 0..120 {
            triples.push_str(&format!(":A{n} a owl:Class ; rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:allValuesFrom :B ] . "));
        }
        let ds = dataset(&triples);
        let m = metrics(&ds, json!({"metrics":true}));
        let el = &m["reports"][0];
        assert_eq!(el["profile"], "el");
        assert_eq!(el["truncated"], true);
        assert_eq!(el["violations"].as_array().unwrap().len(), 100);
        assert_eq!(el["checked"], m["axiomCount"]);
        assert_eq!(m["expressivity"]["notation"], "ALC");
    }
    #[test]
    fn budgets_and_cancellation_never_return_partial_counts() {
        let ds = dataset(":A a owl:Class . :a a :A . :b a :A .");
        let req = OntologyRequest::default();
        let o = ds.ontology().open(&req, &QueryOptions::default()).unwrap();
        let renderer = sparkles_owl::ofn::Renderer::new(Vec::<(String, String)>::new());
        let result = body_with_limit("", &o, &req, &renderer, &QueryOptions::default(), 2);
        assert!(
            result.is_err(),
            "the streamed third axiom must hit the limit"
        );
        let late = QueryOptions {
            deadline: Some(Instant::now()),
            ..Default::default()
        };
        assert!(matches!(
            body("", &o, &req, &renderer, &late),
            Err(Error::Timeout)
        ));
        let cancelled = QueryOptions {
            cancel: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
            ..Default::default()
        };
        assert!(matches!(
            body("", &o, &req, &renderer, &cancelled),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn keys_have_no_supported_containing_notation() {
        let ds = dataset(":A a owl:Class ; owl:hasKey (:p) . :p a owl:ObjectProperty .");
        let m = metrics(&ds, json!({"metrics":true}));
        assert!(m["expressivity"]["notation"].is_null());
        assert!(m["expressivity"]["observedBound"].is_null());
        assert!(
            m["expressivity"]["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r.as_str().unwrap().contains("Keys"))
        );
    }
}
