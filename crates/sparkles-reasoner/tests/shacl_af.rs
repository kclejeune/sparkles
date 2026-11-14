//! The `shacl-af` profile: SHACL Advanced Features rules and SPIN rules (roadmap item
//! 838). TopBraid's rule tests are vendored in `tests/shacl-af/topbraid` (see its
//! README), and the other tests cover the order groups, one pass against a fixed point,
//! the round cap, conditions, blank nodes, SPIN and incremental runs.

use oxrdf::{Graph, NamedNode, Term, Triple};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::shacl_af::ShaclAf;
use sparkles_reasoner::{
    Cache, Extras, GraphRef, INFERRED_GRAPH, Incremental, Method, Profile, ReasonOptions,
    ReasonReport, materialize_incremental, materialize_with,
};
use std::collections::BTreeSet;
use std::path::Path;

const PREFIXES: &str = "@prefix ex: <http://example.org/> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix spin: <http://spinrdf.org/spin#> .
@prefix sp: <http://spinrdf.org/sp#> .
@prefix shnex: <http://www.w3.org/ns/shacl-node-expr#> .
";

fn store_of(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!("{PREFIXES}{ttl}").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn run(s: &Store, cfg: ShaclAf) -> anyhow::Result<ReasonReport> {
    materialize_with(
        s,
        &Profile::ShaclAf(cfg),
        &Extras::default(),
        &ReasonOptions::default(),
    )
}

/// The inferred graph, with `ex:` IRIs as local names.
fn inferred(s: &Store) -> BTreeSet<String> {
    let snap = s.snapshot();
    let mut out = BTreeSet::new();
    let Some(g) = snap.lookup_iri(INFERRED_GRAPH) else {
        return out;
    };
    snap.for_each_quad(|q| {
        if q[3] == g {
            let t = snap.quad_to_terms(q).unwrap();
            let short = |t: String| {
                t.replace("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "a")
                    .replace("http://example.org/", "")
                    .replace(['<', '>'], "")
            };
            out.insert(format!(
                "{} {} {}",
                short(t.subject.to_string()),
                short(t.predicate.to_string()),
                short(t.object.to_string())
            ));
        }
        Ok(())
    })
    .unwrap();
    out
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_triple_rule_infers_from_its_targets() {
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetClass ex:Person ;
           sh:rule [ a sh:TripleRule ; sh:subject sh:this ; sh:predicate rdf:type ;
                     sh:object ex:Agent ] .
         ex:Student rdfs:subClassOf ex:Person .
         ex:a a ex:Person . ex:b a ex:Student . ex:c a ex:Other .",
    );
    let r = run(&s, ShaclAf::default()).unwrap();
    assert_eq!(r.profile, "shacl-af");
    assert_eq!(inferred(&s), set(&["a a Agent", "b a Agent"]));
}

#[test]
fn rules_of_one_order_reach_a_fixed_point_and_one_pass_does_not() {
    // the second rule reads what the first infers, and they share an order
    let data = "ex:S a sh:NodeShape ; sh:targetClass ex:A ;
           sh:rule [ a sh:TripleRule ; sh:subject sh:this ; sh:predicate rdf:type ;
                     sh:object ex:B ] .
         ex:T a sh:NodeShape ; sh:targetClass ex:B ;
           sh:rule [ a sh:TripleRule ; sh:subject sh:this ; sh:predicate ex:flag ;
                     sh:object true ] .
         ex:x a ex:A .";
    let s = store_of(data);
    let r = run(&s, ShaclAf::default()).unwrap();
    assert!(
        inferred(&s).iter().any(|t| t.starts_with("x flag")),
        "{:?}",
        inferred(&s)
    );
    assert_eq!(
        r.iterations, 3,
        "two rounds that add, one that adds nothing"
    );
    let s = store_of(data);
    run(
        &s,
        ShaclAf {
            one_pass: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        !inferred(&s).iter().any(|t| t.starts_with("x flag")),
        "{:?}",
        inferred(&s)
    );
}

#[test]
fn a_later_order_sees_the_facts_of_an_earlier_one_in_one_pass() {
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetClass ex:A ;
           sh:rule [ a sh:TripleRule ; sh:order 2 ; sh:subject sh:this ; sh:predicate ex:flag ;
                     sh:object true ; sh:condition [ sh:class ex:B ] ] ;
           sh:rule [ a sh:TripleRule ; sh:order 1 ; sh:subject sh:this ; sh:predicate rdf:type ;
                     sh:object ex:B ] .
         ex:x a ex:A .",
    );
    run(
        &s,
        ShaclAf {
            one_pass: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        inferred(&s).iter().any(|t| t.starts_with("x flag")),
        "{:?}",
        inferred(&s)
    );
}

#[test]
fn a_condition_that_becomes_true_in_a_later_round_fires_its_rule() {
    // the condition needs ex:width, which a SPARQL rule of the same order infers first
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetClass ex:Box ;
           sh:rule [ a sh:SPARQLRule ;
             sh:construct \"CONSTRUCT { $this <http://example.org/width> ?h } WHERE { $this <http://example.org/height> ?h }\" ] ;
           sh:rule [ a sh:TripleRule ; sh:subject sh:this ; sh:predicate rdf:type ;
                     sh:object ex:Square ;
                     sh:condition [ sh:property [ sh:path ex:width ; sh:minCount 1 ;
                                                  sh:equals ex:height ] ] ] .
         ex:b a ex:Box ; ex:height 3 .",
    );
    run(&s, ShaclAf::default()).unwrap();
    let got = inferred(&s);
    assert!(got.iter().any(|t| t.ends_with("Square")), "{got:?}");
}

#[test]
fn deactivated_rules_and_shapes_do_not_run() {
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetClass ex:A ;
           sh:rule [ a sh:TripleRule ; sh:deactivated true ; sh:subject sh:this ;
                     sh:predicate ex:p ; sh:object 1 ] .
         ex:T a sh:NodeShape ; sh:targetClass ex:A ; sh:deactivated true ;
           sh:rule [ a sh:TripleRule ; sh:subject sh:this ; sh:predicate ex:q ; sh:object 1 ] .
         ex:x a ex:A .",
    );
    run(&s, ShaclAf::default()).unwrap();
    assert!(inferred(&s).is_empty(), "{:?}", inferred(&s));
}

#[test]
fn the_round_cap_ends_a_run_that_keeps_growing_with_an_error_naming_the_order() {
    // each round makes a new literal from the previous one
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetNode ex:x ; sh:rule [ a sh:SPARQLRule ; sh:order 5 ;
           sh:construct \"CONSTRUCT { $this <http://example.org/n> ?m } WHERE { $this <http://example.org/n> ?k BIND(?k + 1 AS ?m) }\" ] .
         ex:x ex:n 0 .",
    );
    let e = run(
        &s,
        ShaclAf {
            max_rounds: 4,
            ..Default::default()
        },
    )
    .unwrap_err();
    let m = format!("{e:#}");
    assert!(m.contains("order 5") && m.contains("4 rounds"), "{m}");
    assert!(inferred(&s).is_empty(), "a failed run writes nothing");
}

#[test]
fn template_blank_nodes_are_stable_so_the_rule_reaches_a_fixed_point() {
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetClass ex:Person ; sh:rule [ a sh:SPARQLRule ;
           sh:construct \"CONSTRUCT { $this <http://example.org/address> _:a . _:a <http://example.org/city> ?c } WHERE { $this <http://example.org/city> ?c }\" ] .
         ex:p a ex:Person ; ex:city \"Paris\" .",
    );
    run(&s, ShaclAf::default()).unwrap();
    let got = inferred(&s);
    assert_eq!(got.len(), 2, "{got:?}");
}

#[test]
fn a_computed_term_is_read_by_a_later_order() {
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetClass ex:R ;
           sh:rule [ a sh:SPARQLRule ; sh:order 1 ;
             sh:construct \"CONSTRUCT { $this <http://example.org/area> ?a } WHERE { $this <http://example.org/w> ?w ; <http://example.org/h> ?h BIND(?w * ?h AS ?a) }\" ] ;
           sh:rule [ a sh:SPARQLRule ; sh:order 2 ;
             sh:construct \"CONSTRUCT { $this a <http://example.org/Big> } WHERE { $this <http://example.org/area> ?a FILTER(?a > 50) }\" ] .
         ex:r a ex:R ; ex:w 7 ; ex:h 8 .",
    );
    run(&s, ShaclAf::default()).unwrap();
    let got = inferred(&s);
    assert!(got.iter().any(|t| t.ends_with("Big")), "{got:?}");
}

#[test]
fn spin_rules_with_their_text_run_as_sparql_rules() {
    let s = store_of(
        "ex:Person spin:rule [ a sp:Construct ;
           sp:text \"CONSTRUCT { ?this a ex:Agent } WHERE { ?this ex:name ?n }\" ] .
         ex:Student rdfs:subClassOf ex:Person .
         ex:a a ex:Student ; ex:name \"A\" . ex:b a ex:Person .",
    );
    let e = run(&s, ShaclAf::default()).unwrap_err();
    assert!(
        format!("{e:#}").contains("ex"),
        "an undeclared prefix fails: {e:#}"
    );
    let s = store_of(
        "ex:Person spin:rule [ a sp:Construct ;
           sp:text \"CONSTRUCT { ?this a <http://example.org/Agent> } WHERE { ?this <http://example.org/name> ?n }\" ] .
         ex:Student rdfs:subClassOf ex:Person .
         ex:a a ex:Student ; ex:name \"A\" . ex:b a ex:Person .",
    );
    run(&s, ShaclAf::default()).unwrap();
    let got = inferred(&s);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got.iter().next().unwrap().starts_with("a "), "{got:?}");
}

#[test]
fn spin_rules_in_rdf_syntax_alone_are_refused() {
    let s = store_of(
        "ex:Person spin:rule [ a sp:Construct ; sp:where () ] .
         ex:a a ex:Person .",
    );
    let e = run(&s, ShaclAf::default()).unwrap_err();
    assert!(format!("{e:#}").contains("sp:text"), "{e:#}");
}

#[test]
fn an_unknown_rule_type_is_refused_by_name() {
    let s =
        store_of("ex:S a sh:NodeShape ; sh:targetNode ex:x ; sh:rule ex:r . ex:r a ex:JSRule .");
    let e = run(&s, ShaclAf::default()).unwrap_err();
    let m = format!("{e:#}");
    assert!(m.contains("ex.org/r") || m.contains("example.org/r"), "{m}");
    assert!(m.contains("unsupported rule type"), "{m}");
}

#[test]
fn the_profile_text_round_trips_its_options() {
    let p: Profile = "shacl-af?shapes=urn:x:shapes&mode=one-pass&max-rounds=7"
        .parse()
        .unwrap();
    let Profile::ShaclAf(c) = &p else {
        panic!("{p:?}")
    };
    assert_eq!(c.shapes, vec!["urn:x:shapes".to_string()]);
    assert!(c.one_pass);
    assert_eq!(c.max_rounds, 7);
    assert_eq!(
        c.text(),
        "shacl-af?shapes=urn:x:shapes&mode=one-pass&max-rounds=7"
    );
    assert_eq!(
        "shacl-af".parse::<Profile>().unwrap(),
        Profile::ShaclAf(ShaclAf::default())
    );
    assert!("shacl-af?mode=twice".parse::<Profile>().is_err());
}

#[test]
fn shapes_can_come_from_their_own_graph() {
    let s = store_of("ex:x a ex:A .");
    s.load(&[Source::from_bytes(
        format!(
            "{PREFIXES}ex:S a sh:NodeShape ; sh:targetClass ex:A ;
               sh:rule [ a sh:TripleRule ; sh:subject sh:this ; sh:predicate ex:p ; sh:object 1 ] ."
        )
        .into_bytes(),
        RdfFormat::Turtle,
        Some(NamedNode::new("urn:x:shapes").unwrap()),
    )])
    .unwrap();
    run(
        &s,
        ShaclAf {
            shapes: vec!["urn:x:shapes".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(inferred(&s).len(), 1, "{:?}", inferred(&s));
}

#[test]
fn an_incremental_run_materializes_in_full_and_says_why() {
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetClass ex:A ;
           sh:rule [ a sh:TripleRule ; sh:subject sh:this ; sh:predicate ex:p ; sh:object 1 ] .
         ex:x a ex:A .",
    );
    let profile = Profile::ShaclAf(ShaclAf::default());
    let cache = Cache::default();
    let opts = ReasonOptions::default();
    let first = materialize_incremental(
        &s,
        &profile,
        &Extras::default(),
        Incremental {
            since: None,
            cache: Some(&cache),
        },
        &opts,
    )
    .unwrap();
    s.load(&[Source::from_bytes(
        format!("{PREFIXES}ex:y a ex:A .").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let since = first.receipt.as_ref().map(|r| r.commit.seq);
    let second = materialize_incremental(
        &s,
        &profile,
        &Extras::default(),
        Incremental {
            since,
            cache: Some(&cache),
        },
        &opts,
    )
    .unwrap();
    assert_eq!(second.method, Method::Full);
    assert!(
        second
            .fallback
            .as_deref()
            .is_some_and(|f| f.contains("958")),
        "{:?}",
        second.fallback
    );
    assert_eq!(inferred(&s).len(), 2, "{:?}", inferred(&s));
}

#[test]
fn other_entry_points_refuse_the_profile_cleanly() {
    let s = store_of("ex:x a ex:A .");
    let p = Profile::ShaclAf(ShaclAf::default());
    let e = sparkles_reasoner::infer(s.snapshot(), &p, &ReasonOptions::default()).unwrap_err();
    assert!(format!("{e:#}").contains("materialization"), "{e:#}");
    assert!(p.rules().is_err());
}

// ------------------------------------------------------------------ TopBraid tests ----

const DASH: &str = "http://datashapes.org/dash#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

fn read_ttl(path: &Path) -> Graph {
    let text = std::fs::read(path).unwrap();
    let (quads, _) =
        sparkles_core::io::parse_to_vec(&Source::from_bytes(text, RdfFormat::Turtle, None))
            .unwrap();
    let mut g = Graph::new();
    for q in quads {
        g.insert(&Triple::new(q.subject, q.predicate, q.object));
    }
    g
}

/// The expected inferences of a `dash:InferencingTestCase`.
fn expected(g: &Graph) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let p = |l: &str| NamedNode::new_unchecked(format!("{RDF}{l}"));
    for t in g.triples_for_predicate(&NamedNode::new_unchecked(format!("{DASH}expectedResult"))) {
        let r: oxrdf::NamedOrBlankNode = match t.object.into_owned() {
            Term::BlankNode(b) => b.into(),
            Term::NamedNode(n) => n.into(),
            _ => continue,
        };
        let one = |pred: &NamedNode| {
            g.object_for_subject_predicate(&r, pred)
                .unwrap()
                .into_owned()
        };
        out.insert(format!(
            "{} {} {}",
            one(&p("subject")),
            one(&p("predicate")),
            one(&p("object"))
        ));
    }
    out
}

fn topbraid(case: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/shacl-af/topbraid");
    let file = dir.join(case);
    let g = read_ttl(&file);
    let s = Store::in_memory(StoreOptions::default());
    let mut sources = vec![Source::from_path(&file, None).unwrap()];
    // an import of a sibling file of the suite, which the store does not fetch
    if std::fs::read_to_string(&file)
        .unwrap()
        .contains("owl:imports <http://datashapes.org/shasf/tests/rules/triple/person>")
    {
        sources.push(Source::from_path(&dir.join("triple/person.ttl"), None).unwrap());
    }
    s.load(&sources).unwrap();
    run(&s, ShaclAf::default()).unwrap_or_else(|e| panic!("{case}: {e:#}"));
    let snap = s.snapshot();
    let gid = snap.lookup_iri(INFERRED_GRAPH).expect("inferences");
    let mut got = BTreeSet::new();
    snap.for_each_quad(|q| {
        if q[3] == gid {
            let t = snap.quad_to_terms(q).unwrap();
            got.insert(format!("{} {} {}", t.subject, t.predicate, t.object));
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(got, expected(&g), "{case}");
}

#[test]
fn topbraid_triple_rectangle() {
    topbraid("triple/rectangle.test.ttl");
}

#[test]
fn topbraid_triple_square() {
    topbraid("triple/square.test.ttl");
}

#[test]
fn topbraid_triple_functions_permutations() {
    topbraid("triple/functions-permutations.test.ttl");
}

#[test]
fn topbraid_triple_person2schema() {
    topbraid("triple/person2schema.test.ttl");
}

#[test]
fn topbraid_triple_schema2person() {
    topbraid("triple/schema2person.test.ttl");
}

#[test]
fn topbraid_sparql_rectangle() {
    topbraid("sparql/rectangle.test.ttl");
}

#[test]
fn topbraid_sparql_classify_square() {
    topbraid("sparql/classify-square.test.ttl");
}

#[test]
fn a_failed_run_leaves_no_computed_terms_in_the_vocabulary() {
    // every round makes a new IRI, so the run passes its round cap and fails
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetNode ex:x ; sh:rule [ a sh:SPARQLRule ;
           sh:construct \"CONSTRUCT { $this <http://example.org/n> ?m . $this <http://example.org/link> ?i } WHERE { $this <http://example.org/n> ?k BIND(?k + 1 AS ?m) BIND(IRI(CONCAT('http://example.org/generated-', STR(?m))) AS ?i) }\" ] .
         ex:x ex:n 0 .",
    );
    let e = run(
        &s,
        ShaclAf {
            max_rounds: 3,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(format!("{e:#}").contains("3 rounds"), "{e:#}");
    // a later commit's snapshot reads the whole vocabulary of its generation
    s.load(&[Source::from_bytes(
        format!("{PREFIXES}ex:y ex:p ex:z .").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let snap = s.snapshot();
    for i in 1..=3 {
        let iri = format!("http://example.org/generated-{i}");
        assert!(
            snap.lookup_iri(&iri).is_none(),
            "{iri} stayed in the vocabulary"
        );
    }
    // and a run that succeeds keeps them
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetNode ex:x ; sh:rule [ a sh:SPARQLRule ;
           sh:construct \"CONSTRUCT { $this <http://example.org/link> ?i } WHERE { $this <http://example.org/n> ?k BIND(IRI(CONCAT('http://example.org/generated-', STR(?k))) AS ?i) }\" ] .
         ex:x ex:n 0 .",
    );
    run(&s, ShaclAf::default()).unwrap();
    assert!(
        s.snapshot()
            .lookup_iri("http://example.org/generated-0")
            .is_some()
    );
}

#[test]
fn the_report_gives_the_round_cap_and_each_groups_rounds() {
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetClass ex:A ;
           sh:rule [ a sh:TripleRule ; sh:order 1 ; sh:subject sh:this ; sh:predicate rdf:type ;
                     sh:object ex:B ] ;
           sh:rule [ a sh:TripleRule ; sh:order 2 ; sh:subject sh:this ; sh:predicate ex:p ;
                     sh:object 1 ] .
         ex:x a ex:A .",
    );
    let r = run(
        &s,
        ShaclAf {
            max_rounds: 9,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.max_rounds, Some(9));
    let groups: Vec<(f64, usize, usize)> = r
        .rule_groups
        .iter()
        .map(|g| (g.order, g.rules, g.rounds))
        .collect();
    assert_eq!(groups, vec![(1.0, 1, 2), (2.0, 1, 2)]);
}

#[test]
fn fresh_blank_nodes_of_two_executions_stay_apart() {
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetNode ex:x, ex:y ; sh:rule [ a sh:SPARQLRule ;
           sh:construct \"CONSTRUCT { $this <http://example.org/q> ?b } WHERE { BIND(BNODE() AS ?b) }\" ] .",
    );
    run(
        &s,
        ShaclAf {
            one_pass: true,
            ..Default::default()
        },
    )
    .unwrap();
    let got = inferred(&s);
    let objects: BTreeSet<&str> = got.iter().map(|t| t.rsplit(' ').next().unwrap()).collect();
    assert_eq!(
        objects.len(),
        2,
        "each focus node gets its own node: {got:?}"
    );
}

#[test]
fn fresh_blank_nodes_of_two_function_calls_stay_apart() {
    let s = store_of(
        "ex:fresh a sh:SPARQLFunction ; sh:parameter [ sh:path ex:arg ] ;
           sh:select \"SELECT (BNODE() AS ?result) WHERE { }\" .
         ex:S a sh:NodeShape ; sh:targetNode ex:x, ex:y ; sh:rule [ a sh:TripleRule ;
           sh:subject sh:this ; sh:predicate ex:q ; sh:object [ ex:fresh ( sh:this ) ] ] .",
    );
    run(
        &s,
        ShaclAf {
            one_pass: true,
            ..Default::default()
        },
    )
    .unwrap();
    let got = inferred(&s);
    let objects: BTreeSet<&str> = got.iter().map(|t| t.rsplit(' ').next().unwrap()).collect();
    assert_eq!(objects.len(), 2, "each call gives its own node: {got:?}");
}

#[test]
fn a_variable_named_with_an_underscore_is_read() {
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetNode ex:x ; sh:rule [ a sh:SPARQLRule ;
           sh:construct \"CONSTRUCT { $this <http://example.org/q> ?_v } WHERE { $this <http://example.org/v> ?_v }\" ] .
         ex:x ex:v 1 .",
    );
    run(&s, ShaclAf::default()).unwrap();
    assert_eq!(
        inferred(&s),
        set(&["x q \"1\"^^http://www.w3.org/2001/XMLSchema#integer"])
    );
}

#[test]
fn a_path_from_several_nodes_gives_the_values_of_each() {
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetNode ex:x ; sh:rule [ a sh:TripleRule ;
           sh:subject sh:this ; sh:predicate ex:friendName ;
           sh:object [ sh:path ex:name ; sh:nodes [ sh:path ex:friend ] ] ] .
         ex:x ex:friend ex:a, ex:b . ex:a ex:name \"A\" . ex:b ex:name \"B\" .",
    );
    run(&s, ShaclAf::default()).unwrap();
    assert_eq!(
        inferred(&s),
        set(&["x friendName \"A\"", "x friendName \"B\""])
    );
}

#[test]
fn a_shacl_1_2_expression_in_a_rule_is_not_a_cycle() {
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetNode ex:x ; sh:rule [ a sh:TripleRule ;
           sh:subject sh:this ; sh:predicate ex:friends ;
           sh:object [ shnex:count [ shnex:pathValues ex:friend ] ] ] .
         ex:x ex:friend ex:a, ex:b .",
    );
    run(&s, ShaclAf::default()).unwrap();
    assert_eq!(
        inferred(&s),
        set(&["x friends \"2\"^^http://www.w3.org/2001/XMLSchema#integer"])
    );
}

#[test]
fn an_optional_function_argument_may_be_empty() {
    let s = store_of(
        "ex:plus a sh:SPARQLFunction ;
           sh:parameter [ sh:path ex:a ] ; sh:parameter [ sh:path ex:b ; sh:optional true ] ;
           sh:select \"SELECT ($a + COALESCE($b, 0) AS ?result) WHERE { }\" .
         ex:S a sh:NodeShape ; sh:targetNode ex:x ; sh:rule [ a sh:TripleRule ;
           sh:subject sh:this ; sh:predicate ex:sum ;
           sh:object [ ex:plus ( [ sh:path ex:v ] [ sh:path ex:missing ] ) ] ] .
         ex:x ex:v 2 .",
    );
    run(&s, ShaclAf::default()).unwrap();
    assert_eq!(
        inferred(&s),
        set(&["x sum \"2\"^^http://www.w3.org/2001/XMLSchema#integer"])
    );
}

#[test]
fn a_function_call_gives_its_first_solution_only() {
    let s = store_of(
        "ex:least a sh:SPARQLFunction ; sh:parameter [ sh:path ex:arg ] ;
           sh:select \"SELECT ?result WHERE { $arg <http://example.org/v> ?result } ORDER BY ?result\" .
         ex:S a sh:NodeShape ; sh:targetNode ex:x ; sh:rule [ a sh:TripleRule ;
           sh:subject sh:this ; sh:predicate ex:least ;
           sh:object [ ex:least ( sh:this ) ] ] .
         ex:x ex:v 3, 1, 2 .",
    );
    run(&s, ShaclAf::default()).unwrap();
    assert_eq!(
        inferred(&s),
        set(&["x least \"1\"^^http://www.w3.org/2001/XMLSchema#integer"])
    );
}

#[test]
fn an_unbound_variable_and_an_empty_string_name_different_blank_nodes() {
    let s = store_of(
        "ex:S a sh:NodeShape ; sh:targetNode ex:x ; sh:rule [ a sh:SPARQLRule ;
           sh:construct \"CONSTRUCT { $this <http://example.org/q> _:b } WHERE { { BIND('' AS ?o) } UNION { } }\" ] .",
    );
    run(&s, ShaclAf::default()).unwrap();
    let got = inferred(&s);
    assert_eq!(got.len(), 2, "{got:?}");
}

#[test]
fn a_spin_rule_whose_variable_only_starts_with_this_runs_once() {
    // ?thisOne is not ?this, so the rule is global and runs although ex:Thing has no
    // instances
    let s = store_of(
        "ex:Thing spin:rule [ a sp:Construct ;
           sp:text \"CONSTRUCT { ?thisOne a <http://example.org/Flagged> } WHERE { ?thisOne <http://example.org/p> ?o }\" ] .
         ex:a ex:p 1 .",
    );
    run(&s, ShaclAf::default()).unwrap();
    assert_eq!(inferred(&s), set(&["a a Flagged"]));
}

#[test]
fn a_shapes_graph_iri_with_a_query_string_round_trips() {
    let cfg = ShaclAf {
        shapes: vec!["http://example.org/g?a=1&b=50%25".into()],
        ..Default::default()
    };
    let text = cfg.text();
    assert_eq!(text, "shacl-af?shapes=http://example.org/g?a=1%26b=50%2525");
    assert_eq!(text.parse::<Profile>().unwrap(), Profile::ShaclAf(cfg));
    assert!("shacl-af?shapes=urn:x%2".parse::<Profile>().is_err());
}

#[test]
fn the_shapes_graphs_are_watched_with_the_inputs() {
    let s = store_of("ex:x a ex:A .");
    s.load(&[Source::from_bytes(
        format!(
            "{PREFIXES}ex:S a sh:NodeShape ; sh:targetClass ex:A ;
               sh:rule [ a sh:TripleRule ; sh:subject sh:this ; sh:predicate ex:p ; sh:object 1 ] ."
        )
        .into_bytes(),
        RdfFormat::Turtle,
        Some(NamedNode::new("urn:x:shapes").unwrap()),
    )])
    .unwrap();
    let r = run(
        &s,
        ShaclAf {
            shapes: vec!["urn:x:shapes".into()],
            ..Default::default()
        },
    )
    .unwrap();
    let inputs = r.inputs.as_ref().unwrap();
    assert!(
        inputs
            .watched()
            .contains(&GraphRef::Named("urn:x:shapes".into())),
        "{inputs:?}"
    );
    assert!(
        !inputs
            .graphs
            .contains(&GraphRef::Named("urn:x:shapes".into()))
    );
}
