//! Graph algorithms (`SERVICE algo:…`, spec F14): the acceptance examples, parameter
//! errors, access control, write-back through SPARQL Update, and the budgets.

use sparkles_core::access::{Caller, GraphAccess, Graphs, Limits, Protection, Rule, TripleRules};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const PREFIXES: &str = "PREFIX ex: <http://ex/> PREFIX algo: <urn:x-sparkles:algo#> PREFIX spk: <urn:x-sparkles:> \
                        PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ";

fn store_of(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!("@prefix ex: <http://ex/> .\n{ttl}").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn short(t: &Option<oxrdf::Term>) -> String {
    match t {
        None => "-".into(),
        Some(oxrdf::Term::NamedNode(n)) => n
            .as_str()
            .strip_prefix("http://ex/")
            .unwrap_or(n.as_str())
            .to_string(),
        Some(oxrdf::Term::Literal(l)) => l.value().to_string(),
        Some(t) => t.to_string(),
    }
}

fn rows_with(s: &Store, q: &str, opts: &QueryOptions) -> Vec<BTreeMap<String, String>> {
    let r =
        query(s.snapshot(), &format!("{PREFIXES}{q}"), opts).unwrap_or_else(|e| panic!("{q}: {e}"));
    r.rows()
        .iter()
        .map(|row| {
            r.vars
                .iter()
                .zip(row)
                .map(|(v, t)| (v.clone(), short(t)))
                .collect()
        })
        .collect()
}

fn rows(s: &Store, q: &str) -> Vec<BTreeMap<String, String>> {
    rows_with(s, q, &QueryOptions::default())
}

fn error(s: &Store, q: &str) -> String {
    match query(
        s.snapshot(),
        &format!("{PREFIXES}{q}"),
        &QueryOptions::default(),
    ) {
        Ok(_) => panic!("{q}: expected an error"),
        Err(e) => e.to_string(),
    }
}

/// The graph of the acceptance examples: `ex:p` edges a→b, b→c, c→a, c→d, d→e, e→d, f→g,
/// and a node h with a type and no edges.
const A: &str = "
ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:p ex:a . ex:c ex:p ex:d .
ex:d ex:p ex:e . ex:e ex:p ex:d . ex:f ex:p ex:g . ex:h a ex:N .
";

/// A map from node to the value of one output.
fn by_node(rows: &[BTreeMap<String, String>], col: &str) -> BTreeMap<String, String> {
    rows.iter()
        .map(|r| (r["n"].clone(), r[col].clone()))
        .collect()
}

fn groups(m: &BTreeMap<String, String>) -> BTreeSet<Vec<String>> {
    let mut g: BTreeMap<&String, Vec<String>> = BTreeMap::new();
    for (n, c) in m {
        g.entry(c).or_default().push(n.clone());
    }
    g.into_values().collect()
}

fn stats_json(s: &Store, body: &str) -> serde_json::Value {
    let result = rows(s, &format!("SELECT ?s WHERE {{ {body} }}"));
    assert_eq!(result.len(), 1, "{body}: {result:?}");
    serde_json::from_str(&result[0]["s"]).unwrap()
}

const PROJECTION_WCC: &str = "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:node ?n ; algo:component ?c } }";

fn projection_cache_store() -> Store {
    let s = Store::in_memory(StoreOptions {
        result_cache_bytes: 0,
        ..Default::default()
    });
    s.load(&[Source::from_bytes(
        b"@prefix ex: <http://ex/> . ex:a ex:p ex:b . ex:c ex:p ex:d .".to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn projection_cache_read(
    snap: Arc<sparkles_core::store::Snapshot>,
    q: &str,
    opts: &QueryOptions,
) -> (usize, u64) {
    let result = query(snap, &format!("{PREFIXES}{q}"), opts).unwrap();
    let rows = result.rows();
    assert_eq!(rows.len(), 4);
    let components: BTreeSet<_> = rows.iter().map(|r| short(&r[1])).collect();
    let leaf = find(&result.plan, "AlgoCall").expect("algorithm executed");
    (
        components.len(),
        leaf.counters.as_ref().unwrap()["cachedProjections"]
            .as_u64()
            .unwrap(),
    )
}

fn projection_cache_txn(s: &Store) -> sparkles_core::store::WriteTxn<'_> {
    s.try_write_with(
        sparkles_core::commit::CommitKind::Update,
        sparkles_core::guard::WriteOptions {
            deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(10)),
            ..Default::default()
        },
    )
    .unwrap()
}

fn projection_cache_bridge(
    t: &mut sparkles_core::store::WriteTxn<'_>,
) -> [sparkles_core::id::Id; 4] {
    let mut iri = |name| {
        t.intern(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            format!("http://ex/{name}"),
        )))
        .unwrap()
    };
    [
        iri("b"),
        iri("p"),
        iri("c"),
        sparkles_core::id::Id::DEFAULT_GRAPH,
    ]
}

#[test]
fn projection_cache_warm_committed_does_not_hide_pending_update() {
    let s = projection_cache_store();
    let opts = QueryOptions::default();
    assert_eq!(
        projection_cache_read(s.snapshot(), PROJECTION_WCC, &opts).0,
        2
    );
    assert_eq!(
        projection_cache_read(s.snapshot(), PROJECTION_WCC, &opts),
        (2, 1)
    );
    update(&s, &format!("{PREFIXES} INSERT DATA {{ ex:b ex:p ex:c }} ; INSERT {{ GRAPH ex:out {{ ?n ex:component ?c }} }} WHERE {{ SERVICE algo:wcc {{ [] algo:predicate ex:p ; algo:node ?n ; algo:component ?c }} }}"), &opts).unwrap();
    let result = rows(
        &s,
        "SELECT ?n ?c WHERE { GRAPH ex:out { ?n ex:component ?c } }",
    );
    assert_eq!(result.len(), 4);
    assert_eq!(
        result
            .iter()
            .map(|r| &r["c"])
            .collect::<BTreeSet<_>>()
            .len(),
        1,
        "pending bridge must be visible inside the multi-operation update: {result:?}"
    );
}

#[test]
fn projection_cache_pending_rollback_does_not_poison_committed() {
    let s = projection_cache_store();
    let mut t = projection_cache_txn(&s);
    let bridge = projection_cache_bridge(&mut t);
    t.insert(bridge).unwrap();
    assert_eq!(
        projection_cache_read(Arc::new(t.view()), PROJECTION_WCC, &QueryOptions::default()),
        (1, 0)
    );
    drop(t);
    assert_eq!(
        projection_cache_read(s.snapshot(), PROJECTION_WCC, &QueryOptions::default()).0,
        2,
        "rolled-back bridge must not enter the committed projection cache"
    );
}

#[test]
fn projection_cache_successive_transaction_views_are_fresh() {
    let s = projection_cache_store();
    let mut t = projection_cache_txn(&s);
    let bridge = projection_cache_bridge(&mut t);
    t.insert(bridge).unwrap();
    assert_eq!(
        projection_cache_read(Arc::new(t.view()), PROJECTION_WCC, &QueryOptions::default()),
        (1, 0)
    );
    t.delete(bridge).unwrap();
    assert_eq!(
        projection_cache_read(Arc::new(t.view()), PROJECTION_WCC, &QueryOptions::default()),
        (2, 0)
    );
    drop(t);
}

#[test]
fn projection_cache_masked_readthrough_pending_view_stays_uncached() {
    let s = projection_cache_store();
    let derived = |snap: &sparkles_core::store::Snapshot| {
        Arc::new(sparkles_core::access::triples::apply(
            &snap.without_cache_fill(),
            Vec::new(),
            "r1-empty-mask".into(),
        ))
    };
    assert_eq!(
        projection_cache_read(
            derived(&s.snapshot()),
            PROJECTION_WCC,
            &QueryOptions::default()
        )
        .0,
        2
    );
    assert_eq!(
        projection_cache_read(
            derived(&s.snapshot()),
            PROJECTION_WCC,
            &QueryOptions::default()
        ),
        (2, 1)
    );
    let mut t = projection_cache_txn(&s);
    let bridge = projection_cache_bridge(&mut t);
    t.insert(bridge).unwrap();
    assert_eq!(
        projection_cache_read(derived(&t.view()), PROJECTION_WCC, &QueryOptions::default()),
        (1, 0)
    );
    drop(t);
}

#[test]
fn projection_cache_no_cache_and_disabled_result_cache_are_independent() {
    let s = projection_cache_store();
    let opts = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    for _ in 0..2 {
        assert_eq!(
            projection_cache_read(s.snapshot(), PROJECTION_WCC, &opts),
            (2, 0)
        );
    }
    assert_eq!(
        projection_cache_read(s.snapshot(), PROJECTION_WCC, &QueryOptions::default()).0,
        2
    );
    assert_eq!(
        projection_cache_read(s.snapshot(), PROJECTION_WCC, &QueryOptions::default()),
        (2, 1)
    );
    assert_eq!(
        projection_cache_read(
            Arc::new(s.snapshot().without_cache_fill()),
            PROJECTION_WCC,
            &QueryOptions::default()
        ),
        (2, 1)
    );
}

#[test]
fn projection_cache_graph_scopes_remain_separate() {
    let s = Store::in_memory(StoreOptions {
        result_cache_bytes: 0,
        ..Default::default()
    });
    s.load(&[Source::from_bytes(b"@prefix ex: <http://ex/> . ex:g1 { ex:a ex:p ex:b . ex:c ex:p ex:d } ex:g2 { ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:p ex:d }".to_vec(), RdfFormat::TriG, None)]).unwrap();
    for round in 0..2 {
        for (graph, count) in [("g1", 2), ("g2", 1)] {
            let q = format!(
                "SELECT ?n ?c WHERE {{ GRAPH ex:{graph} {{ SERVICE algo:wcc {{ [] algo:predicate ex:p ; algo:node ?n ; algo:component ?c }} }} }}"
            );
            let (actual, hit) = projection_cache_read(s.snapshot(), &q, &QueryOptions::default());
            assert_eq!(actual, count);
            if round == 1 {
                assert_eq!(hit, 1);
            }
        }
    }
}

#[test]
fn projection_cache_triple_masks_remain_separate() {
    let s = projection_cache_store();
    update(
        &s,
        &format!("{PREFIXES} INSERT DATA {{ ex:b ex:p ex:c ; a ex:Hidden }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let protection = Protection {
        name: "r1-hidden-bridge".into(),
        predicates: None,
        classes: Some(vec!["http://ex/Hidden".into()]),
        subclasses: false,
        graphs: None,
        pattern: None,
        prefixes: Default::default(),
        hide_inferences: false,
    };
    let masked = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(protection),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        ))),
        ..Default::default()
    };
    for round in 0..2 {
        let (count, hit) =
            projection_cache_read(s.snapshot(), PROJECTION_WCC, &QueryOptions::default());
        assert_eq!(count, 1);
        if round == 1 {
            assert_eq!(hit, 1);
        }
        let result = query(
            s.snapshot(),
            &format!("{PREFIXES}{PROJECTION_WCC}"),
            &masked,
        )
        .unwrap();
        let rows = result.rows();
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows.iter()
                .map(|r| short(&r[1]))
                .collect::<BTreeSet<_>>()
                .len(),
            2
        );
        // Access-controlled plans intentionally redact runtime counters. Semantic
        // results still distinguish this mask from the warmed unrestricted view.
        assert!(find(&result.plan, "AlgoCall").unwrap().counters.is_none());
    }
}

#[test]
fn noncacheable_view_with_enabled_result_backend_neither_reads_nor_populates() {
    let s = Store::in_memory(StoreOptions {
        result_cache_bytes: 1 << 20,
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    s.load(&[Source::from_bytes(
        b"@prefix ex: <http://ex/> . ex:a ex:p ex:b . ex:c ex:p ex:d .".to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let q = format!("{PREFIXES}{PROJECTION_WCC}");
    let opts = QueryOptions::default();
    let components = |r: &sparkles_core::sparql::QueryResult| {
        let rows = r.rows();
        assert_eq!(rows.len(), 4);
        rows.iter()
            .map(|row| short(&row[1]))
            .collect::<BTreeSet<_>>()
            .len()
    };
    assert_eq!(components(&query(s.snapshot(), &q, &opts).unwrap()), 2);
    let committed = query(s.snapshot(), &q, &opts).unwrap();
    assert!(committed.plan.cached);
    assert_eq!(components(&committed), 2);
    let mut t = projection_cache_txn(&s);
    let bridge = projection_cache_bridge(&mut t);
    t.insert(bridge).unwrap();
    let mut pending = t.view().without_cache_fill();
    assert!(!pending.cacheable);
    // Exercise the explicit contract even when a derived view retains a usable
    // backend. Ordinary transaction views also keep their disabled backend.
    pending.results = s.snapshot().results.clone();
    assert!(pending.results.enabled());
    for _ in 0..2 {
        let result = query(Arc::new(pending.clone()), &q, &opts).unwrap();
        assert!(!result.plan.cached);
        assert_eq!(components(&result), 1);
    }
    drop(t);
    let committed = query(s.snapshot(), &q, &opts).unwrap();
    assert!(committed.plan.cached);
    assert_eq!(components(&committed), 2);
}

fn empty_pagerank_stats(populated: bool) {
    let s = store_of(if populated { "ex:a ex:p ex:b ." } else { "" });
    let body = "SERVICE algo:pageRank { [] algo:predicate ex:absent ; algo:summary ?s }";
    let j = stats_json(&s, body);
    assert_eq!(j["nodes"], 0);
    assert_eq!(j["edges"], 0);
    assert_eq!(j["iterations"], 0);
    assert_eq!(j["converged"], true);
    for outputs in [
        "algo:node ?n ; algo:score ?r",
        "algo:node ?n ; algo:summary ?s",
    ] {
        assert!(rows(&s, &format!("SELECT * WHERE {{ SERVICE algo:pageRank {{ [] algo:predicate ex:absent ; {outputs} }} }}")).is_empty());
    }
}

#[test]
fn stats_pagerank_on_empty_dataset() {
    empty_pagerank_stats(false);
}

#[test]
fn stats_pagerank_with_absent_predicate() {
    empty_pagerank_stats(true);
}

#[test]
fn stats_empty_communities_and_similarity() {
    let s = store_of("");
    for algorithm in ["louvain", "leiden", "nodeSimilarity"] {
        let j = stats_json(
            &s,
            &format!("SERVICE algo:{algorithm} {{ [] algo:predicate ex:p ; algo:summary ?s }}"),
        );
        assert_eq!(j["nodes"], 0);
        assert_eq!(j["edges"], 0);
        let outputs = if algorithm == "nodeSimilarity" {
            assert_eq!(j["pairs"], 0);
            "algo:node ?n ; algo:otherNode ?o ; algo:similarity ?v"
        } else {
            assert_eq!(j["levels"], 0);
            assert_eq!(j["communities"], 0);
            assert_eq!(j["modularities"], serde_json::json!([]));
            assert!(j["modularity"].is_null());
            "algo:node ?n ; algo:community ?c ; algo:level ?l"
        };
        assert!(rows(&s, &format!("SELECT * WHERE {{ SERVICE algo:{algorithm} {{ [] algo:predicate ex:p ; {outputs} ; algo:summary ?s }} }}")).is_empty());
    }
}

#[test]
fn stats_similarity_with_no_qualifying_pairs() {
    let s = store_of("ex:a ex:p ex:b . ex:c ex:p ex:d .");
    let j = stats_json(
        &s,
        "SERVICE algo:nodeSimilarity { [] algo:predicate ex:p ; algo:direction algo:forward ; algo:summary ?s }",
    );
    assert_eq!(j["nodes"], 4);
    assert_eq!(j["edges"], 2);
    assert_eq!(j["pairs"], 0);
    assert!(rows(&s, "SELECT * WHERE { SERVICE algo:nodeSimilarity { [] algo:predicate ex:p ; algo:direction algo:forward ; algo:node ?n ; algo:otherNode ?o ; algo:summary ?s } }").is_empty());
}

#[test]
fn stats_similarity_counts_all_qualifying_pairs() {
    let s = store_of("ex:a ex:p ex:b . ex:c ex:p ex:b .");
    let j = stats_json(
        &s,
        "SERVICE algo:nodeSimilarity { [] algo:predicate ex:p ; algo:direction algo:forward ; algo:summary ?s }",
    );
    assert_eq!(j["nodes"], 3);
    assert_eq!(j["edges"], 2);
    assert_eq!(j["pairs"], 2);
}

#[test]
fn stats_follow_concrete_graph_selection() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"@prefix ex: <http://ex/> . ex:g1 { ex:a ex:q ex:b } ex:g2 { ex:c ex:q ex:d }".to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    let j = stats_json(
        &s,
        "GRAPH ex:g1 { SERVICE algo:pageRank { [] algo:predicate ex:absent ; algo:summary ?s } }",
    );
    assert_eq!(j["nodes"], 0);
    assert_eq!(j["edges"], 0);
    let q = "SELECT ?g ?s WHERE { GRAPH ?g { SERVICE algo:pageRank { [] algo:predicate ex:absent ; algo:summary ?s } } }";
    let result = rows(&s, q);
    assert_eq!(result.len(), 2);
    assert_eq!(
        result
            .iter()
            .map(|r| r["g"].as_str())
            .collect::<BTreeSet<_>>(),
        ["g1", "g2"].into()
    );
    for row in result {
        let j: serde_json::Value = serde_json::from_str(&row["s"]).unwrap();
        assert_eq!(j["nodes"], 0);
        assert_eq!(j["edges"], 0);
    }
    assert!(rows(&store_of(""), q).is_empty());
    assert!(rows(&s, "SELECT ?s WHERE { GRAPH ex:neverSelected { SERVICE algo:pageRank { [] algo:summary ?s } } }").is_empty());
}

#[test]
fn stats_unmatched_sources_do_not_invent_results() {
    let s = store_of("ex:a ex:p ex:b . ex:b ex:p ex:c .");
    for algorithm in ["pageRank", "bfs", "sssp"] {
        let body = format!(
            "VALUES ?src {{ ex:missing }} SERVICE algo:{algorithm} {{ [] algo:predicate ex:p ; algo:source ?src ; algo:summary ?s }}"
        );
        let j = stats_json(&s, &body);
        assert_eq!(j["nodes"], 3);
        assert_eq!(j["edges"], 2);
        if algorithm == "pageRank" {
            assert_eq!(j["iterations"], 0);
            assert_eq!(j["converged"], true);
        } else {
            assert_eq!(j["reached"], 0);
        }
        assert!(rows(&s, &format!("SELECT * WHERE {{ VALUES ?src {{ ex:missing }} SERVICE algo:{algorithm} {{ [] algo:predicate ex:p ; algo:source ?src ; algo:node ?n ; algo:summary ?s }} }}")).is_empty());
    }
    for source in ["", "algo:source ex:a ;"] {
        let j = stats_json(
            &s,
            &format!(
                "SERVICE algo:pageRank {{ [] algo:predicate ex:p ; {source} algo:summary ?s }}"
            ),
        );
        assert!(j["iterations"].as_u64().unwrap() > 0);
    }
    let result = rows(
        &s,
        "SELECT ?n ?s WHERE { SERVICE algo:pageRank { [] algo:predicate ex:p ; algo:node ?n ; algo:summary ?s } }",
    );
    assert_eq!(result.len(), 3);
    for row in result {
        let j: serde_json::Value = serde_json::from_str(&row["s"]).unwrap();
        assert_eq!(j["nodes"], 3);
        assert_eq!(j["edges"], 2);
    }
    // One algorithm stats row still joins to the original upstream rows.
    assert!(rows(&s, "SELECT ?s WHERE { VALUES ?src { } SERVICE algo:bfs { [] algo:predicate ex:p ; algo:source ?src ; algo:summary ?s } }").is_empty());
    assert_eq!(rows(&s, "SELECT ?s WHERE { VALUES ?src { ex:a ex:a } SERVICE algo:bfs { [] algo:predicate ex:p ; algo:source ?src ; algo:summary ?s } }").len(), 2);
}

#[test]
fn stats_rows_obey_budgets_and_cancellation() {
    let s = store_of("");
    let q = format!(
        "{PREFIXES} SELECT ?s WHERE {{ SERVICE algo:pageRank {{ [] algo:predicate ex:absent ; algo:summary ?s }} }}"
    );
    for opts in [
        QueryOptions {
            max_rows: Some(0),
            ..Default::default()
        },
        QueryOptions {
            max_rows_produced: Some(0),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            query(s.snapshot(), &q, &opts),
            Err(sparkles_core::Error::BudgetExceeded(_))
        ));
    }
    let opts = QueryOptions {
        cancel: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
        ..Default::default()
    };
    assert!(matches!(
        query(s.snapshot(), &q, &opts),
        Err(sparkles_core::Error::Cancelled)
    ));
}

fn set(items: &[&[&str]]) -> BTreeSet<Vec<String>> {
    items
        .iter()
        .map(|g| g.iter().map(|s| s.to_string()).collect())
        .collect()
}

#[test]
fn a1_wcc() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:node ?n ; algo:component ?c } }",
    );
    assert_eq!(
        groups(&by_node(&r, "c")),
        set(&[&["a", "b", "c", "d", "e"], &["f", "g"]])
    );
    // with the class, h is its own component
    let r = rows(
        &s,
        "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:nodeClass ex:N ; \
         algo:node ?n ; algo:component ?c } }",
    );
    assert_eq!(
        by_node(&r, "c").len(),
        1,
        "only the members of the class are nodes: {r:?}"
    );
}

#[test]
fn a2_scc() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n ?c WHERE { SERVICE algo:scc { [] algo:predicate ex:p ; algo:node ?n ; algo:component ?c } }",
    );
    assert_eq!(
        groups(&by_node(&r, "c")),
        set(&[&["a", "b", "c"], &["d", "e"], &["f"], &["g"]])
    );
}

#[test]
fn a3_pagerank() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n ?r WHERE { SERVICE algo:pageRank { [] algo:predicate ex:p ; algo:maxIterations 100 ; \
         algo:node ?n ; algo:score ?r } }",
    );
    let score: BTreeMap<String, f64> = r
        .iter()
        .map(|x| (x["n"].clone(), x["r"].parse().unwrap()))
        .collect();
    let total: f64 = score.values().sum();
    assert!((total - 1.0).abs() < 1e-6, "{total}");
    for hi in ["d", "e"] {
        for lo in ["a", "b", "c"] {
            assert!(score[hi] > score[lo], "{hi} above {lo}: {score:?}");
        }
    }
    // ORDER BY and LIMIT apply as to any rows
    let top = rows(
        &s,
        "SELECT ?n WHERE { SERVICE algo:pageRank { [] algo:predicate ex:p ; algo:node ?n ; algo:score ?r } } \
         ORDER BY DESC(?r) LIMIT 2",
    );
    let names: BTreeSet<&str> = top.iter().map(|r| r["n"].as_str()).collect();
    assert_eq!(names, ["d", "e"].into());
}

#[test]
fn a4_personalized_pagerank() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n ?r WHERE { SERVICE algo:pageRank { [] algo:predicate ex:p ; algo:maxIterations 100 ; \
         algo:source ex:f ; algo:node ?n ; algo:score ?r } }",
    );
    let score: BTreeMap<String, f64> = r
        .iter()
        .map(|x| (x["n"].clone(), x["r"].parse().unwrap()))
        .collect();
    assert!(score["g"] > 0.0);
    for n in ["a", "b", "c", "d", "e"] {
        assert_eq!(score[n], 0.0, "{n}");
    }
}

#[test]
fn a4_source_from_the_group() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n ?d WHERE { VALUES ?s { ex:a } SERVICE algo:bfs { [] algo:predicate ex:p ; \
         algo:source ?s ; algo:node ?n ; algo:depth ?d } }",
    );
    let d = by_node(&r, "d");
    // A7
    assert_eq!(d["a"], "0");
    assert_eq!(d["b"], "1");
    assert_eq!(d["c"], "2");
    assert_eq!(d["d"], "3");
    assert_eq!(d["e"], "4");
    assert!(!d.contains_key("f"), "unreached nodes are left out");
}

#[test]
fn a5_a6_triangles_and_cores() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n ?t WHERE { SERVICE algo:triangleCount { [] algo:predicate ex:p ; algo:node ?n ; algo:triangles ?t } }",
    );
    let t = by_node(&r, "t");
    for n in ["a", "b", "c"] {
        assert_eq!(t[n], "1", "{n}");
    }
    for n in ["d", "e", "f", "g"] {
        assert_eq!(t[n], "0", "{n}");
    }
    let r = rows(
        &s,
        "SELECT ?n ?k WHERE { SERVICE algo:kCore { [] algo:predicate ex:p ; algo:node ?n ; algo:core ?k } }",
    );
    let k = by_node(&r, "k");
    for n in ["a", "b", "c"] {
        assert_eq!(k[n], "2", "{n}");
    }
    for n in ["d", "e", "f", "g"] {
        assert_eq!(k[n], "1", "{n}");
    }
}

#[test]
fn a8_hidden_triples_are_not_edges() {
    // a protection hides the triples whose subject is typed ex:Hidden: c's edges to a and
    // d are gone, so {a b c} and {d e} are two components
    let s = store_of(&format!("{A} ex:c a ex:Hidden ."));
    let mut p = Protection {
        name: "hidden".into(),
        predicates: None,
        classes: Some(vec!["http://ex/Hidden".into()]),
        subclasses: true,
        graphs: None,
        pattern: None,
        prefixes: Default::default(),
        hide_inferences: false,
    };
    p.subclasses = false;
    let opts = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(p),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        ))),
        ..Default::default()
    };
    let q = "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:node ?n ; algo:component ?c } }";
    let c = by_node(&rows_with(&s, q, &opts), "c");
    assert_eq!(
        groups(&c),
        set(&[&["a", "b", "c"], &["d", "e"], &["f", "g"]])
    );
    // without the protection c joins d
    let c = by_node(&rows(&s, q), "c");
    assert_eq!(groups(&c), set(&[&["a", "b", "c", "d", "e"], &["f", "g"]]));
}

#[test]
fn a10_node_similarity() {
    let s =
        store_of("ex:u1 ex:likes ex:x, ex:y . ex:u2 ex:likes ex:x, ex:y . ex:u3 ex:likes ex:x .");
    let r = rows(
        &s,
        "SELECT ?n ?o ?s WHERE { SERVICE algo:nodeSimilarity { [] algo:predicate ex:likes ; \
         algo:node ?n ; algo:otherNode ?o ; algo:similarity ?s } }",
    );
    let got: BTreeSet<(String, String, String)> = r
        .iter()
        .map(|x| (x["n"].clone(), x["o"].clone(), x["s"].clone()))
        .collect();
    assert!(
        got.contains(&("u1".into(), "u2".into(), "1".into()))
            || got
                .iter()
                .any(|g| g.0 == "u1" && g.1 == "u2" && g.2.parse::<f64>().unwrap() == 1.0),
        "{got:?}"
    );
    assert!(
        got.iter().any(|g| g.0 == "u1"
            && g.1 == "u3"
            && (g.2.parse::<f64>().unwrap() - 0.5).abs() < 1e-12)
    );
}

#[test]
fn a13_betweenness_over_the_node_limit() {
    let s = store_of(A);
    sparkles_core::algo::pool::set_exact_betweenness_max(3);
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE algo:betweenness { [] algo:predicate ex:p ; algo:node ?n ; algo:score ?r } }",
    );
    assert!(e.contains("algo:samples"), "{e}");
    // sampling is allowed
    let r = rows(
        &s,
        "SELECT * WHERE { SERVICE algo:betweenness { [] algo:predicate ex:p ; algo:samples 2 ; algo:node ?n ; algo:score ?r } }",
    );
    assert!(!r.is_empty());
    sparkles_core::algo::pool::set_exact_betweenness_max(100_000);
}

#[test]
fn a14_max_memory_fails_before_building() {
    let s = store_of(A);
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:maxMemory 16 ; algo:node ?n ; algo:component ?c } }",
    );
    assert!(e.contains("memory"), "{e}");
}

#[test]
fn parameter_errors() {
    let s = store_of(A);
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE algo:wcc { [] algo:dampingFactor 0.5 ; algo:node ?n } }",
    );
    assert!(e.contains("not a parameter of algo:wcc"), "{e}");
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE algo:pageRank { [] algo:bogus 1 ; algo:node ?n } }",
    );
    assert!(e.contains("unknown parameter"), "{e}");
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE algo:bfs { [] algo:node ?n ; algo:depth ?d } }",
    );
    assert!(e.contains("algo:source"), "{e}");
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE algo:pageRank { [] algo:node ?n ; algo:score ?n } }",
    );
    assert!(e.contains("each output"), "{e}");
}

#[test]
fn directions_and_weights() {
    let s = store_of(
        "ex:a ex:p ex:b {| ex:w 5 |} . ex:b ex:p ex:c {| ex:w 1 |} . ex:a ex:p ex:c {| ex:w 10 |} .",
    );
    let r = rows(
        &s,
        "SELECT ?n ?d WHERE { SERVICE algo:sssp { [] algo:predicate ex:p ; algo:weight ex:w ; \
         algo:source ex:a ; algo:node ?n ; algo:distance ?d } }",
    );
    let d = by_node(&r, "d");
    assert_eq!(d["c"], "6");
    // backward edges: from c, a is reached through b with the same weights
    let r = rows(
        &s,
        "SELECT ?n ?d WHERE { SERVICE algo:sssp { [] algo:predicate ex:p ; algo:direction algo:backward ; \
         algo:weight ex:w ; algo:source ex:c ; algo:node ?n ; algo:distance ?d } }",
    );
    assert_eq!(by_node(&r, "d")["a"], "6");
}

#[test]
fn graphalytics_clustering_defaults_forward_and_preserves_overrides() {
    use sparkles_core::algo::catalog::{Catalog, PredicateDef, ProjectionDef};
    let s = store_of("ex:a ex:p ex:b, ex:c . ex:b ex:p ex:c .");
    let cat = Arc::new(Catalog::memory());
    for (name, direction) in [
        ("implicit", None),
        ("directed", Some("forward")),
        ("symmetric", Some("undirected")),
    ] {
        cat.put(
            name,
            ProjectionDef {
                predicates: Some(vec![PredicateDef::new("http://ex/p", None)]),
                direction: direction.map(str::to_string),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    }
    let opts = QueryOptions {
        algo_projections: Some(cat),
        ..Default::default()
    };
    let cases = [
        ("algo:predicate ex:p ; algo:graphalytics true", 0.5),
        ("algo:predicate ex:p ; algo:graphalytics false", 1.0),
        (
            "algo:predicate ex:p ; algo:graphalytics true ; algo:direction algo:forward",
            0.5,
        ),
        (
            "algo:predicate ex:p ; algo:graphalytics true ; algo:direction algo:backward",
            0.5,
        ),
        (
            "algo:predicate ex:p ; algo:graphalytics true ; algo:direction algo:undirected",
            1.0,
        ),
        (
            "algo:predicate (ex:p algo:undirected) ; algo:graphalytics true",
            1.0,
        ),
        (
            "algo:predicate (ex:p algo:backward) ; algo:graphalytics true",
            0.5,
        ),
        (
            "algo:predicate (ex:p algo:forward) ; algo:direction algo:undirected ; algo:graphalytics true",
            0.5,
        ),
        ("algo:projection \"implicit\" ; algo:graphalytics true", 0.5),
        ("algo:projection \"directed\" ; algo:graphalytics true", 0.5),
        (
            "algo:projection \"symmetric\" ; algo:graphalytics true",
            1.0,
        ),
    ];
    // Alternate the conditional defaults and overrides twice on the same store/cache.
    for _ in 0..2 {
        for (body, expected) in cases {
            let q = format!(
                "SELECT ?n ?c WHERE {{ SERVICE algo:clustering {{ [] {body} ; algo:node ?n ; algo:coefficient ?c }} }}"
            );
            let result = rows_with(&s, &q, &opts);
            assert_eq!(result.len(), 3, "{q}");
            for row in result {
                assert_eq!(row["c"].parse::<f64>().unwrap(), expected, "{q}: {row:?}");
            }
        }
    }
    let e = error(
        &s,
        "SELECT ?t WHERE { SERVICE algo:clustering { [] algo:predicate ex:p ; algo:graphalytics true ; algo:triangles ?t } }",
    );
    assert!(e.contains("triangles is not available"), "{e}");
}

#[test]
fn graphalytics_clustering_reuses_distinct_default_projections() {
    let s = Store::in_memory(StoreOptions {
        result_cache_bytes: 0,
        ..Default::default()
    });
    s.load(&[Source::from_bytes(
        b"@prefix ex: <http://ex/> . ex:a ex:p ex:b, ex:c . ex:b ex:p ex:c .".to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    for round in 0..2 {
        for (mode, expected) in [(true, 0.5), (false, 1.0)] {
            let q = format!(
                "{PREFIXES} SELECT ?n ?c WHERE {{ SERVICE algo:clustering {{ [] algo:predicate ex:p ; algo:graphalytics {mode} ; algo:node ?n ; algo:coefficient ?c }} }}"
            );
            let result = query(s.snapshot(), &q, &QueryOptions::default()).unwrap();
            assert_eq!(result.rows().len(), 3);
            for row in result.rows() {
                assert_eq!(short(&row[1]).parse::<f64>().unwrap(), expected);
            }
            if round == 1 {
                let leaf = find(&result.plan, "AlgoCall").expect("algorithm executed");
                assert_eq!(leaf.counters.as_ref().unwrap()["cachedProjections"], 1);
            }
        }
    }
}

fn weighted_rank_reference(direction: &str, weights: [f32; 2]) -> Vec<f64> {
    use sparkles_core::algo::{Aggregate, Control, Graph, Par, RawEdges, pagerank};
    let (mut src, mut dst, mut w) = (vec![0, 0], vec![1, 2], weights.to_vec());
    if direction == "backward" {
        std::mem::swap(&mut src, &mut dst);
    } else if direction == "undirected" {
        src.extend([1, 2]);
        dst.extend([0, 0]);
        w.extend(weights);
    }
    let ctl = Control::unlimited();
    let g = Graph::build(
        RawEdges {
            nodes: vec![0, 1, 2],
            src,
            dst,
            w: Some(w),
            symmetric: direction == "undirected",
        },
        Aggregate::None,
        &Par::single(),
        &ctl,
    )
    .unwrap();
    pagerank::run(
        &g,
        &pagerank::Params {
            max_iterations: 100,
            ..Default::default()
        },
        &Par::single(),
        &ctl,
    )
    .unwrap()
    .score
}

fn assert_weighted_ranks(s: &Store, q: &str, direction: &str, weights: [f32; 2]) {
    let actual = by_node(&rows(s, q), "r");
    let reference = weighted_rank_reference(direction, weights);
    assert_eq!(actual.len(), 3);
    for (i, n) in ["a", "b", "c"].into_iter().enumerate() {
        let score: f64 = actual[n].parse().unwrap();
        assert!(
            (score - reference[i]).abs() < 1e-12,
            "{q}: {n}: {score} != {}",
            reference[i]
        );
    }
}

#[test]
fn unit_sum_pagerank_matches_explicit_weighted_reference() {
    let s = store_of("ex:a ex:p ex:b ; ex:q ex:b ; ex:p ex:c .");
    for direction in ["forward", "backward", "undirected"] {
        let q = format!(
            "SELECT ?n ?r WHERE {{ SERVICE algo:pageRank {{ [] algo:predicate ex:p, ex:q ; \
             algo:aggregate algo:sum ; algo:direction algo:{direction} ; algo:maxIterations 100 ; \
             algo:node ?n ; algo:score ?r }} }}"
        );
        assert_weighted_ranks(&s, &q, direction, [2.0, 1.0]);
    }
}

#[test]
fn aggregation_modes_preserve_graph_union_and_explicit_weights() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"@prefix ex: <http://ex/> .\nex:g1 { ex:a ex:p ex:b, ex:c . }\n\
          ex:g2 { ex:a ex:p ex:b, ex:c ; ex:q ex:b . }"
            .to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    for aggregate in ["none", "sum", "max", "count"] {
        for (dataset, graph, weights) in [
            (
                "FROM ex:g1 FROM ex:g2",
                "",
                if aggregate == "max" {
                    [1.0, 1.0]
                } else {
                    [2.0, 1.0]
                },
            ),
            ("", "ex:g1", [1.0, 1.0]),
            (
                "",
                "ex:g2",
                if aggregate == "max" {
                    [1.0, 1.0]
                } else {
                    [2.0, 1.0]
                },
            ),
        ] {
            let body = format!(
                "SERVICE algo:pageRank {{ [] algo:predicate ex:p, ex:q ; \
                algo:aggregate algo:{aggregate} ; algo:maxIterations 100 ; algo:node ?n ; algo:score ?r }}"
            );
            let body = if graph.is_empty() {
                body
            } else {
                format!("GRAPH {graph} {{ {body} }}")
            };
            let q = format!("SELECT ?n ?r {dataset} WHERE {{ {body} }}");
            assert_weighted_ranks(&s, &q, "forward", weights);
        }
    }
    let weighted =
        store_of("ex:a ex:p ex:b {| ex:w 1 |} ; ex:q ex:b {| ex:w 3 |} ; ex:p ex:c {| ex:w 2 |} .");
    for (aggregate, weights) in [
        ("none", [4.0, 2.0]),
        ("sum", [4.0, 2.0]),
        ("max", [3.0, 2.0]),
        ("count", [2.0, 1.0]),
    ] {
        let q = format!(
            "SELECT ?n ?r WHERE {{ SERVICE algo:pageRank {{ [] algo:predicate ex:p, ex:q ; \
             algo:weight ex:w ; algo:aggregate algo:{aggregate} ; algo:maxIterations 100 ; \
             algo:node ?n ; algo:score ?r }} }}"
        );
        assert_weighted_ranks(&weighted, &q, "forward", weights);
    }
}

#[test]
fn write_back_with_sparql_update() {
    let s = store_of(A);
    update(
        &s,
        &format!(
            "{PREFIXES} INSERT {{ GRAPH <urn:graph:rank> {{ ?n ex:rank ?r }} }} \
             WHERE {{ SERVICE algo:pageRank {{ [] algo:predicate ex:p ; algo:maxIterations 50 ; algo:node ?n ; algo:score ?r }} }}"
        ),
        &QueryOptions::default(),
    )
    .unwrap();
    let r = rows(
        &s,
        "SELECT ?n ?r WHERE { GRAPH <urn:graph:rank> { ?n ex:rank ?r } }",
    );
    assert_eq!(r.len(), 7);
    // a second run replaces the values
    update(
        &s,
        &format!(
            "{PREFIXES} DELETE {{ GRAPH <urn:graph:rank> {{ ?n ex:rank ?old }} }} \
             INSERT {{ GRAPH <urn:graph:rank> {{ ?n ex:rank ?r }} }} \
             WHERE {{ SERVICE algo:pageRank {{ [] algo:predicate ex:p ; algo:maxIterations 100 ; algo:node ?n ; algo:score ?r }} \
             OPTIONAL {{ GRAPH <urn:graph:rank> {{ ?n ex:rank ?old }} }} }}"
        ),
        &QueryOptions::default(),
    )
    .unwrap();
    let r = rows(
        &s,
        "SELECT ?n ?r WHERE { GRAPH <urn:graph:rank> { ?n ex:rank ?r } }",
    );
    assert_eq!(r.len(), 7, "no stale value is left: {r:?}");
}

#[test]
fn summary_and_stats_mode() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?s WHERE { SERVICE algo:louvain { [] algo:predicate ex:p ; algo:summary ?s } }",
    );
    assert_eq!(r.len(), 1);
    let v: serde_json::Value = serde_json::from_str(&r[0]["s"]).unwrap();
    assert_eq!(v["nodes"], 7);
    assert!(v["modularity"].is_number());
}

#[test]
fn louvain_levels_and_leiden() {
    let s = store_of(
        "ex:a1 ex:p ex:a2, ex:a3 . ex:a2 ex:p ex:a3 . ex:b1 ex:p ex:b2, ex:b3 . ex:b2 ex:p ex:b3 . ex:a3 ex:p ex:b1 .",
    );
    for algo in ["louvain", "leiden"] {
        let q = format!(
            "SELECT ?n ?c ?l WHERE {{ SERVICE algo:{algo} {{ [] algo:predicate ex:p {seed}; \
             algo:node ?n ; algo:community ?c ; algo:level ?l }} }}",
            seed = if algo == "leiden" {
                "; algo:seed 1 "
            } else {
                ""
            }
        );
        let r = rows(&s, &q);
        assert!(r.iter().any(|x| x["l"] == "0"), "{algo}");
        let last: BTreeMap<String, String> = {
            let top = r
                .iter()
                .map(|x| x["l"].parse::<usize>().unwrap())
                .max()
                .unwrap();
            r.iter()
                .filter(|x| x["l"].parse::<usize>().unwrap() == top)
                .map(|x| (x["n"].clone(), x["c"].clone()))
                .collect()
        };
        assert_eq!(last["a1"], last["a2"], "{algo}");
        assert_eq!(last["b1"], last["b2"], "{algo}");
        assert_ne!(last["a1"], last["b1"], "{algo}");
        // the same on a second run
        assert_eq!(rows(&s, &q), r, "{algo}");
    }
}

#[test]
fn embeddings_have_the_asked_dimension() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n ?e WHERE { SERVICE algo:fastRP { [] algo:predicate ex:p ; algo:dimension 8 ; algo:seed 3 ; \
         algo:node ?n ; algo:embedding ?e } }",
    );
    assert_eq!(r.len(), 7);
    for row in &r {
        assert_eq!(row["e"].split(',').count(), 8, "{}", row["e"]);
    }
}

#[test]
fn nested_pattern_edges() {
    let s = store_of("ex:a ex:knows ex:b . ex:b ex:knows ex:c . ex:x ex:other ex:y .");
    let r = rows(
        &s,
        "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:start ?s ; algo:end ?o ; algo:node ?n ; algo:component ?c . \
         ?s ex:knows ?o } }",
    );
    assert_eq!(groups(&by_node(&r, "c")), set(&[&["a", "b", "c"]]));
}

#[test]
fn limit_takes_the_top_rows() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n WHERE { SERVICE algo:pageRank { [] algo:predicate ex:p ; algo:maxIterations 100 ; \
         algo:limit 2 ; algo:node ?n ; algo:score ?r } }",
    );
    let names: BTreeSet<&str> = r.iter().map(|x| x["n"].as_str()).collect();
    assert_eq!(names, ["d", "e"].into());
}

#[test]
fn graph_variable_runs_once_per_named_graph() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"@prefix ex: <http://ex/> .\nex:g1 { ex:a ex:p ex:b . }\nex:g2 { ex:c ex:p ex:d . ex:d ex:p ex:e . }".to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    let r = rows(
        &s,
        "SELECT ?g ?n ?c WHERE { GRAPH ?g { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:node ?n ; algo:component ?c } } }",
    );
    assert_eq!(r.len(), 5, "{r:?}");
    let g1: BTreeSet<&str> = r
        .iter()
        .filter(|x| x["g"] == "g1")
        .map(|x| x["n"].as_str())
        .collect();
    let g2: BTreeSet<&str> = r
        .iter()
        .filter(|x| x["g"] == "g2")
        .map(|x| x["n"].as_str())
        .collect();
    assert_eq!(g1, ["a", "b"].into());
    assert_eq!(g2, ["c", "d", "e"].into());
    let r = rows(
        &s,
        "SELECT ?n WHERE { GRAPH ex:g2 { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:node ?n } } }",
    );
    assert_eq!(r.len(), 3);
}

#[test]
fn a12_embeddings_written_back_are_searchable() {
    // two triangles joined by nothing: a b c and f g h
    let s =
        store_of("ex:a ex:p ex:b, ex:c . ex:b ex:p ex:c . ex:f ex:p ex:g, ex:h . ex:g ex:p ex:h .");
    update(
        &s,
        &format!(
            "{PREFIXES} INSERT {{ ?n ex:emb ?e }} WHERE {{ SERVICE algo:fastRP {{ [] algo:predicate ex:p ; \
             algo:dimension 16 ; algo:seed 4 ; algo:node ?n ; algo:embedding ?e }} }}"
        ),
        &QueryOptions::default(),
    )
    .unwrap();
    let r = rows(
        &s,
        "SELECT ?s ?score WHERE { ?q ex:emb ?v . FILTER(?q = ex:a) \
         (?s ?score) spk:vectorSearch (ex:emb ?v 6) } ORDER BY DESC(?score)",
    );
    let order: Vec<&str> = r.iter().map(|x| x["s"].as_str()).collect();
    assert_eq!(order[0], "a");
    let pos = |n: &str| order.iter().position(|x| *x == n).unwrap();
    for near in ["b", "c"] {
        for far in ["f", "g", "h"] {
            assert!(pos(near) < pos(far), "{near} before {far}: {order:?}");
        }
    }
}

#[test]
fn projections_are_cached_between_calls() {
    let s = store_of(A);
    let q = "SELECT ?n ?c WHERE { SERVICE algo:kCore { [] algo:predicate ex:p ; algo:node ?n ; algo:core ?c } }";
    let first = rows(&s, q);
    assert!(sparkles_core::algo::cache::stats().1 >= 1);
    // the second call reads the cached build or the cached result
    let second = rows(&s, q);
    assert_eq!(first, second);
}

#[test]
fn named_projections() {
    use sparkles_core::algo::catalog::{Catalog, PredicateDef, ProjectionDef};
    let s = store_of(A);
    let cat = Arc::new(Catalog::memory());
    cat.put(
        "chain",
        ProjectionDef {
            predicates: Some(vec![PredicateDef::new("http://ex/p", None)]),
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap();
    let opts = QueryOptions {
        algo_projections: Some(cat),
        ..Default::default()
    };
    let r = rows_with(
        &s,
        "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:projection \"chain\" ; algo:node ?n ; algo:component ?c } }",
        &opts,
    );
    assert_eq!(
        groups(&by_node(&r, "c")),
        set(&[&["a", "b", "c", "d", "e"], &["f", "g"]])
    );
    let q = |body: &str| {
        format!("{PREFIXES}SELECT ?n WHERE {{ SERVICE algo:wcc {{ [] {body} ; algo:node ?n }} }}")
    };
    let fail = |body: &str| match query(s.snapshot(), &q(body), &opts) {
        Ok(_) => panic!("{body}: expected an error"),
        Err(e) => e.to_string(),
    };
    assert!(fail("algo:projection \"nope\"").contains("no named projection"));
    assert!(fail("algo:projection \"chain\" ; algo:predicate ex:p").contains("cannot be combined"));
}

fn plan_of(s: &Store, q: &str) -> sparkles_core::sparql::PlanInfo {
    query(
        s.snapshot(),
        &format!("{PREFIXES}{q}"),
        &QueryOptions::default(),
    )
    .unwrap()
    .plan
}

fn find<'a>(
    p: &'a sparkles_core::sparql::PlanInfo,
    op: &str,
) -> Option<&'a sparkles_core::sparql::PlanInfo> {
    if p.operator == op {
        return Some(p);
    }
    p.children.iter().find_map(|c| find(c, op))
}

#[test]
fn explain_shows_the_counters_of_the_call() {
    let s = store_of(A);
    let plan = plan_of(
        &s,
        "SELECT ?n ?r WHERE { SERVICE algo:pageRank { [] algo:predicate ex:p ; algo:node ?n ; algo:score ?r } }",
    );
    let leaf = find(&plan, "AlgoCall").unwrap_or_else(|| panic!("no AlgoCall in {plan:#?}"));
    assert!(
        leaf.description.contains("pageRank"),
        "{}",
        leaf.description
    );
    let c = leaf.counters.as_ref().expect("counters");
    assert_eq!(c["nodes"], 7);
    assert_eq!(c["edges"], 7);
    for k in ["projectionMs", "runMs", "peakBytes", "cachedProjections"] {
        assert!(c.contains_key(k), "{k} in {c:?}");
    }
}

/// A random graph large enough for an algorithm to run for a while.
fn big(n: usize, m: usize) -> Store {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let mut ttl = String::with_capacity(m * 24);
    for _ in 0..m {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let (a, b) = (x % n as u64, (x >> 20) % n as u64);
        ttl.push_str(&format!("ex:n{a} ex:p ex:n{b} .\n"));
    }
    store_of(&ttl)
}

#[test]
fn node2vec_service_budgets_walk_scratch_and_preserves_seeded_results() {
    let s = store_of("ex:a ex:p ex:b . ex:b ex:p ex:a .");
    let text = |length| {
        format!(
            "{PREFIXES}SELECT ?n ?e WHERE {{ SERVICE algo:node2vec {{ [] algo:predicate ex:p ; \
         algo:dimension 2 ; algo:walkLength {length} ; algo:walksPerNode 1 ; \
         algo:windowSize 1 ; algo:negativeSamples 0 ; algo:seed 17 ; \
         algo:maxMemory 100000 ; algo:node ?n ; algo:embedding ?e }} }}"
        )
    };
    let options = QueryOptions::default();
    let short = query(s.snapshot(), &text(8), &options).unwrap();
    assert_eq!(short.rows().len(), 2);
    let again = query(s.snapshot(), &text(8), &options).unwrap();
    assert_eq!(short.rows(), again.rows());
    // The same projection and embedding dimensions fit. The long walk's 200,000
    // bytes of capacity alone exceed the budget before the walk is allocated.
    let error = match query(s.snapshot(), &text(50_000), &options) {
        Ok(_) => panic!("unaccounted node2vec walk scratch"),
        Err(e) => e,
    };
    assert!(
        matches!(error, sparkles_core::Error::BudgetExceeded(_)),
        "{error}"
    );
    // Failure releases the call's native temporaries and does not poison a retry.
    assert_eq!(
        query(s.snapshot(), &text(8), &options).unwrap().rows(),
        short.rows()
    );
}

#[test]
fn a_running_algorithm_stops_on_cancel_and_on_timeout() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    let s = big(20_000, 100_000);
    let q = format!(
        "{PREFIXES}SELECT ?n ?e WHERE {{ SERVICE algo:node2vec {{ [] algo:predicate ex:p ; \
         algo:walksPerNode 200 ; algo:epochs 20 ; algo:node ?n ; algo:embedding ?e }} }}"
    );
    // a deadline
    let t = Instant::now();
    let e = query(
        s.snapshot(),
        &q,
        &QueryOptions {
            timeout: Some(Duration::from_millis(100)),
            ..Default::default()
        },
    )
    .map(|_| ())
    .unwrap_err();
    assert!(matches!(e, sparkles_core::Error::Timeout), "{e}");
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    // a cancel flag set while it runs
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = flag.clone();
    let h = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        f2.store(true, Ordering::Relaxed);
    });
    let t = Instant::now();
    let e = query(
        s.snapshot(),
        &q,
        &QueryOptions {
            cancel: Some(flag),
            ..Default::default()
        },
    )
    .map(|_| ())
    .unwrap_err();
    h.join().unwrap();
    assert!(matches!(e, sparkles_core::Error::Cancelled), "{e}");
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
}

#[test]
fn concurrent_calls_agree_and_share_the_thread_budget() {
    let s = Arc::new(big(5_000, 40_000));
    let q = format!(
        "{PREFIXES}SELECT ?n ?r WHERE {{ SERVICE algo:pageRank {{ [] algo:predicate ex:p ; \
         algo:concurrency 4 ; algo:node ?n ; algo:score ?r }} }}"
    );
    let run = |s: Arc<Store>, q: String| {
        std::thread::spawn(move || {
            let r = query(s.snapshot(), &q, &QueryOptions::default()).unwrap();
            let mut rows: Vec<String> = r
                .rows()
                .iter()
                .map(|row| format!("{} {}", short(&row[0]), short(&row[1])))
                .collect();
            rows.sort();
            rows
        })
    };
    let handles: Vec<_> = (0..8).map(|_| run(s.clone(), q.clone())).collect();
    let results: Vec<Vec<String>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(!results[0].is_empty());
    for r in &results[1..] {
        assert_eq!(r, &results[0]);
    }
}

#[test]
fn r7_cached_incoming_requires_each_call_budget() {
    let s = projection_cache_store();
    let warm = "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:direction algo:forward ; algo:node ?n ; algo:component ?c } }";
    let opts = QueryOptions::default();
    let _ = projection_cache_read(s.snapshot(), warm, &opts);
    assert_eq!(projection_cache_read(s.snapshot(), warm, &opts).1, 1);
    let q = format!(
        "{PREFIXES}SELECT ?n ?r WHERE {{ SERVICE algo:pageRank {{ [] algo:predicate ex:p ; algo:direction algo:forward ; algo:maxMemory 248 ; algo:node ?n ; algo:score ?r }} }}"
    );
    // Base80 plus five PageRank arrays160 and one partial8 fits248. Incoming48
    // makes the real peak296. Cold cursor scratch32 ends before PageRank arrays,
    // so the peak is max(80+48+32,80+48+168), not their cumulative sum328.
    // This checks combined per-call accounting, not a no-transpose-allocation rule.
    let cold = query(s.snapshot(), &q, &opts);
    let adequate = query(
        s.snapshot(),
        &q.replace("algo:maxMemory 248", "algo:maxMemory 296"),
        &opts,
    )
    .unwrap();
    assert_eq!(adequate.rows().len(), 4);
    let leaf = find(&adequate.plan, "AlgoCall").unwrap();
    assert_eq!(leaf.counters.as_ref().unwrap()["cachedProjections"], 1);
    println!(
        "R7_ADEQUATE_296 peak_bytes={}",
        leaf.counters.as_ref().unwrap()["peakBytes"]
    );
    // The successful caller has initialized incoming rows for the next low-budget call.
    let initialized = query(s.snapshot(), &q, &opts);
    // Observe both negatives before asserting so the incoming-only counterfactual
    // records both incorrect successes rather than stopping at the first assertion.
    println!(
        "R7_COLD_248 success={} budget_exceeded={}",
        cold.is_ok(),
        matches!(&cold, Err(sparkles_core::Error::BudgetExceeded(_)))
    );
    println!(
        "R7_INITIALIZED_248 success={} budget_exceeded={}",
        initialized.is_ok(),
        matches!(&initialized, Err(sparkles_core::Error::BudgetExceeded(_)))
    );
    let unlimited = query(s.snapshot(), &q.replace("algo:maxMemory 248 ; ", ""), &opts).unwrap();
    assert_eq!(unlimited.rows().len(), 4);
    let leaf = find(&unlimited.plan, "AlgoCall").unwrap();
    assert_eq!(leaf.counters.as_ref().unwrap()["cachedProjections"], 1);
    assert_eq!(adequate.rows(), unlimited.rows());
    assert!(
        matches!(cold, Err(sparkles_core::Error::BudgetExceeded(_))),
        "expected cold incoming refusal"
    );
    assert!(
        matches!(initialized, Err(sparkles_core::Error::BudgetExceeded(_))),
        "expected initialized incoming refusal"
    );
}

#[test]
fn r18_irrelevant_malformed_weight_is_not_applicable() {
    let s = store_of("ex:a ex:p ex:b {| ex:w 2 |} . ex:person ex:w \"heavy\" .");
    let r = rows(
        &s,
        "SELECT ?n ?d WHERE { SERVICE algo:sssp { [] algo:predicate ex:p ; algo:weight ex:w ; algo:source ex:a ; algo:node ?n ; algo:distance ?d } }",
    );
    assert_eq!(by_node(&r, "d")["b"], "2");
}
#[cfg(feature = "failpoints")]
#[test]
fn r6_weights_do_not_materialize_irrelevant_ranges() {
    let mut ttl = String::from("ex:a ex:p ex:b {| ex:w 2 |} . ");
    for i in 0..1000 {
        ttl.push_str(&format!("ex:r{i} ex:w {i} . "));
    }
    let s = store_of(&ttl);
    sparkles_core::algo::set_projection_observer(Some(Arc::new(|event| {
        if let sparkles_core::algo::ProjectionEvent::Materialized("weight-scan-keys", capacity) =
            event
        {
            assert_eq!(
                capacity, 0,
                "global weight scan materialized {capacity} rows"
            );
        }
    })));
    let r = rows(
        &s,
        "SELECT ?n ?d WHERE { SERVICE algo:sssp { [] algo:predicate ex:p ; algo:weight ex:w ; algo:source ex:a ; algo:node ?n ; algo:distance ?d } }",
    );
    sparkles_core::algo::set_projection_observer(None);
    assert_eq!(by_node(&r, "d")["b"], "2");
}
#[cfg(feature = "failpoints")]
#[test]
fn r6_class_growth_is_precharged() {
    let s = store_of("ex:a a ex:N . ex:b a ex:N .");
    sparkles_core::algo::set_projection_observer(Some(Arc::new(|event| {
        if let sparkles_core::algo::ProjectionEvent::Allocation(
            "projection-class",
            _,
            bytes,
            used,
        ) = event
        {
            assert!(
                used >= bytes,
                "class allocation {bytes} preceded admission {used}"
            );
        }
    })));
    let r = rows(
        &s,
        "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate ex:missing ; algo:nodeClass ex:N ; algo:node ?n ; algo:component ?c } }",
    );
    sparkles_core::algo::set_projection_observer(None);
    assert_eq!(r.len(), 2);
}

#[cfg(feature = "failpoints")]
#[test]
fn r6_distinct_refuses_before_first_allocation() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let s = store_of("ex:a ex:p ex:b .");
    let allocated = Arc::new(AtomicBool::new(false));
    let seen = allocated.clone();
    sparkles_core::algo::set_projection_observer(Some(Arc::new(move |event| {
        if let sparkles_core::algo::ProjectionEvent::Materialized("distinct-allocated", n) = event
            && n > 0
        {
            seen.store(true, Ordering::Relaxed);
        }
    })));
    let result = query(
        s.snapshot(),
        &format!(
            "{PREFIXES} SELECT ?n ?c WHERE {{ SERVICE algo:wcc {{ [] algo:start ?s ; algo:end ?e ; algo:maxMemory 64 ; algo:node ?n ; algo:component ?c . ?s ex:p ?e }} }}"
        ),
        &QueryOptions::default(),
    );
    sparkles_core::algo::set_projection_observer(None);
    assert!(
        matches!(result, Err(sparkles_core::Error::BudgetExceeded(_))),
        "expected a typed budget refusal"
    );
    assert!(
        !allocated.load(Ordering::Relaxed),
        "distinct table allocated before admission"
    );
}

#[test]
fn r18_applicable_minimum_classes_and_irrelevant_edges() {
    let s = store_of(
        "ex:a a ex:N; ex:p ex:b {| ex:w 5, 2 |}. ex:b a ex:N. ex:h a ex:N. ex:x ex:p ex:y {| ex:w \"heavy\" |}. ex:a ex:p \"literal\" {| ex:w \"heavy\" |}. ex:x ex:q ex:y {|ex:w \"heavy\"|}.",
    );
    let q = "SELECT ?n ?d WHERE { SERVICE algo:sssp { [] algo:predicate ex:p; algo:nodeClass ex:N; algo:weight ex:w; algo:source ex:a; algo:node ?n; algo:distance ?d } }";
    let d = by_node(&rows(&s, q), "d");
    assert_eq!(d["b"], "2");
    assert!(!d.contains_key("x"));
    let wcc = rows(
        &s,
        "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate ex:p; algo:nodeClass ex:N; algo:weight ex:w; algo:node ?n; algo:component ?c } }",
    );
    assert_eq!(groups(&by_node(&wcc, "c")), set(&[&["a", "b"], &["h"]]));
    update(
        &s,
        &format!("{PREFIXES} INSERT DATA {{ ex:a ex:p ex:b {{|ex:w \"bad\"|}} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    assert!(error(&s, q).contains("not a number"));
}

#[test]
fn r18_graph_union_weights_and_delta_triple_key_visibility() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(b"@prefix ex:<http://ex/>. @prefix rdf:<http://www.w3.org/1999/02/22-rdf-syntax-ns#>. ex:edges { ex:a ex:p ex:b. } ex:types {ex:a a ex:N. ex:b a ex:N.} ex:reifiers {_:r rdf:reifies <<(ex:a ex:p ex:b)>>.} ex:weights {_:r ex:w 5, 2.}".to_vec(),RdfFormat::TriG,None)]).unwrap();
    let body = "SERVICE algo:sssp { [] algo:predicate ex:p; algo:weight ex:w; algo:defaultWeight 7; algo:source ex:a; algo:node ?n; algo:distance ?d }";
    for (dataset, expected) in [
        (
            "FROM ex:edges FROM ex:types FROM ex:reifiers FROM ex:weights",
            "2",
        ),
        ("FROM ex:edges FROM ex:reifiers", "7"),
        ("FROM ex:edges FROM ex:weights", "7"),
    ] {
        let q = format!("SELECT ?n ?d {dataset} WHERE {{ {body} }}");
        assert_eq!(by_node(&rows(&s, &q), "d")["b"], expected);
    }
    let q = format!(
        "SELECT ?n ?d FROM NAMED ex:edges FROM NAMED ex:weights WHERE {{ GRAPH ex:edges {{ {body} }} }}"
    );
    assert_eq!(by_node(&rows(&s, &q), "d")["b"], "7");
    let s = store_of("ex:a ex:p ex:b.");
    update(
        &s,
        &format!("{PREFIXES} INSERT DATA {{ ex:r rdf:reifies <<(ex:a ex:p ex:b)>>;ex:w 3 }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(
        by_node(&rows(&s, &format!("SELECT ?n ?d WHERE {{ {body} }}")), "d")["b"],
        "3"
    );
}

#[test]
fn r18_hidden_reifier_weights_and_absent_assertions_are_irrelevant() {
    let s = store_of(
        "ex:a ex:p ex:b {|ex:w 2|}. ex:r a ex:Hidden; <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> <<(ex:a ex:p ex:b)>>;ex:w \"heavy\". ex:absent <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> <<(ex:x ex:p ex:y)>>;ex:w \"heavy\".",
    );
    let protection = Protection {
        name: "hidden-weights".into(),
        predicates: Some(vec!["http://ex/w".into()]),
        classes: Some(vec!["http://ex/Hidden".into()]),
        subclasses: false,
        graphs: None,
        pattern: None,
        prefixes: Default::default(),
        hide_inferences: false,
    };
    let opts = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(protection),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        ))),
        ..Default::default()
    };
    let q = "SELECT ?n ?d WHERE { SERVICE algo:sssp { [] algo:predicate ex:p;algo:weight ex:w;algo:source ex:a;algo:node ?n;algo:distance ?d } }";
    assert!(error(&s, q).contains("not a number"));
    assert_eq!(by_node(&rows_with(&s, q, &opts), "d")["b"], "2");
}

#[test]
fn r18_pattern_first_pair_weight_and_filtered_endpoints() {
    let s = store_of("ex:a a ex:N. ex:b a ex:N.");
    let q = "SELECT ?n ?d WHERE { SERVICE algo:sssp { [] algo:start ?s;algo:end ?e;algo:edgeWeight ?w;algo:source ex:a;algo:node ?n;algo:distance ?d . VALUES (?s ?e ?w) { (ex:a ex:b 2) (ex:a ex:b \"heavy\") (ex:x ex:y \"heavy\") (ex:a \"literal\" \"heavy\") } FILTER(?s != ex:x) } }";
    assert_eq!(by_node(&rows(&s, q), "d")["b"], "2");
    let s = store_of("ex:a ex:p ex:b.");
    let q = "SELECT ?n ?d WHERE { SERVICE algo:sssp { [] algo:start ?s;algo:end ?e;algo:edgeWeight ?w;algo:defaultWeight 7;algo:source ex:a;algo:node ?n;algo:distance ?d . VALUES (?s ?e ?w) { (ex:a ex:b UNDEF) } FILTER(?s != ex:x) } }";
    assert_eq!(by_node(&rows(&s, q), "d")["b"], "7");
}

#[test]
fn r18_blank_endpoints_preserve_canonical_reifier_keys() {
    let s = store_of("_:a ex:p _:b {|ex:w 2|}. _:b ex:p ex:c {|ex:w 3|}.");
    let q = "SELECT ?n ?d WHERE { SERVICE algo:sssp { [] algo:predicate ex:p;algo:direction algo:backward;algo:weight ex:w;algo:source ex:c;algo:node ?n;algo:distance ?d } }";
    let result = rows(&s, q);
    let distances: BTreeSet<_> = result.iter().map(|r| r["d"].clone()).collect();
    assert_eq!(
        distances,
        BTreeSet::from(["0".into(), "3".into(), "5".into()])
    );
}

// R12 weight failures must be typed before output rows can contain invalid ranks.
fn r12_invalid(s: &Store, body: &str) -> bool {
    matches!(
        query(
            s.snapshot(),
            &format!("{PREFIXES} SELECT ?n ?r WHERE {{ {body} }}"),
            &QueryOptions::default()
        ),
        Err(sparkles_core::Error::Invalid(_))
    )
}

#[test]
fn r12_pagerank_negative_native_equivalents() {
    let s = store_of("ex:a ex:p ex:b {|ex:w -10|}, ex:c {|ex:w 11|}.");
    let reifier = "SERVICE algo:pageRank { [] algo:predicate ex:p;algo:weight ex:w;algo:node ?n;algo:score ?r }";
    let nested = "SERVICE algo:pageRank { [] algo:start ?s;algo:end ?e;algo:edgeWeight ?w;algo:node ?n;algo:score ?r . VALUES (?s ?e ?w) {(ex:a ex:b -10) (ex:a ex:c 11)} }";
    let actual = [r12_invalid(&s, reifier), r12_invalid(&s, nested)];
    assert_eq!(
        actual, [true; 2],
        "reifier and nested must reject signed probability weights"
    );
}

#[test]
fn r12_projection_conversions_remain_finite() {
    let mut missed = Vec::new();
    for lexical in ["NaN", "INF", "-INF", "1e40"] {
        let literal = format!("\"{lexical}\"^^<http://www.w3.org/2001/XMLSchema#double>");
        let s = store_of(&format!("ex:a ex:p ex:b {{|ex:w {literal}|}}."));
        let reifier = "SERVICE algo:pageRank { [] algo:predicate ex:p;algo:weight ex:w;algo:node ?n;algo:score ?r }";
        let nested = format!(
            "SERVICE algo:pageRank {{ [] algo:start ?s;algo:end ?e;algo:edgeWeight ?w;algo:node ?n;algo:score ?r . VALUES (?s ?e ?w) {{(ex:a ex:b {literal})}} }}"
        );
        let default = format!(
            "SERVICE algo:pageRank {{ [] algo:start ?s;algo:end ?e;algo:edgeWeight ?w;algo:defaultWeight {literal};algo:node ?n;algo:score ?r . VALUES (?s ?e ?w) {{(ex:a ex:b UNDEF)}} }}"
        );
        for (path, body) in [
            ("reifier", reifier.to_string()),
            ("nested", nested),
            ("default", default),
        ] {
            let invalid = r12_invalid(&s, &body);
            eprintln!("{path}/{lexical}: typed_invalid={invalid}");
            if !invalid {
                missed.push(format!("{path}/{lexical}"));
            }
        }
    }
    assert!(missed.is_empty(), "unchecked conversions: {missed:?}");
}

fn r12_invalid_diagnostic(s: &Store, q: &str, expected: &str) {
    let e = match query(
        s.snapshot(),
        &format!("{PREFIXES}{q}"),
        &QueryOptions::default(),
    ) {
        Ok(_) => panic!("expected Invalid containing {expected}"),
        Err(e) => e,
    };
    eprintln!("R12 diagnostic: {e}");
    assert!(matches!(e, sparkles_core::Error::Invalid(_)), "{e}");
    assert!(e.to_string().contains(expected), "{e}");
}

#[test]
fn r12_default_applicability_and_aggregate_overflow() {
    let s = store_of("ex:a ex:p ex:b {|ex:w 2|}.");
    let q = "SELECT ?n ?r WHERE { SERVICE algo:pageRank { [] algo:predicate ex:p;algo:weight ex:w;algo:defaultWeight 1e40;algo:node ?n;algo:score ?r } }";
    assert_eq!(
        rows(&s, q).len(),
        2,
        "unused large default is not converted"
    );
    let s = store_of("ex:a ex:p ex:b.");
    r12_invalid_diagnostic(&s, q, "an edge weight is not finite as f32");
    let s = store_of("ex:a ex:p ex:b {|ex:w 3e38|}; ex:q ex:b {|ex:w 3e38|}.");
    r12_invalid_diagnostic(
        &s,
        "SELECT ?n ?r WHERE { SERVICE algo:pageRank { [] algo:predicate ex:p,ex:q;algo:weight ex:w;algo:aggregate algo:sum;algo:node ?n;algo:score ?r } }",
        "summed edge weight is not finite as f32",
    );
}

#[test]
fn r12_normal_and_f32_boundary_sum_controls() {
    let q = |aggregate: &str| {
        format!(
            "SELECT ?n ?d WHERE {{ SERVICE algo:sssp {{ [] algo:predicate ex:p,ex:q;algo:weight ex:w;algo:aggregate algo:{aggregate};algo:source ex:a;algo:node ?n;algo:distance ?d }} }}"
        )
    };
    let s = store_of("ex:a ex:p ex:b {|ex:w 2|}; ex:q ex:b {|ex:w 3|}.");
    let summed = by_node(&rows(&s, &q("sum")), "d");
    let separate = by_node(&rows(&s, &q("none")), "d");
    assert_eq!(summed["b"].parse::<f64>().unwrap(), 5.0);
    assert_eq!(separate["b"].parse::<f64>().unwrap(), 2.0);
    let s = store_of(
        "ex:a ex:p ex:b {|ex:w 1.7014117331926443e38|}; ex:q ex:b {|ex:w 1.7014117331926443e38|}.",
    );
    let boundary = by_node(&rows(&s, &q("sum")), "d");
    let actual = boundary["b"].parse::<f64>().unwrap();
    eprintln!("finite sum boundary: {actual:e}, f32::MAX={:e}", f32::MAX);
    assert!(actual.is_finite());
    assert_eq!(actual as f32, f32::MAX);
    let ranks = rows(
        &s,
        "SELECT ?n ?r WHERE { SERVICE algo:pageRank { [] algo:predicate ex:p,ex:q;algo:weight ex:w;algo:aggregate algo:sum;algo:node ?n;algo:score ?r } }",
    );
    assert_eq!(ranks.len(), 2);
    assert!(
        ranks
            .iter()
            .all(|r| r["r"].parse::<f64>().unwrap().is_finite())
    );
}

#[test]
fn r12_conversion_precedes_minimum_and_preserves_applicability() {
    let s = store_of("ex:a ex:p ex:b {|ex:w 2, 1e40|}.");
    assert!(
        r12_invalid(
            &s,
            "SERVICE algo:wcc { [] algo:predicate ex:p;algo:weight ex:w;algo:node ?n }"
        ),
        "every applicable candidate must fit before taking the minimum"
    );
    let s = store_of("ex:a ex:p ex:b {|ex:w -10|}, ex:c {|ex:w 11|}.");
    assert_eq!(rows(&s, "SELECT ?n WHERE { SERVICE algo:wcc { [] algo:predicate ex:p;algo:weight ex:w;algo:node ?n } }").len(), 3, "projection does not impose PageRank's sign domain on other algorithms");
    let q = "SELECT ?n WHERE { SERVICE algo:wcc { [] algo:start ?s;algo:end ?e;algo:edgeWeight ?w;algo:defaultWeight 1e40;algo:node ?n . VALUES (?s ?e ?w) { (ex:a ex:b 2) (ex:a ex:b 1e40) (ex:a \"literal\" 1e40) } } }";
    assert_eq!(
        rows(&s, q).len(),
        2,
        "unused default, duplicate and non-resource rows are not converted"
    );
}
