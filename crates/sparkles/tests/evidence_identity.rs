//! Identity proposals, their reviews and resolution maps (spec C25 §6 and §9).
//!
//! Each test states the map it expects independently of the operations, as the
//! `ev:resolvesTo` pairs the output graph must hold, and compares the store with it
//! after every step.

use oxrdf::NamedNode;
use sparkles::Dataset;
use sparkles::access::{GraphAccess, GraphRule, Graphs};
use sparkles::evidence::{
    self, DecisionInput, Disposition, EvidenceInput, IdentityInput, IdentityRelation,
    MatchingPolicy, ProposeOptions, Proposed, RecordKey, RecordVersion, ReviewOptions, Reviewed,
    Settings, Sha256Digest,
};
use sparkles::task::Control;
use std::sync::Arc;

const SOURCES: &str = "urn:ev:sources";
const CLAIMS: &str = "urn:ev:claims";
const IDENTITY: &str = "urn:ev:identity";
const HIDDEN: &str = "urn:ev:identity-restricted";
const MAPS: &str = "urn:ev:maps";
const DECISIONS: &str = "urn:ev:decisions";
const MAP: &str = "urn:ev:map";
const EV: &str = "urn:x-sparkles:evidence:";

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn key(s: &str) -> RecordKey {
    RecordKey::new(s).unwrap()
}

fn all() -> Option<Arc<GraphAccess>> {
    Some(Arc::new(GraphAccess::all()))
}

#[track_caller]
fn code<T: std::fmt::Debug>(r: sparkles::Result<T>) -> String {
    match r {
        Err(e) => e.code().to_string(),
        Ok(v) => panic!("succeeded: {v:?}"),
    }
}

fn settings(version: u32) -> Settings {
    settings_with(version, &[IDENTITY, HIDDEN, SOURCES, CLAIMS])
}

fn settings_with(version: u32, boundary: &[&str]) -> Settings {
    Settings {
        enabled: true,
        source_records: vec![SOURCES.into()],
        proposals: vec![CLAIMS.into()],
        decisions: vec![DECISIONS.into()],
        identity: vec![IDENTITY.into(), HIDDEN.into(), MAPS.into()],
        projections: vec![MAP.into()],
        matching_policies: [(
            "people".to_string(),
            MatchingPolicy {
                version,
                boundary: boundary.iter().map(|g| g.to_string()).collect(),
                decisions: DECISIONS.into(),
                maps: MAPS.into(),
                output: MAP.into(),
            },
        )]
        .into_iter()
        .collect(),
        ..Settings::default()
    }
}

fn configured() -> Dataset {
    let ds = Dataset::memory();
    evidence::configure(&ds, &settings(1), &Control::none()).unwrap();
    ds
}

fn e(n: &str) -> NamedNode {
    iri(&format!("urn:person:{n}"))
}

type Target = (NamedNode, Sha256Digest);

fn proposal(
    occurrence: &str,
    relation: IdentityRelation,
    entities: &[&str],
    preferred: Option<&str>,
) -> IdentityInput {
    IdentityInput {
        version: RecordVersion::V1,
        key: key("p"),
        occurrence_key: key(occurrence),
        entities: entities.iter().map(|n| e(n)).collect(),
        relation,
        preferred: preferred.map(e),
        attributed_author: iri("urn:matcher"),
        method: Some(evidence::MethodRef {
            method: iri("urn:method:names"),
            version: key("1"),
        }),
    }
}

fn propose_in(ds: &Dataset, graph: &str, input: IdentityInput) -> Target {
    let mut o = ProposeOptions::new(iri("urn:principal:matcher"));
    o.identity_graph = Some(iri(graph));
    let p: Proposed = evidence::propose(
        None,
        ds,
        ds,
        &EvidenceInput {
            identities: vec![input],
            ..Default::default()
        },
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let r = p.records.iter().find(|r| r.key == "p").unwrap();
    assert_eq!(r.kind, evidence::RecordKind::IdentityProposal);
    assert_eq!(r.graph, iri(graph));
    (r.iri.clone(), r.digest)
}

fn matched(ds: &Dataset, occurrence: &str, entities: &[&str], preferred: Option<&str>) -> Target {
    propose_in(
        ds,
        IDENTITY,
        proposal(
            occurrence,
            IdentityRelation::PossibleMatch,
            entities,
            preferred,
        ),
    )
}

fn distinct(ds: &Dataset, occurrence: &str, entities: &[&str]) -> Target {
    propose_in(
        ds,
        IDENTITY,
        proposal(
            occurrence,
            IdentityRelation::DistinctEntities,
            entities,
            None,
        ),
    )
}

fn decision(k: &str, t: &Target, d: Disposition, supersedes: Option<&NamedNode>) -> DecisionInput {
    DecisionInput {
        key: k.into(),
        target: t.0.clone(),
        target_digest: t.1,
        disposition: d,
        supersedes: supersedes.cloned(),
    }
}

fn reviewer() -> ReviewOptions {
    ReviewOptions::new(iri("urn:principal:reviewer"))
}

fn review_as(
    ds: &Dataset,
    decisions: &[DecisionInput],
    o: &ReviewOptions,
    access: Option<Arc<GraphAccess>>,
    version: u32,
) -> sparkles::Result<Reviewed> {
    let mut o = o.clone();
    if o.if_head.is_none() {
        o.if_head = Some(ds.store().head_commit().seq);
    }
    evidence::review(
        None,
        ds,
        ds,
        "people",
        version,
        decisions,
        &o,
        &mut |_, _| access.clone(),
        &Control::none(),
    )
}

fn review(ds: &Dataset, decisions: &[DecisionInput]) -> sparkles::Result<Reviewed> {
    review_as(ds, decisions, &reviewer(), all(), 1)
}

fn accept(ds: &Dataset, k: &str, t: &Target) -> NamedNode {
    review(ds, &[decision(k, t, Disposition::Accepted, None)])
        .unwrap()
        .decisions[0]
        .iri
        .clone()
}

/// The output graph as sorted `member representative` pairs of local names.
fn map(ds: &Dataset) -> Vec<String> {
    let mut out: Vec<String> = ds
        .select(&format!(
            "SELECT ?s ?o WHERE {{ GRAPH <{MAP}> {{ ?s <{EV}resolvesTo> ?o }} }}"
        ))
        .unwrap()
        .iter()
        .map(|r| {
            let local = |v: &str| v.trim_matches(['<', '>']).replace("urn:person:", "");
            format!(
                "{} {}",
                local(&r.get("s").unwrap().to_string()),
                local(&r.get("o").unwrap().to_string())
            )
        })
        .collect();
    out.sort();
    // the graph holds nothing but the map
    let all = ds
        .select(&format!(
            "SELECT * WHERE {{ GRAPH <{MAP}> {{ ?s ?p ?o }} }}"
        ))
        .unwrap()
        .len();
    assert_eq!(
        all,
        out.len(),
        "the output graph holds only resolvesTo triples"
    );
    out
}

fn pairs(v: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = v.iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

fn count(ds: &Dataset, class: &str, graph: &str) -> usize {
    ds.select(&format!(
        "SELECT ?x WHERE {{ GRAPH <{graph}> {{ ?x a <{EV}{class}> }} }}"
    ))
    .unwrap()
    .len()
}

/// A9: a merge joins clusters, a split reverses one match and restores the earlier
/// map, and every proposal, decision and revision remains.
#[test]
fn merges_and_splits_follow_the_accepted_decisions() {
    let ds = configured();
    let ab = matched(&ds, "ab", &["a", "b"], None);
    let bc = matched(&ds, "bc", &["b", "c"], None);
    // a proposal alone changes nothing
    assert!(map(&ds).is_empty());
    assert!(
        !ds.ask("ASK { GRAPH ?g { ?x <http://www.w3.org/2002/07/owl#sameAs> ?y } }")
            .unwrap()
    );
    let first = review(&ds, &[decision("ab", &ab, Disposition::Accepted, None)]).unwrap();
    assert_eq!(map(&ds), pairs(&["a a", "b a"]));
    let change = first.changed.map.clone().unwrap();
    assert_eq!(change.number, 1);
    assert!(change.previous.is_none());
    assert_eq!(change.entities, vec![e("a"), e("b")]);
    assert_eq!(first.map_revision, Some(change.revision.clone()));
    let dab = first.decisions[0].iri.clone();
    // a transitive merge
    let second = review(&ds, &[decision("bc", &bc, Disposition::Accepted, None)]).unwrap();
    assert_eq!(map(&ds), pairs(&["a a", "b a", "c a"]));
    let change2 = second.changed.map.clone().unwrap();
    assert_eq!(change2.number, 2);
    assert_eq!(change2.previous, Some(change.revision.clone()));
    assert_eq!(change2.entities, vec![e("c")]);
    let before_split = map(&ds);
    // a split: rejecting a~b leaves b~c
    let split = review(
        &ds,
        &[decision("ab2", &ab, Disposition::Rejected, Some(&dab))],
    )
    .unwrap();
    assert_eq!(map(&ds), pairs(&["b b", "c b"]));
    let change3 = split.changed.map.clone().unwrap();
    assert_eq!(change3.number, 3);
    assert_eq!(change3.previous, Some(change2.revision.clone()));
    assert_eq!(change3.decisions_ended, vec![dab.clone()]);
    assert!(change3.decisions_added.is_empty());
    assert_eq!(change3.entities, vec![e("a"), e("b"), e("c")]);
    // accepting it again restores the earlier map
    review(
        &ds,
        &[decision(
            "ab3",
            &ab,
            Disposition::Accepted,
            Some(&split.decisions[0].iri),
        )],
    )
    .unwrap();
    assert_eq!(map(&ds), before_split);
    // nothing was edited in place
    assert_eq!(count(&ds, "IdentityProposal", IDENTITY), 2);
    assert_eq!(count(&ds, "ReviewDecision", DECISIONS), 4);
    assert_eq!(count(&ds, "ResolutionMap", MAPS), 4);
    // the revision chain names what each revision added and ended
    let chain = ds
        .select(&format!(
            "SELECT ?n ?prev WHERE {{ GRAPH <{MAPS}> {{ ?m a <{EV}ResolutionMap> ; <{EV}mapRevision> ?n . OPTIONAL {{ ?m <http://www.w3.org/ns/prov#wasRevisionOf> ?prev }} }} }} ORDER BY ?n"
        ))
        .unwrap();
    assert_eq!(chain.len(), 4);
    assert!(chain.iter().skip(1).all(|r| r.get("prev").is_some()));
}

/// The representative is the smallest preferred entity of the cluster's matches, or
/// its smallest member, whatever order the decisions came in.
#[test]
fn representatives_are_deterministic() {
    let ds = configured();
    let cd = matched(&ds, "cd", &["d", "c"], Some("d"));
    let bd = matched(&ds, "bd", &["b", "d"], None);
    accept(&ds, "cd", &cd);
    assert_eq!(map(&ds), pairs(&["c d", "d d"]));
    accept(&ds, "bd", &bd);
    assert_eq!(map(&ds), pairs(&["b d", "c d", "d d"]));
    // a smaller preferred entity changes the representative
    let ab = matched(&ds, "ab", &["a", "b"], Some("b"));
    let r = review(&ds, &[decision("ab", &ab, Disposition::Accepted, None)]).unwrap();
    assert_eq!(map(&ds), pairs(&["a b", "b b", "c b", "d b"]));
    assert_eq!(
        r.changed.map.unwrap().entities,
        vec![e("a"), e("b"), e("c"), e("d")]
    );
    // one batch in the other order gives the same map
    let other = configured();
    let cd = matched(&other, "cd", &["d", "c"], Some("d"));
    let bd = matched(&other, "bd", &["b", "d"], None);
    let ab = matched(&other, "ab", &["a", "b"], Some("b"));
    review(
        &other,
        &[
            decision("ab", &ab, Disposition::Accepted, None),
            decision("bd", &bd, Disposition::Accepted, None),
            decision("cd", &cd, Disposition::Accepted, None),
        ],
    )
    .unwrap();
    assert_eq!(map(&other), map(&ds));
}

/// Decisions that change no accepted decision write no revision.
#[test]
fn rejections_and_requests_for_evidence_write_no_revision() {
    let ds = configured();
    let ab = matched(&ds, "ab", &["a", "b"], None);
    let r = review(
        &ds,
        &[decision("ab", &ab, Disposition::NeedsEvidence, None)],
    )
    .unwrap();
    assert!(r.committed && r.changed.map.is_none() && r.map_revision.is_none());
    let r2 = review(
        &ds,
        &[decision(
            "ab2",
            &ab,
            Disposition::Rejected,
            Some(&r.decisions[0].iri),
        )],
    )
    .unwrap();
    assert!(r2.changed.map.is_none());
    assert_eq!(count(&ds, "ResolutionMap", MAPS), 0);
    assert!(map(&ds).is_empty());
    // an accepted separation changes the decisions the map follows, but not the map
    let xy = distinct(&ds, "xy", &["x", "y"]);
    let r3 = review(&ds, &[decision("xy", &xy, Disposition::Accepted, None)]).unwrap();
    let change = r3.changed.map.unwrap();
    assert_eq!(change.number, 1);
    assert!(change.entities.is_empty());
    assert!(map(&ds).is_empty());
}

/// An accepted separation keeps its entities in different clusters, in either order,
/// and a split with a separation in one request is decided on its final decisions.
#[test]
fn separations_refuse_joining_and_allow_splits() {
    let ds = configured();
    let ab = matched(&ds, "ab", &["a", "b"], None);
    let bc = matched(&ds, "bc", &["b", "c"], None);
    let ac = distinct(&ds, "ac", &["a", "c"]);
    let dab = accept(&ds, "ab", &ab);
    accept(&ds, "ac", &ac);
    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(review(
            &ds,
            &[decision("bc", &bc, Disposition::Accepted, None)]
        )),
        "identity-conflict"
    );
    assert_eq!(
        ds.store().head_commit().seq,
        head,
        "a refusal writes nothing"
    );
    // the other order: the separation meets an existing cluster
    let other = configured();
    let ab2 = matched(&other, "ab", &["a", "b"], None);
    let bc2 = matched(&other, "bc", &["b", "c"], None);
    let ac2 = distinct(&other, "ac", &["a", "c"]);
    accept(&other, "ab", &ab2);
    accept(&other, "bc", &bc2);
    assert_eq!(
        code(review(
            &other,
            &[decision("ac", &ac2, Disposition::Accepted, None)]
        )),
        "identity-conflict"
    );
    // joining and separating in one request conflicts as well
    let third = configured();
    let ab3 = matched(&third, "ab", &["a", "b"], None);
    let ab3d = distinct(&third, "abd", &["a", "b"]);
    assert_eq!(
        code(review(
            &third,
            &[
                decision("ab", &ab3, Disposition::Accepted, None),
                decision("abd", &ab3d, Disposition::Accepted, None),
            ]
        )),
        "identity-conflict"
    );
    // a split that reverses a~b and accepts b~c, with a~c separate, is allowed
    let r = review(
        &ds,
        &[
            decision("ab2", &ab, Disposition::Rejected, Some(&dab)),
            decision("bc", &bc, Disposition::Accepted, None),
        ],
    )
    .unwrap();
    assert_eq!(map(&ds), pairs(&["b b", "c b"]));
    assert_eq!(r.changed.map.unwrap().decisions_ended, vec![dab]);
}

/// A14: retries and replays never duplicate decisions or revisions, a stale head or
/// precondition writes nothing, and a dry run previews without writing.
#[test]
fn retries_replays_and_stale_requests() {
    let ds = configured();
    let ab = matched(&ds, "ab", &["a", "b"], None);
    let mut o = reviewer();
    o.idempotency_key = Some("k".into());
    o.if_head = Some(ds.store().head_commit().seq);
    let d = [decision("ab", &ab, Disposition::Accepted, None)];
    // a dry run first
    let mut dry = o.clone();
    dry.dry_run = true;
    let preview = review_as(&ds, &d, &dry, all(), 1).unwrap();
    assert!(!preview.committed && preview.preview.is_some());
    assert!(map(&ds).is_empty());
    assert_eq!(count(&ds, "ResolutionMap", MAPS), 0);
    let first = review_as(&ds, &d, &o, all(), 1).unwrap();
    assert!(first.committed);
    let again = review_as(&ds, &d, &o, all(), 1).unwrap();
    assert!(again.replayed && !again.committed);
    assert_eq!(again.commit, first.commit);
    assert_eq!(again.decisions[0].iri, first.decisions[0].iri);
    assert_eq!(again.map_revision, first.map_revision);
    assert_eq!(count(&ds, "ResolutionMap", MAPS), 1);
    assert_eq!(count(&ds, "ReviewDecision", DECISIONS), 1);
    assert_eq!(
        code(review_as(
            &ds,
            &[decision("ab", &ab, Disposition::Rejected, None)],
            &o,
            all(),
            1
        )),
        "idempotency-conflict"
    );
    // without a key the same decision is stale
    assert_eq!(code(review(&ds, &d)), "decision-superseded");
    // a stale head, digest and policy version
    let bc = matched(&ds, "bc", &["b", "c"], None);
    let mut stale = reviewer();
    stale.if_head = Some(first.commit);
    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(review_as(
            &ds,
            &[decision("bc", &bc, Disposition::Accepted, None)],
            &stale,
            all(),
            1
        )),
        "head-changed"
    );
    assert_eq!(
        code(review(
            &ds,
            &[decision(
                "bc",
                &(bc.0.clone(), ab.1),
                Disposition::Accepted,
                None
            )]
        )),
        "target-changed"
    );
    assert_eq!(
        code(review_as(
            &ds,
            &[decision("bc", &bc, Disposition::Accepted, None)],
            &reviewer(),
            all(),
            2
        )),
        "policy-changed"
    );
    assert_eq!(ds.store().head_commit().seq, head);
    assert_eq!(map(&ds), pairs(&["a a", "b a"]));
}

struct Refuse;

impl evidence::AuthorityFence for Refuse {
    fn enter(&self) -> sparkles::Result<Box<dyn std::any::Any>> {
        Err(sparkles::managed::ManagedError::error(
            sparkles::managed::ManagedErrorKind::AuthorityChanged,
            "revoked",
        ))
    }
}

/// A14: an operation refused at its publication publishes nothing, and a cancelled
/// one never starts.
#[test]
fn an_interrupted_review_publishes_nothing() {
    let ds = configured();
    let ab = matched(&ds, "ab", &["a", "b"], None);
    let head = ds.store().head_commit().seq;
    let mut o = reviewer();
    o.authority = Some(Arc::new(Refuse));
    assert_eq!(
        code(review_as(
            &ds,
            &[decision("ab", &ab, Disposition::Accepted, None)],
            &o,
            all(),
            1
        )),
        "authority-changed"
    );
    let control = Control::none();
    control.cancel.cancel();
    let mut o = reviewer();
    o.if_head = Some(head);
    assert!(
        evidence::review(
            None,
            &ds,
            &ds,
            "people",
            1,
            &[decision("ab", &ab, Disposition::Accepted, None)],
            &o,
            &mut |_, _| all(),
            &control,
        )
        .is_err()
    );
    assert_eq!(ds.store().head_commit().seq, head);
    assert!(map(&ds).is_empty());
    assert_eq!(count(&ds, "ReviewDecision", DECISIONS), 0);
    // the same request then succeeds once
    accept(&ds, "ab", &ab);
    assert_eq!(map(&ds), pairs(&["a a", "b a"]));
}

/// A caller whose view hides the preferred entities of the restricted identity graph.
fn masking_preferred_entities() -> Option<Arc<GraphAccess>> {
    use sparkles::access::triples::{Caller, Limits, Protection, Rule, TripleRules};
    Some(Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: "preferred".into(),
                    predicates: Some(vec![format!("{EV}preferredEntity")]),
                    classes: None,
                    subclasses: false,
                    graphs: Some(Graphs::Only(GraphRule::new([HIDDEN], &[]))),
                    pattern: None,
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    )))
}

/// A11: a reviewer who cannot read a proposal that its clusters depend on is refused
/// exactly as for an unreadable target, and one who cannot read an identity graph of
/// the boundary is refused before anything is read.
#[test]
fn hidden_proposals_answer_as_an_unavailable_target() {
    let ds = configured();
    let ab = propose_in(
        &ds,
        HIDDEN,
        proposal(
            "ab",
            IdentityRelation::PossibleMatch,
            &["a", "b"],
            Some("a"),
        ),
    );
    accept(&ds, "ab", &ab);
    let bc = matched(&ds, "bc", &["b", "c"], None);
    let head = ds.store().head_commit().seq;
    let answer = |target: Target, access: Option<Arc<GraphAccess>>| {
        let e = review_as(
            &ds,
            &[decision("x", &target, Disposition::Accepted, None)],
            &reviewer(),
            access,
            1,
        )
        .unwrap_err();
        (e.code().to_string(), e.to_string())
    };
    let missing_target = (iri("urn:uuid:00000000-0000-4000-8000-000000000000"), bc.1);
    // an identity graph of the boundary the caller cannot read refuses every review
    let without_graph = Some(Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(
            [SOURCES, CLAIMS, IDENTITY, MAPS, DECISIONS, MAP],
            &[],
        )),
        Graphs::All,
    )));
    let hidden = answer(bc.clone(), without_graph.clone());
    let missing = answer(missing_target.clone(), without_graph);
    assert_eq!(hidden.0, "not-permitted", "{hidden:?}");
    assert_eq!(hidden, missing);
    // an effective proposal whose triples the caller's view masks
    let hidden = answer(bc.clone(), masking_preferred_entities());
    let missing = answer(missing_target, masking_preferred_entities());
    assert_eq!(hidden.0, "evidence-unavailable");
    assert_eq!(hidden, missing);
    assert_eq!(ds.store().head_commit().seq, head);
    // a reviewer who reads every identity graph accepts
    accept(&ds, "bc", &bc);
    assert_eq!(map(&ds), pairs(&["a a", "b a", "c a"]));
}

/// A masked proposal that no decision accepts takes no part in a cluster, so it
/// neither refuses the review nor counts toward its bounds.
#[test]
fn a_masked_proposal_without_effect_is_left_out() {
    let ds = configured();
    let bd = propose_in(
        &ds,
        HIDDEN,
        proposal(
            "bd",
            IdentityRelation::PossibleMatch,
            &["b", "d"],
            Some("d"),
        ),
    );
    let bc = matched(&ds, "bc", &["b", "c"], None);
    review_as(
        &ds,
        &[decision("bc", &bc, Disposition::Accepted, None)],
        &reviewer(),
        masking_preferred_entities(),
        1,
    )
    .unwrap();
    assert_eq!(map(&ds), pairs(&["b b", "c b"]));
    assert_ne!(bd.0, bc.0);
}

/// Grants: a reviewer needs update and read grants on the decisions, maps and output
/// graphs.
#[test]
fn a_reviewer_without_grants_is_refused() {
    let ds = configured();
    let ab = matched(&ds, "ab", &["a", "b"], None);
    let mut o = reviewer();
    o.write = Some(Arc::new(GraphAccess::graphs(
        Graphs::All,
        Graphs::Only(GraphRule::new([DECISIONS, MAP], &[])),
    )));
    let e = review_as(
        &ds,
        &[decision("ab", &ab, Disposition::Accepted, None)],
        &o,
        all(),
        1,
    )
    .unwrap_err();
    assert!(matches!(e, sparkles::Error::NotPermitted(_)), "{e:?}");
    let unreadable = Some(Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new([IDENTITY, DECISIONS, MAP], &[])),
        Graphs::All,
    )));
    let e = review_as(
        &ds,
        &[decision("ab", &ab, Disposition::Accepted, None)],
        &reviewer(),
        unreadable,
        1,
    )
    .unwrap_err();
    assert!(matches!(e, sparkles::Error::NotPermitted(_)), "{e:?}");
    assert!(map(&ds).is_empty());
}

/// The managed-write integrity acceptance for identity records and maps.
#[test]
fn direct_edits_of_identity_graphs_are_refused() {
    let ds = configured();
    let ab = matched(&ds, "ab", &["a", "b"], None);
    accept(&ds, "ab", &ab);
    for g in [IDENTITY, MAPS, MAP, DECISIONS] {
        let e = ds
            .update(&format!("DELETE WHERE {{ GRAPH <{g}> {{ ?s ?p ?o }} }}"))
            .unwrap_err();
        assert_eq!(e.code(), "profile-managed-graph", "{g}");
        let e = ds
            .update(&format!(
                "INSERT DATA {{ GRAPH <{g}> {{ <urn:person:x> <{EV}resolvesTo> <urn:person:a> }} }}"
            ))
            .unwrap_err();
        assert_eq!(e.code(), "profile-managed-graph", "{g}");
    }
    assert_eq!(map(&ds), pairs(&["a a", "b a"]));
}

/// A detach and new protection of a map graph makes the map unavailable, and a
/// cleared maps graph with a populated output does as well.
#[test]
fn a_changed_protection_makes_the_map_unavailable() {
    let ds = configured();
    let ab = matched(&ds, "ab", &["a", "b"], None);
    let cd = matched(&ds, "cd", &["c", "d"], None);
    accept(&ds, "ab", &ab);
    evidence::detach(&ds, MAPS).unwrap();
    ds.update(&format!("CLEAR GRAPH <{MAPS}>")).unwrap();
    evidence::configure(&ds, &settings(1), &Control::none()).unwrap();
    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(review(
            &ds,
            &[decision("cd", &cd, Disposition::Accepted, None)]
        )),
        "projection-unavailable"
    );
    assert_eq!(
        code(evidence::resolve_entities(
            &ds,
            &ds,
            "people",
            &[e("a")],
            all(),
            &Control::none()
        )),
        "projection-unavailable"
    );
    assert_eq!(ds.store().head_commit().seq, head);
    // a changed protection of the output graph
    let other = configured();
    let ab = matched(&other, "ab", &["a", "b"], None);
    let cd = matched(&other, "cd", &["c", "d"], None);
    accept(&other, "ab", &ab);
    evidence::detach(&other, MAP).unwrap();
    evidence::configure(&other, &settings(1), &Control::none()).unwrap();
    assert_eq!(
        code(review(
            &other,
            &[decision("cd", &cd, Disposition::Accepted, None)]
        )),
        "projection-unavailable"
    );
}

/// Policy versions: decisions under an earlier version stay effective.
#[test]
fn decisions_of_an_earlier_policy_version_stay_effective() {
    let ds = configured();
    let ab = matched(&ds, "ab", &["a", "b"], None);
    let bc = matched(&ds, "bc", &["b", "c"], None);
    accept(&ds, "ab", &ab);
    evidence::configure(&ds, &settings(2), &Control::none()).unwrap();
    review_as(
        &ds,
        &[decision("bc", &bc, Disposition::Accepted, None)],
        &reviewer(),
        all(),
        2,
    )
    .unwrap();
    assert_eq!(map(&ds), pairs(&["a a", "b a", "c a"]));
}

/// The resolution read answers the selected entity, the cluster and the decisions
/// that join it, and refuses a caller that cannot read the map.
#[test]
fn resolution_reads_the_current_map() {
    let ds = configured();
    let ab = matched(&ds, "ab", &["a", "b"], None);
    let bc = matched(&ds, "bc", &["b", "c"], None);
    let dab = accept(&ds, "ab", &ab);
    let dbc = accept(&ds, "bc", &bc);
    let r = evidence::resolve_entities(
        &ds,
        &ds,
        "people",
        &[e("c"), e("z")],
        all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(r.number, 2);
    assert!(r.revision.is_some());
    assert_eq!(r.entities[0].representative, e("a"));
    assert_eq!(r.entities[0].members, vec![e("a"), e("b"), e("c")]);
    assert!(r.entities[0].members_complete);
    let mut expected = vec![dab, dbc];
    expected.sort();
    assert_eq!(r.entities[0].decisions, expected);
    // an entity the map does not name resolves to itself
    assert_eq!(r.entities[1].representative, e("z"));
    assert_eq!(r.entities[1].members, vec![e("z")]);
    assert!(r.entities[1].decisions.is_empty());
    let unreadable = Some(Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new([IDENTITY, DECISIONS, MAPS], &[])),
        Graphs::All,
    )));
    assert!(matches!(
        evidence::resolve_entities(&ds, &ds, "people", &[e("a")], unreadable, &Control::none()),
        Err(sparkles::Error::NotPermitted(_))
    ));
    assert!(
        evidence::resolve_entities(&ds, &ds, "nobody", &[e("a")], all(), &Control::none()).is_err()
    );
    assert!(evidence::resolve_entities(&ds, &ds, "people", &[], all(), &Control::none()).is_err());
}

/// Proposals refuse malformed entity sets, and a review policy and a matching policy
/// each review only their own kind of record.
#[test]
fn proposals_and_policies_keep_their_kinds() {
    let ds = configured();
    let mut o = ProposeOptions::new(iri("urn:principal:matcher"));
    o.identity_graph = Some(iri(IDENTITY));
    for bad in [
        proposal("one", IdentityRelation::PossibleMatch, &["a"], None),
        proposal("twice", IdentityRelation::PossibleMatch, &["a", "a"], None),
        proposal(
            "outside",
            IdentityRelation::PossibleMatch,
            &["a", "b"],
            Some("c"),
        ),
        proposal(
            "distinct",
            IdentityRelation::DistinctEntities,
            &["a", "b"],
            Some("a"),
        ),
    ] {
        let e = evidence::propose(
            None,
            &ds,
            &ds,
            &EvidenceInput {
                identities: vec![bad],
                ..Default::default()
            },
            &o,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap_err();
        assert_eq!(e.code(), "invalid", "{e}");
    }
    // the order of the entities does not change the proposal's identity or digest
    let x = matched(&ds, "same", &["a", "b"], None);
    let y = matched(&ds, "same", &["b", "a"], None);
    assert_eq!(x, y);
    // a proposal is not a claim: the resolver answers a record of another kind as a
    // conflict with the kinds the review expects
    let mut claims = settings(1);
    claims.supports = vec!["urn:ev:supports".into()];
    claims.projections.push("urn:ev:accepted".into());
    claims.policies.insert(
        "claims".into(),
        evidence::Policy {
            version: 1,
            admitted_kinds: vec![evidence::ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: evidence::ConflictMode::Refuse,
            boundary: vec![CLAIMS.into(), SOURCES.into()],
            decisions: DECISIONS.into(),
            supports: "urn:ev:supports".into(),
            output: "urn:ev:accepted".into(),
        },
    );
    evidence::configure(&ds, &claims, &Control::none()).unwrap();
    let mut o = reviewer();
    o.if_head = Some(ds.store().head_commit().seq);
    assert_eq!(
        code(evidence::review(
            None,
            &ds,
            &ds,
            "claims",
            1,
            &[decision("x", &x, Disposition::Accepted, None)],
            &o,
            &mut |_, _| all(),
            &Control::none(),
        )),
        "immutable-record-conflict"
    );
}

/// The proposals, decisions and map revisions the operations write conform to the
/// profile's shapes.
#[cfg(feature = "shacl")]
#[test]
fn written_records_conform_to_the_profile_shapes() {
    use oxrdfio::RdfFormat;
    let ds = configured();
    let ab = matched(&ds, "ab", &["a", "b"], Some("b"));
    let xy = distinct(&ds, "xy", &["x", "y"]);
    let d = accept(&ds, "ab", &ab);
    accept(&ds, "xy", &xy);
    review(
        &ds,
        &[decision("ab2", &ab, Disposition::Rejected, Some(&d))],
    )
    .unwrap();
    let copy = Dataset::memory();
    let mut nt = String::new();
    for g in [IDENTITY, DECISIONS, MAPS] {
        for t in ds
            .construct(&format!(
                "CONSTRUCT {{ ?s ?p ?o }} WHERE {{ GRAPH <{g}> {{ ?s ?p ?o }} }}"
            ))
            .unwrap()
            .iter()
        {
            nt.push_str(&format!("{t} .\n"));
        }
    }
    copy.load_str(&nt, RdfFormat::NTriples).unwrap();
    let shapes = sparkles_shacl::Shapes::parse(
        evidence::profile(1).unwrap().shapes,
        RdfFormat::Turtle,
        None,
    )
    .unwrap();
    let report = copy
        .validation()
        .shacl(&shapes, &Default::default())
        .unwrap();
    assert!(report.conforms, "{report:?}");
    for class in ["IdentityProposal", "ReviewDecision", "ResolutionMap"] {
        assert!(
            copy.ask(&format!("ASK {{ ?x a <{EV}{class}> }}")).unwrap(),
            "{class}"
        );
    }
}

/// A review on a branch changes the branch's map only.
#[test]
fn a_review_on_a_branch_changes_its_map_only() {
    let ds = configured();
    let ab = matched(&ds, "ab", &["a", "b"], None);
    ds.store()
        .create_branch("draft", &Default::default())
        .unwrap();
    let branch = ds.branch("draft").unwrap();
    let mut o = reviewer();
    o.if_head = Some(branch.store().head_commit().seq);
    evidence::review(
        None,
        &ds,
        &branch,
        "people",
        1,
        &[decision("ab", &ab, Disposition::Accepted, None)],
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(map(&branch), pairs(&["a a", "b a"]));
    assert!(map(&ds).is_empty());
    let r = evidence::resolve_entities(&ds, &branch, "people", &[e("b")], all(), &Control::none())
        .unwrap();
    assert_eq!(r.entities[0].representative, e("a"));
}

/// A map is built from the identity graphs of its boundary. Removing one of them, or
/// detaching and protecting it again, leaves the map following matches that are no
/// longer read, so reviews and reads answer `projection-unavailable` and change
/// nothing until the boundary is restored. A graph added to the boundary is accepted.
#[test]
fn a_changed_boundary_makes_the_map_unavailable() {
    let ds = configured();
    let ab = propose_in(
        &ds,
        HIDDEN,
        proposal(
            "ab",
            IdentityRelation::PossibleMatch,
            &["a", "b"],
            Some("b"),
        ),
    );
    accept(&ds, "ab", &ab);
    let bc = matched(&ds, "bc", &["b", "c"], Some("c"));
    assert_eq!(map(&ds), pairs(&["a b", "b b"]));
    let refused = |ds: &Dataset| {
        let head = ds.store().head_commit().seq;
        assert_eq!(
            code(review(
                ds,
                &[decision("bc", &bc, Disposition::Accepted, None)]
            )),
            "projection-unavailable"
        );
        assert_eq!(
            code(evidence::resolve_entities(
                ds,
                ds,
                "people",
                &[e("a")],
                all(),
                &Control::none()
            )),
            "projection-unavailable"
        );
        assert_eq!(ds.store().head_commit().seq, head);
        assert_eq!(map(ds), pairs(&["a b", "b b"]));
    };
    // removed from the boundary
    evidence::configure(
        &ds,
        &settings_with(1, &[IDENTITY, SOURCES, CLAIMS]),
        &Control::none(),
    )
    .unwrap();
    refused(&ds);
    // restored
    evidence::configure(&ds, &settings(1), &Control::none()).unwrap();
    // the smallest preferred entity of the restored cluster
    accept(&ds, "bc", &bc);
    assert_eq!(map(&ds), pairs(&["a b", "b b", "c b"]));

    // detached and protected again, so its proposals no longer authenticate
    let other = configured();
    let ab = propose_in(
        &other,
        HIDDEN,
        proposal(
            "ab",
            IdentityRelation::PossibleMatch,
            &["a", "b"],
            Some("b"),
        ),
    );
    accept(&other, "ab", &ab);
    let bc = matched(&other, "bc", &["b", "c"], Some("c"));
    evidence::detach(&other, HIDDEN).unwrap();
    evidence::configure(&other, &settings(1), &Control::none()).unwrap();
    let head = other.store().head_commit().seq;
    assert_eq!(
        code(review(
            &other,
            &[decision("bc", &bc, Disposition::Accepted, None)]
        )),
        "projection-unavailable"
    );
    assert_eq!(other.store().head_commit().seq, head);
    assert_eq!(map(&other), pairs(&["a b", "b b"]));

    // a graph added to the boundary keeps the map available
    let added = Dataset::memory();
    evidence::configure(
        &added,
        &settings_with(1, &[IDENTITY, SOURCES, CLAIMS]),
        &Control::none(),
    )
    .unwrap();
    let ab = matched(&added, "ab", &["a", "b"], None);
    accept(&added, "ab", &ab);
    evidence::configure(&added, &settings(1), &Control::none()).unwrap();
    let cd = propose_in(
        &added,
        HIDDEN,
        proposal("cd", IdentityRelation::PossibleMatch, &["b", "d"], None),
    );
    accept(&added, "cd", &cd);
    assert_eq!(map(&added), pairs(&["a a", "b a", "d a"]));
}

/// A resolution read of every member of one long cluster reads the cluster once.
#[test]
fn a_resolution_of_every_member_reads_its_cluster_once() {
    let ds = configured();
    let names: Vec<String> = (0..100).map(|i| format!("m{i:03}")).collect();
    let mut decisions = Vec::new();
    for (i, w) in names.windows(2).enumerate() {
        let t = matched(&ds, &format!("chain{i}"), &[&w[0], &w[1]], None);
        decisions.push(decision(&format!("d{i}"), &t, Disposition::Accepted, None));
    }
    review(&ds, &decisions).unwrap();
    let entities: Vec<NamedNode> = names.iter().map(|n| e(n)).collect();
    let r =
        evidence::resolve_entities(&ds, &ds, "people", &entities, all(), &Control::none()).unwrap();
    assert_eq!(r.entities.len(), 100);
    for x in &r.entities {
        assert_eq!(x.representative, e("m000"));
        assert_eq!(x.members.len(), 100);
        assert_eq!(x.decisions.len(), 99);
    }
}

/// A caller whose view hides `predicate` in `graph`.
fn masking(predicate: &str, graph: &str) -> Option<Arc<GraphAccess>> {
    use sparkles::access::triples::{Caller, Limits, Protection, Rule, TripleRules};
    Some(Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: "masked".into(),
                    predicates: Some(vec![predicate.to_string()]),
                    classes: None,
                    subclasses: false,
                    graphs: Some(Graphs::Only(GraphRule::new([graph], &[]))),
                    pattern: None,
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    )))
}

/// More proposals naming an entity than one review reads, all hidden from the
/// caller, answer exactly as an unavailable target and report no count of their own,
/// while the same store read in full answers `incomplete-evidence`.
#[test]
fn hidden_proposals_beyond_the_bound_report_no_count() {
    let ds = configured();
    let mut o = ProposeOptions::new(iri("urn:principal:matcher"));
    o.identity_graph = Some(iri(HIDDEN));
    // one more than the bound, so that the caller's masked view stays within the
    // resolution budget
    for batch in 0..11 {
        let size = if batch == 10 { 1 } else { 1000 };
        let identities = (0..size)
            .map(|i| {
                let n = batch * 1000 + i;
                let mut p = proposal(
                    &format!("hidden{n}"),
                    IdentityRelation::PossibleMatch,
                    &["b", &format!("h{n}")],
                    Some("b"),
                );
                p.key = key(&format!("p{n}"));
                p
            })
            .collect();
        evidence::propose(
            None,
            &ds,
            &ds,
            &EvidenceInput {
                identities,
                ..Default::default()
            },
            &o,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
    }
    let bc = matched(&ds, "bc", &["b", "c"], None);
    // the caller cannot see which entities the hidden proposals name
    let masked = masking(&format!("{EV}entity"), HIDDEN);
    let answer = |target: Target, access: Option<Arc<GraphAccess>>| {
        let e = review_as(
            &ds,
            &[decision("x", &target, Disposition::Accepted, None)],
            &reviewer(),
            access,
            1,
        )
        .unwrap_err();
        (e.code().to_string(), e.to_string())
    };
    let hidden = answer(bc.clone(), masked.clone());
    let missing = answer(
        (iri("urn:uuid:00000000-0000-4000-8000-000000000000"), bc.1),
        masked,
    );
    // With this many hidden triples the resolution budget of the caller's view refuses
    // first, for any target alike, and a smaller hidden set reaches the clusters, which
    // answer as the target being unavailable. Either way the two answers are the same.
    assert_eq!(hidden, missing);
    assert_eq!(answer(bc, all()).0, "incomplete-evidence");
    assert!(map(&ds).is_empty());
}

/// A mask over a revision's boundary protections cannot hide that the map was built
/// from a graph that has left the boundary.
#[test]
fn a_masked_revision_is_refused_by_the_resolution_read() {
    let ds = configured();
    let ab = propose_in(
        &ds,
        HIDDEN,
        proposal("ab", IdentityRelation::PossibleMatch, &["a", "b"], None),
    );
    accept(&ds, "ab", &ab);
    let masked = masking(&format!("{EV}boundaryProtection"), MAPS);
    let read = |access: Option<Arc<GraphAccess>>| {
        evidence::resolve_entities(&ds, &ds, "people", &[e("a")], access, &Control::none())
    };
    // a masked latest revision is refused even before the boundary changes
    assert_eq!(code(read(masked.clone())), "projection-unavailable");
    assert_eq!(read(all()).unwrap().entities[0].representative, e("a"));
    evidence::configure(
        &ds,
        &settings_with(1, &[IDENTITY, SOURCES, CLAIMS]),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(code(read(masked)), "projection-unavailable");
    assert_eq!(code(read(all())), "projection-unavailable");
}

/// When the caller's view splits a cluster, each requested entity answers the
/// decisions of the component it reaches, whatever else the request names and in
/// whatever order.
#[test]
fn a_resolution_answers_each_entity_as_alone() {
    let ds = configured();
    let ab = propose_in(
        &ds,
        HIDDEN,
        proposal("ab", IdentityRelation::PossibleMatch, &["a", "b"], None),
    );
    let bc = matched(&ds, "bc", &["b", "c"], None);
    accept(&ds, "ab", &ab);
    let dbc = accept(&ds, "bc", &bc);
    let masked = masking(&format!("{EV}entity"), HIDDEN);
    let decisions = |entities: &[&str]| -> Vec<Vec<NamedNode>> {
        let entities: Vec<NamedNode> = entities.iter().map(|n| e(n)).collect();
        evidence::resolve_entities(
            &ds,
            &ds,
            "people",
            &entities,
            masked.clone(),
            &Control::none(),
        )
        .unwrap()
        .entities
        .into_iter()
        .map(|x| x.decisions)
        .collect()
    };
    let b = decisions(&["b"]).remove(0);
    assert_eq!(b, vec![dbc]);
    assert_eq!(decisions(&["a", "b"])[1], b);
    assert_eq!(decisions(&["b", "a"])[0], b);
    assert_eq!(decisions(&["a"])[0], decisions(&["b", "a"])[1]);
}

/// A proposal or decision the caller's view hides any triple of takes no part in what
/// the resolution read answers, even when the hidden triple is not one the cluster
/// needs: the read neither counts the proposal toward a cluster nor reveals the
/// decision that accepts it.
#[test]
fn a_resolution_leaves_out_masked_proposals_and_decisions() {
    let ds = configured();
    let ab = propose_in(
        &ds,
        HIDDEN,
        proposal("ab", IdentityRelation::PossibleMatch, &["a", "b"], None),
    );
    let bc = matched(&ds, "bc", &["b", "c"], None);
    let dab = accept(&ds, "ab", &ab);
    let dbc = accept(&ds, "bc", &bc);
    let read = |entities: &[&str], access: Option<Arc<GraphAccess>>| {
        let entities: Vec<NamedNode> = entities.iter().map(|n| e(n)).collect();
        evidence::resolve_entities(&ds, &ds, "people", &entities, access, &Control::none())
            .unwrap()
            .entities
    };
    let mut both = vec![dab.clone(), dbc.clone()];
    both.sort();
    assert_eq!(read(&["b"], all())[0].decisions, both);
    // the restricted proposal's attribution is hidden, so it joins nothing
    let attribution = masking("http://www.w3.org/ns/prov#wasAttributedTo", HIDDEN);
    assert_eq!(
        read(&["b"], attribution.clone())[0].decisions,
        vec![dbc.clone()]
    );
    assert_eq!(
        read(&["a"], attribution.clone())[0].decisions,
        Vec::<NamedNode>::new()
    );
    assert_eq!(read(&["a", "b"], attribution)[1].decisions, vec![dbc]);
    // every decision's target digest is hidden, so no decision is revealed
    let decisions = masking(&format!("{EV}targetDigest"), DECISIONS);
    let r = read(&["b"], decisions);
    assert!(r[0].decisions.is_empty(), "{r:?}");
    // the map itself is read in the caller's view
    assert_eq!(r[0].representative, e("a"));
    // decision targets the view hides are not counted, so they cannot reach a bound
    let targets = masking(&format!("{EV}target"), DECISIONS);
    let r = read(&["b"], targets);
    assert!(r[0].decisions.is_empty(), "{r:?}");
}
