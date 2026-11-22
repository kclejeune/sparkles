//! A text search with a constant query and no limit under the strict cursor policy holds
//! its hits and decodes their rows a page at a time (item 829, text group).
#![cfg(feature = "text")]

use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    CursorOptions, CursorPlan, FallbackPolicy, QueryOptions, query, select_cursor,
};
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use sparkles_core::text::TextConfig;
use sparkles_core::{BudgetKind, Error, Result};
use std::sync::Arc;

const P: &str = "PREFIX ex: <http://example.org/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX text: <http://jena.apache.org/text#> ";

/// Hits on "fox": `n` subjects in two named graphs with a merged default graph, so each
/// triple is hit twice and the second hit is a duplicate. Labels repeat every seven
/// subjects, so many hits tie on their score.
fn store(n: usize, config: TextConfig) -> Store {
    let s = Store::in_memory(StoreOptions {
        union_default_graph: true,
        ..Default::default()
    });
    let mut data = String::new();
    for g in ["g1", "g2"] {
        data.push_str(&format!("<http://example.org/{g}> {{\n"));
        for i in 0..n {
            let words = "quick brown ".repeat(i % 7);
            data.push_str(&format!(
                "<http://example.org/s{i}> <http://www.w3.org/2000/01/rdf-schema#label> \"{words}fox {}\"@en .\n",
                i % 7
            ));
        }
        data.push_str("}\n");
    }
    s.load(&[Source::from_bytes(data.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();
    s.enable_text(config).unwrap();
    s
}

fn opts() -> QueryOptions {
    QueryOptions {
        no_cache: true,
        ..Default::default()
    }
}

fn strict(rows: usize) -> CursorOptions {
    CursorOptions {
        batch_rows: rows,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    }
}

type Rows = Vec<Vec<Option<Term>>>;

fn eager(snap: &Arc<Snapshot>, q: &str, o: &QueryOptions) -> Result<Rows> {
    Ok(query(snap.clone(), &format!("{P}{q}"), o)?
        .rows()?
        .iter()
        .map(|r| r.values().to_vec())
        .collect())
}

/// The rows of a cursor in order, the size of each batch, and its plan once drained.
fn streamed(
    snap: &Arc<Snapshot>,
    q: &str,
    o: &QueryOptions,
    c: &CursorOptions,
) -> Result<(Rows, Vec<usize>, CursorPlan)> {
    let mut cursor = select_cursor(snap.clone(), &format!("{P}{q}"), o, c)?;
    let (mut out, mut sizes) = (Vec::new(), Vec::new());
    while let Some(b) = cursor.next_batch()? {
        sizes.push(b.len());
        for r in 0..b.len() {
            out.push(b.row(r)?.values().to_vec());
        }
    }
    Ok((out, sizes, cursor.plan().clone()))
}

fn materializes(plan: &CursorPlan) -> bool {
    plan.materializes || plan.children.iter().any(materializes)
}

fn find<'a>(plan: &'a CursorPlan, op: &str) -> Option<&'a CursorPlan> {
    if plan.operator.operator == op {
        return Some(plan);
    }
    plan.children.iter().find_map(|c| find(c, op))
}

/// Every batch size gives eager's rows in eager's order. Tied ranks and the pages of
/// hits straddle batch boundaries in one search, and the duplicates of a merged default
/// graph in another. A batch is full until the last, so a run of skipped hits neither
/// ends the answer nor hands out an empty batch.
#[test]
fn a_search_without_a_limit_reads_eagers_rows_at_every_batch_size() {
    let s = store(2500, TextConfig::default());
    let snap = s.snapshot();
    // a graph output keeps both hits of a triple: 5000 rows, more than one page
    let ranked =
        "SELECT ?s ?sc ?lit ?g ?p ?r { (?s ?sc ?lit ?g ?p ?r) text:query (rdfs:label \"fox\") }";
    // without one, the merged default graph keeps each triple once
    let merged = "SELECT ?s ?sc ?lit { (?s ?sc ?lit) text:query (rdfs:label \"fox\") }";
    for (q, n) in [(ranked, 5000), (merged, 2500)] {
        let expected = eager(&snap, q, &opts()).unwrap();
        assert_eq!(expected.len(), n, "{q}");
        if q == ranked {
            let ranks: std::collections::HashSet<_> =
                expected.iter().map(|r| r[5].clone()).collect();
            assert!(ranks.len() < 100, "ties share a rank");
        }
        for rows in [1, 2, 3, 7, 1000, 4096] {
            let mut cursor =
                select_cursor(snap.clone(), &format!("{P}{q}"), &opts(), &strict(rows)).unwrap();
            let initial = find(cursor.plan(), "TextSearch").expect("initial text search node");
            assert!(
                initial.growing_state,
                "initial plan batch {rows}: {initial:#?}"
            );
            let mut answer = Vec::new();
            let mut sizes = Vec::new();
            let mut checked_first = false;
            while let Some(b) = cursor.next_batch().unwrap() {
                sizes.push(b.len());
                for r in 0..b.len() {
                    answer.push(b.row(r).unwrap().values().to_vec());
                }
                drop(b);
                if !checked_first {
                    checked_first = true;
                    let after_first =
                        find(cursor.plan(), "TextSearch").expect("first-batch text search node");
                    assert!(
                        after_first.growing_state,
                        "first-batch plan batch {rows}: {after_first:#?}"
                    );
                }
            }
            let plan = cursor.plan().clone();
            assert_eq!(answer, expected, "batch {rows}: {q}");
            let (last, full) = sizes.split_last().unwrap();
            assert!(full.iter().all(|&n| n == rows), "batch {rows}: {sizes:?}");
            assert!(*last >= 1 && *last <= rows);
            assert!(!materializes(&plan), "{plan:#?}");
            let node = find(&plan, "TextSearch").expect("a text search node");
            assert!(
                node.reason
                    .as_deref()
                    .is_some_and(|r| r.contains("a page at a time")),
                "{node:#?}"
            );
            assert!(node.full_input_before_output);
            assert!(node.growing_state, "drained plan batch {rows}: {node:#?}");
            assert_eq!(node.operator.actual_rows, n as i64);
        }
    }
}

/// Highlighted literals are eager's at every batch size, with a literal repeated across
/// batches highlighted the same way.
#[test]
fn highlights_are_eagers() {
    let s = store(60, TextConfig::default());
    let snap = s.snapshot();
    let q = "SELECT ?s ?lit { (?s ?sc ?lit) text:query (rdfs:label \"fox\" \"highlight:s:<b> | e:</b>\") }";
    let expected = eager(&snap, q, &opts()).unwrap();
    assert!(
        expected
            .iter()
            .all(|r| matches!(&r[1], Some(Term::Literal(l)) if l.value().contains("<b>fox</b>")))
    );
    for rows in [1, 5, 4096] {
        let (answer, _, _) = streamed(&snap, q, &opts(), &strict(rows)).unwrap();
        assert_eq!(answer, expected, "batch {rows}");
    }
}

/// More hits than `maxHits` is the error an eager search gives, before any row.
#[test]
fn more_hits_than_max_hits_is_eagers_error() {
    let s = store(
        40,
        TextConfig {
            max_hits: 50,
            ..Default::default()
        },
    );
    let snap = s.snapshot();
    let q = "SELECT ?s { ?s text:query (rdfs:label \"fox\") }";
    let budget = |e: Error| match e {
        Error::BudgetExceeded(b) => (b.kind, b.limit, b.requested),
        e => panic!("{e}"),
    };
    let eager = budget(eager(&snap, q, &opts()).unwrap_err());
    assert_eq!(eager, (BudgetKind::Rows, 50, 80));
    let streamed = budget(streamed(&snap, q, &opts(), &strict(1)).unwrap_err());
    assert_eq!(streamed, eager);
}

/// The produced-row limit refuses the same search in both modes, with the same count.
#[test]
fn the_produced_row_limit_refuses_alike() {
    let s = store(40, TextConfig::default());
    let snap = s.snapshot();
    let o = QueryOptions {
        max_rows_produced: Some(30),
        ..opts()
    };
    let q = "SELECT ?s { ?s text:query (rdfs:label \"fox\") }";
    let budget = |e: Error| match e {
        Error::BudgetExceeded(b) => (b.kind, b.limit, b.requested),
        e => panic!("{e}"),
    };
    let eager_refusal = budget(eager(&snap, q, &o).unwrap_err());
    assert_eq!(eager_refusal.1, 30);
    assert_eq!(
        budget(streamed(&snap, q, &o, &strict(4)).unwrap_err()),
        eager_refusal
    );
    let fits = QueryOptions {
        max_rows_produced: Some(1000),
        ..opts()
    };
    assert_eq!(
        streamed(&snap, q, &fits, &strict(4)).unwrap().0,
        eager(&snap, q, &fits).unwrap()
    );
}

/// A budget too small for the hit list still gives eager's error when the hits are more
/// than `maxHits` or than the produced-row limit, since the search counts every hit
/// before it reports a refused charge.
#[test]
fn a_refused_charge_comes_after_the_row_limits() {
    let budget = |e: Error| match e {
        Error::BudgetExceeded(b) => (b.kind, b.limit, b.requested),
        e => panic!("{e}"),
    };
    let q = "SELECT ?s ?sc ?lit { (?s ?sc ?lit) text:query (rdfs:label \"fox\") }";
    let tight = QueryOptions {
        max_memory_bytes: Some(64 << 10),
        ..opts()
    };
    let s = store(
        2500,
        TextConfig {
            max_hits: 4000,
            ..Default::default()
        },
    );
    let snap = s.snapshot();
    let eager_refusal = budget(eager(&snap, q, &tight).unwrap_err());
    assert_eq!(eager_refusal, (BudgetKind::Rows, 4000, 5000));
    assert_eq!(
        budget(streamed(&snap, q, &tight, &strict(16)).unwrap_err()),
        eager_refusal
    );
    let s = store(2500, TextConfig::default());
    let snap = s.snapshot();
    let rows = QueryOptions {
        max_rows_produced: Some(3000),
        ..tight.clone()
    };
    let eager_refusal = budget(eager(&snap, q, &rows).unwrap_err());
    assert_eq!(eager_refusal.0, BudgetKind::RowsProduced);
    assert_eq!(eager_refusal.1, 3000);
    assert_eq!(
        budget(streamed(&snap, q, &rows, &strict(16)).unwrap_err()),
        eager_refusal
    );
    // without the row limit, the same budget refuses the hit list
    assert_eq!(
        budget(streamed(&snap, q, &tight, &strict(16)).unwrap_err()).0,
        BudgetKind::Memory
    );
}

/// Hits in several segments, some of whose documents are deleted, are eager's rows and
/// scores, in eager's order.
#[test]
fn hits_across_segments_with_deleted_documents_are_eagers() {
    use sparkles_core::sparql::update::update;
    // 600 triples in each graph, 67 of g1's deleted and 150 added to g2 by each update
    let live = 2 * 600 - 4 * 67 + 4 * 150;
    let s = store(600, TextConfig::default());
    for i in 0..4 {
        let mut data = String::new();
        for j in 0..150 {
            data.push_str(&format!(
                "ex:t{i}_{j} rdfs:label \"{} fox {j}\" . ",
                "brown ".repeat(j % 5)
            ));
        }
        update(
            &s,
            &format!("{P}INSERT DATA {{ GRAPH ex:g2 {{ {data} }} }}"),
            &opts(),
        )
        .unwrap();
        let mut gone = String::new();
        for j in (i..600).step_by(9) {
            let words = "quick brown ".repeat(j % 7);
            gone.push_str(&format!(
                "GRAPH ex:g1 {{ ex:s{j} rdfs:label \"{words}fox {}\"@en }} ",
                j % 7
            ));
        }
        update(&s, &format!("{P}DELETE DATA {{ {gone} }}"), &opts()).unwrap();
    }
    let status = s.text_status().unwrap();
    assert!(status.segments > 1, "{} segments", status.segments);
    let snap = s.snapshot();
    let q = "SELECT ?s ?sc ?lit ?g { (?s ?sc ?lit ?g) text:query (rdfs:label \"fox\") }";
    let expected = eager(&snap, q, &opts()).unwrap();
    assert_eq!(expected.len(), live);
    for rows in [1, 7, 4096] {
        let (answer, _, plan) = streamed(&snap, q, &opts(), &strict(rows)).unwrap();
        assert!(!materializes(&plan), "{plan:#?}");
        assert_eq!(answer, expected, "batch {rows}");
    }
}

/// The hit list is charged before each growth: a budget below it refuses the search
/// with a memory error. A budget that holds it gives a peak of at least each part the
/// reader charges, and bounds it by the hits, not by a table of the rows.
#[test]
fn the_hit_list_is_charged() {
    let s = store(2500, TextConfig::default());
    let snap = s.snapshot();
    // a merged default graph keeps the set of the triples seen
    let q = "SELECT ?s ?sc ?lit { (?s ?sc ?lit) text:query (rdfs:label \"fox\") }";
    let tight = QueryOptions {
        max_memory_bytes: Some(64 << 10),
        ..opts()
    };
    match streamed(&snap, q, &tight, &strict(16)) {
        Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, BudgetKind::Memory),
        other => panic!("{:?}", other.map(|r| r.0.len())),
    }
    let peak = |snap: &Arc<Snapshot>, q: &str| {
        let mut cursor =
            select_cursor(snap.clone(), &format!("{P}{q}"), &opts(), &strict(16)).unwrap();
        while cursor.next_batch().unwrap().is_some() {}
        cursor.stats().mem_peak_bytes
    };
    let none = "SELECT ?s ?sc ?lit { (?s ?sc ?lit) text:query (rdfs:label \"wolf\") }";
    // what the query holds besides the reader's charge
    let base = peak(&snap, none);
    // 5000 hits in a list grown from 1024 to 8192 hits of 12 bytes, a page of 4096
    // decoded hits at 192 bytes, and the set of 2500 triples reserved at once in 4096
    // buckets of 25 bytes
    let charged = 8192 * 12 + 4096 * 192 + (4096 * 25 + 16);
    let merged = peak(&snap, q);
    // the baseline holds a few bytes that need not be live at the peak
    assert!(
        merged + 4096 >= base + charged,
        "{merged} < {base} + {charged}"
    );
    // With 50,000 hits, the list grown to 65,536 hits of 12 bytes and the page are the
    // largest charge, above the 32,768 and 65,536 hits that the last growth holds.
    let s = store(25_000, TextConfig::default());
    let snap = s.snapshot();
    let ranked = "SELECT ?s ?sc ?g { (?s ?sc ?lit ?g) text:query (rdfs:label \"fox\") }";
    let base = peak(&snap, none);
    let charged = 65_536 * 12 + 4096 * 192;
    let peak = peak(&snap, ranked);
    assert!(peak + 4096 >= base + charged, "{peak} < {base} + {charged}");
    assert!(
        peak < base + charged + (256 << 10),
        "{peak} - {base} > {charged}"
    );
}

/// A triple in more named graphs than a page holds gives a merged default graph a page
/// whose every hit is a duplicate. The pull that reaches it reads on to the next page
/// and hands out a full batch, at every batch size.
#[test]
fn a_page_of_skipped_hits_is_read_through() {
    let s = Store::in_memory(StoreOptions {
        union_default_graph: true,
        ..Default::default()
    });
    let mut data = String::new();
    for g in 0..9000 {
        data.push_str(&format!(
            "<http://example.org/g{g}> {{ <http://example.org/top> <http://www.w3.org/2000/01/rdf-schema#label> \"fox\" }}\n"
        ));
    }
    data.push_str("<http://example.org/g0> {\n");
    for i in 0..20 {
        data.push_str(&format!(
            "<http://example.org/s{i}> <http://www.w3.org/2000/01/rdf-schema#label> \"a much longer label about one fox {i}\" .\n"
        ));
    }
    data.push_str("}\n");
    s.load(&[Source::from_bytes(data.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();
    s.enable_text(TextConfig::default()).unwrap();
    let snap = s.snapshot();
    let q = "SELECT ?s ?sc { (?s ?sc) text:query (rdfs:label \"fox\") }";
    let expected = eager(&snap, q, &opts()).unwrap();
    assert_eq!(expected.len(), 21);
    for rows in [1, 2, 4096] {
        let (answer, sizes, _) = streamed(&snap, q, &opts(), &strict(rows)).unwrap();
        assert_eq!(answer, expected, "batch {rows}");
        let (_, full) = sizes.split_last().unwrap();
        assert!(full.iter().all(|&n| n == rows), "batch {rows}: {sizes:?}");
    }
}

/// A search with no hits ends without a batch, so a budget too small for a batch of
/// the default size does not refuse it.
#[test]
fn a_search_without_hits_needs_no_batch() {
    let s = store(10, TextConfig::default());
    let snap = s.snapshot();
    let q = "SELECT ?s { ?s text:query (rdfs:label \"wolf* AND fox\") }";
    let small = QueryOptions {
        max_memory_bytes: Some(24 << 10),
        ..opts()
    };
    assert!(eager(&snap, q, &small).unwrap().is_empty());
    let (answer, sizes, _) = streamed(&snap, q, &small, &strict(4096)).unwrap();
    assert!(answer.is_empty() && sizes.is_empty());
}

/// A search inside EXISTS streams under the strict policy and answers as eager does.
#[test]
fn a_search_inside_exists_streams() {
    let s = store(30, TextConfig::default());
    let snap = s.snapshot();
    let q = "SELECT ?x { VALUES ?x { ex:s3 ex:s99 ex:s12 } FILTER EXISTS { ?x text:query (rdfs:label \"fox\") } }";
    let expected = eager(&snap, q, &opts()).unwrap();
    assert_eq!(expected.len(), 2);
    let (answer, _, plan) = streamed(&snap, q, &opts(), &strict(1)).unwrap();
    assert_eq!(answer, expected);
    assert!(!materializes(&plan), "{plan:#?}");
}

/// A search with nothing to match ends at once.
#[test]
fn a_search_that_matches_nothing_ends() {
    let s = store(10, TextConfig::default());
    let snap = s.snapshot();
    let q = "SELECT ?s { ?s text:query (rdfs:label \"wolf\") }";
    let (answer, sizes, _) = streamed(&snap, q, &opts(), &strict(3)).unwrap();
    assert!(answer.is_empty() && sizes.is_empty());
}

/// A highlighted search with no hits charges no map for highlighting.
#[test]
fn a_highlighted_search_without_hits_needs_no_map() {
    let s = store(10, TextConfig::default());
    let snap = s.snapshot();
    let q = "SELECT ?s ?lit { (?s ?sc ?lit) text:query (rdfs:label \"wolf* AND fox\" \"highlight:s:<b> | e:</b>\") }";
    let small = QueryOptions {
        max_memory_bytes: Some(24 << 10),
        ..opts()
    };
    assert!(eager(&snap, q, &small).unwrap().is_empty());
    let (answer, sizes, _) = streamed(&snap, q, &small, &strict(4096)).unwrap();
    assert!(answer.is_empty() && sizes.is_empty());
}

/// The first batch fits in what the charged hit list leaves of the budget, rather than
/// in what the budget held before the search ran.
#[test]
fn the_first_batch_fits_beside_the_hit_list() {
    let s = store(2500, TextConfig::default());
    let snap = s.snapshot();
    let q = "SELECT ?s ?sc ?lit { (?s ?sc ?lit) text:query (rdfs:label \"fox\") }";
    let budget = QueryOptions {
        max_memory_bytes: Some(1100 << 10),
        ..opts()
    };
    let (answer, sizes, _) = streamed(&snap, q, &budget, &strict(4096)).unwrap();
    assert_eq!(answer, eager(&snap, q, &opts()).unwrap());
    assert!(sizes[0] < 2500, "{sizes:?}");
}

/// Highlighting holds no map whose size a batch would set, so a search of one row per
/// pull runs under a budget that holds its hit list, a page and the highlighted
/// literals it interns, about 1.37 MB, where a map for 4096 literals would not fit.
#[test]
fn highlighting_one_row_per_pull_needs_no_map() {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..4096 {
        data.push_str(&format!(
            "<http://example.org/s{i}> <http://www.w3.org/2000/01/rdf-schema#label> \"fox {i}\" .\n"
        ));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.enable_text(TextConfig::default()).unwrap();
    let snap = s.snapshot();
    let q = "SELECT ?lit { (?s ?sc ?lit) text:query (rdfs:label \"fox\" \"highlight:s:<b> | e:</b>\") }";
    let budget = QueryOptions {
        max_memory_bytes: Some(1400 << 10),
        ..opts()
    };
    let (answer, _, _) = streamed(&snap, q, &budget, &strict(1)).unwrap();
    assert_eq!(answer, eager(&snap, q, &opts()).unwrap());
}
