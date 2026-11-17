//! A cursor scan over an immutable snapshot keeps its place inside an index block
//! between batches without charging the block to the query, so a larger memory budget
//! never refuses a query that a smaller one answers (roadmap item 1039). A scan that kept
//! its block charged it at a whole block's columns, about 1 MiB, once the budget had room
//! for it, and the rest of the query then had about 1 MiB less than under a slightly
//! smaller budget, under which the scan took no block.
use sparkles_core::index::BLOCK_ROWS;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    CursorOptions, ExecutionMode, QueryExecution, QueryOptions, query_execution,
};
use sparkles_core::store::{Store, StoreOptions};

const KIB: u64 = 1024;

/// `subjects` subjects, each of class `<urn:P>` with one name, in a compacted store
/// without a result cache.
fn fixture(subjects: usize) -> Store {
    let store = Store::in_memory(StoreOptions {
        result_cache_bytes: 0,
        ..StoreOptions::default()
    });
    let mut data = String::new();
    for i in 0..subjects {
        data.push_str(&format!(
            "<urn:s{i}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:P> .\n\
             <urn:s{i}> <urn:name> \"n{i}\" .\n"
        ));
    }
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    store.compact().unwrap();
    store
}

/// A count over a path closure that starts at each subject of the class.
const COUNT: &str =
    "SELECT (COUNT(*) AS ?n) WHERE { ?s a <urn:P> . ?s (<urn:name>|<urn:none>)* ?v }";
/// The same closure's rows under a LIMIT that it does not fill.
const LIMIT: &str =
    "SELECT ?s ?v WHERE { ?s a <urn:P> . ?s (<urn:name>|<urn:none>)* ?v } LIMIT 100000000";

/// The rows and the peak charge of `query` under a budget of `kib` KiB, or the error.
fn run(store: &Store, query: &str, kib: u64, mode: ExecutionMode) -> Result<(u64, u64), String> {
    let options = QueryOptions {
        max_memory_bytes: Some(kib * KIB),
        no_cache: true,
        ..Default::default()
    };
    let execution = query_execution(
        store.snapshot(),
        query,
        &options,
        &CursorOptions::default(),
        mode,
    )
    .map_err(|e| e.to_string())?;
    match execution {
        QueryExecution::Select(mut cursor) => {
            let mut rows = 0;
            while let Some(batch) = cursor.next_batch().map_err(|e| e.to_string())? {
                for i in 0..batch.len() {
                    std::hint::black_box(batch.row(i).map_err(|e| e.to_string())?);
                }
                rows += batch.len() as u64;
            }
            Ok((rows, cursor.stats().mem_peak_bytes))
        }
        QueryExecution::Eager(result) => {
            let (rows, peak) = (result.result().len() as u64, result.result().mem_peak_bytes);
            result.into_result().rows().map_err(|e| e.to_string())?;
            Ok((rows, peak))
        }
        _ => Err("not a SELECT".into()),
    }
}

/// Run `query` in `mode` at each budget of `budgets`, in order, reading every row's
/// values, and check that once it answers it answers at every larger budget, with the
/// same rows, and with `flat` a peak that grows by less than a block's charge. Returns
/// the number of budgets it answered at.
fn monotone(
    store: &Store,
    query: &str,
    mode: ExecutionMode,
    budgets: impl IntoIterator<Item = u64>,
    flat: bool,
) -> usize {
    let mut first: Option<(u64, u64, u64)> = None;
    let mut answered = 0;
    for kib in budgets {
        let result = run(store, query, kib, mode);
        match (&first, result) {
            (None, Ok((rows, peak))) => first = Some((kib, rows, peak)),
            (None, Err(_)) => continue,
            (Some((at, rows, peak)), Ok((n, p))) => {
                assert_eq!(n, *rows, "{mode:?} at {kib} KiB: rows");
                assert!(
                    !flat || p < peak + 256 * KIB,
                    "{mode:?}: the peak grew from {peak} at {at} KiB to {p} at {kib} KiB"
                );
            }
            (Some((at, _, _)), Err(e)) => {
                panic!("{mode:?} answered at {at} KiB and is refused at {kib} KiB: {e}")
            }
        }
        answered += 1;
    }
    answered
}

/// On 2,000 subjects the scan reads part of one block. A scan that kept its block was
/// refused the count under automatic and streaming execution at 1,216 and 1,280 KiB, and
/// the LIMIT in every mode at 1,152 and 1,216 KiB, while each answered within 512 KiB.
#[test]
fn small_scan_answers_at_every_larger_budget() {
    let store = fixture(2_000);
    let budgets = || (512..=1_536).step_by(32);
    for mode in [ExecutionMode::Auto, ExecutionMode::Streaming] {
        assert!(
            monotone(&store, COUNT, mode, budgets(), true) > 0,
            "{mode:?}"
        );
    }
    for mode in [
        ExecutionMode::Eager,
        ExecutionMode::Auto,
        ExecutionMode::Streaming,
    ] {
        assert!(
            monotone(&store, LIMIT, mode, budgets(), true) > 0,
            "{mode:?}"
        );
    }
}

/// With twice a block of subjects of the class, the scan reads at least one whole
/// block of 32,768 rows, since no block holds more. A scan that kept its block was
/// refused the count under streaming execution from 1,216 to 1,472 KiB, and under
/// automatic execution there too after it answered at 1,184 KiB, and the LIMIT under
/// streaming execution from 1,152 to 1,408 KiB, while each answered within 1,024 KiB.
#[test]
fn full_block_scan_answers_at_every_larger_budget() {
    let subjects = 2 * BLOCK_ROWS + 4_000;
    let store = fixture(subjects);
    let budgets = || (1_024..=1_600).step_by(32);
    for (query, mode) in [
        (COUNT, ExecutionMode::Streaming),
        (COUNT, ExecutionMode::Auto),
        (LIMIT, ExecutionMode::Streaming),
    ] {
        assert!(
            monotone(&store, query, mode, budgets(), true) > 0,
            "{mode:?}"
        );
    }
}

/// A scan at the root hands out its block's rows without copying them while the block's
/// charge fits, and copies them otherwise, so its answers too are monotone in the
/// budget. Its peak grows where the batches start to hold the block.
#[test]
fn root_scan_answers_at_every_larger_budget() {
    let store = fixture(2 * BLOCK_ROWS + 4_000);
    for mode in [ExecutionMode::Auto, ExecutionMode::Streaming] {
        let answered = monotone(
            &store,
            "SELECT ?s ?v WHERE { ?s <urn:name> ?v }",
            mode,
            (192..=2_048).step_by(128),
            false,
        );
        assert!(answered > 0, "{mode:?}");
    }
}
