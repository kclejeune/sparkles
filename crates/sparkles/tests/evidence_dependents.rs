//! Assessment inputs and the dependents of changed records (spec C25 §6 and §9, F22 §5).
//!
//! An assessment names the records it consumed as `prov:used`. When a claim is
//! withdrawn, a support ends or a resolution map gets a new revision, the operation's
//! changed identities find the assessments that depend on them. Each test states the
//! dependents it expects independently of the operations.

use oxrdf::{Literal, NamedNode, Term, Triple};
use sparkles::Dataset;
use sparkles::access::{GraphAccess, GraphRule, Graphs};
use sparkles::evidence::{
    self, AssessmentInput, ClaimInput, ClaimKind, ConflictMode, DecisionInput, Dependence,
    Disposition, EvidenceInput, IdentityInput, IdentityRelation, MatchingPolicy, MethodRef,
    Polarity, Policy, ProposeOptions, Proposed, RecordKey, RecordRef, RecordVersion, ReviewOptions,
    Reviewed, Settings, Sha256Digest, SourceContent, SourceRevisionInput, WithdrawalInput,
};
use sparkles::task::Control;
use std::sync::Arc;

const SOURCES: &str = "urn:ev:sources";
const CLAIMS: &str = "urn:ev:claims";
const ANALYSES: &str = "urn:ev:analyses";
const DECISIONS: &str = "urn:ev:decisions";
const SUPPORTS: &str = "urn:ev:supports";
const OUTPUT: &str = "urn:ev:accepted";
const IDENTITY: &str = "urn:ev:identity";
const MAPS: &str = "urn:ev:maps";
const MAP: &str = "urn:ev:map";
const PROV_USED: &str = "http://www.w3.org/ns/prov#used";

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn key(s: &str) -> RecordKey {
    RecordKey::new(s).unwrap()
}

fn sha(text: &str) -> Sha256Digest {
    Sha256Digest::of_bytes(text.as_bytes(), &Control::none()).unwrap()
}

fn all() -> Option<Arc<GraphAccess>> {
    Some(Arc::new(GraphAccess::all()))
}

fn settings() -> Settings {
    Settings {
        enabled: true,
        source_records: vec![SOURCES.into()],
        proposals: vec![CLAIMS.into(), ANALYSES.into()],
        decisions: vec![DECISIONS.into()],
        supports: vec![SUPPORTS.into()],
        identity: vec![IDENTITY.into(), MAPS.into()],
        projections: vec![OUTPUT.into(), MAP.into()],
        policies: [(
            "main".to_string(),
            Policy {
                version: 1,
                admitted_kinds: vec![ClaimKind::Reported],
                required_scope: Vec::new(),
                conflicts: ConflictMode::Refuse,
                boundary: vec![SOURCES.into(), CLAIMS.into()],
                decisions: DECISIONS.into(),
                supports: SUPPORTS.into(),
                output: OUTPUT.into(),
            },
        )]
        .into_iter()
        .collect(),
        matching_policies: [(
            "people".to_string(),
            MatchingPolicy {
                version: 1,
                boundary: vec![IDENTITY.into()],
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
    evidence::configure(&ds, &settings(), &Control::none()).unwrap();
    ds
}

fn propose_as(ds: &Dataset, input: &EvidenceInput, graph: &str) -> sparkles::Result<Proposed> {
    let mut o = ProposeOptions::new(iri("urn:principal:importer"));
    // a batch of identity proposals only goes to the identity graph
    if input.identities.is_empty() {
        o.proposal_graph = Some(iri(graph));
    } else {
        o.identity_graph = Some(iri(IDENTITY));
    }
    evidence::propose(None, ds, ds, input, &o, &mut |_, _| all(), &Control::none())
}

type Target = (NamedNode, Sha256Digest);

fn record(p: &Proposed, k: &str) -> Target {
    let r = p.records.iter().find(|r| r.key == k).unwrap();
    (r.iri.clone(), r.digest)
}

/// One claim of one source, in the claims graph.
fn claim(ds: &Dataset, source: &str) -> Target {
    let p = propose_as(
        ds,
        &EvidenceInput {
            revisions: vec![SourceRevisionInput {
                version: RecordVersion::V1,
                key: key("rev"),
                source: iri(&format!("urn:source:{source}")),
                source_key: key(source),
                upstream_version: None,
                content: SourceContent::MetadataOnly {
                    digest: sha(source),
                },
                metadata: Default::default(),
                times: Default::default(),
                lineage: Default::default(),
            }],
            claims: vec![ClaimInput {
                version: RecordVersion::V1,
                key: key("claim"),
                occurrence_key: key("c"),
                revision: RecordRef::Local(key("rev")),
                proposition: Triple::new(iri("urn:release"), iri("urn:affectedBy"), iri("urn:cve")),
                kind: ClaimKind::Reported,
                polarity: Polarity::Positive,
                attributed_author: iri("urn:publisher"),
                scope: Default::default(),
                confidence: Default::default(),
            }],
            ..Default::default()
        },
        CLAIMS,
    )
    .unwrap();
    record(&p, "claim")
}

fn assessment(occurrence: &str, target: &NamedNode, inputs: Vec<RecordRef>) -> AssessmentInput {
    AssessmentInput {
        version: RecordVersion::V1,
        key: key("a"),
        occurrence_key: key(occurrence),
        target: RecordRef::Existing(target.clone()),
        conclusion: Term::Literal(Literal::new_simple_literal("holds")),
        method: MethodRef {
            method: iri("urn:method:analysis"),
            version: key("1"),
        },
        configuration_digest: sha("configuration"),
        evaluated_at: Literal::new_typed_literal(
            "2026-01-01T00:00:00Z",
            iri("http://www.w3.org/2001/XMLSchema#dateTime"),
        ),
        attributed_evaluator: iri("urn:analyst"),
        confidence: None,
        scope: Default::default(),
        inputs,
    }
}

/// An assessment of `target` that used `inputs`, in `graph`.
fn assess_in(
    ds: &Dataset,
    graph: &str,
    occurrence: &str,
    target: &NamedNode,
    inputs: &[&NamedNode],
) -> NamedNode {
    let p = propose_as(
        ds,
        &EvidenceInput {
            assessments: vec![assessment(
                occurrence,
                target,
                inputs
                    .iter()
                    .map(|i| RecordRef::Existing((*i).clone()))
                    .collect(),
            )],
            ..Default::default()
        },
        graph,
    )
    .unwrap();
    record(&p, "a").0
}

fn assess(ds: &Dataset, occurrence: &str, target: &NamedNode, inputs: &[&NamedNode]) -> NamedNode {
    assess_in(ds, ANALYSES, occurrence, target, inputs)
}

fn reviewer() -> ReviewOptions {
    ReviewOptions::new(iri("urn:principal:reviewer"))
}

fn review(ds: &Dataset, policy: &str, decisions: &[DecisionInput]) -> Reviewed {
    let mut o = reviewer();
    o.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        ds,
        ds,
        policy,
        1,
        decisions,
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap()
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

/// The dependents of `changed` as `(assessment, on, dependence, depth)`.
fn dependents(
    ds: &Dataset,
    changed: &[NamedNode],
    transitive: bool,
    access: Option<Arc<GraphAccess>>,
) -> Vec<(NamedNode, NamedNode, Dependence, u32)> {
    let d =
        evidence::find_dependents(ds, ds, changed, transitive, access, &Control::none()).unwrap();
    assert_eq!(d.commit, ds.store().head_commit().seq);
    d.dependents
        .into_iter()
        .map(|x| (x.assessment, x.on, x.dependence, x.depth))
        .collect()
}

fn sorted(
    mut v: Vec<(NamedNode, NamedNode, Dependence, u32)>,
) -> Vec<(NamedNode, NamedNode, Dependence, u32)> {
    v.sort_by(|a, b| (a.3, &a.0).cmp(&(b.3, &b.0)));
    v
}

/// A withdrawn claim finds the assessments that evaluate it and those that used it.
#[test]
fn a_withdrawn_claim_finds_its_assessments() {
    let ds = configured();
    let c = claim(&ds, "advisory");
    let unrelated = claim(&ds, "mirror");
    let evaluates = assess(&ds, "evaluates", &c.0, &[]);
    let uses = assess(&ds, "uses", &unrelated.0, &[&c.0]);
    // the inputs are written as prov:used
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{ANALYSES}> {{ <{}> <{PROV_USED}> <{}> }} }}",
            uses.as_str(),
            c.0.as_str()
        ))
        .unwrap()
    );
    let withdrawn = evidence::withdraw(
        &ds,
        &ds,
        &[WithdrawalInput {
            key: "w".into(),
            target: c.0.clone(),
            target_digest: c.1,
        }],
        &reviewer(),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(withdrawn.withdrawals[0].target, c.0);
    let expected = sorted(vec![
        (evaluates.clone(), c.0.clone(), Dependence::Target, 0),
        (uses.clone(), c.0.clone(), Dependence::Used, 0),
    ]);
    assert_eq!(
        dependents(&ds, std::slice::from_ref(&c.0), false, all()),
        expected
    );
}

/// An ended support and a superseded map revision are among an operation's changed
/// identities, and the assessments that used them are found through them.
#[test]
fn changed_identities_find_the_consumers_of_supports_and_map_revisions() {
    let ds = configured();
    let c = claim(&ds, "advisory");
    let accepted = review(
        &ds,
        "main",
        &[decision("c", &c, Disposition::Accepted, None)],
    );
    let support = accepted.decisions[0].support.clone().unwrap();
    let on_support = assess(&ds, "support", &c.0, &[&support]);
    let withdrawn = evidence::withdraw(
        &ds,
        &ds,
        &[WithdrawalInput {
            key: "w".into(),
            target: c.0.clone(),
            target_digest: c.1,
        }],
        &reviewer(),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let ended = withdrawn.changed.identities();
    assert!(ended.contains(&support), "{ended:?}");
    let found = dependents(&ds, &ended, false, all());
    assert!(
        found.contains(&(on_support, support, Dependence::Used, 0)),
        "{found:?}"
    );

    // a split names the revision it follows
    let p = propose_as(
        &ds,
        &EvidenceInput {
            identities: vec![IdentityInput {
                version: RecordVersion::V1,
                key: key("p"),
                occurrence_key: key("ab"),
                entities: vec![iri("urn:person:a"), iri("urn:person:b")],
                relation: IdentityRelation::PossibleMatch,
                preferred: None,
                attributed_author: iri("urn:matcher"),
                method: None,
            }],
            ..Default::default()
        },
        CLAIMS,
    )
    .unwrap();
    let ab = record(&p, "p");
    let merged = review(
        &ds,
        "people",
        &[decision("ab", &ab, Disposition::Accepted, None)],
    );
    let revision = merged.map_revision.clone().unwrap();
    let on_map = assess(&ds, "map", &ab.0, &[&revision]);
    let split = review(
        &ds,
        "people",
        &[decision(
            "ab",
            &ab,
            Disposition::Rejected,
            Some(&merged.decisions[0].iri),
        )],
    );
    let changed = split.changed.identities();
    assert!(changed.contains(&revision), "{changed:?}");
    assert!(changed.contains(&merged.decisions[0].iri), "{changed:?}");
    let found = dependents(&ds, &changed, false, all());
    assert_eq!(found, vec![(on_map, revision, Dependence::Used, 0)]);
}

/// Transitive dependents are found at their smallest depth, and only when asked for.
#[test]
fn transitive_dependents_are_found_at_their_smallest_depth() {
    let ds = configured();
    let c = claim(&ds, "advisory");
    let first = assess(&ds, "first", &c.0, &[]);
    let second = assess(&ds, "second", &first, &[]);
    // the third evaluates the second and also used the claim directly
    let third = assess(&ds, "third", &second, &[&c.0]);
    assert_eq!(
        dependents(&ds, std::slice::from_ref(&c.0), false, all()),
        sorted(vec![
            (first.clone(), c.0.clone(), Dependence::Target, 0),
            (third.clone(), c.0.clone(), Dependence::Used, 0),
        ])
    );
    assert_eq!(
        dependents(&ds, std::slice::from_ref(&c.0), true, all()),
        sorted(vec![
            (first.clone(), c.0.clone(), Dependence::Target, 0),
            (third.clone(), c.0.clone(), Dependence::Used, 0),
            (second, first, Dependence::Target, 1),
        ])
    );
}

/// A graph the caller cannot read is not searched, so the answer discloses nothing in it.
#[test]
fn unreadable_graphs_are_not_searched() {
    let ds = configured();
    let c = claim(&ds, "advisory");
    let visible = assess_in(&ds, CLAIMS, "visible", &c.0, &[]);
    assess(&ds, "hidden", &c.0, &[]);
    let limited = Some(Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new([SOURCES, CLAIMS], &[])),
        Graphs::All,
    )));
    assert_eq!(
        dependents(&ds, std::slice::from_ref(&c.0), true, limited),
        vec![(visible, c.0.clone(), Dependence::Target, 0)]
    );
    assert_eq!(
        dependents(&ds, std::slice::from_ref(&c.0), false, all()).len(),
        2
    );
}

/// Inputs name existing authenticated records of the allowed kinds, each once.
#[test]
fn assessment_inputs_are_checked() {
    let ds = configured();
    let c = claim(&ds, "advisory");
    let input = |inputs: Vec<RecordRef>| EvidenceInput {
        assessments: vec![assessment("x", &c.0, inputs)],
        ..Default::default()
    };
    let twice = propose_as(
        &ds,
        &input(vec![
            RecordRef::Existing(c.0.clone()),
            RecordRef::Existing(c.0.clone()),
        ]),
        ANALYSES,
    )
    .unwrap_err();
    assert_eq!(twice.code(), "invalid", "{twice:?}");
    let many = propose_as(
        &ds,
        &input(
            (0..65)
                .map(|i| RecordRef::Existing(iri(&format!("urn:record:{i}"))))
                .collect(),
        ),
        ANALYSES,
    )
    .unwrap_err();
    assert_eq!(many.code(), "invalid", "{many:?}");
    // a record that does not exist, and an IRI that is not a record
    let head = ds.store().head_commit().seq;
    for missing in [
        "urn:uuid:00000000-0000-4000-8000-000000000000",
        "urn:release",
    ] {
        let e = propose_as(
            &ds,
            &input(vec![RecordRef::Existing(iri(missing))]),
            ANALYSES,
        )
        .unwrap_err();
        assert_eq!(e.code(), "evidence-unavailable", "{missing}: {e:?}");
    }
    assert_eq!(ds.store().head_commit().seq, head);
    // a batch-local input of a kind an assessment cannot use
    let e = propose_as(
        &ds,
        &EvidenceInput {
            assessments: vec![assessment(
                "y",
                &c.0,
                vec![RecordRef::Local(key("nothing"))],
            )],
            ..Default::default()
        },
        ANALYSES,
    )
    .unwrap_err();
    assert_eq!(e.code(), "invalid", "{e:?}");
}

/// A batch-local claim may be an input of an assessment of the same batch, and inputs
/// that form a cycle with targets are refused.
#[test]
fn batch_local_inputs_resolve_in_order() {
    let ds = configured();
    let c = claim(&ds, "advisory");
    let mut first = assessment("first", &c.0, Vec::new());
    first.key = key("first");
    let mut second = assessment("second", &c.0, vec![RecordRef::Local(key("first"))]);
    second.key = key("second");
    // the second is listed before the assessment it used
    let p = propose_as(
        &ds,
        &EvidenceInput {
            assessments: vec![second.clone(), first.clone()],
            ..Default::default()
        },
        ANALYSES,
    )
    .unwrap();
    let a = record(&p, "first").0;
    let b = record(&p, "second").0;
    assert_eq!(
        dependents(&ds, std::slice::from_ref(&a), false, all()),
        vec![(b, a, Dependence::Used, 0)]
    );
    let mut x = assessment("x", &c.0, vec![RecordRef::Local(key("y"))]);
    x.key = key("x");
    let mut y = assessment("y", &c.0, Vec::new());
    y.key = key("y");
    y.target = RecordRef::Local(key("x"));
    let e = propose_as(
        &ds,
        &EvidenceInput {
            assessments: vec![x, y],
            ..Default::default()
        },
        ANALYSES,
    )
    .unwrap_err();
    assert_eq!(e.code(), "invalid", "{e:?}");
}

/// A caller whose view hides `predicate` in the analyses graph.
fn masking(predicate: &str) -> Option<Arc<GraphAccess>> {
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
                    graphs: Some(Graphs::Only(GraphRule::new([ANALYSES], &[]))),
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

/// An assessment whose triples the caller's view partly hides is neither reported nor
/// followed, whichever of its triples is hidden.
#[test]
fn masked_assessments_are_neither_reported_nor_followed() {
    let ds = configured();
    let c = claim(&ds, "advisory");
    let visible = assess_in(&ds, CLAIMS, "visible", &c.0, &[]);
    let masked = assess(&ds, "masked", &c.0, &[]);
    let after = assess_in(&ds, CLAIMS, "after", &masked, &[]);
    let full = dependents(&ds, std::slice::from_ref(&c.0), true, all());
    assert!(full.contains(&(after.clone(), masked.clone(), Dependence::Target, 1)));
    for predicate in [
        "urn:x-sparkles:evidence:conclusion".to_string(),
        "urn:x-sparkles:evidence:availabilityReceipt".to_string(),
        "urn:x-sparkles:evidence:target".to_string(),
    ] {
        assert_eq!(
            dependents(&ds, std::slice::from_ref(&c.0), true, masking(&predicate)),
            vec![(visible.clone(), c.0.clone(), Dependence::Target, 0)],
            "{predicate}"
        );
    }
}
