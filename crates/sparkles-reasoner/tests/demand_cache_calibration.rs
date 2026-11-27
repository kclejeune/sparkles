//! End-to-end demand-reasoning checks for the calibrated derived-table cache estimator
//! and eviction allowances (spec F17, roadmap item 757).

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::query::QueryReasoner;
use sparkles_reasoner::{Extras, Inputs, Profile, tables};
use std::sync::Mutex;

/// Serialize tests in this binary because `sparkles_reasoner::tables` is process-wide.
static CACHE_LOCK: Mutex<()> = Mutex::new(());

const PREFIXES: &str = "\
@prefix ex: <http://example.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
";

fn run_select(store: &Store, reasoner: &QueryReasoner, sparql: &str) -> (Vec<String>, u64) {
    let opts = QueryOptions {
        reasoning: Some(reasoner.provider()),
        ..Default::default()
    };
    let result = query(store.snapshot(), sparql, &opts).unwrap();
    let cached = result.reasoning().map_or(0, |r| r.report.cached);
    let mut rows: Vec<String> = result
        .rows()
        .unwrap()
        .iter()
        .map(|row| format!("{row:?}"))
        .collect();
    rows.sort();
    (rows, cached)
}

#[test]
fn calibrated_eviction_preserves_demand_answers_across_local_terms_and_shared_chunks() {
    let _guard = CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    tables::clear();
    tables::set_capacity(tables::DEFAULT_CAPACITY);

    let store = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from(PREFIXES);
    ttl.push_str(
        "ex:Sub1 rdfs:subClassOf ex:Base .\n\
         ex:Sub2 rdfs:subClassOf ex:Base .\n\
         ex:Sub3 rdfs:subClassOf ex:Sub2 .\n",
    );
    for i in 0..80 {
        ttl.push_str(&format!(
            "ex:item{i} a ex:Sub1 ; ex:tag \"val-{i:03}\" .\n\
             ex:other{i} a ex:Sub3 ; ex:tag \"oth-{i:03}\" .\n"
        ));
    }
    store
        .load(&[Source::from_bytes(
            ttl.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();

    // Backward rules producing both shared class entailments and local literal terms.
    let rules = "\
@prefix ex: <http://example.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
[sub: (?x rdf:type ?c) <- (?x rdf:type ?s), (?s rdfs:subClassOf ?c)]
[trans: (?a rdfs:subClassOf ?c) <- (?a rdfs:subClassOf ?b), (?b rdfs:subClassOf ?c)]
[lbl: (?x ex:derivedLabel ?out) <- (?x rdf:type ex:Base), (?x ex:tag ?t), strConcat(\"derived:\", ?t, ?out)]
";
    let reasoner = QueryReasoner::new(
        Profile::Rules(rules.into()),
        &Extras::default(),
        Inputs::default(),
    )
    .unwrap();

    let q_multi = "\
PREFIX ex: <http://example.org/>
SELECT ?x ?l WHERE {
  ?x a ex:Base .
  ?x ex:derivedLabel ?l .
}";
    let q_sub2 = "\
PREFIX ex: <http://example.org/>
SELECT ?x WHERE { ?x a ex:Sub2 }";
    let q_single = "\
PREFIX ex: <http://example.org/>
SELECT ?l WHERE { ex:item7 ex:derivedLabel ?l }";

    // Baseline with cache disabled.
    tables::set_capacity(0);
    let (base_multi, cached_off) = run_select(&store, &reasoner, q_multi);
    assert_eq!(base_multi.len(), 160);
    assert_eq!(cached_off, 0);
    let (base_sub2, _) = run_select(&store, &reasoner, q_sub2);
    assert_eq!(base_sub2.len(), 80);
    let (base_single, _) = run_select(&store, &reasoner, q_single);
    assert_eq!(base_single.len(), 1);
    assert_eq!(tables::snapshot().tables, 0);
    assert_eq!(tables::snapshot().estimated_bytes, 0);

    // Warm run with generous capacity populates cached tables including local-term keys.
    tables::set_capacity(16 << 20);
    let (warm_multi_1, _) = run_select(&store, &reasoner, q_multi);
    let (warm_multi_2, cached_hits) = run_select(&store, &reasoner, q_multi);
    assert_eq!(warm_multi_1, base_multi);
    assert_eq!(warm_multi_2, base_multi);
    assert!(cached_hits > 0, "repeat query should hit cached tables");
    let after_multi = tables::snapshot();
    assert!(after_multi.tables > 0);
    assert!(after_multi.estimated_bytes > 0);
    assert!(after_multi.estimated_bytes <= after_multi.capacity_bytes);

    let (warm_sub2, _) = run_select(&store, &reasoner, q_sub2);
    let (warm_single, _) = run_select(&store, &reasoner, q_single);
    assert_eq!(warm_sub2, base_sub2);
    assert_eq!(warm_single, base_single);
    let full = tables::snapshot();
    assert!(full.estimated_bytes > after_multi.estimated_bytes);

    // Tight capacity forces calibrated eviction while preserving exact query answers.
    let tight = (after_multi.estimated_bytes / 2).max(2048);
    tables::set_capacity(tight);
    let after_shrink = tables::snapshot();
    assert!(
        after_shrink.estimated_bytes <= tight,
        "retained estimate {} exceeded shrunk capacity {tight}",
        after_shrink.estimated_bytes
    );

    let (tight_multi, _) = run_select(&store, &reasoner, q_multi);
    let (tight_sub2, _) = run_select(&store, &reasoner, q_sub2);
    let (tight_single, _) = run_select(&store, &reasoner, q_single);
    assert_eq!(tight_multi, base_multi);
    assert_eq!(tight_sub2, base_sub2);
    assert_eq!(tight_single, base_single);
    let final_stats = tables::snapshot();
    assert!(
        final_stats.estimated_bytes <= tight,
        "retained estimate {} exceeded tight capacity {tight}",
        final_stats.estimated_bytes
    );

    // Restore global defaults cleanly.
    tables::clear();
    tables::set_capacity(tables::DEFAULT_CAPACITY);
}
