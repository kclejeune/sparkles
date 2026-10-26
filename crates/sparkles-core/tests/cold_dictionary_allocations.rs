//! Actual cold-index allocation admission through the closed reader implementation.
#![cfg(feature = "failpoints")]
use sparkles_core::{Error, history::HistoryOptions, vocab::DeltaVocab};
#[path = "support/caller_allocations.rs"]
mod caller_allocations;

#[test]
fn cold_index_nodes_and_collision_vectors_are_admitted_before_growth() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("delta.vocab");
    let mut wire = Vec::new();
    for id in 0..10_000 {
        let key = format!("<urn:key:{id:08}>");
        wire.extend_from_slice(&(key.len() as u32).to_le_bytes());
        wire.extend_from_slice(key.as_bytes());
    }
    std::fs::write(&path, &wire).unwrap();
    let cap = 1 << 20;
    let (result, counts) = caller_allocations::measure(|| {
        DeltaVocab::open_read_only_limited_for_test(&path, &HistoryOptions::default(), cap)
    });
    assert!(matches!(result, Err(Error::BudgetExceeded(_))));
    eprintln!(
        "cold index admission: largest={}, peak={}, cap={cap}",
        counts.largest, counts.peak
    );
    assert!(
        counts.largest <= cap as usize,
        "cold index allocation preceded admission"
    );
    assert!(
        counts.peak <= cap as usize,
        "cold index nodes exceeded the shared temporary allowance"
    );
    assert_eq!(std::fs::read(&path).unwrap(), wire);
    let (vocab, charge) =
        DeltaVocab::open_read_only_limited_for_test(&path, &HistoryOptions::default(), 16 << 20)
            .unwrap();
    assert_eq!(vocab.len(), 10_000);
    assert_eq!(vocab.find(b"<urn:key:00005432>"), Some(5432));
    assert!(charge <= 16 << 20);
}
