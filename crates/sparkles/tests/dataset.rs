//! The embedded `Dataset`'s commit receipts and its DESCRIBE setting. These tests moved
//! from the engine's `commits` and `describe` tests when `Dataset` moved to this crate.

use oxrdf::{GraphNameRef, NamedNodeRef, QuadRef, TermRef};
use sparkles::Dataset;
use sparkles::commit::CommitKind;
use sparkles::io::RdfFormat;
use sparkles::sparql::describe::{DescribeMode, DescribeOptions};

fn mode(m: DescribeMode) -> DescribeOptions {
    DescribeOptions {
        mode: m,
        ..Default::default()
    }
}

#[test]
fn library_receipts() {
    let ds = Dataset::memory();
    let s = ds
        .update("INSERT DATA { <urn:a> <urn:p> <urn:o> }")
        .unwrap();
    let r = s.commit.unwrap();
    assert!(r.committed && r.commit.seq == 1);
    assert_eq!(ds.head_commit().seq, 1);
    let q = QuadRef::new(
        NamedNodeRef::new("urn:a").unwrap(),
        NamedNodeRef::new("urn:p").unwrap(),
        TermRef::NamedNode(NamedNodeRef::new("urn:o").unwrap()),
        GraphNameRef::DefaultGraph,
    );
    let (_, r) = ds.transaction_receipt(|tx| tx.insert(q)).unwrap();
    assert!(!r.committed && r.commit.seq == 1);
    let (_, r) = ds.transaction_receipt(|tx| tx.remove(q)).unwrap();
    assert!(r.committed);
    assert_eq!(
        (r.commit.seq, r.commit.deleted, r.commit.kind),
        (2, 1, CommitKind::Transaction)
    );
    let json = serde_json::to_value(r).unwrap();
    assert_eq!(json["commit"]["ref"], "commit:2");
    assert_eq!(json["commit"]["parent"], 1);
    assert_eq!(json["commit"]["kind"], "transaction");
    assert_eq!(json["datasetId"], ds.dataset_id().to_string());
}

#[test]
fn a_dataset_queries_with_its_setting() {
    let ds = Dataset::memory();
    ds.load_str(
        "<http://example.org/a> <http://example.org/p> [ <http://example.org/q> 1 ] .",
        RdfFormat::Turtle,
    )
    .unwrap();
    assert_eq!(
        ds.construct("DESCRIBE <http://example.org/a>")
            .unwrap()
            .len(),
        2
    );
    ds.store()
        .set_describe_settings(Some(mode(DescribeMode::Outgoing)))
        .unwrap();
    assert_eq!(
        ds.construct("DESCRIBE <http://example.org/a>")
            .unwrap()
            .len(),
        1
    );
}
