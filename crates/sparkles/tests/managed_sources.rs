use sparkles::access::source::{SampledSource, SourceOwner, Target};
use sparkles::dataset::StoreHandle;
use sparkles::guard::WriteOptions;
use sparkles::managed_source::ResidentSourceOwner;
use sparkles::store::Store;
use sparkles::{Catalog, Dataset, Error};

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

fn target(dataset: &Dataset) -> Target {
    Target {
        dataset: dataset.dataset_id(),
        branch: dataset.store().branch_id(),
    }
}

#[test]
fn main_source_lease_retains_own_payload_and_catalog_exclusion() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path(), Default::default()).unwrap();
    let request = catalog.create("source", &Default::default()).unwrap();
    assert!(matches!(request.state().store, StoreHandle::Own(_)));
    let root = request.store().root().unwrap().to_path_buf();
    let source_target = target(&request);
    let original = request.store() as *const Store;
    let lease = ResidentSourceOwner::resolve(&catalog, source_target, &Default::default())
        .unwrap()
        .unwrap();
    drop(request);
    let sampled = SampledSource::sample(&lease, source_target).unwrap();
    assert!(std::ptr::eq(sampled.actual_main(), original));
    assert!(std::ptr::eq(sampled.actual_main(), sampled.logical()));
    assert!(matches!(catalog.delete("source"), Err(Error::Conflict(_))));
    assert!(matches!(
        catalog.rename("source", "moved"),
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        Store::open(&root, Default::default()),
        Err(Error::Locked { .. })
    ));
    drop(lease);
    assert!(catalog.delete("source").unwrap());
    assert!(owned_payload(StoreHandle::Own(Store::in_memory(
        Default::default()
    ))));
    assert!(!owned_payload(StoreHandle::Branch(std::sync::Arc::new(
        Store::in_memory(Default::default())
    ))));
}

#[test]
fn resident_branch_resolution_preserves_exact_opening_without_creating_a_facade() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path(), Default::default()).unwrap();
    let request = catalog.create("source", &Default::default()).unwrap();
    let branch = request.create_branch("cold", &Default::default()).unwrap();
    let source_target = Target {
        dataset: request.dataset_id(),
        branch: branch.id,
    };
    // Creation returns branch_info, which opens the new branch. Reopen the
    // persistent family to exercise a real listed, nonresident source instead.
    drop(request);
    drop(catalog);
    let catalog = Catalog::open(directory.path(), Default::default()).unwrap();
    let request = catalog.get("source").unwrap();
    let set = request.store().branch_set().unwrap();
    assert!(set.open_stores().is_empty());
    assert!(
        ResidentSourceOwner::resolve(&catalog, source_target, &Default::default())
            .unwrap()
            .is_none()
    );
    assert!(
        set.open_stores().is_empty(),
        "resolution must not open cold stores"
    );
    let resident = request.store().branch("cold").unwrap().shared().unwrap();
    let lease = ResidentSourceOwner::resolve(&catalog, source_target, &Default::default())
        .unwrap()
        .unwrap();
    assert!(std::ptr::eq(lease.logical(), resident.as_ref()));
    assert!(std::ptr::eq(lease.actual_main(), request.store()));
    request.rename_branch("cold", "renamed").unwrap();
    let renamed = ResidentSourceOwner::resolve(&catalog, source_target, &Default::default())
        .unwrap()
        .unwrap();
    assert!(std::ptr::eq(renamed.logical(), resident.as_ref()));
    drop((renamed, resident, set, request));
    assert!(matches!(catalog.delete("source"), Err(Error::Conflict(_))));
    drop(lease);
    assert!(catalog.delete("source").unwrap());
}

#[test]
fn removed_and_reused_names_do_not_resolve_old_identities() {
    let catalog = Catalog::memory(Default::default());
    let old = catalog
        .attach("source", sparkles::catalog::Attach::Memory)
        .unwrap();
    let old_target = target(&old);
    catalog.delete("source").unwrap();
    let replacement = catalog
        .attach("source", sparkles::catalog::Attach::Memory)
        .unwrap();
    assert_ne!(old.dataset_id(), replacement.dataset_id());
    assert!(
        ResidentSourceOwner::resolve(&catalog, old_target, &Default::default())
            .unwrap()
            .is_none()
    );
    let cancelled = WriteOptions {
        cancel: Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            true,
        ))),
        ..Default::default()
    };
    assert!(matches!(
        ResidentSourceOwner::resolve(&catalog, target(&replacement), &cancelled),
        Err(Error::Cancelled)
    ));
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
