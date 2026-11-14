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

/// `contains_all` answers each quad as `contains` does, in the order given, for quads
/// that are there, quads whose terms the dataset does not know and repeated quads.
#[test]
fn contains_all_answers_each_quad_as_contains_does() {
    let ds = Dataset::memory();
    ds.load_str(
        "<urn:a> <urn:p> <urn:b> .\n\
         <urn:a> <urn:p> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> <urn:g> .\n\
         _:x <urn:p> \"chat\"@fr <urn:g> .\n",
        RdfFormat::NQuads,
    )
    .unwrap();
    let mut quads: Vec<oxrdf::Quad> = ds
        .quads(None, None, None, None)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(quads.len(), 3);
    // the same triples in the other graph, an unknown term in each position, and a repeat
    let other = |q: &oxrdf::Quad| {
        let mut q = q.clone();
        q.graph_name = match q.graph_name {
            oxrdf::GraphName::DefaultGraph => oxrdf::NamedNode::new_unchecked("urn:g").into(),
            _ => oxrdf::GraphName::DefaultGraph,
        };
        q
    };
    let unknown = oxrdf::NamedNode::new_unchecked("urn:unknown");
    let stored = quads.clone();
    for q in &stored {
        quads.push(other(q));
        let mut u = q.clone();
        u.subject = unknown.clone().into();
        quads.push(u);
        let mut u = q.clone();
        u.predicate = unknown.clone();
        quads.push(u);
        let mut u = q.clone();
        u.object = unknown.clone().into();
        quads.push(u);
        let mut u = q.clone();
        u.graph_name = unknown.clone().into();
        quads.push(u);
        quads.push(q.clone());
    }
    let one_by_one: Vec<bool> = quads
        .iter()
        .map(|q| ds.contains(q.as_ref()).unwrap())
        .collect();
    assert_eq!(
        ds.contains_all(quads.iter().map(|q| q.as_ref())).unwrap(),
        one_by_one
    );
    assert_eq!(one_by_one.iter().filter(|found| **found).count(), 6);
    assert!(one_by_one[..3].iter().all(|found| *found));
    assert_eq!(
        ds.contains_all(std::iter::empty()).unwrap(),
        Vec::<bool>::new()
    );
}
