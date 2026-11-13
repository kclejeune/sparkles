//! External-consumer witness for the actual bounded M1 exports. This does not
//! assert the remaining public raw-alias or shared-header ownership gates closed.
use sparkles_budget::{ParentHandle, WorkStopped};
use sparkles_core::access::{GraphAccess, Preparation, PreparedView};
use sparkles_core::history::At;
use sparkles_core::sparql::{CursorOptions, QueryCursor, QueryOptions, select_source_cursor};
use sparkles_core::store::{BoundReadSource, Store, StoreOptions};
use sparkles_core::task::Control;
use std::sync::Arc;

#[test]
fn exported_store_source_control_parent_and_cursor_compile_and_read() {
    let store = Store::in_memory(StoreOptions::default());
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA { <urn:external> <urn:p> 1 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let preparation = Preparation::new(Control::none(), 32 << 20).unwrap();
    let parent: ParentHandle = preparation.memory_parent().unwrap();
    let check: std::result::Result<(), WorkStopped> = preparation.check_work();
    check.unwrap();
    let lease = parent.reserve(128).unwrap();
    let policy = Arc::new(GraphAccess::all());
    let view: PreparedView = store.prepare_read(policy.clone(), &preparation).unwrap();
    // Compile-check the selector/owning-main public signatures, too.
    let _selector = Store::prepare_read_at;
    let _inherited = Store::prepare_read_with_branch;
    let _head = At::Head;
    let source: BoundReadSource = view.read_source();
    let options = QueryOptions {
        graphs: Some(policy),
        ..Default::default()
    };
    let mut cursor: QueryCursor = select_source_cursor(
        source,
        &store,
        "SELECT ?s WHERE { ?s <urn:p> 1 }",
        &options,
        &CursorOptions::default(),
    )
    .unwrap();
    let batch = cursor.next_batch().unwrap().unwrap();
    assert_eq!(batch.len(), 1);
    let term = batch.term(0, 0).unwrap().unwrap();
    assert_eq!(term.as_term().to_string(), "<urn:external>");
    drop(lease);
    drop(cursor);
    drop(batch);
    drop(view);
    drop(store);
    assert_eq!(term.as_term().to_string(), "<urn:external>");
}
