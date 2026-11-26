//! Exact stored-fact resolution: retained commits by branch identity, upstream
//! fallback for starting commits, non-disclosing refusals, anchors and limits.

use super::*;
use crate::access::{GraphRule, Graphs, Protection, Rule, TripleRules};
use crate::branch::{BranchOptions, CommitRef, DeleteOptions};
use crate::evidence::resolve::check_anchor;
use crate::history::Retention;
use crate::store::StoreOptions;
use oxrdf::{BlankNode, Literal};
use std::sync::atomic::AtomicBool;

const G: &str = "urn:g";

fn upd(s: &Store, u: &str) {
    crate::sparql::update::update(s, u, &crate::sparql::QueryOptions::default()).unwrap();
}

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn triple(s: &str, p: &str, o: Term) -> Triple {
    Triple::new(iri(s), iri(p), o)
}

fn fact(store: &Store, branch: Uuid, commit: u64, graph: Option<&str>, t: Triple) -> StoredFactRef {
    StoredFactRef {
        dataset: store.dataset_id(),
        branch,
        commit,
        graph: match graph {
            None => GraphIdentity::Default,
            Some(g) => GraphIdentity::Named(iri(g)),
        },
        expected: t,
        anchor: None,
    }
}

fn all() -> Arc<GraphAccess> {
    Arc::new(GraphAccess::all())
}

fn resolver() -> Resolver {
    Resolver::new(Control::none(), MEMORY_BYTES).unwrap()
}

#[track_caller]
fn code<T: std::fmt::Debug>(r: Result<T>) -> String {
    match r {
        Err(e) => e.code().to_string(),
        Ok(v) => panic!("verified: {v:?}"),
    }
}

#[track_caller]
fn message<T: std::fmt::Debug>(r: Result<T>) -> String {
    match r {
        Err(e) => e.to_string(),
        Ok(v) => panic!("verified: {v:?}"),
    }
}

fn ab() -> Triple {
    triple("urn:a", "urn:p", Term::NamedNode(iri("urn:b")))
}

/// A persistent dataset with `<urn:a> <urn:p> <urn:b>` in graph G at commit 1, removed
/// at commit 2.
fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    upd(
        &s,
        &format!("INSERT DATA {{ GRAPH <{G}> {{ <urn:a> <urn:p> <urn:b> }} }}"),
    );
    upd(
        &s,
        &format!("DELETE DATA {{ GRAPH <{G}> {{ <urn:a> <urn:p> <urn:b> }} }}"),
    );
    assert_eq!(s.head_commit().seq, 2);
    (dir, s)
}

#[test]
fn a_fact_resolves_at_its_exact_commit_and_never_at_the_head() {
    let (_dir, s) = setup();
    let main = s.dataset_id();
    let mut r = resolver();
    let v = r
        .stored_fact("r1", &s, Some(&all()), &fact(&s, main, 1, Some(G), ab()))
        .unwrap();
    assert_eq!((v.commit, v.served_by, v.reifier), (1, main, None));
    // gone at the head, and not looked up there
    assert_eq!(
        code(r.stored_fact("r2", &s, Some(&all()), &fact(&s, main, 2, Some(G), ab()))),
        "evidence-unavailable"
    );
    // present at the head of a later commit but not at the referenced one
    upd(
        &s,
        &format!("INSERT DATA {{ GRAPH <{G}> {{ <urn:a> <urn:p> <urn:b> }} }}"),
    );
    assert_eq!(
        code(r.stored_fact("r3", &s, Some(&all()), &fact(&s, main, 2, Some(G), ab()))),
        "evidence-unavailable"
    );
    // the default graph is not the named graph
    assert_eq!(
        code(r.stored_fact("r4", &s, Some(&all()), &fact(&s, main, 1, None, ab()))),
        "evidence-unavailable"
    );
    // a commit that does not exist yet
    assert_eq!(
        code(r.stored_fact("r5", &s, Some(&all()), &fact(&s, main, 99, Some(G), ab()))),
        "evidence-unavailable"
    );
}

#[test]
fn hidden_and_missing_references_answer_alike() {
    let (_dir, s) = setup();
    let main = s.dataset_id();
    let mut r = resolver();
    let missing = message(r.stored_fact(
        "r",
        &s,
        Some(&all()),
        &fact(&s, main, 1, Some("urn:other"), ab()),
    ));
    // no access to the dataset or branch at all
    let denied = message(r.stored_fact("r", &s, None, &fact(&s, main, 1, Some(G), ab())));
    // the graph is not readable
    let other = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:other"], &[])),
        Graphs::none(),
    ));
    let graph = message(r.stored_fact("r", &s, Some(&other), &fact(&s, main, 1, Some(G), ab())));
    // a branch identity this dataset does not know
    let branch = message(r.stored_fact(
        "r",
        &s,
        Some(&all()),
        &fact(&s, Uuid::from_u128(7), 1, Some(G), ab()),
    ));
    // another dataset's identity
    let mut foreign = fact(&s, main, 1, Some(G), ab());
    foreign.dataset = Uuid::from_u128(9);
    let dataset = message(r.stored_fact("r", &s, Some(&all()), &foreign));
    assert_eq!(missing, "evidence reference `r` is unavailable");
    for m in [denied, graph, branch, dataset] {
        assert_eq!(m, missing);
    }
}

#[test]
fn a_triple_protection_hides_the_fact() {
    let (_dir, s) = setup();
    let main = s.dataset_id();
    let protection = Protection {
        name: "secret".into(),
        predicates: Some(vec!["urn:p".into()]),
        classes: None,
        subclasses: false,
        graphs: None,
        pattern: None,
        prefixes: Default::default(),
        hide_inferences: false,
    };
    let rules = TripleRules {
        rules: vec![Rule {
            protection: Arc::new(protection),
            read: Graphs::none(),
            write: Graphs::none(),
        }],
        caller: Default::default(),
        limits: Default::default(),
    };
    let masked = Arc::new(GraphAccess::with_triples(Graphs::All, Graphs::All, rules));
    let mut r = resolver();
    assert_eq!(
        code(r.stored_fact("r", &s, Some(&masked), &fact(&s, main, 1, Some(G), ab()))),
        "evidence-unavailable"
    );
    // a second policy for the same commit gets its own view
    r.stored_fact("r", &s, Some(&all()), &fact(&s, main, 1, Some(G), ab()))
        .unwrap();
    let mut r = resolver();
    r.stored_fact("r", &s, Some(&all()), &fact(&s, main, 1, Some(G), ab()))
        .unwrap();
}

#[test]
fn branches_resolve_by_identity_and_starting_commits_by_their_upstream() {
    let (_dir, s) = setup();
    let main = s.dataset_id();
    let dev = s.create_branch("dev", &BranchOptions::default()).unwrap();
    let b = s.branch("dev").unwrap();
    upd(&b, "INSERT DATA { <urn:c> <urn:p> <urn:d> }");
    let cd = triple("urn:c", "urn:p", Term::NamedNode(iri("urn:d")));
    let head = b.head_commit().seq;
    drop(b);
    let mut r = resolver();
    // a commit at or below the starting point is served by main
    let v = r
        .stored_fact("r", &s, Some(&all()), &fact(&s, dev.id, 1, Some(G), ab()))
        .unwrap();
    assert_eq!((v.branch, v.served_by), (dev.id, main));
    let v = r
        .stored_fact(
            "r",
            &s,
            Some(&all()),
            &fact(&s, dev.id, head, None, cd.clone()),
        )
        .unwrap();
    assert_eq!(v.served_by, dev.id);
    // main never sees the branch's commit
    assert_eq!(
        code(r.stored_fact(
            "r",
            &s,
            Some(&all()),
            &fact(&s, main, head, None, cd.clone())
        )),
        "evidence-unavailable"
    );
    // a rename keeps the identity
    s.rename_branch("dev", "renamed").unwrap();
    let mut r = resolver();
    r.stored_fact(
        "r",
        &s,
        Some(&all()),
        &fact(&s, dev.id, head, None, cd.clone()),
    )
    .unwrap();
    // a deleted branch and a recreated name do not resolve the old identity
    s.delete_branch_with(
        "renamed",
        &DeleteOptions {
            force: true,
            ..Default::default()
        },
    )
    .unwrap();
    let again = s
        .create_branch("renamed", &BranchOptions::default())
        .unwrap();
    assert_ne!(again.id, dev.id);
    let mut r = resolver();
    assert_eq!(
        code(r.stored_fact("r", &s, Some(&all()), &fact(&s, dev.id, head, None, cd))),
        "evidence-unavailable"
    );
    // a branch store is not the main store that resolves identities
    let b = s.branch("renamed").unwrap();
    assert_eq!(
        code(r.stored_fact("r", &b, Some(&all()), &fact(&s, main, 1, Some(G), ab()))),
        "evidence-unavailable"
    );
}

#[test]
fn a_held_starting_commit_resolves_after_its_history_is_trimmed() {
    let (_dir, s) = setup();
    let dev = s.create_branch("dev", &BranchOptions::default()).unwrap();
    let held = CommitRef {
        branch_id: dev.id,
        seq: 1,
    };
    s.acquire_hold(OwnerKind::Evidence, Uuid::from_u128(1), &[held])
        .unwrap();
    s.set_retention(Retention {
        keep_commits: Some(1),
        keep_age_ms: None,
        max_bytes: None,
    })
    .unwrap();
    for n in 0..3 {
        let b = s.branch("dev").unwrap();
        upd(&b, &format!("INSERT DATA {{ <urn:x> <urn:p> {n} }}"));
        b.compact().unwrap();
        upd(&s, &format!("INSERT DATA {{ <urn:y> <urn:p> {n} }}"));
        s.compact().unwrap();
    }
    let mut r = resolver();
    let v = r
        .stored_fact("r", &s, Some(&all()), &fact(&s, dev.id, 1, Some(G), ab()))
        .unwrap();
    assert_eq!(v.served_by, s.dataset_id());
    // an unheld commit of the same age is gone, and answers as unavailable
    assert_eq!(
        code(r.stored_fact(
            "r",
            &s,
            Some(&all()),
            &fact(&s, s.dataset_id(), 2, Some(G), ab())
        )),
        "evidence-unavailable"
    );
}

#[test]
fn blank_nodes_need_a_stable_reifier() {
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        &format!(
            "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
             INSERT DATA {{ GRAPH <{G}> {{
               _:x <urn:p> \"v\" .
               <urn:r> rdf:reifies <<( _:x <urn:p> \"v\" )>> .
               <urn:a> <urn:p> <urn:b> .
               <urn:ra> rdf:reifies <<( <urn:a> <urn:p> <urn:b> )>> .
               <urn:two> rdf:reifies <<( <urn:a> <urn:p> <urn:b> )>> , <<( _:x <urn:p> \"v\" )>> .
             }} }}"
        ),
    );
    let main = s.dataset_id();
    let commit = s.head_commit().seq;
    let blank = Triple::new(
        BlankNode::new("caller").unwrap(),
        iri("urn:p"),
        Literal::new_simple_literal("v"),
    );
    let mut r = resolver();
    let mut f = fact(&s, main, commit, Some(G), blank.clone());
    // refused before any read, even without access
    assert_eq!(
        code(r.stored_fact("r", &s, None, &f)),
        "unanchored-evidence"
    );
    f.anchor = Some(BlankAnchor::ImportMap(iri("urn:map")));
    assert_eq!(
        code(r.stored_fact("r", &s, None, &f)),
        "unanchored-evidence"
    );
    f.anchor = Some(BlankAnchor::Reifier(iri("urn:r")));
    let v = r.stored_fact("r", &s, Some(&all()), &f).unwrap();
    assert_eq!(v.reifier, Some(iri("urn:r")));
    // a reifier of another triple does not anchor this one
    f.anchor = Some(BlankAnchor::Reifier(iri("urn:ra")));
    assert_eq!(
        code(r.stored_fact("r", &s, Some(&all()), &f)),
        "evidence-unavailable"
    );
    // a reifier of two triples is ambiguous
    f.anchor = Some(BlankAnchor::Reifier(iri("urn:two")));
    assert_eq!(
        code(r.stored_fact("r", &s, Some(&all()), &f)),
        "unanchored-evidence"
    );
    // a ground fact with a reifier checks the reifier too
    let mut g = fact(&s, main, commit, Some(G), ab());
    g.anchor = Some(BlankAnchor::Reifier(iri("urn:ra")));
    r.stored_fact("r", &s, Some(&all()), &g).unwrap();
    g.anchor = Some(BlankAnchor::Reifier(iri("urn:r")));
    assert_eq!(
        code(r.stored_fact("r", &s, Some(&all()), &g)),
        "evidence-unavailable"
    );
    // a blank node repeated in the expected triple must be repeated in the stored one
    assert!(!isomorphic(
        &Triple::new(
            BlankNode::new("a").unwrap(),
            iri("urn:p"),
            BlankNode::new("a").unwrap()
        ),
        &Triple::new(
            BlankNode::new("a").unwrap(),
            iri("urn:p"),
            BlankNode::new("b").unwrap()
        ),
    ));
}

#[test]
fn literals_and_triple_terms_are_looked_up_exactly() {
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        "INSERT DATA { <urn:a> <urn:n> 42 . <urn:a> <urn:t> \"x\"@en . <urn:a> <urn:q> <<( <urn:a> <urn:n> 42 )>> }",
    );
    let main = s.dataset_id();
    let c = s.head_commit().seq;
    let mut r = resolver();
    let int = |v: &str| Term::Literal(Literal::new_typed_literal(v, iri(XSD_INTEGER)));
    r.stored_fact(
        "r",
        &s,
        Some(&all()),
        &fact(&s, main, c, None, triple("urn:a", "urn:n", int("42"))),
    )
    .unwrap();
    assert_eq!(
        code(r.stored_fact(
            "r",
            &s,
            Some(&all()),
            &fact(&s, main, c, None, triple("urn:a", "urn:n", int("43")))
        )),
        "evidence-unavailable"
    );
    let en = Term::Literal(Literal::new_language_tagged_literal("x", "en").unwrap());
    r.stored_fact(
        "r",
        &s,
        Some(&all()),
        &fact(&s, main, c, None, triple("urn:a", "urn:t", en)),
    )
    .unwrap();
    let plain = Term::Literal(Literal::new_simple_literal("x"));
    assert_eq!(
        code(r.stored_fact(
            "r",
            &s,
            Some(&all()),
            &fact(&s, main, c, None, triple("urn:a", "urn:t", plain))
        )),
        "evidence-unavailable"
    );
    let quoted = Term::Triple(Box::new(triple("urn:a", "urn:n", int("42"))));
    r.stored_fact(
        "r",
        &s,
        Some(&all()),
        &fact(&s, main, c, None, triple("urn:a", "urn:q", quoted)),
    )
    .unwrap();
}

#[test]
fn limits_and_controls_refuse_without_a_verdict() {
    let (_dir, s) = setup();
    let main = s.dataset_id();
    // an allowance too small for the view is incomplete, not unavailable
    let mut r = Resolver::new(Control::none(), 2048).unwrap();
    assert_eq!(
        code(r.stored_fact("r", &s, Some(&all()), &fact(&s, main, 1, Some(G), ab()))),
        "incomplete-evidence"
    );
    // cancellation keeps the request error contract
    let flag = Arc::new(AtomicBool::new(false));
    let control = Control::with_cancel(flag.clone());
    let mut r = Resolver::new(control, MEMORY_BYTES).unwrap();
    flag.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(matches!(
        r.stored_fact("r", &s, Some(&all()), &fact(&s, main, 1, Some(G), ab())),
        Err(Error::Cancelled)
    ));
    // views of one commit are prepared once and reused
    let mut r = resolver();
    r.stored_fact("a", &s, Some(&all()), &fact(&s, main, 1, Some(G), ab()))
        .unwrap();
    let used = r.preparation().inner_used();
    r.stored_fact("b", &s, Some(&all()), &fact(&s, main, 1, Some(G), ab()))
        .unwrap();
    assert_eq!(r.preparation().inner_used(), used);
}

use crate::access::{Decider, Exemptions, GuardDecision};

/// A guard decision that exempts the listed graphs through the controlled contract.
struct Exempt(&'static [&'static str]);

impl GuardDecision for Exempt {
    fn exempt(&self, _: &Arc<Snapshot>) -> Vec<String> {
        unreachable!("only the controlled contract is used")
    }
    fn exempt_controlled(
        &self,
        _: &crate::access::GuardSnapshot<'_>,
        preparation: &Preparation,
    ) -> Result<Exemptions> {
        let mut out = Exemptions::new(preparation)?;
        for g in self.0 {
            out.push(g)?;
        }
        Ok(out)
    }
}

/// A guard decided at the snapshot exempts its graph through the controlled provider
/// contract, so a reference into an exempt guarded graph verifies.
#[test]
fn a_guarded_graph_is_decided_at_the_snapshot() {
    let (_dir, s) = setup();
    let main = s.dataset_id();
    let guarded = |exempt: &'static [&'static str]| {
        Arc::new(GraphAccess::graphs(
            Graphs::All.guarded_by("urn:*", Decider(Arc::new(Exempt(exempt)))),
            Graphs::none(),
        ))
    };
    let mut r = resolver();
    r.stored_fact(
        "r",
        &s,
        Some(&guarded(&[G])),
        &fact(&s, main, 1, Some(G), ab()),
    )
    .unwrap();
    let mut r = resolver();
    assert_eq!(
        code(r.stored_fact(
            "r",
            &s,
            Some(&guarded(&[])),
            &fact(&s, main, 1, Some(G), ab())
        )),
        "evidence-unavailable"
    );
}

/// Branch identity lookups wait for the branch table under the request's controls.
#[test]
fn branch_lookups_observe_the_deadline_while_the_table_is_held() {
    let (_dir, s) = setup();
    let dev = s.create_branch("dev", &BranchOptions::default()).unwrap();
    let held = s.hold_branch_table_for_test();
    let soon = || Some(std::time::Instant::now() + std::time::Duration::from_millis(50));
    let started = std::time::Instant::now();
    let options = crate::guard::WriteOptions {
        deadline: soon(),
        ..Default::default()
    };
    assert!(matches!(
        s.listed_branch_name(dev.id, &options),
        Err(Error::Timeout)
    ));
    let control = Control {
        deadline: soon(),
        ..Control::none()
    };
    let mut r = Resolver::new(control.clone(), MEMORY_BYTES).unwrap();
    assert!(matches!(
        r.stored_fact("r", &s, Some(&all()), &fact(&s, dev.id, 1, Some(G), ab())),
        Err(Error::Timeout)
    ));
    let preparation = Preparation::new(
        Control {
            deadline: soon(),
            ..Control::none()
        },
        MEMORY_BYTES,
    )
    .unwrap();
    assert!(matches!(
        s.branch_known_prepared(dev.id, &preparation),
        Err(Error::Timeout)
    ));
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    drop(held);
    assert_eq!(
        s.listed_branch_name(dev.id, &Default::default()).unwrap(),
        Some("dev".to_string())
    );
    assert!(
        s.branch_known_prepared(dev.id, &Preparation::new(Control::none(), 4096).unwrap())
            .unwrap()
    );
}

/// An anchor is decided from the reference alone.
#[test]
fn anchors_are_checked_before_any_lookup() {
    let s = Store::in_memory(StoreOptions::default());
    let mut f = fact(
        &s,
        s.dataset_id(),
        0,
        None,
        Triple::new(BlankNode::new("x").unwrap(), iri("urn:p"), iri("urn:o")),
    );
    assert_eq!(code(check_anchor("r", &f)), "unanchored-evidence");
    f.anchor = Some(BlankAnchor::Reifier(iri("urn:r")));
    assert!(check_anchor("r", &f).unwrap());
    f.expected = ab();
    assert!(!check_anchor("r", &f).unwrap());
}

/// A graph the grants refuse whatever a guard decides is refused before the history is
/// read, so an existing commit and a missing one answer alike even when preparing the
/// existing commit's view would exceed the allowance.
#[test]
fn static_graph_denials_hold_under_an_undecided_guard() {
    let (_dir, s) = setup();
    let main = s.dataset_id();
    let guarded = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:other", "urn:guarded:*"], &[]))
            .guarded_by("urn:guarded:*", Decider(Arc::new(Exempt(&[])))),
        Graphs::none(),
    ));
    assert!(guarded.read.undecided());
    let answer = |commit: u64| {
        let mut r = Resolver::new(Control::none(), 2048).unwrap();
        message(r.stored_fact(
            "r",
            &s,
            Some(&guarded),
            &fact(&s, main, commit, Some(G), ab()),
        ))
    };
    assert_eq!(answer(1), "evidence reference `r` is unavailable");
    assert_eq!(answer(1), answer(99));
    // the same allowance with a readable graph reaches the view and is incomplete
    let mut r = Resolver::new(Control::none(), 2048).unwrap();
    assert_eq!(
        code(r.stored_fact(
            "r",
            &s,
            Some(&guarded),
            &fact(&s, main, 1, Some("urn:other"), ab())
        )),
        "incomplete-evidence"
    );
}

/// Helper: configure an in-memory store with evidence source/proposal graphs and insert
/// authenticated records signed with the current graph protection nonce.
fn configured_evidence_store() -> (Store, Uuid) {
    let s = Store::in_memory(StoreOptions::default());
    let settings = super::super::Settings {
        enabled: true,
        source_records: vec!["urn:ev:sources".into()],
        proposals: vec!["urn:ev:claims".into()],
        ..Default::default()
    };
    let canonical = settings.canonical(&Control::none()).unwrap();
    let entry = crate::managed::OwnerEntry::for_settings(
        settings.profile,
        settings.managed_graphs(),
        canonical.bytes(),
    );
    let graphs = s
        .enable_managed_at(OwnerKind::Evidence, entry, canonical.bytes(), 0)
        .unwrap();
    let nonce = graphs.owners[&OwnerKind::Evidence].protections["urn:ev:sources"];
    (s, nonce)
}

fn insert_managed_quads(s: &Store, quads: Vec<oxrdf::Quad>) {
    let cap = s
        .managed_writer(OwnerKind::Evidence, Uuid::new_v4())
        .unwrap();
    let mut txn = s
        .try_write_with(crate::commit::CommitKind::Transaction, Default::default())
        .unwrap();
    txn.set_managed(cap).unwrap();
    let mut labels = std::collections::HashMap::new();
    for q in &quads {
        let ids = txn.encode_quad(q, &mut labels).unwrap();
        txn.insert(ids).unwrap();
    }
    txn.commit().unwrap();
}

#[test]
fn existing_record_verifiers_enforce_memory_cancellation_and_triple_masks() {
    let (s, nonce) = configured_evidence_store();
    let sg = oxrdf::GraphName::NamedNode(iri("urn:ev:sources"));
    let op = iri("urn:op:1");
    let rev = iri("urn:rev:1");
    let rend = iri("urn:rend:1");
    let pass = iri("urn:pass:1");
    let json_bytes = format!(r#"{{"payload":"{}"}}"#, "x".repeat(4096));
    let json_digest = Sha256Digest::of_bytes(json_bytes.as_bytes(), &Control::none()).unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(json_bytes.as_bytes());
    let rend_text = "alpha café beta".repeat(256);
    let selected: String = rend_text.chars().skip(6).take(4).collect();
    assert_eq!(selected, "café");
    let span_digest = Sha256Digest::of_bytes(selected.as_bytes(), &Control::none()).unwrap();
    let q =
        |sub: &NamedNode, p: &str, o: Term| oxrdf::Quad::new(sub.clone(), iri(p), o, sg.clone());
    let lit = |v: &str| Term::Literal(Literal::new_simple_literal(v));
    let int_lit =
        |n: u64| Term::Literal(Literal::new_typed_literal(n.to_string(), iri(XSD_INTEGER)));
    let sel = iri("urn:pass:1#selector");
    insert_managed_quads(
        &s,
        vec![
            q(&op, RDF_TYPE, iri(&format!("{EV}Operation")).into()),
            q(
                &op,
                &format!("{EV}datasetId"),
                lit(&s.dataset_id().to_string()),
            ),
            q(
                &op,
                &format!("{EV}branchId"),
                lit(&s.branch_id().to_string()),
            ),
            q(&op, &format!("{EV}commit"), int_lit(1)),
            q(
                &op,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            // SourceRevision with retained JSON bytes
            q(
                &rev,
                RDF_TYPE,
                iri(kind_iri(RecordKind::SourceRevision)).into(),
            ),
            q(&rev, &format!("{EV}availabilityReceipt"), op.clone().into()),
            q(
                &rev,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(
                &rev,
                &format!("{EV}contentDigest"),
                lit(&json_digest.to_string()),
            ),
            q(
                &rev,
                &format!("{EV}retainedBytes"),
                Term::Literal(Literal::new_typed_literal(b64, iri(XSD_BASE64_BINARY))),
            ),
            // Rendition with normalizedText
            q(&rend, RDF_TYPE, iri(kind_iri(RecordKind::Rendition)).into()),
            q(
                &rend,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rend,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(&rend, PROV_WAS_DERIVED_FROM, rev.clone().into()),
            q(&rend, &format!("{EV}normalizedText"), lit(&rend_text)),
            // Passage referencing rend and rev
            q(&pass, RDF_TYPE, iri(kind_iri(RecordKind::Passage)).into()),
            q(
                &pass,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &pass,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(&pass, &format!("{EV}sourceRevision"), rev.clone().into()),
            q(&pass, &format!("{OA}hasSource"), rend.clone().into()),
            q(&pass, &format!("{OA}hasSelector"), sel.clone().into()),
            q(&sel, &format!("{OA}start"), int_lit(6)),
            q(&sel, &format!("{OA}end"), int_lit(10)),
            q(
                &pass,
                &format!("{EV}textDigest"),
                lit(&span_digest.to_string()),
            ),
        ],
    );

    let val_digest = Sha256Digest::of_bytes(
        format!(r#""{}""#, "x".repeat(4096)).as_bytes(),
        &Control::none(),
    )
    .unwrap();

    // Baseline verification succeeds in both Delta and compacted Vocab states.
    for compacted in [false, true] {
        if compacted {
            s.compact().unwrap();
        }
        let mut r = resolver();
        r.verify_proposed_passages(
            &s,
            &s,
            Some(&all()),
            &rend,
            &[ProposedPassage {
                key: "p1",
                revision: &rev,
                start: 6,
                end: 10,
                text_digest: span_digest,
                quote: Some("café"),
            }],
        )
        .unwrap();
        r.verify_passage_reference("ref1", &s, &s, Some(&all()), &pass, &rev, &rend)
            .unwrap();
        r.verify_structured_references(
            &s,
            &s,
            Some(&all()),
            &rev,
            &[ProposedStructured {
                key: "s1",
                pointer: "/payload",
                value_digest: val_digest,
            }],
        )
        .unwrap();
    }

    // Tight memory allowance refuses with incomplete-evidence (not unavailable).
    let mut tight = Resolver::new(Control::none(), 16 * 1024).unwrap();
    assert_eq!(
        code(tight.verify_structured_references(
            &s,
            &s,
            Some(&all()),
            &rev,
            &[ProposedStructured {
                key: "s1",
                pointer: "/payload",
                value_digest: val_digest,
            }],
        )),
        "incomplete-evidence"
    );
    let mut tight_p = Resolver::new(Control::none(), 12 * 1024).unwrap();
    assert_eq!(
        code(tight_p.verify_proposed_passages(
            &s,
            &s,
            Some(&all()),
            &rend,
            &[ProposedPassage {
                key: "p1",
                revision: &rev,
                start: 6,
                end: 10,
                text_digest: span_digest,
                quote: Some("café"),
            }],
        )),
        "incomplete-evidence"
    );

    // Cancellation propagates Error::Cancelled.
    let flag = Arc::new(AtomicBool::new(false));
    let mut cancelled = Resolver::new(Control::with_cancel(flag.clone()), MEMORY_BYTES).unwrap();
    flag.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(matches!(
        cancelled.verify_structured_references(
            &s,
            &s,
            Some(&all()),
            &rev,
            &[ProposedStructured {
                key: "s1",
                pointer: "/payload",
                value_digest: val_digest,
            }],
        ),
        Err(Error::Cancelled)
    ));

    // Triple-level protections hiding retainedBytes, normalizedText, or sourceRevision
    // answer identically with evidence-unavailable.
    let hide_pred = |pred: &str| {
        let protection = Protection {
            name: "hidden".into(),
            predicates: Some(vec![pred.to_string()]),
            classes: None,
            subclasses: false,
            graphs: None,
            pattern: None,
            prefixes: Default::default(),
            hide_inferences: false,
        };
        let rules = TripleRules {
            rules: vec![Rule {
                protection: Arc::new(protection),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Default::default(),
            limits: Default::default(),
        };
        Arc::new(GraphAccess::with_triples(Graphs::All, Graphs::All, rules))
    };
    let mut r = resolver();
    assert_eq!(
        code(r.verify_structured_references(
            &s,
            &s,
            Some(&hide_pred(&format!("{EV}retainedBytes"))),
            &rev,
            &[ProposedStructured {
                key: "s1",
                pointer: "/payload",
                value_digest: val_digest,
            }],
        )),
        "evidence-unavailable"
    );
    assert_eq!(
        code(r.verify_proposed_passages(
            &s,
            &s,
            Some(&hide_pred(&format!("{EV}normalizedText"))),
            &rend,
            &[ProposedPassage {
                key: "p1",
                revision: &rev,
                start: 6,
                end: 10,
                text_digest: span_digest,
                quote: Some("café"),
            }],
        )),
        "evidence-unavailable"
    );
    assert_eq!(
        code(r.verify_proposed_passages(
            &s,
            &s,
            Some(&hide_pred(PROV_WAS_DERIVED_FROM)),
            &rend,
            &[ProposedPassage {
                key: "p1",
                revision: &rev,
                start: 6,
                end: 10,
                text_digest: span_digest,
                quote: Some("café"),
            }],
        )),
        "evidence-unavailable"
    );
    assert_eq!(
        code(r.verify_passage_reference(
            "ref1",
            &s,
            &s,
            Some(&hide_pred(&format!("{EV}sourceRevision"))),
            &pass,
            &rev,
            &rend,
        )),
        "evidence-unavailable"
    );
    assert_eq!(
        code(r.verify_structured_references(
            &s,
            &s,
            Some(&hide_pred(&format!("{EV}contentDigest"))),
            &rev,
            &[ProposedStructured {
                key: "s1",
                pointer: "/payload",
                value_digest: val_digest,
            }],
        )),
        "evidence-unavailable"
    );
}

#[test]
fn masked_missing_and_metadata_only_selections_have_full_body_parity() {
    let (s, nonce) = configured_evidence_store();
    let sg = oxrdf::GraphName::NamedNode(iri("urn:ev:sources"));
    let op = iri("urn:op:parity");
    let rev1 = iri("urn:rev:1");
    let rev2 = iri("urn:rev:2");
    let rev_meta = iri("urn:rev:meta");
    let rend1 = iri("urn:rend:1");
    let rend2 = iri("urn:rend:2");
    let rend_meta = iri("urn:rend:meta");
    let pass1 = iri("urn:pass:1");
    let sel1 = iri("urn:pass:1#sel");
    let missing_iri = iri("urn:missing:target");

    let json_bytes = r#"{"k":"v"}"#;
    let json_digest = Sha256Digest::of_bytes(json_bytes.as_bytes(), &Control::none()).unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(json_bytes.as_bytes());
    let val_digest = Sha256Digest::of_bytes(b"\"v\"", &Control::none()).unwrap();
    let text = "hello world";
    let span_digest = Sha256Digest::of_bytes(b"world", &Control::none()).unwrap();

    let q =
        |sub: &NamedNode, p: &str, o: Term| oxrdf::Quad::new(sub.clone(), iri(p), o, sg.clone());
    let lit = |v: &str| Term::Literal(Literal::new_simple_literal(v));
    let int_lit =
        |n: u64| Term::Literal(Literal::new_typed_literal(n.to_string(), iri(XSD_INTEGER)));
    let auth = |sub: &NamedNode, kind: RecordKind| {
        vec![
            q(sub, RDF_TYPE, iri(kind_iri(kind)).into()),
            q(sub, &format!("{EV}availabilityReceipt"), op.clone().into()),
            q(
                sub,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
        ]
    };

    let mut quads = vec![
        q(&op, RDF_TYPE, iri(&format!("{EV}Operation")).into()),
        q(
            &op,
            &format!("{EV}datasetId"),
            lit(&s.dataset_id().to_string()),
        ),
        q(
            &op,
            &format!("{EV}branchId"),
            lit(&s.branch_id().to_string()),
        ),
        q(&op, &format!("{EV}commit"), int_lit(1)),
        q(
            &op,
            &format!("{EV}graphProtection"),
            lit(&nonce.to_string()),
        ),
    ];
    quads.extend(auth(&rev1, RecordKind::SourceRevision));
    quads.push(q(
        &rev1,
        &format!("{EV}contentDigest"),
        lit(&json_digest.to_string()),
    ));
    quads.push(q(
        &rev1,
        &format!("{EV}retainedBytes"),
        Term::Literal(Literal::new_typed_literal(b64, iri(XSD_BASE64_BINARY))),
    ));
    quads.extend(auth(&rev2, RecordKind::SourceRevision));
    quads.extend(auth(&rev_meta, RecordKind::SourceRevision));
    quads.push(q(
        &rev_meta,
        &format!("{EV}contentDigest"),
        lit(&json_digest.to_string()),
    ));

    quads.extend(auth(&rend1, RecordKind::Rendition));
    quads.push(q(&rend1, PROV_WAS_DERIVED_FROM, rev1.clone().into()));
    quads.push(q(&rend1, &format!("{EV}normalizedText"), lit(text)));

    quads.extend(auth(&rend2, RecordKind::Rendition));
    quads.push(q(&rend2, PROV_WAS_DERIVED_FROM, rev2.clone().into()));
    quads.push(q(&rend2, &format!("{EV}normalizedText"), lit(text)));

    quads.extend(auth(&rend_meta, RecordKind::Rendition));
    quads.push(q(&rend_meta, PROV_WAS_DERIVED_FROM, rev1.clone().into()));

    quads.extend(auth(&pass1, RecordKind::Passage));
    quads.push(q(
        &pass1,
        &format!("{EV}sourceRevision"),
        rev1.clone().into(),
    ));
    quads.push(q(&pass1, &format!("{OA}hasSource"), rend1.clone().into()));
    quads.push(q(&pass1, &format!("{OA}hasSelector"), sel1.clone().into()));
    quads.push(q(&sel1, &format!("{OA}start"), int_lit(6)));
    quads.push(q(&sel1, &format!("{OA}end"), int_lit(11)));
    quads.push(q(
        &pass1,
        &format!("{EV}textDigest"),
        lit(&span_digest.to_string()),
    ));
    insert_managed_quads(&s, quads);

    let hide_pred = |pred: &str| {
        let protection = Protection {
            name: "hidden".into(),
            predicates: Some(vec![pred.to_string()]),
            classes: None,
            subclasses: false,
            graphs: None,
            pattern: None,
            prefixes: Default::default(),
            hide_inferences: false,
        };
        let rules = TripleRules {
            rules: vec![Rule {
                protection: Arc::new(protection),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Default::default(),
            limits: Default::default(),
        };
        Arc::new(GraphAccess::with_triples(Graphs::All, Graphs::All, rules))
    };

    // 1. Proposed passage of existing rendition: absent vs metadata-only vs masked
    //    ev:normalizedText / prov:wasDerivedFrom — both matching and MISMATCHED revisions.
    let prop_err = |target: &NamedNode, rev: &NamedNode, access: Arc<GraphAccess>| {
        let mut r = resolver();
        let e = r
            .verify_proposed_passages(
                &s,
                &s,
                Some(&access),
                target,
                &[ProposedPassage {
                    key: "p1",
                    revision: rev,
                    start: 6,
                    end: 11,
                    text_digest: span_digest,
                    quote: Some("world"),
                }],
            )
            .unwrap_err();
        (
            e.code().to_string(),
            e.to_string().replace(target.as_str(), "TARGET"),
        )
    };
    let absent_rend = prop_err(&missing_iri, &rev2, all());
    assert_eq!(absent_rend.0, "evidence-unavailable");
    assert_eq!(absent_rend.1, "evidence reference `TARGET` is unavailable");
    for rev in [&rev1, &rev2] {
        assert_eq!(prop_err(&rend_meta, rev, all()), absent_rend);
        assert_eq!(
            prop_err(&rend1, rev, hide_pred(&format!("{EV}normalizedText"))),
            absent_rend
        );
        assert_eq!(
            prop_err(&rend1, rev, hide_pred(PROV_WAS_DERIVED_FROM)),
            absent_rend
        );
    }

    // 2. Reference to existing passage: absent vs every required link/selector masked —
    //    both with matching and MISMATCHED revision/rendition.
    let pass_ref_err = |target: &NamedNode,
                        exp_rev: &NamedNode,
                        exp_rend: &NamedNode,
                        access: Arc<GraphAccess>| {
        let mut r = resolver();
        let e = r
            .verify_passage_reference("ref1", &s, &s, Some(&access), target, exp_rev, exp_rend)
            .unwrap_err();
        (
            e.code().to_string(),
            e.to_string().replace(target.as_str(), "TARGET"),
        )
    };
    let absent_pass = pass_ref_err(&missing_iri, &rev2, &rend2, all());
    assert_eq!(absent_pass.0, "evidence-unavailable");
    assert_eq!(absent_pass.1, "evidence reference `TARGET` is unavailable");
    for (exp_rev, exp_rend) in [(&rev1, &rend1), (&rev2, &rend1), (&rev1, &rend2)] {
        for pred in [
            format!("{OA}hasSource"),
            format!("{OA}hasSelector"),
            format!("{OA}start"),
            format!("{OA}end"),
            format!("{EV}textDigest"),
            format!("{EV}sourceRevision"),
            format!("{EV}normalizedText"),
            PROV_WAS_DERIVED_FROM.to_string(),
        ] {
            assert_eq!(
                pass_ref_err(&pass1, exp_rev, exp_rend, hide_pred(&pred)),
                absent_pass,
                "masking {pred} must match absent passage"
            );
        }
    }

    // 3. Structured reference: absent vs metadata-only vs masked retainedBytes/contentDigest.
    let struct_err = |target: &NamedNode, access: Arc<GraphAccess>| {
        let mut r = resolver();
        let e = r
            .verify_structured_references(
                &s,
                &s,
                Some(&access),
                target,
                &[ProposedStructured {
                    key: "s1",
                    pointer: "/k",
                    value_digest: val_digest,
                }],
            )
            .unwrap_err();
        (
            e.code().to_string(),
            e.to_string().replace(target.as_str(), "TARGET"),
        )
    };
    let absent_rev = struct_err(&missing_iri, all());
    assert_eq!(absent_rev.0, "evidence-unavailable");
    assert_eq!(absent_rev.1, "evidence reference `TARGET` is unavailable");
    assert_eq!(struct_err(&rev_meta, all()), absent_rev);
    assert_eq!(
        struct_err(&rev1, hide_pred(&format!("{EV}retainedBytes"))),
        absent_rev
    );
    assert_eq!(
        struct_err(&rev1, hide_pred(&format!("{EV}contentDigest"))),
        absent_rev
    );
}

#[test]
fn structured_content_digest_pointer_validation_and_ambiguous_conflicts() {
    let (s, nonce) = configured_evidence_store();
    let sg = oxrdf::GraphName::NamedNode(iri("urn:ev:sources"));
    let op = iri("urn:op:conflicts");
    let q =
        |sub: &NamedNode, p: &str, o: Term| oxrdf::Quad::new(sub.clone(), iri(p), o, sg.clone());
    let lit = |v: &str| Term::Literal(Literal::new_simple_literal(v));
    let int_lit =
        |n: u64| Term::Literal(Literal::new_typed_literal(n.to_string(), iri(XSD_INTEGER)));
    let auth = |sub: &NamedNode, kind: RecordKind| {
        vec![
            q(sub, RDF_TYPE, iri(kind_iri(kind)).into()),
            q(sub, &format!("{EV}availabilityReceipt"), op.clone().into()),
            q(
                sub,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
        ]
    };

    let json = r#"{"a":1}"#;
    let good_digest = Sha256Digest::of_bytes(json.as_bytes(), &Control::none()).unwrap();
    let wrong_digest = Sha256Digest::of_bytes(b"different", &Control::none()).unwrap();
    let val_digest = Sha256Digest::of_bytes(b"1", &Control::none()).unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
    let text = "hello";
    let text_digest = Sha256Digest::of_bytes(b"he", &Control::none()).unwrap();

    let rev_ok = iri("urn:rev:ok");
    let rev_no_digest = iri("urn:rev:no-digest");
    let rev_wrong_digest = iri("urn:rev:wrong-digest");
    let rev_bad_b64 = iri("urn:rev:bad-b64");
    let rev_multi_bytes = iri("urn:rev:multi-bytes");
    let rend_multi_rev = iri("urn:rend:multi-rev");
    let rend_multi_text = iri("urn:rend:multi-text");
    let rend_ok = iri("urn:rend:ok");
    let pass_multi_rev = iri("urn:pass:multi-rev");
    let pass_multi_src = iri("urn:pass:multi-src");
    let sel_mr = iri("urn:pass:multi-rev#sel");
    let sel_ms = iri("urn:pass:multi-src#sel");

    let mut quads = vec![
        q(&op, RDF_TYPE, iri(&format!("{EV}Operation")).into()),
        q(
            &op,
            &format!("{EV}datasetId"),
            lit(&s.dataset_id().to_string()),
        ),
        q(
            &op,
            &format!("{EV}branchId"),
            lit(&s.branch_id().to_string()),
        ),
        q(&op, &format!("{EV}commit"), int_lit(1)),
        q(
            &op,
            &format!("{EV}graphProtection"),
            lit(&nonce.to_string()),
        ),
    ];
    // rev_ok: valid JSON revision
    quads.extend(auth(&rev_ok, RecordKind::SourceRevision));
    quads.push(q(
        &rev_ok,
        &format!("{EV}contentDigest"),
        lit(&good_digest.to_string()),
    ));
    quads.push(q(
        &rev_ok,
        &format!("{EV}retainedBytes"),
        Term::Literal(Literal::new_typed_literal(&b64, iri(XSD_BASE64_BINARY))),
    ));
    // rev_no_digest: retainedBytes without ev:contentDigest
    quads.extend(auth(&rev_no_digest, RecordKind::SourceRevision));
    quads.push(q(
        &rev_no_digest,
        &format!("{EV}retainedBytes"),
        Term::Literal(Literal::new_typed_literal(&b64, iri(XSD_BASE64_BINARY))),
    ));
    // rev_wrong_digest: present ev:contentDigest disagrees with retainedBytes
    quads.extend(auth(&rev_wrong_digest, RecordKind::SourceRevision));
    quads.push(q(
        &rev_wrong_digest,
        &format!("{EV}contentDigest"),
        lit(&wrong_digest.to_string()),
    ));
    quads.push(q(
        &rev_wrong_digest,
        &format!("{EV}retainedBytes"),
        Term::Literal(Literal::new_typed_literal(&b64, iri(XSD_BASE64_BINARY))),
    ));
    // rev_bad_b64: corrupted base64
    quads.extend(auth(&rev_bad_b64, RecordKind::SourceRevision));
    quads.push(q(
        &rev_bad_b64,
        &format!("{EV}contentDigest"),
        lit(&good_digest.to_string()),
    ));
    quads.push(q(
        &rev_bad_b64,
        &format!("{EV}retainedBytes"),
        Term::Literal(Literal::new_typed_literal(
            "!!!not-base64!!!",
            iri(XSD_BASE64_BINARY),
        )),
    ));
    // rev_multi_bytes: multiple ev:retainedBytes
    quads.extend(auth(&rev_multi_bytes, RecordKind::SourceRevision));
    quads.push(q(
        &rev_multi_bytes,
        &format!("{EV}contentDigest"),
        lit(&good_digest.to_string()),
    ));
    quads.push(q(
        &rev_multi_bytes,
        &format!("{EV}retainedBytes"),
        Term::Literal(Literal::new_typed_literal(&b64, iri(XSD_BASE64_BINARY))),
    ));
    quads.push(q(
        &rev_multi_bytes,
        &format!("{EV}retainedBytes"),
        Term::Literal(Literal::new_typed_literal("e30=", iri(XSD_BASE64_BINARY))),
    ));
    // rend_ok
    quads.extend(auth(&rend_ok, RecordKind::Rendition));
    quads.push(q(&rend_ok, PROV_WAS_DERIVED_FROM, rev_ok.clone().into()));
    quads.push(q(&rend_ok, &format!("{EV}normalizedText"), lit(text)));
    // rend_multi_rev: multiple prov:wasDerivedFrom
    quads.extend(auth(&rend_multi_rev, RecordKind::Rendition));
    quads.push(q(
        &rend_multi_rev,
        PROV_WAS_DERIVED_FROM,
        rev_ok.clone().into(),
    ));
    quads.push(q(
        &rend_multi_rev,
        PROV_WAS_DERIVED_FROM,
        rev_no_digest.clone().into(),
    ));
    quads.push(q(
        &rend_multi_rev,
        &format!("{EV}normalizedText"),
        lit(text),
    ));
    // rend_multi_text: multiple ev:normalizedText
    quads.extend(auth(&rend_multi_text, RecordKind::Rendition));
    quads.push(q(
        &rend_multi_text,
        PROV_WAS_DERIVED_FROM,
        rev_ok.clone().into(),
    ));
    quads.push(q(
        &rend_multi_text,
        &format!("{EV}normalizedText"),
        lit(text),
    ));
    quads.push(q(
        &rend_multi_text,
        &format!("{EV}normalizedText"),
        lit("other"),
    ));
    // pass_multi_rev: multiple ev:sourceRevision
    quads.extend(auth(&pass_multi_rev, RecordKind::Passage));
    quads.push(q(
        &pass_multi_rev,
        &format!("{EV}sourceRevision"),
        rev_ok.clone().into(),
    ));
    quads.push(q(
        &pass_multi_rev,
        &format!("{EV}sourceRevision"),
        rev_no_digest.clone().into(),
    ));
    quads.push(q(
        &pass_multi_rev,
        &format!("{OA}hasSource"),
        rend_ok.clone().into(),
    ));
    quads.push(q(
        &pass_multi_rev,
        &format!("{OA}hasSelector"),
        sel_mr.clone().into(),
    ));
    quads.push(q(&sel_mr, &format!("{OA}start"), int_lit(0)));
    quads.push(q(&sel_mr, &format!("{OA}end"), int_lit(2)));
    quads.push(q(
        &pass_multi_rev,
        &format!("{EV}textDigest"),
        lit(&text_digest.to_string()),
    ));
    // pass_multi_src: multiple oa:hasSource
    quads.extend(auth(&pass_multi_src, RecordKind::Passage));
    quads.push(q(
        &pass_multi_src,
        &format!("{EV}sourceRevision"),
        rev_ok.clone().into(),
    ));
    quads.push(q(
        &pass_multi_src,
        &format!("{OA}hasSource"),
        rend_ok.clone().into(),
    ));
    quads.push(q(
        &pass_multi_src,
        &format!("{OA}hasSource"),
        rend_multi_rev.clone().into(),
    ));
    quads.push(q(
        &pass_multi_src,
        &format!("{OA}hasSelector"),
        sel_ms.clone().into(),
    ));
    quads.push(q(&sel_ms, &format!("{OA}start"), int_lit(0)));
    quads.push(q(&sel_ms, &format!("{OA}end"), int_lit(2)));
    quads.push(q(
        &pass_multi_src,
        &format!("{EV}textDigest"),
        lit(&text_digest.to_string()),
    ));
    insert_managed_quads(&s, quads);

    let mut r = resolver();
    // Missing ev:contentDigest -> evidence-unavailable.
    assert_eq!(
        code(r.verify_structured_references(
            &s,
            &s,
            Some(&all()),
            &rev_no_digest,
            &[ProposedStructured {
                key: "s1",
                pointer: "/a",
                value_digest: val_digest,
            }],
        )),
        "evidence-unavailable"
    );
    // Mismatched ev:contentDigest -> span-mismatch (per C25).
    assert_eq!(
        code(r.verify_structured_references(
            &s,
            &s,
            Some(&all()),
            &rev_wrong_digest,
            &[ProposedStructured {
                key: "s1",
                pointer: "/a",
                value_digest: val_digest,
            }],
        )),
        "span-mismatch"
    );
    // Corrupted base64 -> immutable-record-conflict.
    assert_eq!(
        code(r.verify_structured_references(
            &s,
            &s,
            Some(&all()),
            &rev_bad_b64,
            &[ProposedStructured {
                key: "s1",
                pointer: "/a",
                value_digest: val_digest,
            }],
        )),
        "immutable-record-conflict"
    );
    // Malformed RFC 6901 pointers in direct Rust API -> Error::Invalid (not span-mismatch).
    for bad_ptr in ["/bad~2", "/bad~", "no-leading-slash"] {
        let err = r
            .verify_structured_references(
                &s,
                &s,
                Some(&all()),
                &rev_ok,
                &[ProposedStructured {
                    key: "s1",
                    pointer: bad_ptr,
                    value_digest: val_digest,
                }],
            )
            .unwrap_err();
        assert!(
            matches!(err, Error::Invalid(_)),
            "pointer {bad_ptr:?} should return Error::Invalid, got {err:?}"
        );
    }
    // Multi-valued predicates -> immutable-record-conflict.
    assert_eq!(
        code(r.verify_structured_references(
            &s,
            &s,
            Some(&all()),
            &rev_multi_bytes,
            &[ProposedStructured {
                key: "s1",
                pointer: "/a",
                value_digest: val_digest,
            }],
        )),
        "immutable-record-conflict"
    );
    assert_eq!(
        code(r.verify_proposed_passages(
            &s,
            &s,
            Some(&all()),
            &rend_multi_rev,
            &[ProposedPassage {
                key: "p1",
                revision: &rev_ok,
                start: 0,
                end: 2,
                text_digest,
                quote: Some("he"),
            }],
        )),
        "immutable-record-conflict"
    );
    assert_eq!(
        code(r.verify_proposed_passages(
            &s,
            &s,
            Some(&all()),
            &rend_multi_text,
            &[ProposedPassage {
                key: "p1",
                revision: &rev_ok,
                start: 0,
                end: 2,
                text_digest,
                quote: Some("he"),
            }],
        )),
        "immutable-record-conflict"
    );
    assert_eq!(
        code(r.verify_passage_reference(
            "ref1",
            &s,
            &s,
            Some(&all()),
            &pass_multi_rev,
            &rev_ok,
            &rend_ok,
        )),
        "immutable-record-conflict"
    );
    assert_eq!(
        code(r.verify_passage_reference(
            "ref2",
            &s,
            &s,
            Some(&all()),
            &pass_multi_src,
            &rev_ok,
            &rend_ok,
        )),
        "immutable-record-conflict"
    );
}

#[test]
fn compacted_large_json_revision_tight_grammar_scratch_and_midstream_cancel() {
    let (s, nonce) = configured_evidence_store();
    let sg = oxrdf::GraphName::NamedNode(iri("urn:ev:sources"));
    let op = iri("urn:op:large");
    let rev_large = iri("urn:rev:large");
    let q =
        |sub: &NamedNode, p: &str, o: Term| oxrdf::Quad::new(sub.clone(), iri(p), o, sg.clone());
    let lit = |v: &str| Term::Literal(Literal::new_simple_literal(v));
    let int_lit =
        |n: u64| Term::Literal(Literal::new_typed_literal(n.to_string(), iri(XSD_INTEGER)));

    // 13 MiB JSON revision (~17.33 MiB base64) compacted into Vocab: previously
    // exceeded the 64 MiB allowance when with_typed_literal held ~3x base64 workspace
    // across JSON selection.
    let big_json = format!(
        r#"{{"tail":"ok","blob":"{}"}}"#,
        "a".repeat(13 * 1024 * 1024)
    );
    let big_digest = Sha256Digest::of_bytes(big_json.as_bytes(), &Control::none()).unwrap();
    let big_b64 = base64::engine::general_purpose::STANDARD.encode(big_json.as_bytes());
    let ok_digest = Sha256Digest::of_bytes(b"\"ok\"", &Control::none()).unwrap();

    insert_managed_quads(
        &s,
        vec![
            q(&op, RDF_TYPE, iri(&format!("{EV}Operation")).into()),
            q(
                &op,
                &format!("{EV}datasetId"),
                lit(&s.dataset_id().to_string()),
            ),
            q(
                &op,
                &format!("{EV}branchId"),
                lit(&s.branch_id().to_string()),
            ),
            q(&op, &format!("{EV}commit"), int_lit(1)),
            q(
                &op,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(
                &rev_large,
                RDF_TYPE,
                iri(kind_iri(RecordKind::SourceRevision)).into(),
            ),
            q(
                &rev_large,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rev_large,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(
                &rev_large,
                &format!("{EV}contentDigest"),
                lit(&big_digest.to_string()),
            ),
            q(
                &rev_large,
                &format!("{EV}retainedBytes"),
                Term::Literal(Literal::new_typed_literal(big_b64, iri(XSD_BASE64_BINARY))),
            ),
        ],
    );
    s.compact().unwrap();

    // 13 MiB compacted JSON revision succeeds under the standard 64 MiB allowance.
    let mut r = resolver();
    r.verify_structured_references(
        &s,
        &s,
        Some(&all()),
        &rev_large,
        &[ProposedStructured {
            key: "big",
            pointer: "/tail",
            value_digest: ok_digest,
        }],
    )
    .unwrap();

    // Mid-stream cancellation during chunked base64 decoding propagates Error::Cancelled.
    let mut c = Resolver::new(Control::none(), MEMORY_BYTES).unwrap();
    CANCEL_BASE64_AT_CHUNK.with(|at| at.set(Some(2)));
    let cancelled = c.verify_structured_references(
        &s,
        &s,
        Some(&all()),
        &rev_large,
        &[ProposedStructured {
            key: "big",
            pointer: "/tail",
            value_digest: ok_digest,
        }],
    );
    CANCEL_BASE64_AT_CHUNK.with(|at| at.set(None));
    assert!(matches!(cancelled, Err(Error::Cancelled)));

    // Separate store for the shallow JSON grammar-scratch budget test so the 17 MiB
    // literal above cannot share a front-coded Vocab block with the shallow literal.
    let (s_shallow, nonce_shallow) = configured_evidence_store();
    let rev_shallow = iri("urn:rev:shallow");
    let shallow_json = r#"{"a":1}"#;
    let shallow_digest = Sha256Digest::of_bytes(shallow_json.as_bytes(), &Control::none()).unwrap();
    let shallow_b64 = base64::engine::general_purpose::STANDARD.encode(shallow_json.as_bytes());
    insert_managed_quads(
        &s_shallow,
        vec![
            q(&op, RDF_TYPE, iri(&format!("{EV}Operation")).into()),
            q(
                &op,
                &format!("{EV}datasetId"),
                lit(&s_shallow.dataset_id().to_string()),
            ),
            q(
                &op,
                &format!("{EV}branchId"),
                lit(&s_shallow.branch_id().to_string()),
            ),
            q(&op, &format!("{EV}commit"), int_lit(1)),
            q(
                &op,
                &format!("{EV}graphProtection"),
                lit(&nonce_shallow.to_string()),
            ),
            q(
                &rev_shallow,
                RDF_TYPE,
                iri(kind_iri(RecordKind::SourceRevision)).into(),
            ),
            q(
                &rev_shallow,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rev_shallow,
                &format!("{EV}graphProtection"),
                lit(&nonce_shallow.to_string()),
            ),
            q(
                &rev_shallow,
                &format!("{EV}contentDigest"),
                lit(&shallow_digest.to_string()),
            ),
            q(
                &rev_shallow,
                &format!("{EV}retainedBytes"),
                Term::Literal(Literal::new_typed_literal(
                    shallow_b64,
                    iri(XSD_BASE64_BINARY),
                )),
            ),
        ],
    );
    s_shallow.compact().unwrap();

    // Tight preparation budget that admits record-graph decoding (~66 KiB peak),
    // record authentication (~10.4 KiB peak), vocabulary reconstruction, decoded bytes,
    // 256 root pointers (~86.2 KiB), and shallow traversal (< 0.5 KiB), then refuses
    // strictly at `VALIDATE_SCRATCH_BYTES` (4 KiB) grammar validation scratch.
    let mut probe = resolver();
    probe
        .record(
            "probe",
            &s_shallow,
            &s_shallow,
            Some(&all()),
            &RecordRequest {
                iri: rev_shallow.clone(),
                kinds: vec![RecordKind::SourceRevision],
                content_digest: None,
            },
        )
        .unwrap();
    let baseline = probe.preparation().inner_used();
    let shallow_refs: Vec<ProposedStructured<'_>> = (0..256)
        .map(|_| ProposedStructured {
            key: "shallow",
            pointer: "",
            value_digest: shallow_digest,
        })
        .collect();
    let tight_limit = baseline + 88_000;
    let mut tight = Resolver::new(Control::none(), tight_limit).unwrap();
    assert_eq!(
        code(tight.verify_structured_references(
            &s_shallow,
            &s_shallow,
            Some(&all()),
            &rev_shallow,
            &shallow_refs,
        )),
        "incomplete-evidence"
    );
    // Adding room for `VALIDATE_SCRATCH_BYTES` lets the shallow selection succeed.
    let mut enough = Resolver::new(Control::none(), tight_limit + 4096).unwrap();
    enough
        .verify_structured_references(
            &s_shallow,
            &s_shallow,
            Some(&all()),
            &rev_shallow,
            &shallow_refs,
        )
        .unwrap();
}

#[test]
fn repeated_passage_references_scan_rendition_text_once() {
    let (s, nonce) = configured_evidence_store();
    let sg = oxrdf::GraphName::NamedNode(iri("urn:ev:sources"));
    let op = iri("urn:op:scan");
    let rev = iri("urn:rev:scan");
    let rend = iri("urn:rend:scan");
    let pass = iri("urn:pass:scan");
    let sel = iri("urn:pass:scan#sel");
    let text = "abcdefgh".repeat(4096);
    let tail = &text[text.len() - 4..];
    let span_digest = Sha256Digest::of_bytes(tail.as_bytes(), &Control::none()).unwrap();
    let q =
        |sub: &NamedNode, p: &str, o: Term| oxrdf::Quad::new(sub.clone(), iri(p), o, sg.clone());
    let lit = |v: &str| Term::Literal(Literal::new_simple_literal(v));
    let int_lit =
        |n: u64| Term::Literal(Literal::new_typed_literal(n.to_string(), iri(XSD_INTEGER)));
    insert_managed_quads(
        &s,
        vec![
            q(&op, RDF_TYPE, iri(&format!("{EV}Operation")).into()),
            q(
                &op,
                &format!("{EV}datasetId"),
                lit(&s.dataset_id().to_string()),
            ),
            q(
                &op,
                &format!("{EV}branchId"),
                lit(&s.branch_id().to_string()),
            ),
            q(&op, &format!("{EV}commit"), int_lit(1)),
            q(
                &op,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(
                &rev,
                RDF_TYPE,
                iri(kind_iri(RecordKind::SourceRevision)).into(),
            ),
            q(&rev, &format!("{EV}availabilityReceipt"), op.clone().into()),
            q(
                &rev,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(&rend, RDF_TYPE, iri(kind_iri(RecordKind::Rendition)).into()),
            q(
                &rend,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rend,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(&rend, PROV_WAS_DERIVED_FROM, rev.clone().into()),
            q(&rend, &format!("{EV}normalizedText"), lit(&text)),
            q(&pass, RDF_TYPE, iri(kind_iri(RecordKind::Passage)).into()),
            q(
                &pass,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &pass,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(&pass, &format!("{EV}sourceRevision"), rev.clone().into()),
            q(&pass, &format!("{OA}hasSource"), rend.clone().into()),
            q(&pass, &format!("{OA}hasSelector"), sel.clone().into()),
            q(
                &sel,
                &format!("{OA}start"),
                int_lit((text.len() - 4) as u64),
            ),
            q(&sel, &format!("{OA}end"), int_lit(text.len() as u64)),
            q(
                &pass,
                &format!("{EV}textDigest"),
                lit(&span_digest.to_string()),
            ),
        ],
    );

    PASSAGE_TEXT_SCANS.with(|count| count.set(0));
    let mut r = resolver();
    for i in 0..5 {
        r.record(
            &format!("rec-{i}"),
            &s,
            &s,
            Some(&all()),
            &RecordRequest {
                iri: pass.clone(),
                kinds: vec![RecordKind::Passage],
                content_digest: None,
            },
        )
        .unwrap();
        r.verify_passage_reference(
            &format!("ref-{i}"),
            &s,
            &s,
            Some(&all()),
            &pass,
            &rev,
            &rend,
        )
        .unwrap();
    }
    assert_eq!(PASSAGE_TEXT_SCANS.with(std::cell::Cell::get), 1);

    // Re-protecting the graph invalidates the cached passage on the same Resolver.
    let settings = super::super::Settings {
        enabled: true,
        source_records: vec!["urn:ev:sources".into()],
        proposals: vec!["urn:ev:claims".into()],
        ..Default::default()
    };
    let canonical = settings.canonical(&Control::none()).unwrap();
    let entry = crate::managed::OwnerEntry::for_settings(
        settings.profile,
        settings.managed_graphs(),
        canonical.bytes(),
    );
    let reg_rev = s
        .detach_managed(OwnerKind::Evidence, "urn:ev:sources")
        .unwrap()
        .revision;
    s.enable_managed_at(OwnerKind::Evidence, entry, canonical.bytes(), reg_rev)
        .unwrap();
    assert_eq!(
        code(r.verify_passage_reference("ref-stale", &s, &s, Some(&all()), &pass, &rev, &rend)),
        "origin-not-reproduced"
    );
}

#[test]
fn direct_verify_passage_reference_refuses_duplicate_or_malformed_record_digests() {
    let (s, nonce) = configured_evidence_store();
    let sg = oxrdf::GraphName::NamedNode(iri("urn:ev:sources"));
    let op = iri("urn:op:digest-conflict");
    let rev = iri("urn:rev:dc");
    let rend = iri("urn:rend:dc");
    let pass_dup = iri("urn:pass:dc-dup");
    let sel_dup = iri("urn:pass:dc-dup#sel");
    let pass_bad = iri("urn:pass:dc-bad");
    let sel_bad = iri("urn:pass:dc-bad#sel");
    let text = "hello world";
    let span_digest = Sha256Digest::of_bytes(b"hello", &Control::none()).unwrap();
    let d1 = Sha256Digest::of_bytes(b"rec-1", &Control::none()).unwrap();
    let d2 = Sha256Digest::of_bytes(b"rec-2", &Control::none()).unwrap();
    let q =
        |sub: &NamedNode, p: &str, o: Term| oxrdf::Quad::new(sub.clone(), iri(p), o, sg.clone());
    let lit = |v: &str| Term::Literal(Literal::new_simple_literal(v));
    let int_lit =
        |n: u64| Term::Literal(Literal::new_typed_literal(n.to_string(), iri(XSD_INTEGER)));

    insert_managed_quads(
        &s,
        vec![
            q(&op, RDF_TYPE, iri(&format!("{EV}Operation")).into()),
            q(
                &op,
                &format!("{EV}datasetId"),
                lit(&s.dataset_id().to_string()),
            ),
            q(
                &op,
                &format!("{EV}branchId"),
                lit(&s.branch_id().to_string()),
            ),
            q(&op, &format!("{EV}commit"), int_lit(1)),
            q(
                &op,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(&rend, RDF_TYPE, iri(kind_iri(RecordKind::Rendition)).into()),
            q(
                &rend,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rend,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(&rend, PROV_WAS_DERIVED_FROM, rev.clone().into()),
            q(&rend, &format!("{EV}normalizedText"), lit(text)),
            // Passage 1: valid span and lineage, but two visible ev:recordDigest values.
            q(
                &pass_dup,
                RDF_TYPE,
                iri(kind_iri(RecordKind::Passage)).into(),
            ),
            q(
                &pass_dup,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &pass_dup,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(
                &pass_dup,
                &format!("{EV}sourceRevision"),
                rev.clone().into(),
            ),
            q(&pass_dup, &format!("{OA}hasSource"), rend.clone().into()),
            q(
                &pass_dup,
                &format!("{OA}hasSelector"),
                sel_dup.clone().into(),
            ),
            q(&sel_dup, &format!("{OA}start"), int_lit(0)),
            q(&sel_dup, &format!("{OA}end"), int_lit(5)),
            q(
                &pass_dup,
                &format!("{EV}textDigest"),
                lit(&span_digest.to_string()),
            ),
            q(
                &pass_dup,
                &format!("{EV}recordDigest"),
                lit(&d1.to_string()),
            ),
            q(
                &pass_dup,
                &format!("{EV}recordDigest"),
                lit(&d2.to_string()),
            ),
            // Passage 2: valid span and lineage, but malformed ev:contentDigest.
            q(
                &pass_bad,
                RDF_TYPE,
                iri(kind_iri(RecordKind::Passage)).into(),
            ),
            q(
                &pass_bad,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &pass_bad,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(
                &pass_bad,
                &format!("{EV}sourceRevision"),
                rev.clone().into(),
            ),
            q(&pass_bad, &format!("{OA}hasSource"), rend.clone().into()),
            q(
                &pass_bad,
                &format!("{OA}hasSelector"),
                sel_bad.clone().into(),
            ),
            q(&sel_bad, &format!("{OA}start"), int_lit(0)),
            q(&sel_bad, &format!("{OA}end"), int_lit(5)),
            q(
                &pass_bad,
                &format!("{EV}textDigest"),
                lit(&span_digest.to_string()),
            ),
            q(
                &pass_bad,
                &format!("{EV}contentDigest"),
                lit("not-a-digest"),
            ),
        ],
    );

    // Cold-cache direct calls must reject both passages with immutable-record-conflict.
    let mut r_dup = resolver();
    assert_eq!(
        code(r_dup.verify_passage_reference("dup", &s, &s, Some(&all()), &pass_dup, &rev, &rend,)),
        "immutable-record-conflict"
    );
    let mut r_bad = resolver();
    assert_eq!(
        code(r_bad.verify_passage_reference("bad", &s, &s, Some(&all()), &pass_bad, &rev, &rend,)),
        "immutable-record-conflict"
    );
}

#[test]
fn probe_plus_verify_reads_each_retained_literal_once_on_compacted_store() {
    let (s, nonce) = configured_evidence_store();
    let sg = oxrdf::GraphName::NamedNode(iri("urn:ev:sources"));
    let op = iri("urn:op:single-read");
    let rev_ok = iri("urn:rev:single-ok");
    let rev_meta = iri("urn:rev:single-meta");
    let rev_bad_shape = iri("urn:rev:single-bad-shape");
    let rend_ok = iri("urn:rend:single-ok");
    let rend_meta = iri("urn:rend:single-meta");
    let rend_bad_shape = iri("urn:rend:single-bad-shape");
    let json = r#"{"score":42}"#;
    let json_digest = Sha256Digest::of_bytes(json.as_bytes(), &Control::none()).unwrap();
    let val_digest = Sha256Digest::of_bytes(b"42", &Control::none()).unwrap();
    let json_b64 = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
    let text = "single vocabulary read check";
    let span_digest = Sha256Digest::of_bytes(b"single", &Control::none()).unwrap();

    let q =
        |sub: &NamedNode, p: &str, o: Term| oxrdf::Quad::new(sub.clone(), iri(p), o, sg.clone());
    let lit = |v: &str| Term::Literal(Literal::new_simple_literal(v));
    let int_lit =
        |n: u64| Term::Literal(Literal::new_typed_literal(n.to_string(), iri(XSD_INTEGER)));

    insert_managed_quads(
        &s,
        vec![
            q(&op, RDF_TYPE, iri(&format!("{EV}Operation")).into()),
            q(
                &op,
                &format!("{EV}datasetId"),
                lit(&s.dataset_id().to_string()),
            ),
            q(
                &op,
                &format!("{EV}branchId"),
                lit(&s.branch_id().to_string()),
            ),
            q(&op, &format!("{EV}commit"), int_lit(1)),
            q(
                &op,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            // Retained JSON SourceRevision
            q(
                &rev_ok,
                RDF_TYPE,
                iri(kind_iri(RecordKind::SourceRevision)).into(),
            ),
            q(
                &rev_ok,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rev_ok,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(
                &rev_ok,
                &format!("{EV}contentDigest"),
                lit(&json_digest.to_string()),
            ),
            q(
                &rev_ok,
                &format!("{EV}retainedBytes"),
                Term::Literal(Literal::new_typed_literal(json_b64, iri(XSD_BASE64_BINARY))),
            ),
            // Metadata-only SourceRevision
            q(
                &rev_meta,
                RDF_TYPE,
                iri(kind_iri(RecordKind::SourceRevision)).into(),
            ),
            q(
                &rev_meta,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rev_meta,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(
                &rev_meta,
                &format!("{EV}contentDigest"),
                lit(&json_digest.to_string()),
            ),
            // SourceRevision whose ev:retainedBytes is a Vocab IRI (wrong literal shape)
            q(
                &rev_bad_shape,
                RDF_TYPE,
                iri(kind_iri(RecordKind::SourceRevision)).into(),
            ),
            q(
                &rev_bad_shape,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rev_bad_shape,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(
                &rev_bad_shape,
                &format!("{EV}contentDigest"),
                lit(&json_digest.to_string()),
            ),
            q(
                &rev_bad_shape,
                &format!("{EV}retainedBytes"),
                iri("urn:not:base64").into(),
            ),
            // Retained Rendition
            q(
                &rend_ok,
                RDF_TYPE,
                iri(kind_iri(RecordKind::Rendition)).into(),
            ),
            q(
                &rend_ok,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rend_ok,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(&rend_ok, PROV_WAS_DERIVED_FROM, rev_ok.clone().into()),
            q(&rend_ok, &format!("{EV}normalizedText"), lit(text)),
            // Metadata-only Rendition
            q(
                &rend_meta,
                RDF_TYPE,
                iri(kind_iri(RecordKind::Rendition)).into(),
            ),
            q(
                &rend_meta,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rend_meta,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(&rend_meta, PROV_WAS_DERIVED_FROM, rev_ok.clone().into()),
            // Rendition whose ev:normalizedText is a Vocab IRI (wrong literal shape)
            q(
                &rend_bad_shape,
                RDF_TYPE,
                iri(kind_iri(RecordKind::Rendition)).into(),
            ),
            q(
                &rend_bad_shape,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rend_bad_shape,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(
                &rend_bad_shape,
                PROV_WAS_DERIVED_FROM,
                rev_ok.clone().into(),
            ),
            q(
                &rend_bad_shape,
                &format!("{EV}normalizedText"),
                iri("urn:not:simple-literal").into(),
            ),
        ],
    );
    s.compact().unwrap();

    // 1. Proposed passage on compacted rendition: Phase 1 probe does 0 literal reads,
    //    Phase 2 verify does 1 literal read -> 1 total read.
    let mut r = resolver();
    SIMPLE_LITERAL_READS.with(|c| c.set(0));
    let probe = r
        .probe_record_with_selection(
            rend_ok.as_str(),
            &s,
            &s,
            Some(&all()),
            &rend_ok,
            &[RecordKind::Rendition],
            SelectionRequirements {
                rendition_content: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(probe, SelectionProbe::DeferredLiteralShape);
    assert_eq!(SIMPLE_LITERAL_READS.with(std::cell::Cell::get), 0);
    r.verify_proposed_passages(
        &s,
        &s,
        Some(&all()),
        &rend_ok,
        &[ProposedPassage {
            key: "p0",
            revision: &rev_ok,
            start: 0,
            end: 6,
            text_digest: span_digest,
            quote: Some("single"),
        }],
    )
    .unwrap();
    assert_eq!(SIMPLE_LITERAL_READS.with(std::cell::Cell::get), 1);

    // 2. Structured reference on compacted revision: Phase 1 probe does 0 literal reads,
    //    Phase 2 verify does 1 literal read -> 1 total read.
    TYPED_LITERAL_READS.with(|c| c.set(0));
    let probe = r
        .probe_record_with_selection(
            rev_ok.as_str(),
            &s,
            &s,
            Some(&all()),
            &rev_ok,
            &[RecordKind::SourceRevision],
            SelectionRequirements {
                structured_content: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(probe, SelectionProbe::DeferredLiteralShape);
    assert_eq!(TYPED_LITERAL_READS.with(std::cell::Cell::get), 0);
    r.verify_structured_references(
        &s,
        &s,
        Some(&all()),
        &rev_ok,
        &[ProposedStructured {
            key: "s0",
            pointer: "/score",
            value_digest: val_digest,
        }],
    )
    .unwrap();
    assert_eq!(TYPED_LITERAL_READS.with(std::cell::Cell::get), 1);

    // 3. Metadata-only targets fail in Phase 1 with 0 literal reads.
    SIMPLE_LITERAL_READS.with(|c| c.set(0));
    TYPED_LITERAL_READS.with(|c| c.set(0));
    assert_eq!(
        code(r.probe_record_with_selection(
            rend_meta.as_str(),
            &s,
            &s,
            Some(&all()),
            &rend_meta,
            &[RecordKind::Rendition],
            SelectionRequirements {
                rendition_content: true,
                ..Default::default()
            },
        )),
        "evidence-unavailable"
    );
    assert_eq!(
        code(r.probe_record_with_selection(
            rev_meta.as_str(),
            &s,
            &s,
            Some(&all()),
            &rev_meta,
            &[RecordKind::SourceRevision],
            SelectionRequirements {
                structured_content: true,
                ..Default::default()
            },
        )),
        "evidence-unavailable"
    );
    assert_eq!(SIMPLE_LITERAL_READS.with(std::cell::Cell::get), 0);
    assert_eq!(TYPED_LITERAL_READS.with(std::cell::Cell::get), 0);

    // 4. Incompatible kind constraint on selected target: verifies literal shape once on
    //    a fresh resolver so wrong-shape targets answer `evidence-unavailable` while
    //    readable targets preserve `immutable-record-conflict`, and a subsequent fallback
    //    `verify_selection_literal_shape` reuses the cached outcome with 0 extra reads.
    let mut r_incompat = resolver();
    SIMPLE_LITERAL_READS.with(|c| c.set(0));
    assert_eq!(
        code(r_incompat.probe_record_with_selection(
            rend_ok.as_str(),
            &s,
            &s,
            Some(&all()),
            &rend_ok,
            &[RecordKind::SourceRevision],
            SelectionRequirements {
                rendition_content: true,
                ..Default::default()
            },
        )),
        "immutable-record-conflict"
    );
    assert_eq!(SIMPLE_LITERAL_READS.with(std::cell::Cell::get), 1);
    r_incompat
        .verify_selection_literal_shape(
            rend_ok.as_str(),
            &s,
            &s,
            Some(&all()),
            &rend_ok,
            SelectionRequirements {
                rendition_content: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(SIMPLE_LITERAL_READS.with(std::cell::Cell::get), 1);
    assert_eq!(
        code(r_incompat.probe_record_with_selection(
            rend_bad_shape.as_str(),
            &s,
            &s,
            Some(&all()),
            &rend_bad_shape,
            &[RecordKind::SourceRevision],
            SelectionRequirements {
                rendition_content: true,
                ..Default::default()
            },
        )),
        "evidence-unavailable"
    );
    assert_eq!(SIMPLE_LITERAL_READS.with(std::cell::Cell::get), 2);
    assert_eq!(
        code(r_incompat.verify_selection_literal_shape(
            rend_bad_shape.as_str(),
            &s,
            &s,
            Some(&all()),
            &rend_bad_shape,
            SelectionRequirements {
                rendition_content: true,
                ..Default::default()
            },
        )),
        "evidence-unavailable"
    );
    assert_eq!(SIMPLE_LITERAL_READS.with(std::cell::Cell::get), 2);

    // 5. Fallback literal-shape verifier catches wrong-shape Vocab targets cleanly.
    TYPED_LITERAL_READS.with(|c| c.set(0));
    assert_eq!(
        code(r.verify_selection_literal_shape(
            rev_bad_shape.as_str(),
            &s,
            &s,
            Some(&all()),
            &rev_bad_shape,
            SelectionRequirements {
                structured_content: true,
                ..Default::default()
            },
        )),
        "evidence-unavailable"
    );
    assert_eq!(TYPED_LITERAL_READS.with(std::cell::Cell::get), 1);
}
