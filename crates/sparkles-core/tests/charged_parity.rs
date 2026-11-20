//! Budget parity of the operators moved onto charged buffers (item 1183). Each fixture
//! runs one streamed query at every budget of a sweep and must fit every budget that it
//! fitted before the move, with the answer it gave before.
//!
//! Each fixture pins the budgets of its sweep that the operator fitted before the move
//! and the ones it fits now and did not before, so a budget that changes either way
//! fails, and the answer the operator gave before, as a row count and a digest of the
//! rows. `PARITY_PRINT=1` prints the budgets each fixture fits and its answer, which is
//! how the pinned values were read from the operator as it was, at commit 74f229b9, with
//! this file copied onto that commit.
//! Each fixture's sweep is a test of its own, so that no one test runs long enough to
//! reach nextest's limit on a slow runner.

mod common;

use oxrdfio::RdfFormat;
use sparkles_core::Error;
use sparkles_core::io::Source;
use sparkles_core::sparql::{CursorOptions, CursorPlan, QueryOptions, select_cursor};
use sparkles_core::spill::{Spill, SpillOptions};
use sparkles_core::store::{Store, StoreOptions};
use std::hash::{BuildHasher, Hash, Hasher};

const K: u64 = 1 << 10;

/// `n` subjects, each with an integer `p` and one to four `q` objects.
fn store(n: usize) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..n {
        data.push_str(&format!(
            "<urn:s:{i}> <urn:p> \"{}\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n",
            i % 97
        ));
        for j in 0..(1 + i % 4) {
            data.push_str(&format!(
                "<urn:s:{i}> <urn:q> <urn:o:{}> .\n",
                (i * 7 + j * 331) % 3000
            ));
        }
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NQuads,
        None,
    )])
    .unwrap();
    s
}

struct Fixture {
    name: &'static str,
    query: &'static str,
    /// Whether the query may spill, which makes an unordered DISTINCT pass rows on.
    spill: bool,
    /// The budgets swept: from, to and step.
    range: (u64, u64, u64),
    /// The budgets of the sweep the operator fitted before the move, as ranges from and
    /// to inclusive.
    before: &'static [(u64, u64)],
    /// The budgets it fits now and did not before.
    gained: &'static [(u64, u64)],
    /// The rows and their digest before the move.
    answer: (usize, u64),
    /// Whether the query orders its rows, which makes the digest follow their order.
    ordered: bool,
    /// Whether the DISTINCT numbers its rows below an ORDER BY.
    numbered: bool,
    /// Whether a budget of the sweep makes the DISTINCT spill.
    spills: bool,
}

fn within(ranges: &[(u64, u64)], budget: u64) -> bool {
    ranges
        .iter()
        .any(|&(from, to)| (from..=to).contains(&budget))
}

/// The budgets of a sweep in `fits` as ranges from and to inclusive.
fn ranges(fits: &[u64], step: u64) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    for &b in fits {
        match out.last_mut() {
            Some((_, to)) if *to + step == b => *to = b,
            _ => out.push((b, b)),
        }
    }
    out
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "distinct one column",
        query: "SELECT DISTINCT ?o { ?s <urn:q> ?o }",
        spill: false,
        range: (64 * K, 512 * K, 4 * K),
        before: &[(126976, 520192)],
        gained: &[],
        answer: (3000, 11144957503537614256),
        ordered: false,
        numbered: false,
        spills: false,
    },
    Fixture {
        name: "distinct two columns",
        query: "SELECT DISTINCT ?v ?o { ?s <urn:p> ?v . ?s <urn:q> ?o }",
        spill: false,
        range: (512 * K, 4096 * K, 32 * K),
        before: &[(1933312, 4161536)],
        gained: &[(1769472, 1900544)],
        answer: (50000, 9997031802626865456),
        ordered: false,
        numbered: false,
        spills: false,
    },
    Fixture {
        name: "distinct segmented",
        query: "SELECT DISTINCT ?s ?v { ?s <urn:q> ?o . ?s <urn:p> ?v }",
        spill: false,
        range: (16 * K, 512 * K, 4 * K),
        before: &[(57344, 57344), (65536, 520192)],
        gained: &[],
        answer: (20000, 10692260916293497201),
        ordered: false,
        numbered: false,
        spills: false,
    },
    Fixture {
        name: "distinct numbered",
        query: "SELECT DISTINCT ?v ?o { ?s <urn:p> ?v . ?s <urn:q> ?o } ORDER BY ?v",
        spill: true,
        range: (128 * K, 2048 * K, 32 * K),
        before: &[(196608, 2064384)],
        gained: &[],
        answer: (50000, 18443397356448823050),
        ordered: true,
        numbered: true,
        spills: true,
    },
    // Subjects 3000 apart have the same objects, so each pair of a subject's objects
    // recurs every 3000 subjects, across batches and spills, and pairs tied on `?o`
    // arrive as A, B, A. The order of their first rows differs from that of their last.
    Fixture {
        name: "distinct numbered duplicates",
        query: "SELECT DISTINCT ?o ?x { ?s <urn:q> ?o . ?s <urn:q> ?x } ORDER BY ?o",
        spill: true,
        range: (64 * K, 4096 * K, 32 * K),
        before: &[(196608, 196608), (262144, 4161536)],
        gained: &[],
        answer: (15000, 10725031878691118488),
        ordered: true,
        numbered: true,
        spills: true,
    },
    Fixture {
        name: "distinct one column spilled",
        query: "SELECT DISTINCT ?o { ?s <urn:q> ?o }",
        spill: true,
        range: (16 * K, 512 * K, 4 * K),
        before: &[(57344, 520192)],
        gained: &[(40960, 53248)],
        answer: (3000, 11144957503537614256),
        ordered: false,
        numbered: false,
        spills: true,
    },
    Fixture {
        name: "distinct spilled",
        query: "SELECT DISTINCT ?v ?o { ?s <urn:p> ?v . ?s <urn:q> ?o }",
        spill: true,
        range: (64 * K, 2048 * K, 32 * K),
        before: &[(131072, 2064384)],
        gained: &[(98304, 98304)],
        answer: (50000, 9997031802626865456),
        ordered: false,
        numbered: false,
        spills: true,
    },
];

/// The row count and a digest of the rows of `f` at `budget`, which follows the rows'
/// order when the fixture orders them, and the bytes its DISTINCT spilled. The plan
/// must run the DISTINCT the fixture names.
fn answer(
    s: &Store,
    f: &Fixture,
    budget: u64,
    dir: &std::path::Path,
) -> Result<((usize, u64), u64), Error> {
    let spill = f.spill.then(|| {
        Spill::open(&SpillOptions {
            dir: dir.to_path_buf(),
            max_bytes: 1 << 30,
            query_bytes: 1 << 30,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap()
    });
    let opts = QueryOptions {
        no_cache: true,
        max_memory_bytes: Some(budget),
        spill,
        ..Default::default()
    };
    let cursor = CursorOptions {
        batch_rows: 1024,
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), f.query, &opts, &cursor)?;
    let (mut rows, mut digest) = (0, 0u64);
    while let Some(b) = c.next_batch()? {
        for row in 0..b.len() {
            let mut h = rustc_hash::FxBuildHasher.build_hasher();
            for column in 0..b.width() {
                format!("{:?}", b.term(row, column)?).hash(&mut h);
            }
            digest = if f.ordered {
                digest
                    .wrapping_mul(0x100_0000_01b3)
                    .wrapping_add(h.finish())
            } else {
                digest.wrapping_add(h.finish())
            };
        }
        rows += b.len();
    }
    let spilled = distinct(f, c.plan());
    Ok(((rows, digest), spilled))
}

/// The bytes the plan's one DISTINCT spilled. A numbered DISTINCT has one column more
/// than its input, the row number, and lies below an ORDER BY.
fn distinct(f: &Fixture, plan: &CursorPlan) -> u64 {
    fn walk<'a>(
        plan: &'a CursorPlan,
        path: &mut Vec<&'a str>,
        out: &mut Vec<(&'a CursorPlan, bool)>,
    ) {
        if plan.operator.operator == "Distinct" {
            out.push((plan, path.contains(&"OrderBy")));
        }
        path.push(&plan.operator.operator);
        for child in &plan.children {
            walk(child, path, out);
        }
        path.pop();
    }
    let mut found = Vec::new();
    walk(plan, &mut Vec::new(), &mut found);
    assert_eq!(found.len(), 1, "{}: one DISTINCT in {plan:?}", f.name);
    let (node, ordered) = found[0];
    assert!(!node.materializes, "{}: a native DISTINCT", f.name);
    let numbered = ordered
        && node.children.len() == 1
        && node.operator.columns.len() == node.children[0].operator.columns.len() + 1;
    assert_eq!(
        numbered, f.numbered,
        "{}: numbered DISTINCT in {plan:?}",
        f.name
    );
    node.operator
        .counters
        .as_ref()
        .and_then(|c| c.get("spilledBytes"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

/// The fixture of FIXTURES named `name`.
fn fixture(name: &str) -> &'static Fixture {
    FIXTURES.iter().find(|f| f.name == name).unwrap()
}

/// The fixture named `name` fits each budget it fitted before the move, with the answer it
/// gave before, and fits no budget beyond those its row names.
fn sweep(name: &str) {
    let f = fixture(name);
    let s = store(20_000);
    let tmp = tempfile::tempdir().unwrap();
    let print = std::env::var_os("PARITY_PRINT").is_some();
    let (from, to, step) = f.range;
    let (expected, _) = answer(&s, f, 1 << 30, tmp.path()).unwrap();
    let (mut fits, mut spilled) = (Vec::new(), false);
    for budget in (from..to).step_by(step as usize) {
        match answer(&s, f, budget, tmp.path()) {
            Ok((answer, spill)) => {
                assert_eq!(answer, expected, "{}, {budget} bytes", f.name);
                spilled |= spill > 0;
                fits.push(budget);
            }
            Err(e) => {
                if print {
                    println!("MISS {} {budget}: {e}", f.name);
                }
                assert!(common::heap::refused(&e), "{}, {budget} bytes: {e}", f.name)
            }
        }
    }
    if print {
        println!(
            "PARITY {:28} answer {expected:?} spilled {spilled} fits {:?}",
            f.name,
            ranges(&fits, step)
        );
        return;
    }
    assert_eq!(expected, f.answer, "{}: the answer before the move", f.name);
    assert_eq!(spilled, f.spills, "{}: a budget that spills", f.name);
    assert!(
        !fits.is_empty() && fits.len() < ((to - from) / step) as usize,
        "{}: the sweep crosses no threshold",
        f.name
    );
    for budget in (from..to).step_by(step as usize) {
        let fit = fits.contains(&budget);
        assert!(
            !within(f.before, budget) || fit,
            "{}: fitted {budget} bytes before the move and does not now",
            f.name
        );
        assert_eq!(
            fit,
            within(f.before, budget) || within(f.gained, budget),
            "{}: {budget} bytes, which the fixture's pinned budgets do not name",
            f.name
        );
    }
    println!("PARITY {:28} fits {:?}", f.name, ranges(&fits, step));
}

/// One test per fixture, named after it, which sweeps that fixture.
macro_rules! sweeps {
    ($($test:ident => $name:literal,)*) => {
        $(
            #[test]
            fn $test() {
                sweep($name);
            }
        )*

        /// Every fixture has its test.
        #[test]
        fn every_fixture_is_swept() {
            let swept = [$($name),*];
            for f in FIXTURES {
                assert!(swept.contains(&f.name), "{} has no sweep", f.name);
            }
            assert_eq!(swept.len(), FIXTURES.len());
        }
    };
}

sweeps! {
    distinct_one_column_fits_every_budget_it_fitted_before => "distinct one column",
    distinct_two_columns_fits_every_budget_it_fitted_before => "distinct two columns",
    distinct_segmented_fits_every_budget_it_fitted_before => "distinct segmented",
    distinct_numbered_fits_every_budget_it_fitted_before => "distinct numbered",
    distinct_numbered_duplicates_fits_every_budget_it_fitted_before => "distinct numbered duplicates",
    distinct_one_column_spilled_fits_every_budget_it_fitted_before => "distinct one column spilled",
    distinct_spilled_fits_every_budget_it_fitted_before => "distinct spilled",
}

/// The rows of `query` at an unlimited budget, each as the text of its terms.
fn rows(s: &Store, query: &str) -> Vec<Vec<String>> {
    let opts = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), query, &opts, &CursorOptions::default()).unwrap();
    let mut out = Vec::new();
    while let Some(b) = c.next_batch().unwrap() {
        for row in 0..b.len() {
            out.push(
                (0..b.width())
                    .map(|column| format!("{:?}", b.term(row, column).unwrap()))
                    .collect(),
            );
        }
    }
    out
}

/// The numbered DISTINCT of the duplicates fixture keeps each row's first occurrence
/// among the rows tied on the order key, which the order of the rows' last occurrences
/// would not give, so the fixture's pinned answer tells the two apart.
#[test]
fn the_numbered_duplicates_keep_their_first_rows() {
    let s = store(20_000);
    let f = fixture("distinct numbered duplicates");
    let all = rows(&s, &f.query.replacen("DISTINCT ", "", 1));
    let mut seen = std::collections::HashSet::new();
    let first: Vec<_> = all.iter().filter(|r| seen.insert(*r)).cloned().collect();
    let mut seen = std::collections::HashSet::new();
    let mut last: Vec<_> = all
        .iter()
        .rev()
        .filter(|r| seen.insert(*r))
        .cloned()
        .collect();
    last.reverse();
    assert!(all.len() > first.len(), "the fixture has no duplicates");
    assert_ne!(first, last, "the first and last rows give one order");
    assert_eq!(rows(&s, f.query), first);
}
