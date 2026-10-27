//! The missing-pattern fallback admits one additional copy of the stored reason.
use sparkles_core::patterns::{Loaded, PatternFile, PatternInfo};
#[path = "support/caller_allocations.rs"]
mod caller_allocations;

#[test]
fn missing_pattern_reason_never_keeps_a_string_copy_beside_its_arc() {
    let directory = tempfile::tempdir().unwrap();
    let info = PatternInfo::skipped("s".repeat(8 << 20));
    let allowance = info.skipped.as_ref().unwrap().len()
        + std::mem::size_of::<PatternFile>()
        + 2048
        + directory.path().as_os_str().len() * 3;
    let (loaded, counts) =
        caller_allocations::measure(|| Loaded::open(directory.path(), Some(&info)));
    let peak = counts.peak;
    eprintln!(
        "missing-pattern diagnostic: largest={}, peak={peak}, allowance={allowance}",
        counts.largest
    );
    assert_eq!(loaded.missing.as_deref(), info.skipped.as_deref());
    assert!(
        peak <= allowance,
        "diagnostic retained an unadmitted String beside its Arc"
    );
}
