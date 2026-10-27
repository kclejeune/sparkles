//! Attribute actual reader allocations, including malformed native diagnostics.
#![cfg(feature = "failpoints")]
use sparkles_core::{
    Error, commit::read_dataset_identity_limited_for_test, history::HistoryOptions,
};
#[path = "support/caller_allocations.rs"]
mod caller_allocations;

// Leave room for JSON framing below the next power-of-two file-buffer growth.
const PAYLOAD: usize = (1 << 20) - 4096;

fn malformed(field: &str) -> (tempfile::TempDir, Vec<u8>) {
    let root = tempfile::tempdir().unwrap();
    let mut value = serde_json::json!({
        "format": 1, "minimumReader": 1,
        "id": uuid::Uuid::new_v4(), "created": "2020-01-01T00:00:00Z", "origin": "create"
    });
    value[field] = if field == "forkedFrom" {
        serde_json::json!({"id": uuid::Uuid::new_v4(), "seq": "x".repeat(PAYLOAD)})
    } else {
        "x".repeat(PAYLOAD).into()
    };
    let bytes = serde_json::to_vec(&value).unwrap();
    std::fs::write(root.path().join("dataset.json"), &bytes).unwrap();
    (root, bytes)
}

fn refuse_without_native_diagnostic_copy(field: &str) {
    let (root, bytes) = malformed(field);
    let (result, counts) = caller_allocations::measure(|| {
        read_dataset_identity_limited_for_test(root.path(), &HistoryOptions::default(), 256 << 20)
    });
    assert!(matches!(result, Err(Error::Corrupt(_))));
    eprintln!(
        "malformed {field}: largest={}, peak={}",
        counts.largest, counts.peak
    );
    // Parsing the admitted input needs one payload-sized string/scratch buffer.
    // Echoing it in a native error grows a second allocation past this bound.
    assert!(
        counts.largest <= PAYLOAD + PAYLOAD / 2,
        "native metadata diagnostic copied the oversized input: {}",
        counts.largest
    );
    assert!(
        counts.peak <= PAYLOAD * 5,
        "native metadata diagnostic escaped the measured parse envelope: {}",
        counts.peak
    );
    assert_eq!(
        std::fs::read(root.path().join("dataset.json")).unwrap(),
        bytes
    );
}

#[test]
fn malformed_version_does_not_allocate_an_oversized_native_diagnostic() {
    refuse_without_native_diagnostic_copy("minimumReader");
}

#[test]
fn malformed_nested_sequence_does_not_allocate_an_oversized_native_diagnostic() {
    refuse_without_native_diagnostic_copy("forkedFrom");
}

#[test]
fn metadata_file_growth_is_refused_before_its_buffer_allocation() {
    let (root, bytes) = malformed("id");
    let cap = 64 << 10;
    let (result, counts) = caller_allocations::measure(|| {
        read_dataset_identity_limited_for_test(root.path(), &HistoryOptions::default(), cap)
    });
    assert!(matches!(result, Err(Error::BudgetExceeded(_))));
    assert!(
        counts.largest <= cap as usize,
        "raw metadata allocation preceded admission"
    );
    assert!(
        counts.peak <= cap as usize,
        "raw metadata parse peak escaped the shared cap"
    );
    assert_eq!(
        std::fs::read(root.path().join("dataset.json")).unwrap(),
        bytes
    );
}
