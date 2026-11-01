use sparkles::Dataset;
use sparkles::dataset::StoreHandle;
use sparkles::store::Store;

// An external crate can still exhaustively match precisely the two shipped
// variants and move the original payloads out without a fallback arm.
fn owned_payload(handle: StoreHandle) -> bool {
    match handle {
        StoreHandle::Own(store) => {
            drop(store);
            true
        }
        StoreHandle::Branch(store) => {
            drop(store);
            false
        }
    }
}

#[test]
fn store_handles_keep_their_two_public_variants() {
    assert!(owned_payload(StoreHandle::Own(Store::in_memory(
        Default::default()
    ))));
    assert!(!owned_payload(StoreHandle::Branch(std::sync::Arc::new(
        Store::in_memory(Default::default())
    ))));
}

#[test]
fn owned_persistent_view_definitions_still_use_the_owned_store_root() {
    let directory = tempfile::tempdir().unwrap();
    let dataset = Dataset::open(directory.path()).unwrap();
    assert!(matches!(dataset.state().store, StoreHandle::Own(_)));
    dataset
        .views()
        .put(
            "items",
            sparkles::incr::ViewDef::table("SELECT ?s WHERE { ?s <urn:p> ?o }"),
        )
        .unwrap();
    assert!(directory.path().join("views/items/view.json").is_file());
    assert!(matches!(dataset.state().store, StoreHandle::Own(_)));
}
