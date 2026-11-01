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

/// A guard decided at the snapshot exempts its graph through the controlled provider
/// contract, so a reference into an exempt guarded graph verifies.
#[test]
fn a_guarded_graph_is_decided_at_the_snapshot() {
    use crate::access::{Decider, Exemptions, GuardDecision};
    struct Exempt(&'static [&'static str]);
    impl GuardDecision for Exempt {
        fn exempt(&self, _: &Arc<Snapshot>) -> Vec<String> {
            unreachable!("only the controlled contract is used")
        }
        fn exempt_controlled(
            &self,
            _: &Arc<Snapshot>,
            preparation: &Preparation,
        ) -> Result<Exemptions> {
            let mut out = Exemptions::new(preparation)?;
            for g in self.0 {
                out.push(g)?;
            }
            Ok(out)
        }
    }
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
