//! Query-local state is charged to the query in both accounting modes: the terms that
//! expressions compute are retained until the query ends, so a budget that cannot hold
//! them fails the query instead of leaving the values unbound (item 453 stage 4).

use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{CursorOptions, QueryOptions, query, select_cursor};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};

const ROWS: usize = 50_000;
const Q: &str = "SELECT ?s ?u WHERE { ?s <urn:p> ?o BIND(STRUUID() AS ?u) }";

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let data: String = (0..ROWS)
        .map(|i| format!("<urn:s{i}> <urn:p> \"{i}\" .\n"))
        .collect();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s
}

fn budget(bytes: u64) -> QueryOptions {
    QueryOptions {
        max_memory_bytes: Some(bytes),
        ..Default::default()
    }
}

fn is_memory(error: &Error) -> bool {
    matches!(error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory)
}

/// Each row's new UUID string is a query-local term of about 130 bytes, which 50,000
/// rows cannot keep within 3 MiB, while the solutions themselves fit.
#[test]
fn computed_terms_beyond_the_budget_fail_the_query_in_both_modes() {
    let s = store();
    let all = query(s.snapshot(), Q, &Default::default()).unwrap();
    let rows = all.rows();
    assert_eq!(rows.len(), ROWS);
    assert!(rows.iter().all(|r| r.iter().all(Option::is_some)));

    let eager = query(s.snapshot(), Q, &budget(3 << 20));
    match eager {
        Err(error) => assert!(is_memory(&error), "{error:?}"),
        Ok(r) => panic!("eager answered {} rows within the budget", r.rows().len()),
    }

    let cursor = (|| -> sparkles_core::Result<usize> {
        let mut c = select_cursor(s.snapshot(), Q, &budget(3 << 20), &CursorOptions::default())?;
        let mut n = 0;
        while let Some(b) = c.next_batch()? {
            for i in 0..b.len() {
                assert!(b.row(i)?.iter().all(Option::is_some));
            }
            n += b.len();
        }
        Ok(n)
    })();
    match cursor {
        Err(error) => assert!(is_memory(&error), "{error:?}"),
        Ok(n) => panic!("the cursor answered {n} rows within the budget"),
    }
}
