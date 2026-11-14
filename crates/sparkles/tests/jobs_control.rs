//! The job ledger lies in the dataset's control keyspace, outside the RDF dataset (spec
//! C26 §2.1 and §2.3). A15: control-only and control-carrying transactions that
//! interleave with ordinary commits leave history, the change feed, commit numbers and
//! counts, export, a compaction and a branch's base as a store given only the business
//! writes has them.

use oxrdf::{GraphName, Literal, NamedNode, Quad};
use sparkles::Dataset;
use sparkles::commit::CommitRange;
use sparkles::jobs::{Caller, Definition, Publication, Settings, Submission};

const G: &str = "urn:g";

fn enable(ds: &Dataset) {
    let mut s = Settings {
        enabled: true,
        ..Default::default()
    };
    s.definitions
        .insert("collect".into(), Definition::quads(vec![G.into()]));
    ds.jobs().configure(&s).unwrap();
}

fn quad(k: u32) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("urn:s{k}")),
        NamedNode::new_unchecked("urn:p"),
        Literal::new_simple_literal(format!("v{k}")),
        GraphName::NamedNode(NamedNode::new_unchecked(G)),
    )
}

fn business(ds: &Dataset, k: u32) {
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{G}> {{ <urn:b{k}> <urn:p> {k} }} }}"
    ))
    .unwrap();
}

/// The business writes, with a job's transitions between them on `jobs`.
fn workload(ds: &Dataset, jobs: bool) {
    let admin = Caller::admin(NamedNode::new_unchecked("urn:x-sparkles:principal:ops"));
    let w = Caller::user(NamedNode::new_unchecked("urn:x-sparkles:principal:w"));
    business(ds, 1);
    if jobs {
        enable(ds);
        for k in 0..3 {
            ds.jobs()
                .submit(
                    &Submission::new("collect", serde_json::json!({ "k": k }), &format!("k{k}")),
                    &admin,
                )
                .unwrap();
        }
    }
    business(ds, 2);
    if jobs {
        let c = ds.jobs().claim(&[], &w).unwrap().unwrap();
        ds.jobs().renew(c.run, &c.token, None, None, &w).unwrap();
        let mut p = Publication::new(uuid::Uuid::new_v4());
        p.insert.push(quad(1));
        ds.jobs().publish(c.run, &c.token, &p, &w).unwrap();
        // a publication with no quads makes no commit
        let c = ds.jobs().claim(&[], &w).unwrap().unwrap();
        let p = Publication::new(uuid::Uuid::new_v4());
        ds.jobs().publish(c.run, &c.token, &p, &w).unwrap();
        let c = ds.jobs().claim(&[], &w).unwrap().unwrap();
        ds.jobs().cancel(c.run, &admin).unwrap();
    } else {
        ds.update(&format!(
            "INSERT DATA {{ GRAPH <{G}> {{ <urn:s1> <urn:p> \"v1\" }} }}"
        ))
        .unwrap();
    }
    business(ds, 3);
}

/// The change feed's commits and counts, `None` when a compaction made its start
/// unreconstructable.
type Feed = Option<Vec<(u64, u64, u64)>>;

/// Each commit's number and counts, the change feed and the sorted export.
type Observed = (Vec<(u64, u64, u64, u64)>, Feed, Vec<String>);

/// What A15 compares: each commit's number, kind and counts, the change feed's counts
/// and the dataset's export.
fn observed(ds: &Dataset) -> Observed {
    let mut commits: Vec<_> = ds
        .history()
        .commits(CommitRange::Latest, 1000)
        .commits
        .iter()
        .map(|c| (c.seq, c.inserted, c.deleted, c.quads))
        .collect();
    commits.sort();
    let changes = ds.history().changes(0, &Default::default()).ok().map(|f| {
        f.commits
            .iter()
            .map(|c| (c.commit.seq, c.added, c.removed))
            .collect()
    });
    let mut out = Vec::new();
    ds.dump(&mut out, oxrdfio::RdfFormat::NQuads).unwrap();
    let mut lines: Vec<String> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    (commits, changes, lines)
}

#[test]
fn a15_control_transactions_leave_every_business_surface_unchanged() {
    let (with, without) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = Dataset::open(with.path()).unwrap();
    let b = Dataset::open(without.path()).unwrap();
    workload(&a, true);
    workload(&b, false);
    assert!(observed(&a).1.is_some());
    assert_eq!(observed(&a), observed(&b));
    assert_eq!(a.store().head_commit().seq, b.store().head_commit().seq);
    // a compaction and a reopen keep them equal
    a.compact().unwrap();
    b.compact().unwrap();
    assert_eq!(observed(&a), observed(&b));
    drop((a, b));
    let a = Dataset::open(with.path()).unwrap();
    let b = Dataset::open(without.path()).unwrap();
    assert_eq!(observed(&a), observed(&b));
    // a branch's base holds the business quads alone
    for ds in [&a, &b] {
        ds.create_branch("side", &Default::default()).unwrap();
    }
    assert_eq!(
        observed(&a.branch("side").unwrap()).2,
        observed(&b.branch("side").unwrap()).2
    );
    // the ledger is still there for its typed inspection
    let runs = a
        .jobs()
        .list(
            &Default::default(),
            &Caller::admin(NamedNode::new_unchecked("urn:x")),
        )
        .unwrap();
    assert_eq!(runs.len(), 3);
}
