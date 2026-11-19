//! Budget parity of the operators moved onto charged buffers (item 1183). Each fixture
//! runs one streamed query at every budget of a range and must fit every budget that it
//! fitted before the move, within the slack its row states, with the same answer.
//!
//! The budgets the operator fitted before the move are pinned as the smallest fit of
//! each fixture, read from the operator as it was. `PARITY_PRINT=1` prints the budgets
//! each fixture fits now, which is how the pinned values were read.

mod common;

use common::heap;
use oxrdfio::RdfFormat;
use sparkles_core::Error;
use sparkles_core::io::Source;
use sparkles_core::sparql::{CursorOptions, QueryOptions, select_cursor};
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
    /// The smallest budget the operator fitted before the move.
    before: u64,
    /// How far above a budget it fitted before the move may need now.
    slack: u64,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "distinct one column",
        query: "SELECT DISTINCT ?o { ?s <urn:q> ?o }",
        spill: false,
        range: (64 * K, 512 * K, 4 * K),
        before: 126976,
        slack: 0,
    },
    Fixture {
        name: "distinct two columns",
        query: "SELECT DISTINCT ?v ?o { ?s <urn:p> ?v . ?s <urn:q> ?o }",
        spill: false,
        range: (512 * K, 4096 * K, 32 * K),
        before: 1933312,
        slack: 0,
    },
    Fixture {
        name: "distinct segmented",
        query: "SELECT DISTINCT ?s ?v { ?s <urn:q> ?o . ?s <urn:p> ?v }",
        spill: false,
        range: (16 * K, 512 * K, 4 * K),
        before: 65536,
        slack: 0,
    },
    Fixture {
        name: "distinct numbered",
        query: "SELECT DISTINCT ?v ?o { ?s <urn:p> ?v . ?s <urn:q> ?o } ORDER BY ?v",
        spill: false,
        range: (512 * K, 8192 * K, 64 * K),
        before: 6619136,
        slack: 0,
    },
    Fixture {
        name: "distinct one column spilled",
        query: "SELECT DISTINCT ?o { ?s <urn:q> ?o }",
        spill: true,
        range: (16 * K, 512 * K, 4 * K),
        before: 57344,
        slack: 0,
    },
    Fixture {
        name: "distinct spilled",
        query: "SELECT DISTINCT ?v ?o { ?s <urn:p> ?v . ?s <urn:q> ?o }",
        spill: true,
        range: (64 * K, 2048 * K, 32 * K),
        before: 131072,
        slack: 0,
    },
];

/// The row count and an order-free digest of the rows of `f` at `budget`.
fn answer(
    s: &Store,
    f: &Fixture,
    budget: u64,
    dir: &std::path::Path,
) -> Result<(usize, u64), Error> {
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
            digest = digest.wrapping_add(h.finish());
        }
        rows += b.len();
    }
    Ok((rows, digest))
}

/// Every DISTINCT fixture fits each budget it fitted before the move, with the same
/// answer.
#[test]
fn distinct_fits_every_budget_it_fitted_before() {
    let s = store(20_000);
    let tmp = tempfile::tempdir().unwrap();
    for f in FIXTURES {
        let expected = answer(&s, f, 1 << 30, tmp.path()).unwrap();
        let (from, to, step) = f.range;
        if std::env::var_os("PARITY_PRINT").is_some() {
            let fit = heap::fits(from..to, step, |b| answer(&s, f, b, tmp.path()).map(|_| ()));
            println!(
                "PARITY {:24} smallest fit {:?} of {} fits, largest miss {:?}",
                f.name,
                fit.first(),
                fit.len(),
                (from..to)
                    .step_by(step as usize)
                    .filter(|b| !fit.contains(b))
                    .last()
            );
            continue;
        }
        let refusal = || answer(&s, f, 1 << 10, tmp.path()).unwrap_err();
        let worst = heap::parity(
            f.name,
            from..to,
            step,
            f.slack,
            |b| {
                if b >= f.before {
                    Ok(expected)
                } else {
                    Err(refusal())
                }
            },
            |b| answer(&s, f, b, tmp.path()),
        );
        println!("PARITY {:24} needed {worst} bytes more", f.name);
    }
}
