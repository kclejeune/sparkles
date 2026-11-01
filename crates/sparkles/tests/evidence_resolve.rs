//! Retained-reference and record resolution (spec C25 §3, §7 and §8): references
//! resolve by dataset and branch identity across a catalog, access is decided on
//! current names before a read, and records resolve only as authenticated profile
//! records of the protected evidence graphs.

use oxrdf::{NamedNode, Triple};
use sparkles::access::GraphAccess;
use sparkles::evidence::resolve::{
    GraphIdentity, RecordKind, RecordRequest, Reference, Resolution, StoredFactRef, Verified,
};
use sparkles::evidence::{self, Settings};
use sparkles::managed::OwnerKind;
use sparkles::task::Control;
use sparkles::{Catalog, Dataset};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

const SOURCES: &str = "urn:ev:sources";
const CLAIMS: &str = "urn:ev:claims";
const EV: &str = "urn:x-sparkles:evidence:";

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn all() -> Option<Arc<GraphAccess>> {
    Some(Arc::new(GraphAccess::all()))
}

#[track_caller]
fn code<T: std::fmt::Debug>(r: sparkles::Result<T>) -> String {
    match r {
        Err(e) => e.code().to_string(),
        Ok(v) => panic!("verified: {v:?}"),
    }
}

#[track_caller]
fn message<T: std::fmt::Debug>(r: sparkles::Result<T>) -> String {
    match r {
        Err(e) => e.to_string(),
        Ok(v) => panic!("verified: {v:?}"),
    }
}

fn ab() -> Triple {
    Triple::new(iri("urn:a"), iri("urn:p"), iri("urn:b"))
}

fn fact(ds: &Dataset, branch: Uuid, commit: u64) -> StoredFactRef {
    StoredFactRef {
        dataset: ds.dataset_id(),
        branch,
        commit,
        graph: GraphIdentity::Default,
        expected: ab(),
        anchor: None,
    }
}

#[test]
fn stored_facts_resolve_by_dataset_and_branch_identity_across_renames() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
    let home = catalog.create("home", &Default::default()).unwrap();
    let other = catalog.create("other", &Default::default()).unwrap();
    other
        .update("INSERT DATA { <urn:a> <urn:p> <urn:b> }")
        .unwrap();
    let dev = other.create_branch("dev", &Default::default()).unwrap();
    let commit = other.store().head_commit().seq;
    let reference = fact(&other, other.dataset_id(), commit);
    let on_dev = fact(&other, dev.id, commit);
    drop(other);

    let mut asked = Vec::new();
    let mut resolution = Resolution::new(Control::none()).unwrap();
    let v = resolution
        .stored_fact("r", &catalog, &reference, |ds, branch| {
            asked.push((ds.name().map(str::to_string), branch.to_string()));
            all()
        })
        .unwrap();
    assert_eq!(v.commit, commit);
    resolution
        .stored_fact("r", &catalog, &on_dev, |ds, branch| {
            asked.push((ds.name().map(str::to_string), branch.to_string()));
            all()
        })
        .unwrap();
    assert_eq!(
        asked,
        [
            (Some("other".into()), "main".into()),
            (Some("other".into()), "dev".into())
        ]
    );

    // renames keep the identity, and access is decided on the current names
    catalog.rename("other", "moved").unwrap();
    let moved = catalog.get("moved").unwrap();
    moved.rename_branch("dev", "feature").unwrap();
    drop(moved);
    let mut asked = Vec::new();
    let mut resolution = Resolution::new(Control::none()).unwrap();
    resolution
        .stored_fact("r", &catalog, &on_dev, |ds, branch| {
            asked.push((ds.name().map(str::to_string), branch.to_string()));
            all()
        })
        .unwrap();
    assert_eq!(asked, [(Some("moved".into()), "feature".into())]);

    // a refusal and a missing dataset answer alike, without asking about the latter
    let denied = message(resolution.stored_fact("r", &catalog, &reference, |_, _| None));
    let mut gone = reference.clone();
    gone.dataset = Uuid::new_v4();
    let missing = message(resolution.stored_fact("r", &catalog, &gone, |_, _| {
        panic!("asked about a dataset that does not exist")
    }));
    assert_eq!(denied, missing);
    assert_eq!(missing, "evidence reference `r` is unavailable");

    // a recreated name never satisfies the old identity
    catalog.delete("moved").unwrap();
    let again = catalog.create("moved", &Default::default()).unwrap();
    again
        .update("INSERT DATA { <urn:a> <urn:p> <urn:b> }")
        .unwrap();
    assert_eq!(
        code(resolution.stored_fact("r", &catalog, &reference, |_, _| all())),
        "evidence-unavailable"
    );
    // `all` resolves facts of the home dataset without a catalog, and others with one
    let refs = vec![(
        "x".to_string(),
        Reference::StoredFact(fact(
            &again,
            again.dataset_id(),
            again.store().head_commit().seq,
        )),
    )];
    let mut resolution = Resolution::new(Control::none()).unwrap();
    let out = resolution
        .all(Some(&catalog), &home, &home, &refs, &mut |_, _| all())
        .unwrap();
    assert!(matches!(&out[..], [Verified::Fact(_)]));
    let mut resolution = Resolution::new(Control::none()).unwrap();
    assert_eq!(
        code(resolution.all(None, &home, &home, &refs, &mut |_, _| all())),
        "evidence-unavailable"
    );
}

fn configured() -> Dataset {
    let ds = Dataset::memory();
    let settings = Settings {
        enabled: true,
        source_records: vec![SOURCES.into()],
        proposals: vec![CLAIMS.into()],
        ..Settings::default()
    };
    evidence::configure(&ds, &settings, &Control::none()).unwrap();
    ds
}

/// Publish `trig` through the evidence owner's typed writer. `{receipt}` in the text is
/// replaced by a receipt naming this dataset, its branch and the commit being made.
fn typed(ds: &Dataset, trig: &str) -> u64 {
    let store = ds.store();
    let mut t = store.write();
    t.set_managed(
        store
            .managed_writer(OwnerKind::Evidence, Uuid::new_v4())
            .unwrap(),
    )
    .unwrap();
    let (seq, _) = t.managed_stamp().unwrap();
    let receipt = format!(
        "ev:datasetId \"{}\" ; ev:branchId \"{}\" ; ev:commit {seq}",
        ds.dataset_id(),
        store.branch_id()
    );
    let text = format!(
        "PREFIX ev: <{EV}>\nPREFIX oa: <http://www.w3.org/ns/oa#>\n{}",
        trig.replace("{receipt}", &receipt)
    );
    let mut labels = HashMap::new();
    for quad in oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::TriG).for_slice(text.as_bytes())
    {
        let q = t.encode_quad(&quad.unwrap(), &mut labels).unwrap();
        t.insert(q).unwrap();
    }
    t.commit().unwrap();
    seq
}

fn sha(text: &str) -> String {
    sparkles::evidence::Sha256Digest::of_bytes(text.as_bytes(), &Control::none())
        .unwrap()
        .to_string()
}

fn request(record: &str, kinds: &[RecordKind]) -> RecordRequest {
    RecordRequest {
        iri: iri(record),
        kinds: kinds.to_vec(),
        content_digest: None,
    }
}

fn record(
    ds: &Dataset,
    req: &RecordRequest,
) -> sparkles::Result<sparkles::evidence::resolve::VerifiedRecord> {
    Resolution::new(Control::none())
        .unwrap()
        .record("r", ds, ds, all(), req)
}

#[test]
fn records_resolve_only_as_authenticated_profile_records() {
    let ds = configured();
    let digest = sha("source bytes");
    let commit = typed(
        &ds,
        &format!(
            "GRAPH <{SOURCES}> {{
               <urn:rev> a ev:SourceRevision ; ev:contentDigest \"{digest}\" ;
                 ev:availabilityReceipt <urn:receipt> .
               <urn:receipt> {{receipt}} .
             }}"
        ),
    );
    let v = record(&ds, &request("urn:rev", &[RecordKind::SourceRevision])).unwrap();
    assert_eq!(v.kind, RecordKind::SourceRevision);
    assert_eq!(v.graph.as_str(), SOURCES);
    assert_eq!(
        (v.receipt.dataset, v.receipt.commit),
        (ds.dataset_id(), commit)
    );
    assert_eq!(v.content_digest.unwrap().to_string(), digest);
    // any kind is accepted when none is named
    record(&ds, &request("urn:rev", &[])).unwrap();
    // another kind or another digest conflicts with the stored record
    assert_eq!(
        code(record(&ds, &request("urn:rev", &[RecordKind::Claim]))),
        "immutable-record-conflict"
    );
    let mut other_digest = request("urn:rev", &[]);
    other_digest.content_digest = Some(sha("other").parse().unwrap());
    assert_eq!(
        code(record(&ds, &other_digest)),
        "immutable-record-conflict"
    );
    // a caller that cannot read the graph gets the answer of a missing record
    let claims_only = Arc::new(GraphAccess::graphs(
        sparkles::access::Graphs::Only(sparkles::access::GraphRule::new([CLAIMS], &[])),
        sparkles::access::Graphs::none(),
    ));
    let hidden = message(Resolution::new(Control::none()).unwrap().record(
        "r",
        &ds,
        &ds,
        Some(claims_only),
        &request("urn:rev", &[]),
    ));
    let missing = message(record(&ds, &request("urn:missing", &[])));
    let denied = message(Resolution::new(Control::none()).unwrap().record(
        "r",
        &ds,
        &ds,
        None,
        &request("urn:rev", &[]),
    ));
    assert_eq!(hidden, missing);
    assert_eq!(denied, missing);
}

#[test]
fn unauthenticated_copies_and_foreign_receipts_do_not_resolve() {
    let ds = configured();
    // an ordinary graph's copy is not a profile record
    ds.update(&format!(
        "PREFIX ev: <{EV}> INSERT DATA {{ GRAPH <urn:plain> {{ <urn:copy> a ev:SourceRevision }} }}"
    ))
    .unwrap();
    assert_eq!(
        code(record(&ds, &request("urn:copy", &[]))),
        "evidence-unavailable"
    );
    // a record without a receipt, or with a receipt of another dataset
    typed(
        &ds,
        &format!(
            "GRAPH <{SOURCES}> {{
               <urn:bare> a ev:SourceRevision .
               <urn:foreign> a ev:SourceRevision ; ev:availabilityReceipt <urn:fr> .
               <urn:fr> ev:datasetId \"{}\" ; ev:branchId \"{}\" ; ev:commit 1 .
               <urn:twice> a ev:SourceRevision ; ev:availabilityReceipt <urn:t1> .
               <urn:t1> {{receipt}} .
             }}
             GRAPH <{CLAIMS}> {{
               <urn:twice> a ev:SourceRevision ; ev:availabilityReceipt <urn:t1> .
               <urn:t1> {{receipt}} .
             }}",
            Uuid::new_v4(),
            Uuid::new_v4()
        ),
    );
    assert_eq!(
        code(record(&ds, &request("urn:bare", &[]))),
        "origin-not-reproduced"
    );
    assert_eq!(
        code(record(&ds, &request("urn:foreign", &[]))),
        "origin-not-reproduced"
    );
    assert_eq!(
        code(record(&ds, &request("urn:twice", &[]))),
        "immutable-record-conflict"
    );
    // a detached graph is no longer protected, so its records do not resolve
    typed(
        &ds,
        &format!(
            "GRAPH <{CLAIMS}> {{ <urn:claim> a ev:Claim ; ev:availabilityReceipt <urn:c1> .
               <urn:c1> {{receipt}} . }}"
        ),
    );
    record(&ds, &request("urn:claim", &[RecordKind::Claim])).unwrap();
    evidence::detach(&ds, CLAIMS).unwrap();
    assert_eq!(
        code(record(&ds, &request("urn:claim", &[]))),
        "evidence-unavailable"
    );
}

#[test]
fn passages_verify_their_selection_against_the_retained_rendition() {
    let ds = configured();
    let text = "naïve café 🦀 text";
    let selected: String = text.chars().skip(6).take(4).collect();
    assert_eq!(selected, "café");
    typed(
        &ds,
        &format!(
            "GRAPH <{SOURCES}> {{
               <urn:rendition> a ev:Rendition ; ev:normalizedText \"{text}\" ;
                 ev:availabilityReceipt <urn:r0> .
               <urn:r0> {{receipt}} .
               <urn:good> a ev:Passage ; oa:hasSource <urn:rendition> ;
                 oa:hasSelector [ a oa:TextPositionSelector ; oa:start 6 ; oa:end 10 ] ;
                 ev:textDigest \"{good}\" ; ev:availabilityReceipt <urn:r1> .
               <urn:r1> {{receipt}} .
               <urn:wrong> a ev:Passage ; oa:hasSource <urn:rendition> ;
                 oa:hasSelector <urn:wrong-selector> ;
                 ev:textDigest \"{good}\" ; ev:availabilityReceipt <urn:r2> .
               <urn:wrong-selector> oa:start 5 ; oa:end 9 .
               <urn:r2> {{receipt}} .
               <urn:past> a ev:Passage ; oa:hasSource <urn:rendition> ;
                 oa:hasSelector [ oa:start 6 ; oa:end 400 ] ;
                 ev:textDigest \"{good}\" ; ev:availabilityReceipt <urn:r3> .
               <urn:r3> {{receipt}} .
               <urn:empty> a ev:Passage ; oa:hasSource <urn:rendition> ;
                 oa:hasSelector [ oa:start 3 ; oa:end 3 ] ;
                 ev:textDigest \"{empty}\" ; ev:availabilityReceipt <urn:r4> .
               <urn:r4> {{receipt}} .
             }}",
            good = sha(&selected),
            empty = sha(""),
        ),
    );
    let v = record(&ds, &request("urn:good", &[RecordKind::Passage])).unwrap();
    let span = v.span.unwrap();
    assert_eq!((span.start, span.end), (6, 10));
    assert_eq!(span.rendition.as_str(), "urn:rendition");
    record(&ds, &request("urn:empty", &[])).unwrap();
    assert_eq!(
        code(record(&ds, &request("urn:wrong", &[]))),
        "span-mismatch"
    );
    assert_eq!(
        code(record(&ds, &request("urn:past", &[]))),
        "span-mismatch"
    );
    // a rendition is a record too; its text is not taken from an unprotected copy
    assert!(record(&ds, &request("urn:rendition", &[RecordKind::Rendition])).is_ok());
}
