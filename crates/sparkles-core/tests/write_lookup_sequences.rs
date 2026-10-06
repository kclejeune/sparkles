//! Vocabulary-only DELETE DATA lookup through transaction/replay boundaries.
use oxrdf::{GraphName, Literal, NamedNode, Quad};
use sparkles_core::index::Perm;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::{update, update_in};
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use std::collections::BTreeSet;

const MIXED: &str = r#"
<urn:fresh:s> <urn:fresh:p> <urn:fresh:o> .
<urn:lang> <urn:fresh:p> "bonjour"@fr .
<urn:direction> <urn:fresh:p> "bonjour"@fr--ltr .
<urn:typed> <urn:fresh:p> "long typed value"^^<urn:fresh:datatype> .
GRAPH <urn:fresh:g> {
 <urn:quoted> <urn:fresh:p> <<( <urn:fresh:nested> <urn:fresh:inner> "value" )>> .
 <urn:number> <urn:fresh:p> 73 .
}
"#;

#[test]
fn delete_data_sees_fresh_noninline_terms_and_preserves_net_zero_receipts() {
    let store = Store::in_memory(StoreOptions::default());
    let head = store.head_commit();
    let stats = update(
        &store,
        &format!("INSERT DATA {{ {MIXED} }}; DELETE DATA {{ {MIXED} }}; DELETE DATA {{ {MIXED} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!((stats.inserted, stats.deleted, stats.operations), (6, 6, 3));
    assert!(!stats.commit.unwrap().committed);
    assert_eq!(store.head_commit(), head);
    assert!(store.snapshot().is_empty());
    // The same lookup contract applies to vocabulary added by earlier native work
    // and operations in an explicitly retained writer.
    let mut txn = store.write();
    let native = quad(999);
    let ids = txn.encode_quad(&native, &mut Default::default()).unwrap();
    assert!(txn.insert(ids).unwrap());
    update_in(
        &mut txn,
        &format!("INSERT DATA {{ {MIXED} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let held = txn.view();
    let stats = update_in(
        &mut txn,
        &format!("DELETE DATA {{ {MIXED} {} }}", sparql_data(&[native])),
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(stats.deleted, 7);
    assert!(txn.view().is_empty());
    assert_eq!(held.len(), 7);
    assert!(!txn.commit().unwrap().committed);
    assert!(store.snapshot().is_empty());
}

fn quad(n: i64) -> Quad {
    Quad::new(
        NamedNode::new(format!("urn:s:{n}")).unwrap(),
        NamedNode::new("urn:p").unwrap(),
        Literal::new_simple_literal(format!("non-inline value {n}")),
        if n % 2 == 0 {
            GraphName::DefaultGraph
        } else {
            NamedNode::new("urn:g").unwrap().into()
        },
    )
}
fn contents(s: &Snapshot) -> BTreeSet<String> {
    s.scan_keys(Perm::Spo, &[])
        .unwrap()
        .into_iter()
        .map(|k| s.quad_to_terms(&Perm::Spo.to_quad(&k)).unwrap().to_string())
        .collect()
}
fn check(s: &Snapshot, expected: &BTreeSet<String>) {
    assert_eq!(s.len(), expected.len() as u64);
    for order in Perm::ALL {
        let keys = s.scan_keys(order, &[]).unwrap();
        assert_eq!(keys.len(), expected.len(), "{order:?}");
        let actual: BTreeSet<_> = keys
            .iter()
            .map(|k| s.quad_to_terms(&order.to_quad(k)).unwrap().to_string())
            .collect();
        assert_eq!(&actual, expected, "{order:?}");
    }
}
fn sparql_data(quads: &[Quad]) -> String {
    quads
        .iter()
        .map(|q| match &q.graph_name {
            GraphName::DefaultGraph => format!("{} {} {} .\n", q.subject, q.predicate, q.object),
            g => format!(
                "GRAPH {g} {{ {} {} {} . }}\n",
                q.subject, q.predicate, q.object
            ),
        })
        .collect()
}

#[test]
fn large_mixed_delete_keeps_all_orders_snapshots_and_replayed_generations() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    let base: Vec<_> = (0..128).map(quad).collect();
    let nquads: String = base.iter().map(|q| format!("{q} .\n")).collect();
    store
        .load(&[Source::from_bytes(
            nquads.into_bytes(),
            RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    let delta: Vec<_> = (128..192).map(quad).collect();
    update(
        &store,
        &format!("INSERT DATA {{ {} }}", sparql_data(&delta)),
        &QueryOptions::default(),
    )
    .unwrap();
    let held = store.snapshot();
    let before = contents(&held);
    let deleted: Vec<_> = (0..72).chain(128..160).map(quad).collect();
    let mut request = deleted.clone();
    request.extend(deleted.iter().take(16).cloned());
    request.extend((400..416).map(quad));
    let stats = update(
        &store,
        &format!("DELETE DATA {{ {} }}", sparql_data(&request)),
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(stats.deleted, 104);
    let removed: BTreeSet<_> = deleted.iter().map(ToString::to_string).collect();
    let expected = before.difference(&removed).cloned().collect();
    check(&store.snapshot(), &expected);
    check(&held, &before);
    drop(held);
    drop(store);
    let reopened = Store::open(&root, StoreOptions::default()).unwrap();
    check(&reopened.snapshot(), &expected);
    reopened.compact().unwrap();
    check(&reopened.snapshot(), &expected);
}
