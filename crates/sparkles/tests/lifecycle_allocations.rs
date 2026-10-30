//! Actual caller-thread close allocations, including the private parking Store.
#[path = "../../sparkles-core/tests/support/caller_allocations.rs"]
mod caller_allocations;
use sparkles::Catalog;

#[test]
fn actual_catalog_close_allocation_receipts() {
    for branches in [0, 1, 16] {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let dataset = catalog.create("source", &Default::default()).unwrap();
        for index in 0..branches {
            let name = format!("branch-{index}");
            dataset.create_branch(&name, &Default::default()).unwrap();
            drop(dataset.branch(&name).unwrap());
        }
        drop(dataset);
        let (result, allocation) = caller_allocations::measure(|| catalog.delete("source"));
        assert!(result.unwrap());
        assert!(catalog.get("source").is_none());
        eprintln!(
            "C25_CLOSE_ALLOCATION branches={branches} caller_peak={} largest={}",
            allocation.peak, allocation.largest
        );
    }
}
