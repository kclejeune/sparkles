//! Public decoded cells/rows keep their real query contribution until final drop.
use oxrdf::Term;
use sparkles_core::sparql::output::{OwnedRow, OwnedTerm};
use sparkles_core::sparql::{CursorOptions, QueryOptions, pool::QueryPool, select_cursor};
use sparkles_core::store::{Store, StoreOptions};

// Test-only borrows preserve the same regression body across the adopted API
// transition. They never clone a value or make an application copy to hide it.
trait CellBorrow {
    fn borrowed_term(&self) -> &Term;
}
impl CellBorrow for Term {
    fn borrowed_term(&self) -> &Term {
        self
    }
}
impl CellBorrow for OwnedTerm {
    fn borrowed_term(&self) -> &Term {
        self.as_term()
    }
}
trait RowBorrow {
    fn borrowed_values(&self) -> &[Option<Term>];
}
impl RowBorrow for Vec<Option<Term>> {
    fn borrowed_values(&self) -> &[Option<Term>] {
        self
    }
}
impl RowBorrow for OwnedRow {
    fn borrowed_values(&self) -> &[Option<Term>] {
        self.values()
    }
}

fn fixture() -> Store {
    let store = Store::in_memory(StoreOptions::default());
    let mut tx = store.write();
    let subject = tx
        .intern(&oxrdf::NamedNode::new_unchecked("urn:s").into())
        .unwrap();
    let predicate = tx
        .intern(&oxrdf::NamedNode::new_unchecked("urn:p").into())
        .unwrap();
    let object = tx
        .intern(&oxrdf::Literal::new_simple_literal("x".repeat(64 << 10)).into())
        .unwrap();
    tx.insert([
        subject,
        predicate,
        object,
        sparkles_core::id::Id::DEFAULT_GRAPH,
    ])
    .unwrap();
    tx.commit().unwrap();
    store
}

#[test]
fn retained_batch_cells_keep_real_credit_after_cursor_and_batch_close() {
    let store = fixture();
    let pool = QueryPool::new(16 << 20);
    let options = QueryOptions {
        pool: Some(pool.clone()),
        max_memory_bytes: Some(4 << 20),
        no_cache: true,
        ..Default::default()
    };
    let mut cursor = select_cursor(
        store.snapshot(),
        "SELECT ?o WHERE { <urn:s> <urn:p> ?o }",
        &options,
        &CursorOptions {
            batch_rows: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let batch = cursor.next_batch().unwrap().unwrap();
    let mut retained = Vec::new();
    for _ in 0..32 {
        retained.push(batch.term(0, 0).unwrap().unwrap());
    }
    cursor.close();
    drop(cursor);
    drop(batch);
    assert_eq!(retained.len(), 32);
    let Term::Literal(value) = retained[0].borrowed_term() else {
        panic!("stored literal");
    };
    assert_eq!(value.value().len(), 64 << 10);
    assert!(
        pool.granted() >= 32 * (64 << 10),
        "real retained cells must keep their original query credit after cursor and batch drop; observed {}",
        pool.granted()
    );
    drop(retained);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn retained_batch_rows_keep_real_credit_after_cursor_and_batch_close() {
    let store = fixture();
    let pool = QueryPool::new(16 << 20);
    let options = QueryOptions {
        pool: Some(pool.clone()),
        max_memory_bytes: Some(4 << 20),
        no_cache: true,
        ..Default::default()
    };
    let mut cursor = select_cursor(
        store.snapshot(),
        "SELECT ?o WHERE { <urn:s> <urn:p> ?o }",
        &options,
        &CursorOptions {
            batch_rows: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let batch = cursor.next_batch().unwrap().unwrap();
    let mut retained = Vec::new();
    for _ in 0..32 {
        retained.push(batch.row(0).unwrap());
    }
    cursor.close();
    drop(cursor);
    drop(batch);
    assert_eq!(retained.len(), 32);
    let Term::Literal(value) = retained[0].borrowed_values()[0].as_ref().unwrap() else {
        panic!("stored literal");
    };
    assert_eq!(value.value().len(), 64 << 10);
    assert!(
        pool.granted() >= 32 * (64 << 10),
        "real retained rows must keep their original query credit after cursor and batch drop; observed {}",
        pool.granted()
    );
    drop(retained);
    assert_eq!(pool.granted(), 0);
}

fn real_batch(
    memory: u64,
) -> (
    std::sync::Arc<sparkles_core::sparql::pool::QueryPool>,
    sparkles_core::sparql::QueryCursor,
    sparkles_core::sparql::QueryBatch,
) {
    let store = fixture();
    let pool = QueryPool::new(16 << 20);
    let options = QueryOptions {
        pool: Some(pool.clone()),
        max_memory_bytes: Some(memory),
        no_cache: true,
        ..Default::default()
    };
    let mut cursor = select_cursor(
        store.snapshot(),
        "SELECT ?o WHERE { <urn:s> <urn:p> ?o }",
        &options,
        &CursorOptions {
            batch_rows: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let batch = cursor.next_batch().unwrap().unwrap();
    (pool, cursor, batch)
}

#[test]
fn decoded_cell_clones_keep_one_physical_backing_through_one_and_eight_threads() {
    use std::sync::{Arc, Barrier};
    for threads in [1, 8] {
        let (pool, mut cursor, batch) = real_batch(4 << 20);
        let term = batch.term(0, 0).unwrap().unwrap();
        cursor.close();
        drop(cursor);
        drop(batch);
        let held = pool.granted();
        assert!(held >= 64 << 10);
        let ready = Arc::new(Barrier::new(threads + 1));
        let release = Arc::new(Barrier::new(threads + 1));
        let mut workers = Vec::new();
        for _ in 0..threads {
            let (term, ready, release) = (term.clone(), ready.clone(), release.clone());
            workers.push(std::thread::spawn(move || {
                let Term::Literal(value) = term.as_term() else {
                    panic!("literal");
                };
                assert_eq!(value.value().len(), 64 << 10);
                ready.wait();
                release.wait();
                drop(term);
            }));
        }
        ready.wait();
        assert_eq!(
            pool.granted(),
            held,
            "cloning a cell cannot allocate another payload or mint credit"
        );
        drop(term);
        assert_eq!(
            pool.granted(),
            held,
            "actual thread owners still retain the payload"
        );
        release.wait();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(pool.granted(), 0);
    }
}

#[test]
fn aggregate_decoded_output_refuses_without_retiring_existing_cells() {
    let (pool, mut cursor, batch) = real_batch(4 << 20);
    let mut retained = Vec::new();
    let mut refused = false;
    for _ in 0..128 {
        match batch.term(0, 0) {
            Ok(Some(term)) => retained.push(term),
            Err(sparkles_core::Error::BudgetExceeded(budget))
                if budget.kind == sparkles_core::BudgetKind::Memory =>
            {
                refused = true;
                break;
            }
            _ => panic!("only original local-memory refusal can end this retained-output test"),
        }
    }
    assert!(refused && retained.len() >= 32 && retained.len() < 128);
    assert!(pool.granted() >= retained.len() as u64 * (64 << 10));
    cursor.close();
    drop(cursor);
    drop(batch);
    assert!(pool.granted() >= retained.len() as u64 * (64 << 10));
    for term in &retained {
        let Term::Literal(value) = term.as_term() else {
            panic!("literal");
        };
        assert_eq!(value.value().len(), 64 << 10);
    }
    drop(retained);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn receiving_query_copy_refusal_preserves_both_original_accounts() {
    let (pool, mut cursor, batch) = real_batch(4 << 20);
    let term = batch.term(0, 0).unwrap().unwrap();
    cursor.close();
    drop(cursor);
    drop(batch);
    let source_credit = pool.granted();
    let receiver_store = Store::in_memory(Default::default());
    let receiving_pool = QueryPool::new(16 << 20);
    let options = QueryOptions {
        pool: Some(receiving_pool.clone()),
        max_memory_bytes: Some(64 << 10),
        ..Default::default()
    };
    let mut receiving = select_cursor(
        receiver_store.snapshot(),
        "SELECT ?x WHERE { VALUES ?x { 1 } }",
        &options,
        &CursorOptions::default(),
    )
    .unwrap();
    let input = receiving.next_batch().unwrap().unwrap();
    let destination = input.output_budget();
    let receiving_credit = receiving_pool.granted();
    assert!(
        matches!(term.try_copy_in(&destination), Err(sparkles_core::Error::BudgetExceeded(budget)) if budget.kind == sparkles_core::BudgetKind::Memory)
    );
    assert_eq!(pool.granted(), source_credit);
    assert_eq!(receiving_pool.granted(), receiving_credit);
    receiving.close();
    drop(receiving);
    drop(input);
    drop(destination);
    assert_eq!(receiving_pool.granted(), 0);
    assert_eq!(pool.granted(), source_credit);
    drop(term);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn decoded_batch_checks_controls_before_unbound_inline_and_complete_row_success() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let store = Store::in_memory(Default::default());
    let cancel = Arc::new(AtomicBool::new(false));
    let options = QueryOptions {
        cancel: Some(cancel.clone()),
        ..Default::default()
    };
    let mut cursor = select_cursor(
        store.snapshot(),
        "SELECT ?x ?y WHERE { VALUES (?x ?y) { (UNDEF 1) } }",
        &options,
        &CursorOptions::default(),
    )
    .unwrap();
    let batch = cursor.next_batch().unwrap().unwrap();
    assert!(batch.term(0, 0).unwrap().is_none());
    assert!(batch.term(0, 1).unwrap().is_some());
    assert!(matches!(
        batch.term(1, 0),
        Err(sparkles_core::Error::Invalid(_))
    ));
    assert!(matches!(
        batch.term(0, 2),
        Err(sparkles_core::Error::Invalid(_))
    ));
    assert!(matches!(
        batch.row(1),
        Err(sparkles_core::Error::Invalid(_))
    ));
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(
        batch.term(0, 0),
        Err(sparkles_core::Error::Cancelled)
    ));
    assert!(matches!(
        batch.term(0, 1),
        Err(sparkles_core::Error::Cancelled)
    ));
    assert!(matches!(batch.row(0), Err(sparkles_core::Error::Cancelled)));
}

#[test]
fn decoded_zero_width_row_keeps_shape_and_header_credit_until_last_clone() {
    let store = Store::in_memory(Default::default());
    let pool = QueryPool::new(16 << 20);
    let options = QueryOptions {
        pool: Some(pool.clone()),
        max_memory_bytes: Some(4 << 20),
        ..Default::default()
    };
    let mut cursor = select_cursor(
        store.snapshot(),
        "SELECT * WHERE { VALUES () { () () } }",
        &options,
        &CursorOptions::default(),
    )
    .unwrap();
    let batch = cursor.next_batch().unwrap().unwrap();
    assert_eq!((batch.len(), batch.width()), (2, 0));
    let row = batch.row(1).unwrap();
    let last = row.clone();
    cursor.close();
    drop(cursor);
    drop(batch);
    assert!(row.is_empty());
    assert!(
        pool.granted() > 0,
        "a zero-width row still owns its actual backing header"
    );
    drop(row);
    assert!(pool.granted() > 0);
    drop(last);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn retained_batch_variable_names_keep_original_credit_after_cursor_close() {
    let store = Store::in_memory(Default::default());
    let pool = QueryPool::new(32 << 20);
    let name = "v".repeat(128 << 10);
    let query = format!("SELECT ?{name} WHERE {{ VALUES ?{name} {{ 1 }} }}");
    let options = QueryOptions {
        pool: Some(pool.clone()),
        max_memory_bytes: Some(16 << 20),
        no_cache: true,
        ..Default::default()
    };
    let mut cursor = select_cursor(
        store.snapshot(),
        &query,
        &options,
        &CursorOptions {
            batch_rows: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let batch = cursor.next_batch().unwrap().unwrap();
    assert_eq!(batch.variables(), &[name.clone()]);
    cursor.close();
    drop(cursor);
    assert_eq!(batch.variables()[0].len(), 128 << 10);
    assert!(
        pool.granted() >= batch.variables()[0].len() as u64,
        "the actual retained variable-name allocation must keep its original credit; observed {}",
        pool.granted()
    );
    drop(batch);
    assert_eq!(pool.granted(), 0);
}

// The adapters below only move old returned payloads or borrow new immutable
// owners. They preserve the semantic regression body across the eager API cut.
trait CheckedCell {
    type Cell: CellBorrow;
    fn checked(self) -> sparkles_core::Result<Option<Self::Cell>>;
}
impl CheckedCell for Option<Term> {
    type Cell = Term;
    fn checked(self) -> sparkles_core::Result<Option<Self::Cell>> {
        Ok(self)
    }
}
impl CheckedCell for sparkles_core::Result<Option<OwnedTerm>> {
    type Cell = OwnedTerm;
    fn checked(self) -> sparkles_core::Result<Option<Self::Cell>> {
        self
    }
}
trait FirstDecodedRow {
    type Row: RowBorrow;
    fn first_decoded(self) -> sparkles_core::Result<Self::Row>;
}
impl FirstDecodedRow for Vec<Vec<Option<Term>>> {
    type Row = Vec<Option<Term>>;
    fn first_decoded(self) -> sparkles_core::Result<Self::Row> {
        Ok(self.into_iter().next().expect("one real stored row"))
    }
}
impl FirstDecodedRow for sparkles_core::Result<sparkles_core::sparql::output::OwnedRows> {
    type Row = OwnedRow;
    fn first_decoded(self) -> sparkles_core::Result<Self::Row> {
        self?.row_handle(0)
    }
}
fn first_eager_id(result: &sparkles_core::sparql::QueryResult) -> sparkles_core::id::Id {
    result.table().get(0, 0).unwrap()
}

#[test]
fn retained_eager_cells_keep_original_credit_after_result_drop() {
    let store = fixture();
    let pool = QueryPool::new(16 << 20);
    let result = sparkles_core::sparql::query(
        store.snapshot(),
        "SELECT ?o WHERE { <urn:s> <urn:p> ?o }",
        &QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    let id = first_eager_id(&result);
    let mut retained = Vec::new();
    for _ in 0..32 {
        retained.push(result.term(id).checked().unwrap().unwrap());
    }
    drop(result);
    assert_eq!(retained.len(), 32);
    let Term::Literal(value) = retained[0].borrowed_term() else {
        panic!("stored literal");
    };
    assert_eq!(value.value().len(), 64 << 10);
    assert!(
        pool.granted() >= 32 * (64 << 10),
        "real eager cells must retain original credit after result drop; observed {}",
        pool.granted()
    );
    drop(retained);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn retained_eager_rows_keep_original_credit_after_result_drop() {
    let store = fixture();
    let pool = QueryPool::new(16 << 20);
    let result = sparkles_core::sparql::query(
        store.snapshot(),
        "SELECT ?o WHERE { <urn:s> <urn:p> ?o }",
        &QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    let mut retained = Vec::new();
    for _ in 0..32 {
        retained.push(result.rows().first_decoded().unwrap());
    }
    drop(result);
    assert_eq!(retained.len(), 32);
    let Term::Literal(value) = retained[0].borrowed_values()[0].as_ref().unwrap() else {
        panic!("stored literal");
    };
    assert_eq!(value.value().len(), 64 << 10);
    assert!(
        pool.granted() >= 32 * (64 << 10),
        "real eager rows must retain original credit after result drop; observed {}",
        pool.granted()
    );
    drop(retained);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn immutable_eager_result_clones_share_real_names_and_ids_through_last_drop() {
    use std::sync::{Arc, Barrier};
    for threads in [1, 8] {
        let store = Store::in_memory(Default::default());
        let pool = QueryPool::new(32 << 20);
        let name = "v".repeat(128 << 10);
        let result = sparkles_core::sparql::query(
            store.snapshot(),
            &format!("SELECT ?{name} WHERE {{ VALUES ?{name} {{ 7 }} }}"),
            &QueryOptions {
                pool: Some(pool.clone()),
                max_memory_bytes: Some(16 << 20),
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        let name_address = result.variables()[0].as_ptr() as usize;
        let column_address = result.table().column(0).unwrap().as_ptr() as usize;
        let held = pool.granted();
        assert!(held >= 128 << 10);
        let ready = Arc::new(Barrier::new(threads + 1));
        let release = Arc::new(Barrier::new(threads + 1));
        let mut workers = Vec::new();
        for _ in 0..threads {
            let (result, ready, release) = (result.clone(), ready.clone(), release.clone());
            workers.push(std::thread::spawn(move || {
                assert_eq!(result.variables()[0].len(), 128 << 10);
                assert_eq!(result.variables()[0].as_ptr() as usize, name_address);
                assert_eq!(
                    result.table().column(0).unwrap().as_ptr() as usize,
                    column_address
                );
                assert_eq!(
                    result.table().get(0, 0).unwrap(),
                    sparkles_core::id::Id::from_i64(7).unwrap()
                );
                ready.wait();
                release.wait();
                drop(result);
            }));
        }
        ready.wait();
        assert_eq!(pool.granted(), held);
        drop(store);
        drop(result);
        assert_eq!(
            pool.granted(),
            held,
            "actual clone owners retain the payload"
        );
        release.wait();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(pool.granted(), 0);
    }
}
