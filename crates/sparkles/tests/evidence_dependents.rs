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
                was_revision_of: None,
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
                validity: None,
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

const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
const REPORTS_OTHER: &str = "urn:x-sparkles:graphrag:team";
const DISCLOSURE: &str = "urn:x-sparkles:graphrag:disclosure";
const GR: &str = "urn:x-sparkles:graphrag#";
const PROV_WAS_GENERATED_BY: &str = "http://www.w3.org/ns/prov#wasGeneratedBy";
const PROV_WAS_REVISION_OF: &str = "http://www.w3.org/ns/prov#wasRevisionOf";
const PROV_ACTIVITY: &str = "http://www.w3.org/ns/prov#Activity";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// Insert a generated `gr:Report` with its `prov:Activity` and `prov:used` inputs in `graph`.
fn add_report(ds: &Dataset, graph: &str, report: &str, community: Option<&str>, used: &[&str]) {
    let act = format!("{report}#activity");
    let mut body = format!(
        "<{report}> <{RDF_TYPE}> <{GR}Report> ; <{PROV_WAS_GENERATED_BY}> <{act}> .\n\
         <{act}> <{RDF_TYPE}> <{PROV_ACTIVITY}> .\n"
    );
    if let Some(c) = community {
        body.push_str(&format!("<{report}> <{GR}community> <{c}> .\n"));
    }
    for u in used {
        body.push_str(&format!("<{act}> <{PROV_USED}> <{u}> .\n"));
    }
    ds.update(&format!("INSERT DATA {{ GRAPH <{graph}> {{ {body} }} }}"))
        .unwrap();
}

/// Insert a person-edited `gr:Report` (`gr:editOf` + `prov:wasRevisionOf`, no activity).
fn add_report_edit(ds: &Dataset, graph: &str, edit: &str, of: &str, community: &str) {
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{graph}> {{ \
         <{edit}> <{RDF_TYPE}> <{GR}Report> ; \
         <{GR}community> <{community}> ; \
         <{GR}editOf> <{of}> ; \
         <{PROV_WAS_REVISION_OF}> <{of}> . }} }}"
    ))
    .unwrap();
}

/// A simple `GuardDecision` decider that exempts `exempt` graphs when bound to a snapshot.
struct FixedGuard(Vec<String>);

impl sparkles::access::GuardDecision for FixedGuard {
    fn exempt(&self, _snap: &Arc<sparkles::store::Snapshot>) -> Vec<String> {
        self.0.clone()
    }

    fn exempt_controlled(
        &self,
        _snap: &sparkles::access::GuardSnapshot<'_>,
        preparation: &sparkles::access::Preparation,
    ) -> sparkles::Result<sparkles::access::Exemptions> {
        let mut out = sparkles::access::Exemptions::new(preparation)?;
        for g in &self.0 {
            out.push(g)?;
        }
        Ok(out)
    }
}

/// Community reports whose activity used a changed claim, decision or source revision
/// appear in `reports`, while unrelated reports, unguarded graphs and the disclosure
/// graph are excluded. Transitive search follows parent reports and `gr:editOf` chains.
#[test]
fn reports_whose_activity_used_changed_records_are_found() {
    let ds = configured();
    let p = propose_as(
        &ds,
        &EvidenceInput {
            revisions: vec![SourceRevisionInput {
                version: RecordVersion::V1,
                key: key("rev"),
                source: iri("urn:source:advisory"),
                source_key: key("advisory"),
                upstream_version: None,
                content: SourceContent::MetadataOnly {
                    digest: sha("advisory"),
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
                validity: None,
            }],
            ..Default::default()
        },
        CLAIMS,
    )
    .unwrap();
    let rev = record(&p, "rev").0;
    let c = record(&p, "claim");
    let unrelated_claim = claim(&ds, "unrelated");
    let accepted = review(
        &ds,
        "main",
        &[decision("c", &c, Disposition::Accepted, None)],
    );
    let dec = accepted.decisions[0].iri.clone();

    let leaf = "urn:report:leaf";
    let leaf_edit = "urn:report:leaf-edit";
    let leaf_edit2 = "urn:report:leaf-edit-2";
    let replaced = "urn:report:replaced-by-build";
    let parent = "urn:report:parent";
    let direct_and_parent = "urn:report:both";
    let on_rev = "urn:report:on-rev";
    let unrelated = "urn:report:unrelated";
    let fake_unguarded = "urn:report:fake-unguarded";
    let fake_disclosure = "urn:report:fake-disclosure";

    add_report(
        &ds,
        REPORTS,
        leaf,
        Some("urn:comm:leaf"),
        &[c.0.as_str(), dec.as_str()],
    );
    add_report_edit(&ds, REPORTS, leaf_edit, leaf, "urn:comm:leaf");
    add_report_edit(&ds, REPORTS, leaf_edit2, leaf_edit, "urn:comm:leaf");
    // A build replacement carries prov:wasRevisionOf without gr:editOf and is not followed.
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{REPORTS}> {{ \
         <{replaced}> <{RDF_TYPE}> <{GR}Report> ; <{PROV_WAS_REVISION_OF}> <{leaf}> . }} }}"
    ))
    .unwrap();
    // Parent report used the edited leaf report; another parent used both the claim and the leaf.
    add_report(&ds, REPORTS, parent, Some("urn:comm:top"), &[leaf_edit2]);
    add_report(
        &ds,
        REPORTS,
        direct_and_parent,
        Some("urn:comm:both"),
        &[c.0.as_str(), leaf],
    );
    // Report in a second guarded reports graph that used the source revision.
    add_report(&ds, REPORTS_OTHER, on_rev, None, &[rev.as_str()]);
    // Unrelated report that only used unrelated_claim.
    add_report(
        &ds,
        REPORTS,
        unrelated,
        Some("urn:comm:other"),
        &[unrelated_claim.0.as_str()],
    );
    // Fake reports outside the guarded namespace or inside the disclosure graph must never match.
    add_report(
        &ds,
        "urn:not:guarded",
        fake_unguarded,
        Some("urn:comm:fake"),
        &[c.0.as_str()],
    );
    add_report(
        &ds,
        DISCLOSURE,
        fake_disclosure,
        Some("urn:comm:disc"),
        &[c.0.as_str()],
    );

    // Direct search on claim finds leaf, its gr:editOf chain, and direct_and_parent at depth 0.
    let direct = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        false,
        all(),
        &Control::none(),
    )
    .unwrap();
    let direct_ids: Vec<(&str, Option<&str>, &str, u32)> = direct
        .reports
        .iter()
        .map(|r| {
            (
                r.report.as_str(),
                r.community.as_ref().map(|c| c.as_str()),
                r.on.as_str(),
                r.depth,
            )
        })
        .collect();
    assert_eq!(
        direct_ids,
        vec![
            (direct_and_parent, Some("urn:comm:both"), c.0.as_str(), 0),
            (leaf, Some("urn:comm:leaf"), c.0.as_str(), 0),
            (leaf_edit, Some("urn:comm:leaf"), c.0.as_str(), 0),
            (leaf_edit2, Some("urn:comm:leaf"), c.0.as_str(), 0),
        ]
    );

    // Transitive search also finds parent at depth 1 via leaf_edit2, keeps direct_and_parent
    // at depth 0, and excludes unrelated, replaced, fake_unguarded and fake_disclosure.
    let trans = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        true,
        all(),
        &Control::none(),
    )
    .unwrap();
    let trans_ids: Vec<(&str, &str, u32)> = trans
        .reports
        .iter()
        .map(|r| (r.report.as_str(), r.on.as_str(), r.depth))
        .collect();
    assert_eq!(
        trans_ids,
        vec![
            (direct_and_parent, c.0.as_str(), 0),
            (leaf, c.0.as_str(), 0),
            (leaf_edit, c.0.as_str(), 0),
            (leaf_edit2, c.0.as_str(), 0),
            (parent, leaf_edit2, 1),
        ]
    );

    // Searching by decision finds leaf + edits; searching by source revision finds on_rev.
    let by_dec = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&dec),
        false,
        all(),
        &Control::none(),
    )
    .unwrap();
    assert!(by_dec.reports.iter().any(|r| r.report.as_str() == leaf));
    assert!(
        by_dec
            .reports
            .iter()
            .any(|r| r.report.as_str() == leaf_edit)
    );
    assert!(
        !by_dec
            .reports
            .iter()
            .any(|r| r.report.as_str() == unrelated)
    );

    let by_rev = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&rev),
        false,
        all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(by_rev.reports.len(), 1);
    assert_eq!(by_rev.reports[0].report.as_str(), on_rev);
    assert_eq!(by_rev.reports[0].community, None);
    assert_eq!(by_rev.reports[0].graph.as_str(), REPORTS_OTHER);
}

/// Undecided C22 §6 guards, graph grants, pre-bound snapshot validation and triple masks
/// control report visibility without leaking hidden reports.
#[test]
fn report_dependents_enforce_guards_grants_and_triple_masks() {
    use sparkles::access::{Decider, triples};
    let ds = configured();
    let c = claim(&ds, "advisory");
    let r1 = "urn:report:visible";
    let r2 = "urn:report:other-graph";
    let r_parent = "urn:report:parent";
    add_report(&ds, REPORTS, r1, Some("urn:comm:1"), &[c.0.as_str()]);
    add_report(&ds, REPORTS, r_parent, Some("urn:comm:0"), &[r1]);
    add_report(&ds, REPORTS_OTHER, r2, Some("urn:comm:2"), &[c.0.as_str()]);

    // 1. Undecided guard that exempts REPORTS sees only REPORTS, not REPORTS_OTHER.
    let proven_access = Arc::new(GraphAccess::graphs(
        Graphs::All.guarded_by(
            "urn:x-sparkles:graphrag:*",
            Decider(Arc::new(FixedGuard(vec![REPORTS.into()]))),
        ),
        Graphs::none(),
    ));
    let found = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        true,
        Some(proven_access.clone()),
        &Control::none(),
    )
    .unwrap();
    let ids: Vec<&str> = found.reports.iter().map(|r| r.report.as_str()).collect();
    assert_eq!(ids, vec![r1, r_parent]);

    // 2. Undecided guard that exempts nothing hides all guarded reports.
    let unproven_access = Arc::new(GraphAccess::graphs(
        Graphs::All.guarded_by(
            "urn:x-sparkles:graphrag:*",
            Decider(Arc::new(FixedGuard(Vec::new()))),
        ),
        Graphs::none(),
    ));
    let hidden = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        true,
        Some(unproven_access),
        &Control::none(),
    )
    .unwrap();
    assert!(hidden.reports.is_empty());

    // 3. Even when the guard exempts REPORTS, lacking a base graph grant on REPORTS hides it.
    let no_grant = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new([CLAIMS, ANALYSES], &[])).guarded_by(
            "urn:x-sparkles:graphrag:*",
            Decider(Arc::new(FixedGuard(vec![REPORTS.into()]))),
        ),
        Graphs::none(),
    ));
    let hidden = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        true,
        Some(no_grant),
        &Control::none(),
    )
    .unwrap();
    assert!(hidden.reports.is_empty());

    // 4. A policy pre-bound to an earlier snapshot is refused at a later head.
    let old_snap = ds.store().snapshot();
    let stale_bound = proven_access.bound(&old_snap);
    ds.update("INSERT DATA { <urn:x> <urn:p> <urn:o> }")
        .unwrap();
    let err = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        false,
        Some(stale_bound),
        &Control::none(),
    )
    .unwrap_err();
    assert_eq!(err.code(), "invalid", "{err:?}");

    // 5. A triple protection hiding a triple of a report or its activity omits it and
    //    does not follow it transitively.
    let mask_report = Arc::new(GraphAccess::with_triples(
        Graphs::All.guarded_by(
            "urn:x-sparkles:graphrag:*",
            Decider(Arc::new(FixedGuard(vec![REPORTS.into()]))),
        ),
        Graphs::none(),
        triples::TripleRules {
            rules: vec![triples::Rule {
                protection: Arc::new(triples::Protection {
                    name: "hide-community".into(),
                    predicates: Some(vec![format!("{GR}community")]),
                    classes: None,
                    subclasses: false,
                    graphs: Some(Graphs::Only(GraphRule::new([REPORTS], &[]))),
                    pattern: None,
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: triples::Caller::default(),
            limits: triples::Limits::default(),
        },
    ));
    let masked = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        true,
        Some(mask_report),
        &Control::none(),
    )
    .unwrap();
    assert!(masked.reports.is_empty());
}

/// Branch and historical `BoundEvidenceView` reads evaluate both report triples and
/// undecided guards against the exact selected snapshot.
#[test]
fn report_dependents_respect_branches_and_historical_views() {
    let ds = Dataset::memory();
    let prep = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    ds.store()
        .activate_memory_configuration_history(&prep)
        .unwrap();
    evidence::configure(&ds, &settings(), &Control::none()).unwrap();
    let c = claim(&ds, "advisory");
    let before = ds.store().head_commit().seq;
    // Capture the historical BoundEvidenceView before the report is committed.
    let view_before = ds
        .store()
        .evidence_view(&sparkles::history::At::Commit(before), &prep)
        .unwrap();
    let r_v1 = "urn:report:v1";
    add_report(&ds, REPORTS, r_v1, Some("urn:comm:1"), &[c.0.as_str()]);
    let at_v1 = ds.store().head_commit().seq;
    let view_v1 = ds
        .store()
        .evidence_view(&sparkles::history::At::Commit(at_v1), &prep)
        .unwrap();

    // Create a branch and add a branch-only report there.
    ds.store()
        .create_branch("work", &sparkles::branch::BranchOptions::default())
        .unwrap();
    let work = ds.branch("work").unwrap();
    let r_branch = "urn:report:branch-only";
    add_report(
        &work,
        REPORTS,
        r_branch,
        Some("urn:comm:b"),
        &[c.0.as_str()],
    );

    // Main sees only r_v1; branch sees r_v1 and r_branch.
    let main_found = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        false,
        all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(
        main_found
            .reports
            .iter()
            .map(|r| r.report.as_str())
            .collect::<Vec<_>>(),
        vec![r_v1]
    );
    let branch_found = evidence::find_dependents(
        &ds,
        &work,
        std::slice::from_ref(&c.0),
        false,
        all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(
        branch_found
            .reports
            .iter()
            .map(|r| r.report.as_str())
            .collect::<Vec<_>>(),
        vec![r_branch, r_v1]
    );

    // Add a later report on main, then read the retained historical BoundEvidenceViews.
    let r_v2 = "urn:report:v2";
    add_report(&ds, REPORTS, r_v2, Some("urn:comm:2"), &[c.0.as_str()]);
    let found_before = evidence::find_dependents_at(
        &view_before,
        std::slice::from_ref(&c.0),
        false,
        all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(found_before.commit, before);
    assert!(found_before.reports.is_empty());

    let found_v1 = evidence::find_dependents_at(
        &view_v1,
        std::slice::from_ref(&c.0),
        false,
        all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(found_v1.commit, at_v1);
    assert_eq!(
        found_v1
            .reports
            .iter()
            .map(|r| r.report.as_str())
            .collect::<Vec<_>>(),
        vec![r_v1]
    );
}

/// More than `MAX_EVIDENCE` visible report candidates refuse with `incomplete-evidence`,
/// while an unproven caller never trips the bound on hidden reports.
#[test]
fn report_dependents_bound_counts_only_visible_candidates() {
    use sparkles::access::Decider;
    let ds = configured();
    let c = claim(&ds, "advisory");
    let mut trig = format!("GRAPH <{REPORTS}> {{\n");
    for i in 0..10_001 {
        trig.push_str(&format!(
            "<urn:act:{i}> <{PROV_USED}> <{}> .\n",
            c.0.as_str()
        ));
    }
    trig.push_str("}\n");
    ds.update(&format!("INSERT DATA {{ {trig} }}")).unwrap();

    // Unrestricted caller exceeds the 10,000 candidate bound.
    let err = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        false,
        all(),
        &Control::none(),
    )
    .unwrap_err();
    assert_eq!(err.code(), "incomplete-evidence", "{err:?}");

    // Unproven caller cannot read REPORTS and must not observe the bound or hidden count.
    let unproven = Arc::new(GraphAccess::graphs(
        Graphs::All.guarded_by(
            "urn:x-sparkles:graphrag:*",
            Decider(Arc::new(FixedGuard(Vec::new()))),
        ),
        Graphs::none(),
    ));
    let hidden = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        false,
        Some(unproven),
        &Control::none(),
    )
    .unwrap();
    assert!(hidden.reports.is_empty());

    // Cancelled control stops immediately.
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let cancelled = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        false,
        all(),
        &Control::with_cancel(cancel),
    )
    .unwrap_err();
    assert!(
        matches!(cancelled, sparkles::Error::Cancelled),
        "{cancelled:?}"
    );
}

/// A restricted `GraphAccess` without a snapshot-bound disclosure guard never reads
/// guarded reports graphs, even when it explicitly grants the reports graph or masks
/// triples on `Graphs::All`. Applies to both head (`find_dependents`) and historical
/// (`find_dependents_at`) calls.
#[test]
fn report_dependents_require_positive_guard_proof_for_restricted_policies() {
    use sparkles::access::{Decider, triples};

    let ds = Dataset::memory();
    let prep = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    ds.store()
        .activate_memory_configuration_history(&prep)
        .unwrap();
    evidence::configure(&ds, &settings(), &Control::none()).unwrap();
    let c = claim(&ds, "advisory");
    let r = "urn:report:guarded";
    add_report(&ds, REPORTS, r, Some("urn:comm:1"), &[c.0.as_str()]);
    let seq = ds.store().head_commit().seq;
    let view = ds
        .store()
        .evidence_view(&sparkles::history::At::Commit(seq), &prep)
        .unwrap();

    // 1. Bare restricted graph grant explicitly naming REPORTS, with no guard.
    let bare_grant = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new([REPORTS], &[])),
        Graphs::none(),
    ));
    // 2. Triple-masked Graphs::All with no guard (restricted because it hides triples).
    let masked_without_guard = Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::none(),
        triples::TripleRules {
            rules: vec![triples::Rule {
                protection: Arc::new(triples::Protection {
                    name: "hide-other".into(),
                    predicates: Some(vec!["urn:unrelated:pred".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: None,
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: triples::Caller::default(),
            limits: triples::Limits::default(),
        },
    ));
    // 3. Statically constructed unbound guard (not bound to a snapshot).
    let unbound_static = Arc::new(GraphAccess::graphs(
        Graphs::All.guarded("urn:x-sparkles:graphrag:*", &[REPORTS.into()]),
        Graphs::none(),
    ));
    for policy in [bare_grant, masked_without_guard, unbound_static] {
        let head_res = evidence::find_dependents(
            &ds,
            &ds,
            std::slice::from_ref(&c.0),
            true,
            Some(policy.clone()),
            &Control::none(),
        )
        .unwrap();
        assert!(head_res.reports.is_empty(), "{head_res:?}");

        let hist_res = evidence::find_dependents_at(
            &view,
            std::slice::from_ref(&c.0),
            true,
            Some(policy),
            &Control::none(),
        )
        .unwrap();
        assert!(hist_res.reports.is_empty(), "{hist_res:?}");
    }

    // Positive controls: undecided guard bound during the call, and pre-bound policy
    // matching the exact snapshot.
    let proven_undecided = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new([REPORTS], &[])).guarded_by(
            "urn:x-sparkles:graphrag:*",
            Decider(Arc::new(FixedGuard(vec![REPORTS.into()]))),
        ),
        Graphs::none(),
    ));
    let proven_prebound = proven_undecided.bound(&ds.store().snapshot());
    for policy in [proven_undecided, proven_prebound] {
        let head_res = evidence::find_dependents(
            &ds,
            &ds,
            std::slice::from_ref(&c.0),
            false,
            Some(policy.clone()),
            &Control::none(),
        )
        .unwrap();
        assert_eq!(head_res.reports.len(), 1);
        assert_eq!(head_res.reports[0].report.as_str(), r);

        let hist_res = evidence::find_dependents_at(
            &view,
            std::slice::from_ref(&c.0),
            false,
            Some(policy),
            &Control::none(),
        )
        .unwrap();
        assert_eq!(hist_res.reports.len(), 1);
        assert_eq!(hist_res.reports[0].report.as_str(), r);
    }
}

/// Both `find_dependents` and `find_dependents_at` refuse with `unsupported-profile`
/// when the evidence profile is unconfigured or disabled at the read view.
#[test]
fn dependents_refuses_when_profile_is_disabled_or_unconfigured() {
    let ds = Dataset::memory();
    let prep = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    ds.store()
        .activate_memory_configuration_history(&prep)
        .unwrap();
    let dummy = iri("urn:claim:any");
    add_report(&ds, REPORTS, "urn:report:early", None, &[dummy.as_str()]);
    let pre_seq = ds.store().head_commit().seq;
    let pre_view = ds
        .store()
        .evidence_view(&sparkles::history::At::Commit(pre_seq), &prep)
        .unwrap();

    // Historical view captured before evidence::configure must refuse.
    let err = evidence::find_dependents_at(
        &pre_view,
        std::slice::from_ref(&dummy),
        false,
        all(),
        &Control::none(),
    )
    .unwrap_err();
    assert_eq!(err.code(), "unsupported-profile", "{err:?}");

    // Unconfigured head must refuse with `unsupported-profile`.
    let uncfg_head_err = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&dummy),
        false,
        all(),
        &Control::none(),
    )
    .unwrap_err();
    assert_eq!(
        uncfg_head_err.code(),
        "unsupported-profile",
        "{uncfg_head_err:?}"
    );

    // Enable the profile, detach its managed graphs, and then explicitly disable it so
    // disabled owner settings bytes (`enabled: false`) are genuinely recorded in both
    // the live managed registry and configuration history.
    let enabled = settings();
    evidence::configure(&ds, &enabled, &Control::none()).unwrap();
    for g in enabled.managed_graphs() {
        evidence::detach(&ds, &g).unwrap();
    }
    let disabled = Settings {
        enabled: false,
        ..enabled
    };
    evidence::configure(&ds, &disabled, &Control::none()).unwrap();
    let live_settings_bytes = ds
        .store()
        .managed_snapshot()
        .unwrap()
        .expect("managed snapshot recorded")
        .settings
        .get(&sparkles::managed::OwnerKind::Evidence)
        .cloned()
        .expect("disabled evidence settings recorded at head");
    let live_settings = Settings::from_slice(&live_settings_bytes, &Control::none()).unwrap();
    assert!(
        !live_settings.enabled,
        "head must carry recorded disabled settings"
    );

    // Advance RDF history after disabling so the new commit binds the recorded disabled
    // configuration generation in configuration history.
    add_report(
        &ds,
        REPORTS,
        "urn:report:after-disable",
        None,
        &[dummy.as_str()],
    );
    let dis_seq = ds.store().head_commit().seq;
    assert!(dis_seq > pre_seq);
    let dis_view = ds
        .store()
        .evidence_view(&sparkles::history::At::Commit(dis_seq), &prep)
        .unwrap();
    let hist_settings_bytes = dis_view
        .owner_settings(sparkles::managed::OwnerKind::Evidence)
        .expect("disabled evidence settings recorded in historical view");
    let hist_settings = Settings::from_slice(hist_settings_bytes, &Control::none()).unwrap();
    assert!(
        !hist_settings.enabled,
        "historical view must carry recorded disabled settings"
    );

    let head_err = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&dummy),
        false,
        all(),
        &Control::none(),
    )
    .unwrap_err();
    assert_eq!(head_err.code(), "unsupported-profile", "{head_err:?}");

    let hist_err = evidence::find_dependents_at(
        &dis_view,
        std::slice::from_ref(&dummy),
        false,
        all(),
        &Control::none(),
    )
    .unwrap_err();
    assert_eq!(hist_err.code(), "unsupported-profile", "{hist_err:?}");
}

/// Partially masked activities, reports and edits are excluded before charging
/// `MAX_EVIDENCE`, so >10,000 partially masked records do not trigger
/// `incomplete-evidence` and produce the exact same result as a dataset without them.
#[test]
fn partially_masked_records_do_not_trigger_candidate_bound() {
    use sparkles::access::{Decider, triples};

    let hide_secret = || {
        Arc::new(GraphAccess::with_triples(
            Graphs::All.guarded_by(
                "urn:x-sparkles:graphrag:*",
                Decider(Arc::new(FixedGuard(vec![REPORTS.into()]))),
            ),
            Graphs::none(),
            triples::TripleRules {
                rules: vec![triples::Rule {
                    protection: Arc::new(triples::Protection {
                        name: "hide-secret".into(),
                        predicates: Some(vec!["urn:secret:tag".into()]),
                        classes: None,
                        subclasses: false,
                        graphs: Some(Graphs::Only(GraphRule::new([REPORTS], &[]))),
                        pattern: None,
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: triples::Caller::default(),
                limits: triples::Limits::default(),
            },
        ))
    };

    // Baseline dataset with only the one visible report (and one visible edit).
    let baseline = configured();
    let c_base = claim(&baseline, "advisory");
    let vis_rep = "urn:report:visible";
    let vis_edit = "urn:report:visible-edit";
    add_report(
        &baseline,
        REPORTS,
        vis_rep,
        Some("urn:comm:v"),
        &[c_base.0.as_str()],
    );
    add_report_edit(&baseline, REPORTS, vis_edit, vis_rep, "urn:comm:v");
    let expected = evidence::find_dependents(
        &baseline,
        &baseline,
        std::slice::from_ref(&c_base.0),
        true,
        Some(hide_secret()),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(expected.reports.len(), 2);

    let shape = |reps: &[evidence::ReportDependent], expected_on: &NamedNode| {
        reps.iter()
            .map(|r| {
                assert_eq!(&r.on, expected_on);
                (
                    r.report.clone(),
                    r.community.clone(),
                    r.graph.clone(),
                    r.dependence,
                    r.depth,
                )
            })
            .collect::<Vec<_>>()
    };
    let expected_shape = shape(&expected.reports, &c_base.0);

    // 1. 10,001 partially masked reports pointing to the visible activity.
    let ds_reps = configured();
    let c = claim(&ds_reps, "advisory");
    add_report(
        &ds_reps,
        REPORTS,
        vis_rep,
        Some("urn:comm:v"),
        &[c.0.as_str()],
    );
    add_report_edit(&ds_reps, REPORTS, vis_edit, vis_rep, "urn:comm:v");
    let mut trig = format!("GRAPH <{REPORTS}> {{\n");
    for i in 0..10_001 {
        trig.push_str(&format!(
            "<urn:rep:masked:{i}> <{RDF_TYPE}> <{GR}Report> ; \
             <{PROV_WAS_GENERATED_BY}> <{vis_rep}#activity> ; \
             <urn:secret:tag> \"x\" .\n"
        ));
    }
    trig.push_str("}\n");
    ds_reps
        .update(&format!("INSERT DATA {{ {trig} }}"))
        .unwrap();
    let got_reps = evidence::find_dependents(
        &ds_reps,
        &ds_reps,
        std::slice::from_ref(&c.0),
        true,
        Some(hide_secret()),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(shape(&got_reps.reports, &c.0), expected_shape);

    // 2. 10,001 partially masked activities using the claim.
    let ds_acts = configured();
    let c2 = claim(&ds_acts, "advisory");
    add_report(
        &ds_acts,
        REPORTS,
        vis_rep,
        Some("urn:comm:v"),
        &[c2.0.as_str()],
    );
    add_report_edit(&ds_acts, REPORTS, vis_edit, vis_rep, "urn:comm:v");
    let mut trig = format!("GRAPH <{REPORTS}> {{\n");
    for i in 0..10_001 {
        trig.push_str(&format!(
            "<urn:act:masked:{i}> <{RDF_TYPE}> <{PROV_ACTIVITY}> ; \
             <{PROV_USED}> <{}> ; <urn:secret:tag> \"x\" .\n",
            c2.0.as_str()
        ));
    }
    trig.push_str("}\n");
    ds_acts
        .update(&format!("INSERT DATA {{ {trig} }}"))
        .unwrap();
    let got_acts = evidence::find_dependents(
        &ds_acts,
        &ds_acts,
        std::slice::from_ref(&c2.0),
        true,
        Some(hide_secret()),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(shape(&got_acts.reports, &c2.0), expected_shape);

    // 3. 10,001 partially masked edits of the visible report.
    let ds_edits = configured();
    let c3 = claim(&ds_edits, "advisory");
    add_report(
        &ds_edits,
        REPORTS,
        vis_rep,
        Some("urn:comm:v"),
        &[c3.0.as_str()],
    );
    add_report_edit(&ds_edits, REPORTS, vis_edit, vis_rep, "urn:comm:v");
    let mut trig = format!("GRAPH <{REPORTS}> {{\n");
    for i in 0..10_001 {
        trig.push_str(&format!(
            "<urn:edit:masked:{i}> <{RDF_TYPE}> <{GR}Report> ; \
             <{GR}editOf> <{vis_rep}> ; <urn:secret:tag> \"x\" .\n"
        ));
    }
    trig.push_str("}\n");
    ds_edits
        .update(&format!("INSERT DATA {{ {trig} }}"))
        .unwrap();
    let got_edits = evidence::find_dependents(
        &ds_edits,
        &ds_edits,
        std::slice::from_ref(&c3.0),
        true,
        Some(hide_secret()),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(shape(&got_edits.reports, &c3.0), expected_shape);
}

/// Masking a triple on `<report#activity>` omits the report and prevents transitive
/// traversal to parents even when the report itself has no hidden triples.
#[test]
fn report_dependents_omit_report_when_activity_is_masked() {
    use sparkles::access::{Decider, triples};

    let ds = configured();
    let c = claim(&ds, "advisory");
    let leaf = "urn:report:leaf";
    let parent = "urn:report:parent";
    add_report(&ds, REPORTS, leaf, Some("urn:comm:1"), &[c.0.as_str()]);
    add_report(&ds, REPORTS, parent, Some("urn:comm:0"), &[leaf]);
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{REPORTS}> {{ <{leaf}#activity> <urn:sparkles:model> \"secret-model\" . }} }}"
    ))
    .unwrap();

    let mask_activity = Arc::new(GraphAccess::with_triples(
        Graphs::All.guarded_by(
            "urn:x-sparkles:graphrag:*",
            Decider(Arc::new(FixedGuard(vec![REPORTS.into()]))),
        ),
        Graphs::none(),
        triples::TripleRules {
            rules: vec![triples::Rule {
                protection: Arc::new(triples::Protection {
                    name: "hide-model".into(),
                    predicates: Some(vec!["urn:sparkles:model".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: Some(Graphs::Only(GraphRule::new([REPORTS], &[]))),
                    pattern: None,
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: triples::Caller::default(),
            limits: triples::Limits::default(),
        },
    ));
    let hidden = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        true,
        Some(mask_activity),
        &Control::none(),
    )
    .unwrap();
    assert!(hidden.reports.is_empty(), "{hidden:?}");

    // Control without mask sees both leaf and parent.
    let visible = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        true,
        all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(visible.reports.len(), 2);
}

/// Direct lookup by an edited report IRI requires the predecessor to be a visible,
/// unmasked `gr:Report` in that proven graph. Missing or masked predecessors yield
/// nothing, while a visible predecessor yields its edit chain and transitive parents.
#[test]
fn direct_edit_lookup_requires_visible_unmasked_predecessor() {
    use sparkles::access::{Decider, triples};

    let ds = configured();
    let missing = iri("urn:report:missing");
    let masked_base = iri("urn:report:masked-base");
    let visible_base = iri("urn:report:visible-base");
    let edit_missing = "urn:report:edit-missing";
    let edit_masked = "urn:report:edit-masked";
    let parent_masked = "urn:report:parent-of-masked-edit";
    let edit_vis_1 = "urn:report:edit-vis-1";
    let edit_vis_2 = "urn:report:edit-vis-2";
    let parent_vis = "urn:report:parent-vis";

    add_report_edit(&ds, REPORTS, edit_missing, missing.as_str(), "urn:comm:m");
    add_report(
        &ds,
        REPORTS,
        masked_base.as_str(),
        Some("urn:comm:mb"),
        &["urn:claim:x"],
    );
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{REPORTS}> {{ <{}> <urn:secret:tag> \"x\" . }} }}",
        masked_base.as_str()
    ))
    .unwrap();
    add_report_edit(
        &ds,
        REPORTS,
        edit_masked,
        masked_base.as_str(),
        "urn:comm:mb",
    );
    add_report(
        &ds,
        REPORTS,
        parent_masked,
        Some("urn:comm:pm"),
        &[edit_masked],
    );

    add_report(
        &ds,
        REPORTS,
        visible_base.as_str(),
        Some("urn:comm:vb"),
        &["urn:claim:y"],
    );
    add_report_edit(
        &ds,
        REPORTS,
        edit_vis_1,
        visible_base.as_str(),
        "urn:comm:vb",
    );
    add_report_edit(&ds, REPORTS, edit_vis_2, edit_vis_1, "urn:comm:vb");
    add_report(&ds, REPORTS, parent_vis, Some("urn:comm:pv"), &[edit_vis_2]);

    // 1. Missing predecessor returns no reports.
    let res_missing = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&missing),
        true,
        all(),
        &Control::none(),
    )
    .unwrap();
    assert!(res_missing.reports.is_empty(), "{res_missing:?}");

    // 2. Masked predecessor hides its visible edit and transitive parent.
    let mask_access = Arc::new(GraphAccess::with_triples(
        Graphs::All.guarded_by(
            "urn:x-sparkles:graphrag:*",
            Decider(Arc::new(FixedGuard(vec![REPORTS.into()]))),
        ),
        Graphs::none(),
        triples::TripleRules {
            rules: vec![triples::Rule {
                protection: Arc::new(triples::Protection {
                    name: "hide-secret".into(),
                    predicates: Some(vec!["urn:secret:tag".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: Some(Graphs::Only(GraphRule::new([REPORTS], &[]))),
                    pattern: None,
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: triples::Caller::default(),
            limits: triples::Limits::default(),
        },
    ));
    let res_masked = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&masked_base),
        true,
        Some(mask_access.clone()),
        &Control::none(),
    )
    .unwrap();
    assert!(res_masked.reports.is_empty(), "{res_masked:?}");

    // 3. Visible unmasked predecessor returns its edit chain (depth 0) and parent (depth 1).
    let res_vis = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&visible_base),
        true,
        Some(mask_access),
        &Control::none(),
    )
    .unwrap();
    let got: Vec<(&str, &str, u32)> = res_vis
        .reports
        .iter()
        .map(|r| (r.report.as_str(), r.on.as_str(), r.depth))
        .collect();
    assert_eq!(
        got,
        vec![
            (edit_vis_1, visible_base.as_str(), 0),
            (edit_vis_2, visible_base.as_str(), 0),
            (parent_vis, edit_vis_2, 1),
        ]
    );
}

/// Duplicate IRIs in `changed`, transitive `gr:editOf` chains near `MAX_EVIDENCE`,
/// blank-node subjects, and cyclic `gr:editOf` / `prov:used` graphs all avoid
/// double-counting and terminate cleanly.
#[test]
fn repeated_inputs_transitive_edits_blank_nodes_and_cycles_behave_cleanly() {
    // 1. Repeated `changed` IRIs do not double-count near MAX_EVIDENCE, and blank-node
    //    subjects do not consume the candidate budget.
    let ds = configured();
    let c = claim(&ds, "advisory");
    let mut trig = format!("GRAPH <{REPORTS}> {{\n");
    trig.push_str(&format!(
        "<urn:act:0> <{RDF_TYPE}> <{PROV_ACTIVITY}> ; <{PROV_USED}> <{}> .\n",
        c.0.as_str()
    ));
    for i in 0..5_500 {
        trig.push_str(&format!(
            "<urn:rep:{i}> <{RDF_TYPE}> <{GR}Report> ; <{PROV_WAS_GENERATED_BY}> <urn:act:0> .\n"
        ));
    }
    for i in 0..500 {
        trig.push_str(&format!(
            "_:bact{i} <{PROV_USED}> <{}> .\n_:brep{i} <{PROV_WAS_GENERATED_BY}> <urn:act:0> .\n",
            c.0.as_str()
        ));
    }
    trig.push_str("}\n");
    ds.update(&format!("INSERT DATA {{ {trig} }}")).unwrap();
    let once = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        false,
        all(),
        &Control::none(),
    )
    .unwrap();
    let repeated = evidence::find_dependents(
        &ds,
        &ds,
        &[c.0.clone(), c.0.clone(), c.0.clone()],
        false,
        all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(once.reports.len(), 5_500);
    assert_eq!(repeated.reports, once.reports);

    // 2. Transitive `gr:editOf` edges are not double-scanned/double-counted.
    //    1 activity + 1 base report + 6,000 edits = 6,002 <= 10,000 (double-counting
    //    would hit 12,002 and falsely refuse with `incomplete-evidence`).
    let ds_edits = configured();
    let c2 = claim(&ds_edits, "advisory");
    add_report(&ds_edits, REPORTS, "urn:rep:base", None, &[c2.0.as_str()]);
    let mut trig = format!("GRAPH <{REPORTS}> {{\n");
    for i in 0..6_000 {
        trig.push_str(&format!(
            "<urn:edit:{i}> <{RDF_TYPE}> <{GR}Report> ; <{GR}editOf> <urn:rep:base> .\n"
        ));
    }
    trig.push_str("}\n");
    ds_edits
        .update(&format!("INSERT DATA {{ {trig} }}"))
        .unwrap();
    let trans_edits = evidence::find_dependents(
        &ds_edits,
        &ds_edits,
        std::slice::from_ref(&c2.0),
        true,
        all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(trans_edits.reports.len(), 6_001);

    // 3. Cyclic `gr:editOf` (2-cycle + self-loop) and cyclic `prov:used` terminate cleanly.
    let ds_cyc = configured();
    let c3 = claim(&ds_cyc, "advisory");
    add_report(&ds_cyc, REPORTS, "urn:cyc:r1", None, &[c3.0.as_str()]);
    add_report_edit(&ds_cyc, REPORTS, "urn:cyc:r2", "urn:cyc:r1", "urn:comm:c");
    add_report_edit(&ds_cyc, REPORTS, "urn:cyc:r1", "urn:cyc:r2", "urn:comm:c");
    add_report_edit(&ds_cyc, REPORTS, "urn:cyc:r1", "urn:cyc:r1", "urn:comm:c");
    add_report(
        &ds_cyc,
        REPORTS,
        "urn:cyc:p1",
        None,
        &["urn:cyc:r2", "urn:cyc:p2"],
    );
    add_report(&ds_cyc, REPORTS, "urn:cyc:p2", None, &["urn:cyc:p1"]);
    let cyc = evidence::find_dependents(
        &ds_cyc,
        &ds_cyc,
        std::slice::from_ref(&c3.0),
        true,
        all(),
        &Control::none(),
    )
    .unwrap();
    let got: Vec<(&str, u32)> = cyc
        .reports
        .iter()
        .map(|r| (r.report.as_str(), r.depth))
        .collect();
    assert_eq!(
        got,
        vec![
            ("urn:cyc:r1", 0),
            ("urn:cyc:r2", 0),
            ("urn:cyc:p1", 1),
            ("urn:cyc:p2", 2),
        ]
    );
}

/// A long stored input IRI repeated across reports is charged against the preparation
/// allowance before cloning `on` into retained results.
#[test]
fn long_input_iri_repeated_across_reports_hits_memory_budget() {
    let ds = configured();
    let long_input = NamedNode::new(format!("urn:input:{}", "x".repeat(64 * 1024))).unwrap();
    let mut trig = format!("GRAPH <{REPORTS}> {{\n");
    trig.push_str(&format!(
        "<urn:act:0> <{RDF_TYPE}> <{PROV_ACTIVITY}> ; <{PROV_USED}> <{}> .\n",
        long_input.as_str()
    ));
    for i in 0..300 {
        trig.push_str(&format!(
            "<urn:rep:{i}> <{RDF_TYPE}> <{GR}Report> ; <{PROV_WAS_GENERATED_BY}> <urn:act:0> .\n"
        ));
    }
    trig.push_str("}\n");
    ds.update(&format!("INSERT DATA {{ {trig} }}")).unwrap();

    // 300 copies of a 64 KiB `on` IRI exceed the 16 MiB preparation budget despite
    // staying well below the 10,000-candidate limit.
    let err = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&long_input),
        false,
        all(),
        &Control::none(),
    )
    .unwrap_err();
    assert!(matches!(err, sparkles::Error::BudgetExceeded(_)), "{err:?}");
}

/// An undecided legacy write guard on `access.write` does not block read-only
/// `find_dependents` or `find_dependents_at`, whether alone or combined with an
/// undecided controlled read guard or a triple mask that requires view preparation.
#[test]
fn undecided_write_guard_does_not_block_read_only_dependents() {
    use sparkles::access::{Decider, GuardDecision, triples};

    struct LegacyWriteGuard;
    impl GuardDecision for LegacyWriteGuard {
        fn exempt(&self, _snap: &Arc<sparkles::store::Snapshot>) -> Vec<String> {
            Vec::new()
        }
        // Uses default `exempt_controlled`, which returns `Error::Unsupported`.
    }

    let ds = Dataset::memory();
    let prep = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    ds.store()
        .activate_memory_configuration_history(&prep)
        .unwrap();
    evidence::configure(&ds, &settings(), &Control::none()).unwrap();
    let c = claim(&ds, "advisory");
    add_report(&ds, REPORTS, "urn:report:ok", None, &[c.0.as_str()]);
    add_report(&ds, REPORTS, "urn:report:secret", None, &[c.0.as_str()]);
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{REPORTS}> {{ <urn:report:secret> <urn:secret:tag> \"hidden\" . }} }}"
    ))
    .unwrap();
    let seq = ds.store().head_commit().seq;
    let view = ds
        .store()
        .evidence_view(&sparkles::history::At::Commit(seq), &prep)
        .unwrap();

    let legacy_write = || Graphs::All.guarded_by("urn:g:*", Decider(Arc::new(LegacyWriteGuard)));

    // 1. Legacy write guard alone (no view preparation needed).
    let write_only = Arc::new(GraphAccess::graphs(Graphs::All, legacy_write()));
    let res = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        false,
        Some(write_only),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(res.reports.len(), 2);

    // 2. Legacy write guard combined with an undecided controlled read guard.
    let with_read_guard = Arc::new(GraphAccess::graphs(
        Graphs::All.guarded_by(
            "urn:x-sparkles:graphrag:*",
            Decider(Arc::new(FixedGuard(vec![REPORTS.into()]))),
        ),
        legacy_write(),
    ));
    let head_rg = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        false,
        Some(with_read_guard.clone()),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(head_rg.reports.len(), 2);
    let hist_rg = evidence::find_dependents_at(
        &view,
        std::slice::from_ref(&c.0),
        false,
        Some(with_read_guard),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(hist_rg.reports.len(), 2);

    // 3. Legacy write guard combined with a triple mask (and controlled read guard).
    let secret_rules = || triples::TripleRules {
        rules: vec![triples::Rule {
            protection: Arc::new(triples::Protection {
                name: "hide-secret".into(),
                predicates: Some(vec!["urn:secret:tag".into()]),
                classes: None,
                subclasses: false,
                graphs: Some(Graphs::Only(GraphRule::new([REPORTS], &[]))),
                pattern: None,
                prefixes: Default::default(),
                hide_inferences: false,
            }),
            read: Graphs::none(),
            write: Graphs::none(),
        }],
        caller: triples::Caller::default(),
        limits: triples::Limits::default(),
    };
    let with_mask_and_guard = Arc::new(GraphAccess::with_triples(
        Graphs::All.guarded_by(
            "urn:x-sparkles:graphrag:*",
            Decider(Arc::new(FixedGuard(vec![REPORTS.into()]))),
        ),
        legacy_write(),
        secret_rules(),
    ));
    let head_masked = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        false,
        Some(with_mask_and_guard.clone()),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(head_masked.reports.len(), 1);
    assert_eq!(head_masked.reports[0].report.as_str(), "urn:report:ok");
    let hist_masked = evidence::find_dependents_at(
        &view,
        std::slice::from_ref(&c.0),
        false,
        Some(with_mask_and_guard),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(hist_masked.reports.len(), 1);
    assert_eq!(hist_masked.reports[0].report.as_str(), "urn:report:ok");

    // 4. Legacy write guard combined with a triple mask alone (no read guard).
    let with_mask_only = Arc::new(GraphAccess::with_triples(
        Graphs::All,
        legacy_write(),
        secret_rules(),
    ));
    let head_mask_only = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        false,
        Some(with_mask_only.clone()),
        &Control::none(),
    )
    .unwrap();
    assert!(head_mask_only.reports.is_empty());
    let hist_mask_only = evidence::find_dependents_at(
        &view,
        std::slice::from_ref(&c.0),
        false,
        Some(with_mask_only),
        &Control::none(),
    )
    .unwrap();
    assert!(hist_mask_only.reports.is_empty());
}

/// An unreadable guarded graph with an oversized (140 KiB) IRI carrying a hidden
/// `rdf:type gr:Report` (or hidden `prov:used` / `gr:editOf`) must not turn an empty
/// lookup into `BudgetExceeded(Memory)`, and many admitted report-type graphs with
/// no edit edges return cleanly without retaining all membership graphs.
#[test]
fn unreadable_long_report_type_graph_and_many_admitted_graphs_without_edits() {
    let ds = configured();
    let target = iri("urn:known:target");
    let narrow_read = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new([CLAIMS], &[])),
        Graphs::none(),
    ));

    // Baseline without the hidden type statement.
    let before = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&target),
        false,
        Some(narrow_read.clone()),
        &Control::none(),
    )
    .unwrap();
    assert!(before.dependents.is_empty());
    assert!(before.reports.is_empty());

    // Add a hidden `rdf:type gr:Report` (plus hidden `prov:used` and `gr:editOf`) in
    // an unreadable guarded graph with a 140 KiB IRI. Under `with_term` (128x charge),
    // decoding this graph IRI alone would exceed the 16 MiB preparation cap.
    let huge_unreadable = format!("urn:x-sparkles:graphrag:hidden-{}", "x".repeat(140 * 1024));
    ds.update(&format!(
        "INSERT DATA {{ \
            GRAPH <{huge_unreadable}> {{ \
                <{t}> <{RDF_TYPE}> <{GR}Report> . \
                <urn:act:hidden> <{RDF_TYPE}> <{PROV_ACTIVITY}> ; <{PROV_USED}> <{t}> . \
                <urn:edit:hidden> <{RDF_TYPE}> <{GR}Report> ; <{GR}editOf> <{t}> . \
            }} \
        }}",
        t = target.as_str()
    ))
    .unwrap();

    let after = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&target),
        false,
        Some(narrow_read),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(after.dependents, before.dependents);
    assert_eq!(after.reports, before.reports);

    // Many admitted membership graphs with `rdf:type gr:Report` and no `gr:editOf`
    // edges succeed cleanly under unrestricted read access.
    let ds_many = configured();
    let mut trig = String::new();
    for i in 0..300 {
        trig.push_str(&format!(
            "GRAPH <urn:x-sparkles:graphrag:empty-edits-{i}> {{ <{t}> <{RDF_TYPE}> <{GR}Report> . }}\n",
            t = target.as_str()
        ));
    }
    ds_many
        .update(&format!("INSERT DATA {{ {trig} }}"))
        .unwrap();
    let many = evidence::find_dependents(
        &ds_many,
        &ds_many,
        std::slice::from_ref(&target),
        false,
        all(),
        &Control::none(),
    )
    .unwrap();
    assert!(many.dependents.is_empty());
    assert!(many.reports.is_empty());
}

/// Historical `find_dependents_at` binds undecided guards and triple protections against
/// the exact historical snapshot rather than head.
#[test]
fn historical_dependents_bind_guard_and_mask_to_selected_snapshot() {
    use sparkles::access::{Decider, GuardDecision, triples};

    struct CommitAwareGuard {
        allow_commit: u64,
    }
    impl GuardDecision for CommitAwareGuard {
        fn exempt(&self, snap: &Arc<sparkles::store::Snapshot>) -> Vec<String> {
            if snap.commit == self.allow_commit {
                vec![REPORTS.into()]
            } else {
                Vec::new()
            }
        }
        fn exempt_controlled(
            &self,
            snap: &sparkles::access::GuardSnapshot<'_>,
            preparation: &sparkles::access::Preparation,
        ) -> sparkles::Result<sparkles::access::Exemptions> {
            let mut out = sparkles::access::Exemptions::new(preparation)?;
            if snap.commit() == self.allow_commit {
                out.push(REPORTS)?;
            }
            Ok(out)
        }
    }

    let ds = Dataset::memory();
    let prep = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    ds.store()
        .activate_memory_configuration_history(&prep)
        .unwrap();
    evidence::configure(&ds, &settings(), &Control::none()).unwrap();
    let c = claim(&ds, "advisory");
    let r = "urn:report:hist";
    add_report(&ds, REPORTS, r, Some("urn:comm:1"), &[c.0.as_str()]);
    let at_v1 = ds.store().head_commit().seq;
    let view_v1 = ds
        .store()
        .evidence_view(&sparkles::history::At::Commit(at_v1), &prep)
        .unwrap();

    // After v1, add a triple on `r` that a triple protection masks.
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{REPORTS}> {{ <{r}> <urn:secret:tag> \"added-later\" . }} }}"
    ))
    .unwrap();
    let at_head = ds.store().head_commit().seq;
    assert_ne!(at_v1, at_head);

    let mask_later = |allow_commit: u64| {
        Arc::new(GraphAccess::with_triples(
            Graphs::All.guarded_by(
                "urn:x-sparkles:graphrag:*",
                Decider(Arc::new(CommitAwareGuard { allow_commit })),
            ),
            Graphs::none(),
            triples::TripleRules {
                rules: vec![triples::Rule {
                    protection: Arc::new(triples::Protection {
                        name: "hide-secret".into(),
                        predicates: Some(vec!["urn:secret:tag".into()]),
                        classes: None,
                        subclasses: false,
                        graphs: Some(Graphs::Only(GraphRule::new([REPORTS], &[]))),
                        pattern: None,
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: triples::Caller::default(),
                limits: triples::Limits::default(),
            },
        ))
    };

    // Guard proves at v1 (and secret tag does not exist yet at v1): historical read sees `r`,
    // while head read sees nothing (guard denies at head AND secret tag masks `r` at head).
    let policy_v1 = mask_later(at_v1);
    let hist = evidence::find_dependents_at(
        &view_v1,
        std::slice::from_ref(&c.0),
        false,
        Some(policy_v1.clone()),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(hist.reports.len(), 1);
    assert_eq!(hist.reports[0].report.as_str(), r);

    let head = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        false,
        Some(policy_v1),
        &Control::none(),
    )
    .unwrap();
    assert!(head.reports.is_empty());

    // Guard proves only at head: historical read sees nothing, and head still hides `r`
    // because the triple mask hides `urn:secret:tag` at head.
    let policy_head = mask_later(at_head);
    let hist_denied = evidence::find_dependents_at(
        &view_v1,
        std::slice::from_ref(&c.0),
        false,
        Some(policy_head.clone()),
        &Control::none(),
    )
    .unwrap();
    assert!(hist_denied.reports.is_empty());

    let head_masked = evidence::find_dependents(
        &ds,
        &ds,
        std::slice::from_ref(&c.0),
        false,
        Some(policy_head),
        &Control::none(),
    )
    .unwrap();
    assert!(head_masked.reports.is_empty());
}

/// Combined graph restrictions and triple masks at `find_dependents` and
/// `find_dependents_at` must not decode unreadable 140 KiB report graph IRIs into a
/// `BudgetExceeded(Memory)` refusal during mask preparation, while still masking hidden
/// triples in both readable proposal graphs and admitted report graphs.
#[test]
fn combined_graph_restriction_and_triple_mask_skip_oversized_unreadable_graph() {
    use sparkles::access::{Decider, triples};

    let ds = Dataset::memory();
    let prep = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    ds.store()
        .activate_memory_configuration_history(&prep)
        .unwrap();
    evidence::configure(&ds, &settings(), &Control::none()).unwrap();

    let c = claim(&ds, "advisory");
    let a_ok = assess_in(&ds, CLAIMS, "a-visible", &c.0, &[]);
    let a_secret = assess(&ds, "a-secret", &c.0, &[]);
    add_report(
        &ds,
        REPORTS,
        "urn:report:ok",
        Some("urn:comm:1"),
        &[c.0.as_str()],
    );
    add_report(
        &ds,
        REPORTS,
        "urn:report:secret",
        Some("urn:comm:2"),
        &[c.0.as_str()],
    );

    let huge_unreadable = format!("urn:x-sparkles:graphrag:hidden-{}", "x".repeat(140 * 1024));
    ds.update(&format!(
        "INSERT DATA {{ \
            GRAPH <{REPORTS}> {{ \
                <urn:report:secret> <urn:secret:tag> \"hidden\" . \
            }} \
            GRAPH <{huge_unreadable}> {{ \
                <{c_iri}> <{RDF_TYPE}> <{GR}Report> . \
                <urn:hidden:rep> <{RDF_TYPE}> <{GR}Report> ; \
                    <urn:x-sparkles:evidence:conclusion> \"hidden\" ; \
                    <urn:secret:tag> \"hidden\" . \
                <urn:hidden:act> <{RDF_TYPE}> <{PROV_ACTIVITY}> ; <{PROV_USED}> <{c_iri}> . \
            }} \
        }}",
        c_iri = c.0.as_str(),
    ))
    .unwrap();

    let seq = ds.store().head_commit().seq;
    let view = ds
        .store()
        .evidence_view(&sparkles::history::At::Commit(seq), &prep)
        .unwrap();

    let combined_mask_rules = || triples::TripleRules {
        rules: vec![
            triples::Rule {
                protection: Arc::new(triples::Protection {
                    name: "hide-analyses-conclusion".into(),
                    predicates: Some(vec!["urn:x-sparkles:evidence:conclusion".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: None,
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::Only(GraphRule::new([CLAIMS], &[])),
                write: Graphs::none(),
            },
            triples::Rule {
                protection: Arc::new(triples::Protection {
                    name: "hide-secret-tag".into(),
                    predicates: Some(vec!["urn:secret:tag".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: None,
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            },
        ],
        caller: triples::Caller::default(),
        limits: triples::Limits::default(),
    };

    // 1. Exact graph allowlist + triple mask covering predicates present in
    //    `huge_unreadable` and `ANALYSES`. Both head and historical lookups succeed,
    //    hide `a_secret` in the readable proposal graph, and ignore `huge_unreadable`.
    let narrow_masked = Arc::new(GraphAccess::with_triples(
        Graphs::Only(GraphRule::new([CLAIMS, ANALYSES], &[])),
        Graphs::none(),
        combined_mask_rules(),
    ));
    for res in [
        evidence::find_dependents(
            &ds,
            &ds,
            std::slice::from_ref(&c.0),
            false,
            Some(narrow_masked.clone()),
            &Control::none(),
        )
        .unwrap(),
        evidence::find_dependents_at(
            &view,
            std::slice::from_ref(&c.0),
            false,
            Some(narrow_masked),
            &Control::none(),
        )
        .unwrap(),
    ] {
        assert_eq!(res.dependents.len(), 1);
        assert_eq!(res.dependents[0].assessment, a_ok);
        assert!(res.reports.is_empty());
    }

    // 2. Guarded wildcard policy + triple mask: both readable proposal graphs and
    //    admitted report graphs stay masked, and `huge_unreadable` does not trigger a
    //    memory refusal during mask preparation.
    let guarded_masked = Arc::new(GraphAccess::with_triples(
        Graphs::All.guarded_by(
            "urn:x-sparkles:graphrag:*",
            Decider(Arc::new(FixedGuard(vec![REPORTS.into()]))),
        ),
        Graphs::none(),
        combined_mask_rules(),
    ));
    for res in [
        evidence::find_dependents(
            &ds,
            &ds,
            std::slice::from_ref(&c.0),
            false,
            Some(guarded_masked.clone()),
            &Control::none(),
        )
        .unwrap(),
        evidence::find_dependents_at(
            &view,
            std::slice::from_ref(&c.0),
            false,
            Some(guarded_masked),
            &Control::none(),
        )
        .unwrap(),
    ] {
        assert_eq!(res.dependents.len(), 1);
        assert_eq!(res.dependents[0].assessment, a_ok);
        assert_eq!(res.reports.len(), 1);
        assert_eq!(res.reports[0].report.as_str(), "urn:report:ok");
        assert_eq!(res.reports[0].graph.as_str(), REPORTS);
    }

    // 3. Class-based triple mask on `gr:Report` (present in `huge_unreadable`) combined
    //    with the narrow allowlist also succeeds at both head and historical entry points.
    let report_class_masked = Arc::new(GraphAccess::with_triples(
        Graphs::Only(GraphRule::new([CLAIMS, ANALYSES], &[])),
        Graphs::none(),
        triples::TripleRules {
            rules: vec![triples::Rule {
                protection: Arc::new(triples::Protection {
                    name: "hide-reports".into(),
                    predicates: None,
                    classes: Some(vec![format!("{GR}Report")]),
                    subclasses: false,
                    graphs: None,
                    pattern: None,
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: triples::Caller::default(),
            limits: triples::Limits::default(),
        },
    ));
    for res in [
        evidence::find_dependents(
            &ds,
            &ds,
            std::slice::from_ref(&c.0),
            false,
            Some(report_class_masked.clone()),
            &Control::none(),
        )
        .unwrap(),
        evidence::find_dependents_at(
            &view,
            std::slice::from_ref(&c.0),
            false,
            Some(report_class_masked),
            &Control::none(),
        )
        .unwrap(),
    ] {
        let found: Vec<_> = res
            .dependents
            .iter()
            .map(|d| d.assessment.clone())
            .collect();
        assert!(found.contains(&a_ok));
        assert!(found.contains(&a_secret));
        assert_eq!(res.dependents.len(), 2);
        assert!(res.reports.is_empty());
    }
}

/// Combined graph restrictions and pattern-based triple masks at both `find_dependents`
/// and `find_dependents_at` must not retain denied report graph IRIs before evaluating
/// protection patterns, while preserving C12b cross-graph pattern semantics across
/// readable proposal graphs and admitted report graphs.
#[test]
fn combined_graph_restriction_and_pattern_mask_bound_denied_report_graphs() {
    use sparkles::access::{Decider, triples};

    let ds = Dataset::memory();
    let prep = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    ds.store()
        .activate_memory_configuration_history(&prep)
        .unwrap();
    evidence::configure(&ds, &settings(), &Control::none()).unwrap();

    let c = claim(&ds, "advisory");
    let a_ok = assess_in(&ds, CLAIMS, "a-visible", &c.0, &[]);
    let a_secret = assess(&ds, "a-secret", &c.0, &[]);
    add_report(
        &ds,
        REPORTS,
        "urn:report:ok",
        Some("urn:comm:1"),
        &[c.0.as_str()],
    );
    add_report(
        &ds,
        REPORTS,
        "urn:report:secret",
        Some("urn:comm:2"),
        &[c.0.as_str()],
    );

    let witness = "urn:x-sparkles:witness";
    let mut insert = format!(
        "INSERT DATA {{\n\
            GRAPH <{REPORTS}> {{\n\
                <urn:report:ok> <urn:secret:tag> \"guarded\" .\n\
                <urn:report:secret> <urn:secret:tag> \"guarded\" .\n\
            }}\n\
            GRAPH <{witness}> {{\n\
                <{a_ok}> <urn:visible-to> \"alice\" .\n\
                <urn:report:ok> <urn:visible-to> \"alice\" .\n\
                <{witness}> <urn:active> \"true\" .\n\
            }}\n",
        a_ok = a_ok.as_str(),
    );
    // 125 denied report graphs * 140 KiB = ~17.5 MiB of graph IRIs, exceeding the
    // 16 MiB dependents budget if pattern preparation retained denied graph names.
    let pad = "p".repeat(140 * 1024);
    for i in 0..125 {
        insert.push_str(&format!(
            "GRAPH <urn:x-sparkles:graphrag:denied-{i:03}-{pad}> {{\n\
                 <urn:hidden:rep:{i}> <{RDF_TYPE}> <{GR}Report> ; \
                     <urn:only-denied:tag> \"x\" ; \
                     <urn:secret:tag> \"x\" .\n\
             }}\n"
        ));
    }
    insert.push('}');
    ds.update(&insert).unwrap();

    let seq = ds.store().head_commit().seq;
    let view = ds
        .store()
        .evidence_view(&sparkles::history::At::Commit(seq), &prep)
        .unwrap();

    // 1. Narrow exact allowlist + `FILTER(false)` pattern on a predicate that only
    //    exists in denied graphs: skips pattern evaluation and succeeds at both entry points.
    let skip_policy = Arc::new(GraphAccess::with_triples(
        Graphs::Only(GraphRule::new([CLAIMS, ANALYSES], &[])),
        Graphs::none(),
        triples::TripleRules {
            rules: vec![triples::Rule {
                protection: Arc::new(triples::Protection {
                    name: "denied-only-pattern".into(),
                    predicates: Some(vec!["urn:only-denied:tag".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: Some("FILTER(false)".into()),
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: triples::Caller::default(),
            limits: triples::Limits::default(),
        },
    ));
    for res in [
        evidence::find_dependents(
            &ds,
            &ds,
            std::slice::from_ref(&c.0),
            false,
            Some(skip_policy.clone()),
            &Control::none(),
        )
        .unwrap(),
        evidence::find_dependents_at(
            &view,
            std::slice::from_ref(&c.0),
            false,
            Some(skip_policy),
            &Control::none(),
        )
        .unwrap(),
    ] {
        let found: Vec<_> = res
            .dependents
            .iter()
            .map(|d| d.assessment.clone())
            .collect();
        assert_eq!(res.dependents.len(), 2);
        assert!(found.contains(&a_ok));
        assert!(found.contains(&a_secret));
        assert!(res.reports.is_empty());
    }

    // 2. Active cross-graph pattern protections under both narrow exact allowlist and
    //    guarded wildcard policy: witness triples live in `<witness>` (outside the
    //    caller's allowlist) and are read through the unmasked default dataset + `GRAPH`.
    let pattern_rules = || triples::TripleRules {
        rules: vec![
            triples::Rule {
                protection: Arc::new(triples::Protection {
                    name: "pattern-conclusion".into(),
                    predicates: Some(vec!["urn:x-sparkles:evidence:conclusion".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: Some(format!(
                        "?s <urn:visible-to> ?user . GRAPH <{witness}> {{ <{witness}> <urn:active> \"true\" }}"
                    )),
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            },
            triples::Rule {
                protection: Arc::new(triples::Protection {
                    name: "pattern-secret-tag".into(),
                    predicates: Some(vec!["urn:secret:tag".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: Some(format!(
                        "?s <urn:visible-to> ?user . GRAPH <{witness}> {{ <{witness}> <urn:active> \"true\" }}"
                    )),
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            },
        ],
        caller: triples::Caller {
            user: Some("alice".into()),
            ..triples::Caller::default()
        },
        limits: triples::Limits::default(),
    };

    let exact_pattern = Arc::new(GraphAccess::with_triples(
        Graphs::Only(GraphRule::new([CLAIMS, ANALYSES], &[])),
        Graphs::none(),
        pattern_rules(),
    ));
    for res in [
        evidence::find_dependents(
            &ds,
            &ds,
            std::slice::from_ref(&c.0),
            false,
            Some(exact_pattern.clone()),
            &Control::none(),
        )
        .unwrap(),
        evidence::find_dependents_at(
            &view,
            std::slice::from_ref(&c.0),
            false,
            Some(exact_pattern),
            &Control::none(),
        )
        .unwrap(),
    ] {
        assert_eq!(res.dependents.len(), 1);
        assert_eq!(res.dependents[0].assessment, a_ok);
        assert!(res.reports.is_empty());
    }

    let guarded_pattern = Arc::new(GraphAccess::with_triples(
        Graphs::All.guarded_by(
            "urn:x-sparkles:graphrag:*",
            Decider(Arc::new(FixedGuard(vec![REPORTS.into()]))),
        ),
        Graphs::none(),
        pattern_rules(),
    ));
    for res in [
        evidence::find_dependents(
            &ds,
            &ds,
            std::slice::from_ref(&c.0),
            false,
            Some(guarded_pattern.clone()),
            &Control::none(),
        )
        .unwrap(),
        evidence::find_dependents_at(
            &view,
            std::slice::from_ref(&c.0),
            false,
            Some(guarded_pattern),
            &Control::none(),
        )
        .unwrap(),
    ] {
        assert_eq!(res.dependents.len(), 1);
        assert_eq!(res.dependents[0].assessment, a_ok);
        assert_eq!(res.reports.len(), 1);
        assert_eq!(res.reports[0].report.as_str(), "urn:report:ok");
        assert_eq!(res.reports[0].graph.as_str(), REPORTS);
    }
}
