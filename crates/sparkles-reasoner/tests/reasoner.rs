use oxrdf::Term;
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::{QueryOptions, query};
use sparkles::store::{Store, StoreOptions};
use sparkles_reasoner::{INFERRED_GRAPH, Profile, ReasonOptions, ReasonReport, clear, materialize};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const PREFIXES: &str = "@prefix ex: <http://ex.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
";

fn store(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!("{PREFIXES}{ttl}").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn run(s: &Store, p: Profile) -> ReasonReport {
    materialize(s, &p, &ReasonOptions::default()).unwrap_or_else(|e| panic!("{e:#}"))
}

fn qopts(reasoning: bool) -> QueryOptions {
    QueryOptions {
        prefixes: vec![
            ("ex".into(), "http://ex.org/".into()),
            (
                "rdf".into(),
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#".into(),
            ),
            (
                "rdfs".into(),
                "http://www.w3.org/2000/01/rdf-schema#".into(),
            ),
            ("owl".into(), "http://www.w3.org/2002/07/owl#".into()),
            ("xsd".into(), "http://www.w3.org/2001/XMLSchema#".into()),
        ],
        default_graph_extra: if reasoning {
            vec![INFERRED_GRAPH.into()]
        } else {
            vec![]
        },
        ..Default::default()
    }
}

/// ASK against default graph ∪ inferred graph.
fn ask(s: &Store, pattern: &str) -> bool {
    let q = format!("ASK {{ {pattern} }}");
    query(s.snapshot(), &q, &qopts(true))
        .unwrap_or_else(|e| panic!("{q}: {e}"))
        .boolean
}

/// Sorted, compact rendering of SELECT results (local names / lexical forms).
fn select(s: &Store, q: &str, reasoning: bool) -> Vec<String> {
    let r = query(s.snapshot(), q, &qopts(reasoning)).unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut v: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| match t {
                    Some(Term::NamedNode(n)) => {
                        n.as_str().rsplit(['/', '#']).next().unwrap().to_string()
                    }
                    Some(Term::Literal(l)) => l.value().to_string(),
                    Some(t) => t.to_string(),
                    None => "UNDEF".into(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    v.sort();
    v
}

fn assert_entails(s: &Store, patterns: &[&str]) {
    for p in patterns {
        assert!(ask(s, p), "expected entailment: {p}");
    }
}

fn assert_not_entails(s: &Store, patterns: &[&str]) {
    for p in patterns {
        assert!(!ask(s, p), "unexpected entailment: {p}");
    }
}

// ---------------------------------------------------------------- RDFS -------

const ONTOLOGY: &str = r#"
ex:Student rdfs:subClassOf ex:Person .
ex:Person rdfs:subClassOf ex:Agent .
ex:Agent rdfs:subClassOf ex:Thing .
ex:teaches rdfs:domain ex:Teacher ; rdfs:range ex:Course .
ex:Teacher rdfs:subClassOf ex:Person .
ex:hasMother rdfs:subPropertyOf ex:hasParent .
ex:hasParent rdfs:subPropertyOf ex:hasAncestor .
ex:hasParent rdfs:domain ex:Person .
ex:alice a ex:Student ; ex:hasMother ex:carol .
ex:bob ex:teaches ex:math .
ex:alice rdfs:label "Alice" .
"#;

#[test]
fn rdfs_simple_ontology() {
    let s = store(ONTOLOGY);
    let r = run(&s, Profile::RdfsSimple);
    assert_eq!(r.profile, "rdfs-simple");
    assert_eq!(r.rules, 6);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_entails(
        &s,
        &[
            "ex:alice a ex:Person, ex:Agent, ex:Thing",
            "ex:Student rdfs:subClassOf ex:Agent, ex:Thing",
            "ex:bob a ex:Teacher, ex:Person, ex:Agent, ex:Thing",
            "ex:math a ex:Course",
            "ex:hasMother rdfs:subPropertyOf ex:hasAncestor",
            "ex:alice ex:hasParent ex:carol ; ex:hasAncestor ex:carol",
        ],
    );
    // no RDFS axioms / resource typing at the simple level
    assert_not_entails(
        &s,
        &[
            "ex:alice a rdfs:Resource",
            "ex:teaches a rdf:Property",
            "ex:carol a ex:Person",
        ],
    );
    // inferences live in the inferred graph only
    let types = "SELECT ?t WHERE { ex:alice a ?t }";
    assert_eq!(select(&s, types, false), ["Student"]);
    assert_eq!(
        select(&s, types, true),
        ["Agent", "Person", "Student", "Thing"]
    );
    // derived triples never duplicate asserted ones
    let dup = format!("ASK {{ ?s ?p ?o . GRAPH <{INFERRED_GRAPH}> {{ ?s ?p ?o }} }}");
    assert!(!query(s.snapshot(), &dup, &qopts(false)).unwrap().boolean);
    let n = select(
        &s,
        &format!("SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <{INFERRED_GRAPH}> {{ ?s ?p ?o }} }}"),
        false,
    );
    assert_eq!(n, [r.inferred.to_string()]);
}

#[test]
fn rdfs_full() {
    let s = store(&format!(
        "{ONTOLOGY}\n ex:list rdf:_1 ex:first ; rdf:_2 ex:second .\n ex:myInt a rdfs:Datatype .\n"
    ));
    let r = run(&s, Profile::Rdfs);
    assert!(r.iterations >= 2);
    // literal-subject triples (e.g. "Alice" a rdfs:Resource) are not written
    assert!(
        r.warnings.iter().any(|w| w.contains("generalized")),
        "{:?}",
        r.warnings
    );
    assert_entails(
        &s,
        &[
            "ex:alice a ex:Thing, rdfs:Resource",
            "ex:teaches a rdf:Property",
            "ex:teaches rdfs:subPropertyOf ex:teaches",
            "ex:Student a rdfs:Class",
            "ex:Student rdfs:subClassOf ex:Student, rdfs:Resource",
            "rdf:_1 a rdfs:ContainerMembershipProperty ; rdfs:subPropertyOf rdfs:member",
            "ex:list rdfs:member ex:first, ex:second",
            "ex:myInt rdfs:subClassOf rdfs:Literal",
            "rdfs:subClassOf rdfs:domain rdfs:Class",
            "rdf:type a rdf:Property",
        ],
    );
    assert_not_entails(&s, &["ex:carol a ex:Person", "ex:math a ex:Teacher"]);
    let lit = "ASK { GRAPH <urn:x-sparkles:inferred> { ?s ?p ?o FILTER(isLiteral(?s)) } }";
    assert!(!query(s.snapshot(), lit, &qopts(false)).unwrap().boolean);
}

/// W3C RDF 1.1 Semantics RDFS entailment patterns, each checked in isolation, plus
/// non-entailments.
#[test]
fn w3c_rdfs_entailment_cases() {
    struct Case {
        name: &'static str,
        data: &'static str,
        entailed: &'static [&'static str],
        not_entailed: &'static [&'static str],
    }
    let cases = [
        Case {
            name: "rdfs2 domain",
            data: "ex:p rdfs:domain ex:C . ex:x ex:p ex:y .",
            entailed: &["ex:x a ex:C"],
            not_entailed: &["ex:y a ex:C"],
        },
        Case {
            name: "rdfs3 range",
            data: "ex:p rdfs:range ex:C . ex:x ex:p ex:y .",
            entailed: &["ex:y a ex:C"],
            not_entailed: &["ex:x a ex:C"],
        },
        Case {
            name: "rdfs5 subPropertyOf transitive",
            data: "ex:p rdfs:subPropertyOf ex:q . ex:q rdfs:subPropertyOf ex:r . ex:r rdfs:subPropertyOf ex:s .",
            entailed: &[
                "ex:p rdfs:subPropertyOf ex:r, ex:s",
                "ex:q rdfs:subPropertyOf ex:s",
            ],
            not_entailed: &["ex:s rdfs:subPropertyOf ex:p"],
        },
        Case {
            name: "rdfs7 subPropertyOf inheritance",
            data: "ex:p rdfs:subPropertyOf ex:q . ex:x ex:p ex:y .",
            entailed: &["ex:x ex:q ex:y"],
            not_entailed: &["ex:y ex:q ex:x"],
        },
        Case {
            name: "rdfs9 subClassOf inheritance",
            data: "ex:A rdfs:subClassOf ex:B . ex:x a ex:A .",
            entailed: &["ex:x a ex:B"],
            not_entailed: &["ex:x a ex:C"],
        },
        Case {
            name: "rdfs11 subClassOf transitive",
            data: "ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:C .",
            entailed: &["ex:A rdfs:subClassOf ex:C"],
            not_entailed: &["ex:C rdfs:subClassOf ex:A"],
        },
        Case {
            name: "domain through subPropertyOf (rdfs7 + rdfs2)",
            data: "ex:p rdfs:domain ex:C . ex:q rdfs:subPropertyOf ex:p . ex:x ex:q ex:y .",
            entailed: &["ex:x a ex:C", "ex:x ex:p ex:y"],
            not_entailed: &[],
        },
        Case {
            name: "domain is not inherited upwards",
            data: "ex:p rdfs:domain ex:C . ex:p rdfs:subPropertyOf ex:q . ex:x ex:q ex:y .",
            entailed: &[],
            not_entailed: &["ex:x a ex:C"],
        },
        Case {
            name: "range + subClassOf (rdfs3 + rdfs9)",
            data: "ex:p rdfs:range ex:C . ex:C rdfs:subClassOf ex:D . ex:x ex:p ex:y .",
            entailed: &["ex:y a ex:C, ex:D"],
            not_entailed: &["ex:x a ex:D"],
        },
        Case {
            name: "rdf1 / rdfs4 / rdfs6 / rdfs8 / rdfs10",
            data: "ex:x ex:p ex:y . ex:C a rdfs:Class .",
            entailed: &[
                "ex:p a rdf:Property",
                "ex:x a rdfs:Resource",
                "ex:y a rdfs:Resource",
                "ex:p rdfs:subPropertyOf ex:p",
                "ex:C rdfs:subClassOf rdfs:Resource, ex:C",
            ],
            not_entailed: &["ex:x a rdf:Property"],
        },
        Case {
            name: "rdfs12 container membership",
            data: "ex:bag rdf:_3 ex:z .",
            entailed: &[
                "ex:bag rdfs:member ex:z",
                "rdf:_3 rdfs:subPropertyOf rdfs:member",
            ],
            not_entailed: &[],
        },
        Case {
            name: "cycle in subClassOf",
            data: "ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:A . ex:x a ex:A .",
            entailed: &[
                "ex:x a ex:B",
                "ex:A rdfs:subClassOf ex:A",
                "ex:B rdfs:subClassOf ex:B",
            ],
            not_entailed: &[],
        },
    ];
    for c in cases {
        let s = store(c.data);
        run(&s, Profile::Rdfs);
        for p in c.entailed {
            assert!(ask(&s, p), "{}: expected entailment {p}", c.name);
        }
        for p in c.not_entailed {
            assert!(!ask(&s, p), "{}: unexpected entailment {p}", c.name);
        }
    }
}

// ---------------------------------------------------------------- OWL RL -----

#[test]
fn owl_rl_properties() {
    let s = store(
        r#"
        ex:hasChild owl:inverseOf ex:hasParent .
        ex:alice ex:hasChild ex:bob .
        ex:marriedTo a owl:SymmetricProperty .
        ex:alice ex:marriedTo ex:dan .
        ex:ancestorOf a owl:TransitiveProperty .
        ex:a ex:ancestorOf ex:b . ex:b ex:ancestorOf ex:c . ex:c ex:ancestorOf ex:d .
        ex:hasBirthMother a owl:FunctionalProperty .
        ex:bob ex:hasBirthMother ex:alice, ex:alicia .
        ex:ssn a owl:InverseFunctionalProperty .
        ex:p1 ex:ssn "123" . ex:p2 ex:ssn "123" .
        ex:hasUncle owl:propertyChainAxiom ( ex:hasParent ex:hasBrother ) .
        ex:alice ex:hasBrother ex:eve .
        ex:knows owl:equivalentProperty ex:acquaintedWith .
        ex:alice ex:knows ex:frank .
        "#,
    );
    let r = run(&s, Profile::OwlRl);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_entails(
        &s,
        &[
            // inverseOf
            "ex:bob ex:hasParent ex:alice",
            "ex:hasParent owl:inverseOf ex:hasChild",
            // symmetric
            "ex:dan ex:marriedTo ex:alice",
            // transitive
            "ex:a ex:ancestorOf ex:c, ex:d",
            "ex:b ex:ancestorOf ex:d",
            // functional → sameAs (and the equality closure)
            "ex:alice owl:sameAs ex:alicia",
            "ex:alicia owl:sameAs ex:alice",
            "ex:alicia ex:marriedTo ex:dan",
            "ex:dan ex:marriedTo ex:alicia",
            // inverse functional
            "ex:p1 owl:sameAs ex:p2",
            // property chain
            "ex:bob ex:hasUncle ex:eve",
            // equivalentProperty
            "ex:alice ex:acquaintedWith ex:frank",
            "ex:knows rdfs:subPropertyOf ex:acquaintedWith",
        ],
    );
    assert_not_entails(
        &s,
        &[
            "ex:d ex:ancestorOf ex:a",
            "ex:alice owl:sameAs ex:alice",
            "ex:bob ex:hasParent ex:bob",
        ],
    );
}

#[test]
fn owl_rl_classes() {
    let s = store(
        r#"
        # equivalentClass
        ex:Human owl:equivalentClass ex:Person .
        ex:ann a ex:Human .
        ex:Person rdfs:subClassOf ex:Mammal .

        # hasValue
        ex:RedThing a owl:Restriction ; owl:onProperty ex:color ; owl:hasValue ex:red .
        ex:apple ex:color ex:red .
        ex:cherry a ex:RedThing .

        # someValuesFrom
        ex:Parent a owl:Restriction ; owl:onProperty ex:hasChild ; owl:someValuesFrom ex:Person .
        ex:ann ex:hasChild ex:ben .
        ex:ben a ex:Person .
        ex:cat ex:hasChild ex:kitten .

        # allValuesFrom
        ex:VeganMeal a owl:Restriction ; owl:onProperty ex:ingredient ; owl:allValuesFrom ex:Plant .
        ex:salad a ex:VeganMeal ; ex:ingredient ex:lettuce .

        # intersectionOf
        ex:Mother owl:intersectionOf ( ex:Woman ex:Parent ) .
        ex:ann a ex:Woman .
        ex:zoe a ex:Woman .
        ex:mia a ex:Mother .

        # unionOf
        ex:Pet owl:unionOf ( ex:Cat ex:Dog ) .
        ex:rex a ex:Dog .

        # oneOf
        ex:Primary owl:oneOf ( ex:red ex:green ex:blue ) .

        # sameAs
        ex:ann owl:sameAs ex:annie .
        ex:ann ex:age 42 .
        "#,
    );
    run(&s, Profile::OwlRl);
    assert_entails(
        &s,
        &[
            "ex:ann a ex:Person, ex:Mammal",
            "ex:Human rdfs:subClassOf ex:Person",
            "ex:Person owl:equivalentClass ex:Human",
            "ex:apple a ex:RedThing",
            "ex:cherry ex:color ex:red",
            "ex:ann a ex:Parent",
            "ex:lettuce a ex:Plant",
            "ex:ann a ex:Mother",
            "ex:mia a ex:Woman, ex:Parent",
            "ex:Mother rdfs:subClassOf ex:Woman, ex:Parent",
            "ex:rex a ex:Pet",
            "ex:Cat rdfs:subClassOf ex:Pet",
            "ex:green a ex:Primary",
            "ex:annie a ex:Human, ex:Person, ex:Mother ; ex:age 42 ; ex:hasChild ex:ben",
            "ex:annie owl:sameAs ex:ann",
        ],
    );
    assert_not_entails(
        &s,
        &[
            "ex:cat a ex:Parent",
            "ex:zoe a ex:Mother",
            "ex:ann a ex:Pet",
        ],
    );
}

#[test]
fn owl_sameas_clique_is_bounded() {
    // 30 aliases of one individual with 10 properties each: the replacement rules
    // produce the full clique (30 * 29 sameAs + 30 * 300 property triples) and stop.
    let mut ttl = String::new();
    for i in 1..30 {
        ttl.push_str(&format!("ex:a{} owl:sameAs ex:a{} .\n", i - 1, i));
    }
    for i in 0..30 {
        ttl.push_str(&format!("ex:a{i} ex:p{} ex:v{i} .\n", i % 10));
    }
    let s = store(&ttl);
    let r = run(&s, Profile::OwlRl);
    let n = select(
        &s,
        "SELECT (COUNT(*) AS ?n) WHERE { ?x owl:sameAs ?y }",
        true,
    );
    assert_eq!(n, ["870"]);
    assert!(ask(&s, "ex:a0 ex:p9 ex:v29"));
    assert!(ask(&s, "ex:a29 ex:p0 ex:v0"));
    assert!(r.inferred < 20_000, "{}", r.inferred);
}

// ---------------------------------------------------------------- custom -----

#[test]
fn custom_rules_with_builtins() {
    let s = store(
        r#"
        ex:alice ex:age 30 ; ex:first "Alice" ; ex:last "Smith" ; ex:email "alice@example.org" .
        ex:bob ex:age 17 ; ex:first "Bob" ; ex:last "Jones" .
        ex:carol ex:age "45"^^xsd:int .
        ex:item1 ex:price 10 ; ex:qty 3 .
        ex:item2 ex:price 2.5 ; ex:qty 4 .
        ex:dave ex:parent ex:erin .
        "#,
    );
    let rules = r#"
        @prefix ex: <http://ex.org/> .
        # comparison against a bare number; typed int compared by value
        [adult: (?p ex:age ?a) ge(?a, 18) -> (?p rdf:type ex:Adult)]
        [minor: (?p ex:age ?a), lessThan(?a, 18) -> (?p rdf:type ex:Minor)]
        # arithmetic binders
        [older: (?p ex:age ?a) sum(?a, 10, ?b) -> (?p ex:ageIn10 ?b)]
        [next:  (?p ex:age ?a) addOne(?a, ?b) -> (?p ex:nextAge ?b)]
        [total: (?i ex:price ?x) (?i ex:qty ?q) product(?x, ?q, ?t) -> (?i ex:total ?t)]
        [half:  (?i ex:qty ?q) quotient(?q, 2, ?h) -> (?i ex:half ?h)]
        [maxv:  (?i ex:qty ?q) max(?q, 3, ?m) -> (?i ex:atLeast3 ?m)]
        # strings / IRIs
        [name:  (?p ex:first ?f) (?p ex:last ?l) strConcat(?f, ' ', ?l, ?n) -> (?p ex:name ?n)]
        [page:  (?p ex:first ?f) uriConcat('http://ex.org/people/', ?f, ?u) -> (?p ex:page ?u)]
        [dom:   (?p ex:email ?e) regex(?e, '[^@]+@(.*)', ?d) -> (?p ex:domain ?d)]
        # type tests
        [lit:   (?p ex:first ?f) isLiteral(?f) notBNode(?f) isDType(?f, xsd:string) -> (?p ex:hasLiteralName 'yes')]
        [int:   (?p ex:age ?a) isDType(?a, xsd:integer) -> (?p ex:intAge 'yes')]
        # negation as failure against the current state
        [noemail: (?p ex:first ?f) noValue(?p, ex:email) -> (?p rdf:type ex:NoEmail)]
        # blank nodes
        [sk:    (?c ex:parent ?p) makeSkolem(?x, ?c, ?p) -> (?c ex:link ?x), (?x ex:to ?p)]
        # equality
        [eq:    (?a ex:age ?x) (?b ex:age ?y) notEqual(?a, ?b) equal(?x, ?y) -> (?a ex:sameAge ?b)]
        # backward rule: parsed but skipped
        [bw:    (?a ex:q ?b) <- (?a ex:first ?b)]
        # nested backward rule in a head: flattened
        [nest:  (?p ex:age ?a) -> [(?p ex:called ?f) <- (?p ex:first ?f)]]
        # ignored head builtin
        [pr:    (?p ex:first ?f) -> print(?f), (?p ex:hasFirst 'true')]
    "#;
    let r = run(&s, Profile::Rules(rules.into()));
    assert_eq!(r.profile, "rules");
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("'bw'") && w.contains("backward")),
        "{:?}",
        r.warnings
    );
    assert!(
        r.warnings.iter().any(|w| w.contains("print")),
        "{:?}",
        r.warnings
    );
    assert_eq!(r.rules, 17);
    assert_entails(
        &s,
        &[
            "ex:alice a ex:Adult",
            "ex:carol a ex:Adult",
            "ex:bob a ex:Minor",
            "ex:alice ex:ageIn10 40",
            "ex:bob ex:nextAge 18",
            "ex:item1 ex:total 30",
            "ex:item2 ex:total ?t FILTER(?t = 10 && datatype(?t) = xsd:decimal)",
            "ex:item1 ex:half 1",
            "ex:item1 ex:atLeast3 3",
            "ex:item2 ex:atLeast3 4",
            "ex:alice ex:name 'Alice Smith'",
            "ex:bob ex:page <http://ex.org/people/Bob>",
            "ex:alice ex:domain 'example.org'",
            "ex:alice ex:hasLiteralName 'yes'",
            "ex:carol ex:intAge 'yes'",
            "ex:bob a ex:NoEmail",
            "ex:dave ex:link ?x . ?x ex:to ex:erin FILTER(isBlank(?x))",
            "ex:alice ex:called 'Alice'",
            "ex:bob ex:hasFirst 'true'",
        ],
    );
    assert_not_entails(
        &s,
        &[
            "ex:bob a ex:Adult",
            "ex:alice a ex:Minor",
            "ex:alice a ex:NoEmail",
            "ex:alice ex:q ?x",
            "ex:alice ex:sameAge ?x",
            "ex:carol ex:called ?x",
        ],
    );
    // makeSkolem is deterministic: one blank node for the one (child, parent) pair
    assert_eq!(
        select(&s, "SELECT (COUNT(*) AS ?n) WHERE { ?c ex:link ?x }", true),
        ["1"]
    );
}

#[test]
fn custom_rules_equal_by_value_and_skip_unsupported() {
    let s = store("ex:a ex:v 1 . ex:b ex:v \"1\"^^xsd:int . ex:c ex:v 2 .");
    let rules = r#"
        @prefix ex: <http://ex.org/>.
        [eq: (?x ex:v ?a) (?y ex:v ?b) notEqual(?x, ?y) equal(?a, ?b) -> (?x ex:sameValue ?y)]
        [f: (?x ex:v ?a) -> (?x ex:f some(?a))]
        [u: (?x ex:v ?a) frobnicate(?a) -> (?x ex:g ?a)]
        [h: (?x ex:v ?a) -> (?x ex:h ?unbound)]
    "#;
    let r = run(&s, Profile::Rules(rules.into()));
    assert_eq!(r.rules, 1, "{:?}", r.warnings);
    assert_eq!(r.warnings.len(), 3, "{:?}", r.warnings);
    assert!(r.warnings.iter().any(|w| w.contains("functor")));
    assert!(r.warnings.iter().any(|w| w.contains("frobnicate")));
    assert!(r.warnings.iter().any(|w| w.contains("?unbound")));
    assert_entails(&s, &["ex:a ex:sameValue ex:b", "ex:b ex:sameValue ex:a"]);
    assert_not_entails(&s, &["ex:a ex:sameValue ex:c"]);
}

#[test]
fn parse_errors_are_reported() {
    let s = store("ex:a ex:p ex:b .");
    let e = materialize(
        &s,
        &Profile::Rules("[r: (?a ex:p ?b) -> (?b ex:p ?a)]".into()),
        &ReasonOptions::default(),
    )
    .unwrap_err();
    let msg = format!("{e:#}");
    assert!(
        msg.contains("line 1") && msg.contains("unknown prefix"),
        "{msg}"
    );
}

// ------------------------------------------------------ lifecycle & limits ---

#[test]
fn rematerialize_updates_and_clear() {
    let s = store(
        "ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:C . ex:x a ex:A . ex:y a ex:B .",
    );
    let r1 = run(&s, Profile::RdfsSimple);
    assert_eq!(r1.inferred, 4); // A⊑C, x:B, x:C, y:C
    let v1 = s.snapshot().version;
    // idempotent: same content, nothing written
    let r2 = run(&s, Profile::RdfsSimple);
    assert_eq!(r2.inferred, 4);
    assert_eq!(
        s.snapshot().version,
        v1,
        "no-op rematerialization should not commit"
    );
    // remove a base triple: stale inferences disappear
    sparkles::sparql::update::update(
        &s,
        "DELETE DATA { <http://ex.org/B> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex.org/C> }",
        &Default::default(),
    )
    .unwrap();
    let r3 = run(&s, Profile::RdfsSimple);
    assert_eq!(r3.inferred, 1);
    assert!(ask(&s, "ex:x a ex:B"));
    assert!(!ask(&s, "ex:x a ex:C"));
    // switching profile replaces the graph
    let r4 = run(&s, Profile::Rdfs);
    assert!(r4.inferred > 10);
    assert_eq!(clear(&s).unwrap(), r4.inferred);
    assert!(!ask(&s, "ex:x a ex:B"));
    assert_eq!(clear(&s).unwrap(), 0);
    // base data untouched
    assert!(ask(&s, "ex:x a ex:A"));
}

#[test]
fn limits_and_cancellation() {
    let s = store(
        "ex:p a owl:TransitiveProperty . ex:a0 ex:p ex:a1 . ex:a1 ex:p ex:a2 . ex:a2 ex:p ex:a3 . ex:a3 ex:p ex:a4 .",
    );
    let e = materialize(
        &s,
        &Profile::OwlRl,
        &ReasonOptions {
            max_inferred: 3,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(e.to_string().contains("limit"), "{e}");
    let e = materialize(
        &s,
        &Profile::OwlRl,
        &ReasonOptions {
            max_iterations: 1,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(e.to_string().contains("fixpoint"), "{e}");
    let cancel = Arc::new(AtomicBool::new(true));
    let e = materialize(
        &s,
        &Profile::OwlRl,
        &ReasonOptions {
            cancel: Some(cancel),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(e.to_string().contains("cancelled"), "{e}");
    // nothing was written by the failed runs
    assert!(!ask(&s, "ex:a0 ex:p ex:a2"));
    // progress callback
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let c2 = calls.clone();
    let opts = ReasonOptions {
        progress: Some(Arc::new(move |f, m: &str| {
            c2.lock().unwrap().push((f, m.to_string()))
        })),
        ..Default::default()
    };
    materialize(&s, &Profile::OwlRl, &opts).unwrap();
    let calls = calls.lock().unwrap();
    assert!(calls.len() >= 3);
    assert_eq!(calls.last().unwrap().0, 1.0);
    assert!(calls.windows(2).all(|w| w[0].0 <= w[1].0));
    assert!(ask(&s, "ex:a0 ex:p ex:a4"));
}

#[test]
fn profile_from_str() {
    assert_eq!("rdfs".parse::<Profile>().unwrap(), Profile::Rdfs);
    assert_eq!(
        "RDFS-simple".parse::<Profile>().unwrap(),
        Profile::RdfsSimple
    );
    assert_eq!("owl-rl".parse::<Profile>().unwrap(), Profile::OwlRl);
    assert_eq!("owl".parse::<Profile>().unwrap(), Profile::OwlRl);
    assert!("owl-dl".parse::<Profile>().is_err());
    // the built-in rule sets parse and compile without warnings
    for p in [Profile::Rdfs, Profile::RdfsSimple, Profile::OwlRl] {
        let s = store("ex:a ex:p ex:b .");
        let r = run(&s, p.clone());
        assert!(r.warnings.is_empty(), "{p}: {:?}", r.warnings);
        assert_eq!(
            r.rules,
            p.rules()
                .unwrap()
                .iter()
                .filter(|r| !r.body.is_empty() || !r.head.is_empty())
                .count()
        );
    }
}

// ---------------------------------------------------------- end to end ------

#[test]
fn end_to_end_sparql_over_default_and_inferred() {
    let s = store(ONTOLOGY);
    run(&s, Profile::RdfsSimple);
    let q = "SELECT ?x WHERE { ?x a ex:Person }";
    // plain query: asserted data only
    assert!(select(&s, q, false).is_empty());
    // default graph ∪ inferred graph via QueryOptions::default_graph_extra
    assert_eq!(select(&s, q, true), ["alice", "bob"]);
    // the same via an explicit UNION
    let u = format!(
        "SELECT ?x WHERE {{ {{ ?x a ex:Person }} UNION {{ GRAPH <{INFERRED_GRAPH}> {{ ?x a ex:Person }} }} }}"
    );
    assert_eq!(select(&s, &u, false), ["alice", "bob"]);
    // and via protocol default-graph-uri with Jena's special default graph IRI
    let opts = QueryOptions {
        default_graph_uris: vec!["urn:x-arq:DefaultGraph".into(), INFERRED_GRAPH.into()],
        ..qopts(false)
    };
    let r = query(s.snapshot(), q, &opts).unwrap();
    assert_eq!(r.len(), 2);
    // joins across asserted and inferred triples
    let j = "SELECT ?x ?c WHERE { ?x a ex:Agent ; ex:hasAncestor ?c }";
    assert_eq!(select(&s, j, true), ["alice carol"]);
}

#[test]
fn persistent_store_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        s.load(&[Source::from_bytes(
            format!("{PREFIXES}{ONTOLOGY}").into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        run(&s, Profile::OwlRl);
        assert!(ask(&s, "ex:alice a ex:Thing"));
    }
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert!(ask(&s, "ex:alice a ex:Thing"));
    s.compact().unwrap();
    assert!(ask(&s, "ex:alice a ex:Thing"));
    assert!(clear(&s).unwrap() > 0);
    assert!(!ask(&s, "ex:alice a ex:Thing"));
}

/// Semi-naive evaluation must reach the same fixpoint as a direct computation:
/// transitive closure of a pseudo-random graph, with a 2-atom and a 3-atom rule.
#[test]
fn semi_naive_matches_direct_closure() {
    let n = 60u64;
    let mut edges = std::collections::BTreeSet::new();
    let mut x = 12345u64;
    for _ in 0..90 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let a = (x >> 33) % n;
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let b = (x >> 33) % n;
        edges.insert((a, b));
    }
    // direct closure
    let mut closure = edges.clone();
    loop {
        let mut add = Vec::new();
        for &(a, b) in &closure {
            for &(c, d) in &closure {
                if b == c && !closure.contains(&(a, d)) {
                    add.push((a, d));
                }
            }
        }
        if add.is_empty() {
            break;
        }
        closure.extend(add);
    }
    let ttl: String = edges
        .iter()
        .map(|(a, b)| format!("ex:n{a} ex:p ex:n{b} .\n"))
        .collect();
    for rules in [
        "@prefix ex: <http://ex.org/>. [t: (?a ex:p ?b) (?b ex:p ?c) -> (?a ex:p ?c)]",
        "@prefix ex: <http://ex.org/>. [t: (?a ex:p ?b) (?b ex:p ?c) (?c ex:p ?d) -> (?a ex:p ?d)] [t2: (?a ex:p ?b) (?b ex:p ?c) -> (?a ex:p ?c)]",
    ] {
        let s = store(&ttl);
        let r = run(&s, Profile::Rules(rules.into()));
        assert_eq!(r.inferred as usize, closure.len() - edges.len(), "{rules}");
        let total = select(&s, "SELECT (COUNT(*) AS ?n) WHERE { ?a ex:p ?b }", true);
        assert_eq!(total, [closure.len().to_string()]);
    }
}

/// A rule that moves an object into subject position must not turn an RDF 1.2 triple
/// term into a subject, in `infer` (dry run) or `materialize` alike.
#[test]
fn triple_terms_never_become_subjects() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"<http://ex.org/a> <http://ex.org/p> <<( <http://ex.org/x> <http://ex.org/y> <http://ex.org/z> )>> .
          <http://ex.org/b> <http://ex.org/p> <http://ex.org/c> ."
            .to_vec(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let rules = Profile::Rules(
        "[flip: (?s <http://ex.org/p> ?o) -> (?o <http://ex.org/q> ?s)]".to_string(),
    );
    let (derived, _) =
        sparkles_reasoner::infer(s.snapshot(), &rules, &ReasonOptions::default()).unwrap();
    let subjects: Vec<String> = derived.iter().map(|t| t.subject.to_string()).collect();
    assert_eq!(subjects, ["<http://ex.org/c>"]);
    let report = run(&s, rules);
    assert_eq!(report.inferred, 1);
    assert!(ask(
        &s,
        "<http://ex.org/c> <http://ex.org/q> <http://ex.org/b>"
    ));
    // everything materialized is exportable RDF
    let mut buf = Vec::new();
    assert_eq!(s.dump_nquads(&mut buf).unwrap(), s.snapshot().len());
}
