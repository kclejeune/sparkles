//! Explanations over a materialized closure (spec U10 §3.3).

use oxrdf::{GraphName, NamedNode, Quad, Term};
use sparkles_core::id::Id;
use sparkles_core::index::Perm;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::explain::{
    Derivation, DerivationKind, ExplainBudgetExceeded, ExplainInput, ExplainOptions, Explained,
    explain,
};
use sparkles_reasoner::{
    Extras, GraphRef, INFERRED_GRAPH, Incremental, Inputs, Profile, ReasonOptions,
    materialize_incremental,
};
use std::collections::BTreeSet;

const EX: &str = "http://ex.org/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const HR: &str = "http://ex.org/hr";
const ONTO: &str = "http://ex.org/ontology";

const TRIG: &str = "@prefix ex: <http://ex.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:hr { ex:alice ex:manages ex:team7 . }
ex:ontology {
  ex:Manager rdfs:subClassOf ex:Employee .
  ex:manages rdfs:domain ex:Manager .
}
";

fn store(trig: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        trig.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    s
}

fn inputs(graphs: &[&str]) -> Inputs {
    Inputs {
        data_graphs: graphs
            .iter()
            .map(|g| match *g {
                "default" => GraphRef::Default,
                g => GraphRef::Named(g.to_string()),
            })
            .collect(),
        ..Default::default()
    }
}

fn reason(s: &Store, p: &Profile, graphs: &[&str], since: Option<u64>) -> u64 {
    let opts = ReasonOptions {
        inputs: inputs(graphs),
        ..Default::default()
    };
    let r = materialize_incremental(
        s,
        p,
        &Extras::default(),
        Incremental { since, cache: None },
        &opts,
    )
    .unwrap_or_else(|e| panic!("{e:#}"));
    r.receipt.map(|r| r.commit.seq).unwrap_or(0)
}

fn n(iri: &str) -> Term {
    Term::NamedNode(NamedNode::new(iri).unwrap())
}

fn ex(l: &str) -> Term {
    n(&format!("{EX}{l}"))
}

fn ids(s: &Store, graphs: &[&str]) -> Vec<Id> {
    let snap = s.snapshot();
    graphs
        .iter()
        .filter_map(|g| match *g {
            "default" => Some(Id::DEFAULT_GRAPH),
            g => snap.lookup_iri(g),
        })
        .collect()
}

fn why(
    s: &Store,
    p: &Profile,
    readable: &[&str],
    t: [Term; 3],
    opts: &ExplainOptions,
) -> anyhow::Result<Explained> {
    let snap = s.snapshot();
    let inputs = ids(s, readable);
    let inferred = snap.lookup_iri(INFERRED_GRAPH);
    explain(
        &ExplainInput {
            snap,
            profile: p,
            extras: &Extras::default(),
            inputs: &inputs,
            inferred,
        },
        &t,
        opts,
    )
}

fn trees(e: Explained) -> (Vec<Derivation>, bool) {
    match e {
        Explained::Found { trees, more } => (trees, more),
        Explained::NotFound => panic!("no derivation"),
    }
}

/// Checks that no fact is among its own premises, that a shared node's id was given
/// before, and that every derived node's premises are shallower; returns the facts.
fn well_founded(d: &Derivation, path: &mut Vec<[Term; 3]>, seen: &mut BTreeSet<u32>) {
    assert!(
        !path.contains(&d.triple),
        "{:?} is among its own premises",
        d.triple
    );
    if d.shared {
        assert!(
            seen.contains(&d.id),
            "shared node {} not given before",
            d.id
        );
        return;
    }
    seen.insert(d.id);
    path.push(d.triple.clone());
    for p in &d.premises {
        if d.kind == DerivationKind::Derived && p.kind != DerivationKind::Ref {
            assert!(
                p.depth < d.depth,
                "premise depth {} of {}",
                p.depth,
                d.depth
            );
        }
        well_founded(p, path, seen);
    }
    path.pop();
}

fn rule(d: &Derivation) -> &str {
    &d.rule.as_ref().unwrap().name
}

#[test]
fn a_subclass_derivation_names_its_rules_and_graphs() {
    let s = store(TRIG);
    reason(&s, &Profile::Rdfs, &[HR, ONTO], None);
    let (ts, _) = trees(
        why(
            &s,
            &Profile::Rdfs,
            &[HR, ONTO],
            [ex("alice"), n(&format!("{RDF}type")), ex("Employee")],
            &ExplainOptions::default(),
        )
        .unwrap(),
    );
    let root = &ts[0];
    assert_eq!(root.kind, DerivationKind::Derived);
    assert_eq!(rule(root), "rdfs9");
    assert_eq!(root.rule.as_ref().unwrap().profile, "rdfs");
    assert_eq!(root.rule.as_ref().unwrap().owl2rl, None);
    assert_eq!(root.depth, 2);
    let manager = root
        .premises
        .iter()
        .find(|p| p.triple[2] == ex("Manager"))
        .expect("alice a Manager");
    assert_eq!(manager.kind, DerivationKind::Derived);
    assert_eq!(rule(manager), "rdfs2");
    let graphs: BTreeSet<(String, String)> = manager
        .premises
        .iter()
        .map(|p| {
            assert_eq!(p.kind, DerivationKind::Asserted);
            (p.triple[1].to_string(), p.graph.clone().unwrap())
        })
        .collect();
    assert_eq!(
        graphs,
        BTreeSet::from([
            (format!("<{EX}manages>"), HR.to_string()),
            (format!("<{RDFS}domain>"), ONTO.to_string()),
        ])
    );
    let sco = root
        .premises
        .iter()
        .find(|p| p.triple[1] == n(&format!("{RDFS}subClassOf")))
        .unwrap();
    assert_eq!(sco.kind, DerivationKind::Asserted);
    assert_eq!(sco.graph, Some(ONTO.to_string()));
    well_founded(root, &mut Vec::new(), &mut BTreeSet::new());
}

#[test]
fn a_hidden_premise_answers_as_not_entailed() {
    let s = store(TRIG);
    reason(&s, &Profile::Rdfs, &[HR, ONTO], None);
    let t = [ex("alice"), n(&format!("{RDF}type")), ex("Employee")];
    let hidden = why(&s, &Profile::Rdfs, &[HR], t, &ExplainOptions::default()).unwrap();
    assert!(matches!(hidden, Explained::NotFound));
    let absent = why(
        &s,
        &Profile::Rdfs,
        &[HR, ONTO],
        [ex("bob"), n(&format!("{RDF}type")), ex("Employee")],
        &ExplainOptions::default(),
    )
    .unwrap();
    assert!(matches!(absent, Explained::NotFound));
}

#[test]
fn an_asserted_triple_is_its_own_leaf() {
    let s = store(TRIG);
    reason(&s, &Profile::Rdfs, &[HR, ONTO], None);
    let (ts, _) = trees(
        why(
            &s,
            &Profile::Rdfs,
            &[HR, ONTO],
            [ex("alice"), ex("manages"), ex("team7")],
            &ExplainOptions::default(),
        )
        .unwrap(),
    );
    assert_eq!(ts[0].kind, DerivationKind::Asserted);
    assert_eq!(ts[0].graph, Some(HR.to_string()));
}

#[test]
fn an_axiom_says_so() {
    let s = store(TRIG);
    reason(&s, &Profile::Rdfs, &[HR, ONTO], None);
    let (ts, _) = trees(
        why(
            &s,
            &Profile::Rdfs,
            &[HR, ONTO],
            [
                n(&format!("{RDFS}Resource")),
                n(&format!("{RDF}type")),
                n(&format!("{RDFS}Class")),
            ],
            &ExplainOptions::default(),
        )
        .unwrap(),
    );
    assert_eq!(ts[0].kind, DerivationKind::Axiom);
    assert!(ts[0].premises.is_empty());
    assert_eq!(ts[0].depth, 1);
}

#[test]
fn a_tree_past_the_depth_limit_is_cut_as_a_ref() {
    let s = store(TRIG);
    reason(&s, &Profile::Rdfs, &[HR, ONTO], None);
    let (ts, _) = trees(
        why(
            &s,
            &Profile::Rdfs,
            &[HR, ONTO],
            [ex("alice"), n(&format!("{RDF}type")), ex("Employee")],
            &ExplainOptions {
                max_depth: 1,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let manager = ts[0]
        .premises
        .iter()
        .find(|p| p.triple[2] == ex("Manager"))
        .unwrap();
    assert_eq!(manager.kind, DerivationKind::Ref);
    assert!(manager.premises.is_empty());
    assert!(manager.rule.is_none());
}

#[test]
fn the_instance_budget_stops_the_search() {
    let s = store(TRIG);
    reason(&s, &Profile::Rdfs, &[HR, ONTO], None);
    let e = why(
        &s,
        &Profile::Rdfs,
        &[HR, ONTO],
        [ex("alice"), n(&format!("{RDF}type")), ex("Employee")],
        &ExplainOptions {
            max_instances: 1,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(e.downcast_ref::<ExplainBudgetExceeded>().is_some(), "{e:#}");
    let e = why(
        &s,
        &Profile::Rdfs,
        &[HR, ONTO],
        [ex("alice"), n(&format!("{RDF}type")), ex("Employee")],
        &ExplainOptions {
            deadline: Some(std::time::Instant::now()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(e.downcast_ref::<ExplainBudgetExceeded>().is_some(), "{e:#}");
}

#[test]
fn same_as_is_explained_without_a_cycle() {
    let s = store(
        "@prefix ex: <http://ex.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
         ex:a owl:sameAs ex:b . ex:b ex:p ex:c .",
    );
    reason(&s, &Profile::OwlRl, &["default"], None);
    let same = n(&format!("{OWL}sameAs"));
    let (ts, _) = trees(
        why(
            &s,
            &Profile::OwlRl,
            &["default"],
            [ex("b"), same.clone(), ex("a")],
            &ExplainOptions {
                max_trees: 5,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    assert_eq!(rule(&ts[0]), "eq-sym");
    assert_eq!(
        ts[0].rule.as_ref().unwrap().owl2rl.as_deref(),
        Some("eq-sym")
    );
    assert_eq!(ts[0].premises.len(), 1);
    assert_eq!(ts[0].premises[0].kind, DerivationKind::Asserted);
    let mut seen = BTreeSet::new();
    for t in &ts {
        well_founded(t, &mut Vec::new(), &mut seen);
    }
    // the replaced subject comes from the asserted triple and the sameAs fact
    let (ts, _) = trees(
        why(
            &s,
            &Profile::OwlRl,
            &["default"],
            [ex("a"), ex("p"), ex("c")],
            &ExplainOptions::default(),
        )
        .unwrap(),
    );
    assert_eq!(rule(&ts[0]), "eq-rep-s");
    well_founded(&ts[0], &mut Vec::new(), &mut BTreeSet::new());
}

const SHARED_RULES: &str = "@prefix ex: <http://ex.org/> .
[r1: (?x ex:p ?y) -> (?x ex:q ?y)]
[r3: (?x ex:q ?y) -> (?x ex:s ?y)]
[r2: (?x ex:q ?y) (?x ex:s ?y) -> (?x ex:r ?y)]
";

#[test]
fn a_repeated_premise_is_shared_by_id() {
    let s = store("@prefix ex: <http://ex.org/> . ex:a ex:p ex:b .");
    let p = Profile::Rules(SHARED_RULES.into());
    reason(&s, &p, &["default"], None);
    let (ts, more) = trees(
        why(
            &s,
            &p,
            &["default"],
            [ex("a"), ex("r"), ex("b")],
            &ExplainOptions::default(),
        )
        .unwrap(),
    );
    assert!(!more);
    let root = &ts[0];
    assert_eq!(rule(root), "r2");
    assert_eq!(root.rule.as_ref().unwrap().profile, "rules");
    assert_eq!(root.premises.len(), 2);
    let q = &root.premises[0];
    assert_eq!(q.triple[1], ex("q"));
    assert!(!q.shared);
    let s_node = &root.premises[1];
    assert_eq!(rule(s_node), "r3");
    let q2 = &s_node.premises[0];
    assert!(q2.shared);
    assert_eq!(q2.id, q.id);
    assert!(q2.premises.is_empty());
    well_founded(root, &mut Vec::new(), &mut BTreeSet::new());
}

#[test]
fn next_derivations_have_other_top_rules() {
    let s = store(
        "@prefix ex: <http://ex.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:alice ex:manages ex:team7 . ex:manages rdfs:domain ex:Manager .
         ex:bob ex:reportsTo ex:alice . ex:reportsTo rdfs:range ex:Manager .
         ex:Lead rdfs:subClassOf ex:Manager . ex:alice a ex:Lead .",
    );
    reason(&s, &Profile::Rdfs, &["default"], None);
    let (ts, more) = trees(
        why(
            &s,
            &Profile::Rdfs,
            &["default"],
            [ex("alice"), n(&format!("{RDF}type")), ex("Manager")],
            &ExplainOptions {
                max_trees: 5,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let rules: BTreeSet<&str> = ts.iter().map(rule).collect();
    assert_eq!(rules, BTreeSet::from(["rdfs2", "rdfs3", "rdfs9"]));
    assert_eq!(ts.len(), 3);
    assert!(!more);
    let (one, more) = trees(
        why(
            &s,
            &Profile::Rdfs,
            &["default"],
            [ex("alice"), n(&format!("{RDF}type")), ex("Manager")],
            &ExplainOptions::default(),
        )
        .unwrap(),
    );
    assert_eq!(one.len(), 1);
    assert!(more);
}

/// Every triple of the inferred graph has a well-founded derivation, after a full run and
/// after incremental runs that delete support.
#[test]
fn every_inferred_triple_is_explained_after_incremental_deletions() {
    let s = store(
        "@prefix ex: <http://ex.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         @prefix owl: <http://www.w3.org/2002/07/owl#> .
         ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:C . ex:C rdfs:subClassOf ex:A .
         ex:x a ex:A . ex:y a ex:B . ex:p rdfs:domain ex:C . ex:z ex:p ex:w .
         ex:q owl:inverseOf ex:p . ex:w ex:q ex:v . ex:t a owl:TransitiveProperty .
         ex:m ex:t ex:n . ex:n ex:t ex:o . ex:o ex:t ex:m .
         ex:x owl:sameAs ex:x2 .",
    );
    let p = Profile::OwlRl;
    let mut since = reason(&s, &p, &["default"], None);
    let deletions = [
        (ex("x"), n(&format!("{RDF}type")), ex("A")),
        (ex("B"), n(&format!("{RDFS}subClassOf")), ex("C")),
        (ex("n"), ex("t"), ex("o")),
    ];
    for step in 0..=deletions.len() {
        let snap = s.snapshot();
        let g = snap.lookup_iri(INFERRED_GRAPH).unwrap();
        let mut inferred: Vec<[Id; 3]> = Vec::new();
        snap.scan(Perm::Gspo, &[g.0], |c| {
            match c {
                sparkles_core::store::Chunk::Block(b, st, e) => inferred.extend(
                    (st..e).map(|i| [Id(b.cols[1][i]), Id(b.cols[2][i]), Id(b.cols[3][i])]),
                ),
                sparkles_core::store::Chunk::Row(k) => {
                    inferred.push([Id(k[1]), Id(k[2]), Id(k[3])])
                }
            }
            Ok(true)
        })
        .unwrap();
        assert!(!inferred.is_empty());
        for t in inferred {
            let terms = t.map(|i| snap.term(i).unwrap());
            let e = why(
                &s,
                &p,
                &["default"],
                terms.clone(),
                &ExplainOptions::default(),
            )
            .unwrap_or_else(|e| panic!("step {step} {terms:?}: {e:#}"));
            let (ts, _) = match e {
                Explained::Found { trees, more } => (trees, more),
                Explained::NotFound => panic!("step {step}: {terms:?} has no derivation"),
            };
            let mut seen = BTreeSet::new();
            for d in &ts {
                assert_eq!(d.triple, terms);
                well_founded(d, &mut Vec::new(), &mut seen);
            }
        }
        if step == deletions.len() {
            break;
        }
        let (a, b, c) = &deletions[step];
        let mut txn = s.write();
        let mut labels = Default::default();
        let (Term::NamedNode(a), Term::NamedNode(b)) = (a, b) else {
            unreachable!()
        };
        let q = Quad::new(a.clone(), b.clone(), c.clone(), GraphName::DefaultGraph);
        let q = txn.encode_quad(&q, &mut labels).unwrap();
        txn.delete(q).unwrap();
        txn.commit().unwrap();
        since = reason(&s, &p, &["default"], Some(since));
    }
}

#[test]
fn another_top_rule_never_derives_the_root_from_itself() {
    // q by r1 from p; s by r3 from q; and q again by r4 from s, whose only support is q
    let s = store("@prefix ex: <http://ex.org/> . ex:a ex:p ex:b .");
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/> .
[r1: (?x ex:p ?y) -> (?x ex:q ?y)]
[r3: (?x ex:q ?y) -> (?x ex:s ?y)]
[r4: (?x ex:s ?y) -> (?x ex:q ?y)]
"
        .into(),
    );
    reason(&s, &p, &["default"], None);
    let (ts, _) = trees(
        why(
            &s,
            &p,
            &["default"],
            [ex("a"), ex("q"), ex("b")],
            &ExplainOptions {
                max_trees: 5,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    for t in &ts {
        well_founded(t, &mut Vec::new(), &mut BTreeSet::new());
    }
    assert_eq!(ts.len(), 1, "{ts:#?}");
    assert_eq!(rule(&ts[0]), "r1");
}

#[test]
fn the_deadline_stops_a_rule_whose_candidates_all_fail() {
    // r scans every ex:p fact and rejects each by its builtin, so it emits no instance
    let mut trig = String::from("@prefix ex: <http://ex.org/> . ex:a ex:base ex:b .\n");
    for i in 0..200_000 {
        trig.push_str(&format!("ex:i{i} ex:p {i} .\n"));
    }
    let s = store(&trig);
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/> .
[r: (?s ex:p ?n) greaterThan(?n, 10000000) -> (ex:a ex:t ex:y)]
[r0: (ex:a ex:base ex:b) -> (ex:a ex:t ex:y)]
"
        .into(),
    );
    reason(&s, &p, &["default"], None);
    let started = std::time::Instant::now();
    let e = why(
        &s,
        &p,
        &["default"],
        [ex("a"), ex("t"), ex("y")],
        &ExplainOptions {
            deadline: Some(started + std::time::Duration::from_millis(20)),
            ..Default::default()
        },
    );
    let e = e.expect_err("the search ran past its deadline");
    assert!(e.downcast_ref::<ExplainBudgetExceeded>().is_some(), "{e:#}");
}

#[test]
fn the_memory_limit_bounds_the_search() {
    let mut trig = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..10_000 {
        trig.push_str(&format!("ex:i{i} ex:p ex:o .\n"));
    }
    let s = store(&trig);
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/> .
[r: (?x ex:p ?y) -> (ex:a ex:t ex:y)]
"
        .into(),
    );
    reason(&s, &p, &["default"], None);
    let t = [ex("a"), ex("t"), ex("y")];
    assert!(why(&s, &p, &["default"], t.clone(), &ExplainOptions::default()).is_ok());
    let e = why(
        &s,
        &p,
        &["default"],
        t,
        &ExplainOptions {
            max_memory: Some(65_536),
            ..Default::default()
        },
    );
    assert!(e.is_err(), "10,000 instances fit in 64 KiB: {e:?}");
}

#[test]
fn the_deadline_unwinds_a_nested_join() {
    // r joins two relations of 3,000 rows and rejects every pair, so without unwinding
    // each outer row restarts the inner scan after the deadline
    let mut trig = String::from("@prefix ex: <http://ex.org/> . ex:a ex:base ex:b .\n");
    for i in 0..3_000 {
        trig.push_str(&format!(
            "ex:i{i} ex:p {i} .\nex:j{i} ex:q {} .\n",
            i + 100_000
        ));
    }
    let s = store(&trig);
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/> .
[r: (?a ex:p ?x) (?b ex:q ?y) greaterThan(?x, ?y) -> (ex:a ex:t ex:y)]
[r0: (ex:a ex:base ex:b) -> (ex:a ex:t ex:y)]
"
        .into(),
    );
    reason(&s, &p, &["default"], None);
    let started = std::time::Instant::now();
    let e = why(
        &s,
        &p,
        &["default"],
        [ex("a"), ex("t"), ex("y")],
        &ExplainOptions {
            deadline: Some(started + std::time::Duration::from_millis(20)),
            ..Default::default()
        },
    );
    let took = started.elapsed();
    let e = e.expect_err("the search ran past its deadline");
    assert!(e.downcast_ref::<ExplainBudgetExceeded>().is_some(), "{e:#}");
    assert!(
        took < std::time::Duration::from_millis(500),
        "the search stopped {took:?} after it started, for a deadline of 20 ms"
    );
}

#[test]
fn a_deep_chain_fits_a_memory_limit_its_facts_fit() {
    // 120 rules in a chain derive 120 facts, which need far less than the limit
    let mut rules = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..120 {
        rules.push_str(&format!(
            "[c{i}: (?x ex:p{i} ?y) -> (?x ex:p{} ?y)]\n",
            i + 1
        ));
    }
    let s = store("@prefix ex: <http://ex.org/> . ex:a ex:p0 ex:b .");
    let p = Profile::Rules(rules);
    reason(&s, &p, &["default"], None);
    let (ts, _) = trees(
        why(
            &s,
            &p,
            &["default"],
            [ex("a"), ex("p120"), ex("b")],
            &ExplainOptions {
                max_memory: Some(120_000),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e:#}")),
    );
    assert_eq!(rule(&ts[0]), "c119");
}

#[test]
fn the_memory_limit_counts_the_terms_of_the_answer() {
    // one instance with ten premises, each holding a literal of 100,000 bytes
    let big = "x".repeat(100_000);
    let mut trig = String::from("@prefix ex: <http://ex.org/> .\n");
    let mut body = String::new();
    for i in 0..10 {
        trig.push_str(&format!("ex:a ex:p{i} \"{big}{i}\" .\n"));
        body.push_str(&format!("(ex:a ex:p{i} ?l{i}) "));
    }
    let s = store(&trig);
    let p = Profile::Rules(format!(
        "@prefix ex: <http://ex.org/> .\n[r: {body}-> (ex:a ex:t ex:y)]\n"
    ));
    reason(&s, &p, &["default"], None);
    let t = [ex("a"), ex("t"), ex("y")];
    let (ts, _) = trees(why(&s, &p, &["default"], t.clone(), &ExplainOptions::default()).unwrap());
    assert_eq!(ts[0].premises.len(), 10);
    let e = why(
        &s,
        &p,
        &["default"],
        t,
        &ExplainOptions {
            max_memory: Some(500_000),
            ..Default::default()
        },
    )
    .map(|_| ())
    .expect_err("a million bytes of terms fit in 500,000");
    assert!(e.downcast_ref::<ExplainBudgetExceeded>().is_some(), "{e:#}");
}

#[test]
fn a_fact_cut_at_the_depth_limit_is_not_shared_as_derived() {
    // root from a and b, a from b, b from p: with depth 2, b under a is cut, and b as
    // root's own premise must still show its support
    let s = store("@prefix ex: <http://ex.org/> . ex:x ex:p ex:y .");
    let p = Profile::Rules(
        "@prefix ex: <http://ex.org/> .
[rb: (?x ex:p ?y) -> (?x ex:b ?y)]
[ra: (?x ex:b ?y) -> (?x ex:a ?y)]
[rr: (?x ex:a ?y) (?x ex:b ?y) -> (?x ex:root ?y)]
"
        .into(),
    );
    reason(&s, &p, &["default"], None);
    let (ts, _) = trees(
        why(
            &s,
            &p,
            &["default"],
            [ex("x"), ex("root"), ex("y")],
            &ExplainOptions {
                max_depth: 2,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    fn shared_to_ref(d: &Derivation, refs: &mut BTreeSet<u32>, bad: &mut bool) {
        if d.kind == DerivationKind::Ref {
            refs.insert(d.id);
        }
        if d.shared && refs.contains(&d.id) {
            *bad = true;
        }
        for p in &d.premises {
            shared_to_ref(p, refs, bad);
        }
    }
    let mut bad = false;
    shared_to_ref(&ts[0], &mut BTreeSet::new(), &mut bad);
    assert!(!bad, "a shared node points at a cut one: {:#?}", ts[0]);
    let b = ts[0]
        .premises
        .iter()
        .find(|d| d.triple[1] == ex("b"))
        .unwrap();
    assert!(b.shared || !b.premises.is_empty(), "{b:#?}");
}

#[test]
fn an_asserted_triple_still_answers_to_the_controls() {
    let s = store(TRIG);
    reason(&s, &Profile::Rdfs, &[HR, ONTO], None);
    let t = [ex("alice"), ex("manages"), ex("team7")];
    let e = why(
        &s,
        &Profile::Rdfs,
        &[HR, ONTO],
        t.clone(),
        &ExplainOptions {
            deadline: Some(std::time::Instant::now()),
            ..Default::default()
        },
    )
    .map(|_| ())
    .unwrap_err();
    assert!(e.downcast_ref::<ExplainBudgetExceeded>().is_some(), "{e:#}");
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    assert!(
        why(
            &s,
            &Profile::Rdfs,
            &[HR, ONTO],
            t,
            &ExplainOptions {
                cancel: Some(cancel),
                ..Default::default()
            },
        )
        .is_err()
    );
}
