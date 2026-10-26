//! Native input payloads must be charged before a temporary literal clone is made.
//! This isolated integration-test binary measures actual allocations, following the
//! counting allocator used by projection_memory rather than inferring copies from errors.

use oxrdf::{Literal, NamedNode, Triple, vocab::xsd};
use sparkles_core::{
    Budget, BudgetKind, Error,
    evidence::{
        AssessmentInput, ClaimInput, ClaimKind, Confidence, ConfidenceFacets, EvidenceInput,
        MethodRef, Polarity, RecordKey, RecordRef, RecordVersion, Sha256Digest, SourceContent,
        SourceLineage, SourceMetadata, SourceRevisionInput, SourceTimes, prepare_input,
    },
    task::Control,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Counting;
static ENABLED: AtomicBool = AtomicBool::new(false);
static LARGEST: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every allocation, reallocation and deallocation is delegated unchanged
// to System. The counters only observe sizes and never inspect or change a pointer.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            LARGEST.fetch_max(layout.size(), Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            LARGEST.fetch_max(size, Ordering::Relaxed);
        }
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn node(value: &str) -> NamedNode {
    NamedNode::new(value).unwrap()
}
fn key(value: &str) -> RecordKey {
    RecordKey::new(value).unwrap()
}
fn digest() -> Sha256Digest {
    Sha256Digest::of_bytes(b"configuration", &Control::none()).unwrap()
}
fn method() -> MethodRef {
    MethodRef {
        method: node("urn:method"),
        version: key("1"),
    }
}

fn input(kind: &str) -> EvidenceInput {
    let value = Literal::new_typed_literal("0".repeat(RecordKey::MAX_BYTES + 1), xsd::DATE_TIME);
    let mut input = EvidenceInput::default();
    match kind {
        "publishedAt" | "reportedAvailableAt" => {
            let times = if kind == "publishedAt" {
                SourceTimes {
                    published_at: Some(value),
                    ..SourceTimes::default()
                }
            } else {
                SourceTimes {
                    reported_available_at: Some(value),
                    ..SourceTimes::default()
                }
            };
            input.revisions.push(SourceRevisionInput {
                version: RecordVersion::V1,
                key: key("source"),
                source: node("urn:source"),
                source_key: key("revision"),
                upstream_version: None,
                content: SourceContent::MetadataOnly { digest: digest() },
                metadata: SourceMetadata::default(),
                times,
                lineage: SourceLineage::default(),
            });
        }
        "confidence" => input.claims.push(ClaimInput {
            version: RecordVersion::V1,
            key: key("claim"),
            occurrence_key: key("one"),
            revision: RecordRef::Existing(node("urn:revision")),
            proposition: Triple::new(node("urn:s"), node("urn:p"), node("urn:o")),
            kind: ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: node("urn:author"),
            scope: BTreeMap::new(),
            confidence: ConfidenceFacets {
                source: Some(Confidence {
                    scale: node("urn:uninterpreted-scale"),
                    value,
                    evaluator: node("urn:evaluator"),
                    method: None,
                }),
                ..ConfidenceFacets::default()
            },
        }),
        "evaluatedAt" => input.assessments.push(AssessmentInput {
            version: RecordVersion::V1,
            key: key("assessment"),
            occurrence_key: key("one"),
            target: RecordRef::Existing(node("urn:claim")),
            conclusion: node("urn:conclusion").into(),
            method: method(),
            configuration_digest: digest(),
            evaluated_at: value,
            attributed_evaluator: node("urn:evaluator"),
            confidence: None,
            scope: BTreeMap::new(),
        }),
        _ => unreachable!(),
    }
    input
}

#[test]
fn oversized_native_literal_fields_are_refused_without_copying_the_payload() {
    let mut observations = Vec::new();
    for kind in [
        "publishedAt",
        "reportedAvailableAt",
        "confidence",
        "evaluatedAt",
    ] {
        // Allocate the caller's input and any runtime initialization before measuring.
        let input = input(kind);
        let control = Control::none();
        LARGEST.store(0, Ordering::Relaxed);
        ENABLED.store(true, Ordering::Relaxed);
        let result = prepare_input(&input, &control);
        ENABLED.store(false, Ordering::Relaxed);
        let largest = LARGEST.load(Ordering::Relaxed);
        let budget = matches!(result, Err(Error::BudgetExceeded(Budget {
            kind: BudgetKind::DecompressedBytes,
            limit,
            ..
        })) if limit == RecordKey::MAX_BYTES as u64);
        observations.push((kind, largest, budget));
    }
    eprintln!("preparation allocations: {observations:?}");
    assert!(observations.iter().all(|(_, _, budget)| *budget));
    // Small alias/reference maps are permitted. A 16 MiB temporary input copy is not.
    assert!(
        observations
            .iter()
            .all(|(_, largest, _)| *largest < 1024 * 1024)
    );
}
