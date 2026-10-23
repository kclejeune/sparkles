//! An automatic SELECT whose eager run exceeds its memory budget runs again as a cursor
//! when every operator of its plan streams, also without disk spill. SP2Bench v1.01 q4
//! joins articles of the same journal before a FILTER and a DISTINCT keep few of the
//! pairs. Eager execution holds the whole join, and the cursor holds only the smaller
//! join input and the distinct pairs.
use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    CursorOptions, ExecutionMode, QueryExecution, QueryOptions, query, query_execution,
};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};

const Q4: &str = include_str!("fixtures/sp2b-q4.rq");

const JOURNALS: usize = 8;
const ARTICLES: usize = 60;

fn fixture() -> Store {
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::from(
        "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
         @prefix bench: <http://localhost/vocabulary/bench/> .\n\
         @prefix dc: <http://purl.org/dc/elements/1.1/> .\n\
         @prefix foaf: <http://xmlns.com/foaf/0.1/> .\n\
         @prefix swrc: <http://swrc.ontoware.org/ontology#> .\n",
    );
    for j in 0..JOURNALS {
        for a in 0..ARTICLES {
            // authors recur across journals, so DISTINCT removes repeated pairs
            let person = (j * 17 + a) % 200;
            data.push_str(&format!(
                "<urn:article:{j}:{a}> rdf:type bench:Article ; dc:creator <urn:person:{person}> ; \
                 swrc:journal <urn:journal:{j}> .\n"
            ));
        }
    }
    for p in 0..200 {
        data.push_str(&format!("<urn:person:{p}> foaf:name \"Person {p:03}\" .\n"));
    }
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    store
}

type Rows = Vec<Vec<Option<Term>>>;

fn sorted(mut rows: Rows) -> Vec<String> {
    let mut out: Vec<String> = rows.drain(..).map(|r| format!("{r:?}")).collect();
    out.sort();
    out
}

/// The rows of an automatic execution, and its cursor plan when it ran as a cursor.
fn automatic(s: &Store, q: &str, opts: &QueryOptions) -> Result<(Rows, Option<String>), Error> {
    match query_execution(
        s.snapshot(),
        q,
        opts,
        &CursorOptions::default(),
        ExecutionMode::Auto,
    )? {
        QueryExecution::Select(mut c) => {
            let mut rows = Vec::new();
            while let Some(b) = c.next_batch()? {
                for i in 0..b.len() {
                    rows.push(b.row(i).unwrap());
                }
            }
            Ok((rows, Some(c.plan_json()?)))
        }
        QueryExecution::Eager(r) => Ok((r.into_result().rows(), None)),
        _ => unreachable!("a SELECT"),
    }
}

/// A budget the eager join's 28,800 rows of seven columns exceed, while the cursor's
/// join input of 480 rows and its distinct pairs fit.
const MEMORY: u64 = 1 << 20;

#[test]
fn q4_eager_memory_failure_runs_again_as_a_cursor_without_spill() {
    let s = fixture();
    let expected = sorted(query(s.snapshot(), Q4, &Default::default()).unwrap().rows());
    assert!(expected.len() > 1000, "{}", expected.len());
    let opts = QueryOptions {
        max_memory_bytes: Some(MEMORY),
        no_cache: true,
        ..Default::default()
    };
    // Eager execution alone fails within the budget.
    let eager = query(s.snapshot(), Q4, &opts);
    assert!(
        matches!(&eager, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "{:?}",
        eager.err()
    );
    let (rows, plan) = automatic(&s, Q4, &opts).unwrap();
    assert_eq!(sorted(rows), expected);
    let plan = plan.expect("a retried cursor");
    assert!(plan.contains("\"eagerMemoryRetry\":1"), "{plan}");
}

#[test]
fn a_retry_that_the_cursor_cannot_hold_still_fails_within_the_budget() {
    let s = fixture();
    // ORDER BY over the join holds every row in the cursor's sort too, and without
    // spill the retried cursor exceeds the same budget.
    let q = Q4.replace("SELECT DISTINCT", "SELECT").replace(
        "FILTER (?name1<?name2)\n}",
        "FILTER (?name1<?name2)\n} ORDER BY (CONCAT(?name1, ?name2))",
    );
    let opts = QueryOptions {
        max_memory_bytes: Some(MEMORY),
        no_cache: true,
        ..Default::default()
    };
    assert!(matches!(
        automatic(&s, &q, &opts),
        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory
    ));
}
