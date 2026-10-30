//! Positive ownership control using only APIs already present on the main base.
use sparkles::store::Store;
use sparkles::{Catalog, Error};

#[test]
fn actual_catalog_dataset_ownership_is_the_preexisting_disposal_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path(), Default::default()).unwrap();
    let request = catalog.create("source", &Default::default()).unwrap();
    let id = request.dataset_id();
    let root = request.store().root().unwrap().to_path_buf();
    let retained = catalog.get_by_id(id).unwrap();
    drop(request);
    assert!(matches!(catalog.delete("source"), Err(Error::Conflict(_))));
    assert!(matches!(
        catalog.rename("source", "moved"),
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        Store::open(&root, Default::default()),
        Err(Error::Locked { .. })
    ));
    drop(retained);
    assert!(catalog.delete("source").unwrap());
}
