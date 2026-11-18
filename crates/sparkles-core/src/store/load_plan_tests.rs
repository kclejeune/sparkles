//! The plan each entry point chooses, at every boundary of its rule. A change to the rule
//! shows here as a changed row.

use super::{BYTES_PER_QUAD, Entry, LoadInputs, LoadPlan, SMALL_STREAM_BYTES};
use crate::access::{GraphAccess, GraphRule, Graphs};
use crate::commit::CommitKind;
use crate::guard::WriteOptions;
use crate::id::Id;
use crate::io::Source;
use crate::store::{PatchOptions, ReplaceTarget, Store, StoreOptions};
use oxrdf::NamedNode;
use oxrdfio::RdfFormat;
use std::sync::Arc;

use Entry::{InsertBulk, Load, Patch, Replace};
use LoadPlan::{Bulk, Transactional as Txn};

/// The threshold of the table.
const T: u64 = 1_000;

/// One row: the inputs, the decompressed bytes of the streams among the sources, the
/// plan, and the byte limits the probe was asked about, in order.
struct Row {
    name: &'static str,
    inputs: LoadInputs,
    stream_bytes: u64,
    plan: LoadPlan,
    probes: &'static [u64],
}

/// Inputs of `entry` with `quads` from files, or a batch of `quads`.
fn sized(entry: Entry, quads: u64) -> LoadInputs {
    let files = matches!(entry, Load | Replace) as usize;
    LoadInputs {
        quads,
        sources: files,
        ..LoadInputs::new(entry, T)
    }
}

/// Inputs of `entry` with one stream and nothing else.
fn stream(entry: Entry) -> LoadInputs {
    LoadInputs {
        sources: 1,
        streams: 1,
        ..LoadInputs::new(entry, T)
    }
}

fn empty(i: LoadInputs) -> LoadInputs {
    LoadInputs {
        empty_head: true,
        ..i
    }
}

fn restricted(i: LoadInputs) -> LoadInputs {
    LoadInputs {
        restricted: true,
        ..i
    }
}

/// Inputs of `entry` with a small file and a stream.
fn mixed(entry: Entry) -> LoadInputs {
    LoadInputs {
        quads: 1,
        sources: 2,
        streams: 1,
        ..LoadInputs::new(entry, T)
    }
}

/// A write limited to some graphs whose triple protections limit writes.
fn protected(i: LoadInputs) -> LoadInputs {
    LoadInputs {
        triple_checks: true,
        ..restricted(i)
    }
}

const SMALL: u64 = SMALL_STREAM_BYTES;
const LARGE: u64 = T * BYTES_PER_QUAD;

/// The table: a name, the inputs, the bytes of the streams, the plan and the probes.
#[rustfmt::skip]
fn rows() -> Vec<Row> {
    let row = |name, inputs, stream_bytes, plan, probes| Row { name, inputs, stream_bytes, plan, probes };
    vec![
        // load_with
        row("load: empty head, a small file", empty(sized(Load, 1)), 0, Bulk, &[]),
        row("load: empty head, no sources", empty(LoadInputs::new(Load, T)), 0, Bulk, &[]),
        row("load: empty head, stream at the small bound", empty(stream(Load)), SMALL, Txn, &[SMALL]),
        row("load: empty head, stream above the small bound", empty(stream(Load)), SMALL + 1, Bulk, &[SMALL]),
        row("load: empty head, stream above the large bound", empty(stream(Load)), LARGE + 1, Txn, &[SMALL]),
        row("load: empty head, a file and a stream", empty(mixed(Load)), 1, Bulk, &[]),
        row("load: estimate at the threshold", sized(Load, T), 0, Txn, &[]),
        row("load: estimate above the threshold", sized(Load, T + 1), 0, Bulk, &[]),
        row("load: stream at the large bound", stream(Load), LARGE, Txn, &[LARGE]),
        row("load: stream above the large bound", stream(Load), LARGE + 1, Bulk, &[LARGE]),
        row("load: a small file and a large stream", mixed(Load), LARGE + 1, Bulk, &[LARGE]),
        row("load: restricted, empty head", restricted(empty(sized(Load, 1))), 0, Txn, &[]),
        row("load: restricted, above the threshold", restricted(sized(Load, T + 1)), 0, Txn, &[]),
        row("load: restricted, empty head, large stream", restricted(empty(stream(Load))), SMALL + 1, Txn, &[]),
        // replace_with
        row("replace: empty head, a small file", empty(sized(Replace, 1)), 0, Bulk, &[]),
        row("replace: empty head, no sources", empty(LoadInputs::new(Replace, T)), 0, Bulk, &[]),
        row("replace: empty head, a small stream", empty(stream(Replace)), 1, Txn, &[SMALL]),
        row("replace: empty head, stream above the large bound", empty(stream(Replace)), LARGE + 1, Txn, &[SMALL]),
        row("replace: empty head, stream above the small bound", empty(stream(Replace)), SMALL + 1, Bulk, &[SMALL]),
        row("replace: estimate at the threshold", sized(Replace, T), 0, Txn, &[]),
        row("replace: estimate above the threshold", sized(Replace, T + 1), 0, Bulk, &[]),
        row("replace: stream at the large bound", stream(Replace), LARGE, Txn, &[LARGE]),
        row("replace: stream above the large bound", stream(Replace), LARGE + 1, Bulk, &[LARGE]),
        row("replace: a small file and a large stream", mixed(Replace), LARGE + 1, Bulk, &[LARGE]),
        row("replace: restricted, above the threshold", restricted(sized(Replace, T + 1)), 0, Txn, &[]),
        row("replace: restricted, large stream", restricted(stream(Replace)), LARGE + 1, Txn, &[]),
        // insert_bulk
        row("insert_bulk: below the threshold", sized(InsertBulk, T - 1), 0, Txn, &[]),
        row("insert_bulk: at the threshold", sized(InsertBulk, T), 0, Txn, &[]),
        row("insert_bulk: above the threshold", sized(InsertBulk, T + 1), 0, Bulk, &[]),
        row("insert_bulk: empty head, small batch", empty(sized(InsertBulk, 1)), 0, Txn, &[]),
        row("insert_bulk: restricted", restricted(sized(InsertBulk, T + 1)), 0, Bulk, &[]),
        row("insert_bulk: triple protections", protected(sized(InsertBulk, T + 1)), 0, Txn, &[]),
        row("insert_bulk: managed", LoadInputs { managed: true, ..sized(InsertBulk, T + 1) }, 0, Txn, &[]),
        row("insert_bulk: control records", LoadInputs { control: true, ..sized(InsertBulk, T + 1) }, 0, Txn, &[]),
        // patch_apply
        row("patch: below the threshold", sized(Patch, T - 1), 0, Txn, &[]),
        row("patch: at the threshold", sized(Patch, T), 0, Txn, &[]),
        row("patch: above the threshold", sized(Patch, T + 1), 0, Bulk, &[]),
        row("patch: a delete row, even one that removes nothing", LoadInputs { deleted: true, ..sized(Patch, T + 1) }, 0, Txn, &[]),
        row("patch: restricted", restricted(sized(Patch, T + 1)), 0, Bulk, &[]),
        row("patch: triple protections", protected(sized(Patch, T + 1)), 0, Txn, &[]),
        row("patch: managed", LoadInputs { managed: true, ..sized(Patch, T + 1) }, 0, Txn, &[]),
        row("patch: control records", LoadInputs { control: true, ..sized(Patch, T + 1) }, 0, Txn, &[]),
    ]
}

#[test]
fn plan_table() {
    let mut failures = Vec::new();
    for r in rows() {
        let mut probes = Vec::new();
        let plan = LoadPlan::choose(&r.inputs, |limit| {
            probes.push(limit);
            Ok(r.stream_bytes > limit)
        })
        .unwrap();
        if plan != r.plan || probes != r.probes {
            failures.push(format!(
                "{}: chose {plan:?} probing {probes:?}, expected {:?} probing {:?}",
                r.name, r.plan, r.probes
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// The entry points themselves, which must pass the inputs the table assumes. A bulk plan
// rebuilds the generation and leaves no delta, and a transactional one leaves its quads
// in the delta.

/// The threshold of the entry-point tests.
const E: u64 = 10;

fn store() -> Store {
    Store::in_memory(StoreOptions {
        bulk_threshold: E,
        ..StoreOptions::default()
    })
}

/// N-Triples of `n` triples numbered from `from`, each line padded to `BYTES_PER_QUAD`
/// bytes so that the estimate of a file of them is `n`.
fn ntriples(from: u64, n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    for i in from..from + n {
        let head = format!("<http://ex.org/s{i}> <http://ex.org/p> \"");
        let pad = BYTES_PER_QUAD as usize - head.len() - "\" .\n".len();
        out.extend_from_slice(head.as_bytes());
        out.extend(std::iter::repeat_n(b'x', pad));
        out.extend_from_slice(b"\" .\n");
    }
    out
}

fn bytes(from: u64, n: u64) -> Source {
    Source::from_bytes(ntriples(from, n), RdfFormat::NTriples, None)
}

fn reader(from: u64, n: u64) -> Source {
    Source::from_reader(
        std::io::Cursor::new(ntriples(from, n)),
        RdfFormat::NTriples,
        None,
    )
}

/// A store holding `n` quads in its delta, so that a bulk plan shows as an empty delta.
fn seeded(n: u64) -> Store {
    let s = store();
    let mut t = s.write();
    let p = t
        .intern(&oxrdf::Term::NamedNode(NamedNode::new_unchecked(
            "http://ex.org/seed",
        )))
        .unwrap();
    for i in 0..n {
        t.insert([
            Id::bnode(i),
            p,
            Id::from_i64(i as i64).unwrap(),
            Id::DEFAULT_GRAPH,
        ])
        .unwrap();
    }
    t.commit().unwrap();
    assert!(!s.snapshot().delta.is_empty());
    s
}

fn plan_of(s: &Store) -> LoadPlan {
    if s.snapshot().delta.is_empty() {
        Bulk
    } else {
        Txn
    }
}

fn restricted_opts() -> WriteOptions {
    WriteOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(
            Graphs::All,
            Graphs::Only(GraphRule::new(["default"], &[])),
        ))),
        ..Default::default()
    }
}

#[test]
fn load_with_plans() {
    let o = WriteOptions::default();
    let cases: Vec<(&str, Store, Source, LoadPlan)> = vec![
        ("empty head, small bytes", store(), bytes(0, 1), Bulk),
        ("empty head, small stream", store(), reader(0, 3), Txn),
        ("at the threshold", seeded(2), bytes(100, E), Txn),
        ("above the threshold", seeded(2), bytes(100, E + 1), Bulk),
        ("large stream", seeded(2), reader(100, 5 * E), Bulk),
    ];
    for (name, s, src, plan) in cases {
        s.load_with(&[src], CommitKind::Load, &o).unwrap();
        assert_eq!(plan_of(&s), plan, "{name}");
    }
    let s = store();
    s.load_with(&[bytes(0, 1)], CommitKind::Load, &restricted_opts())
        .unwrap();
    assert_eq!(plan_of(&s), Txn, "restricted, empty head");
}

#[test]
fn replace_with_plans() {
    let o = WriteOptions::default();
    let cases: Vec<(&str, Store, Source, LoadPlan)> = vec![
        ("empty head, small bytes", store(), bytes(0, 1), Bulk),
        ("at the threshold", seeded(2), bytes(100, E), Txn),
        ("above the threshold", seeded(2), bytes(100, E + 1), Bulk),
        ("stream at the large bound", seeded(2), reader(100, E), Txn),
        (
            "stream above the large bound",
            seeded(2),
            reader(100, E + 1),
            Bulk,
        ),
    ];
    for (name, s, src, plan) in cases {
        s.replace_with(ReplaceTarget::Default, &[src], CommitKind::GspPut, &o)
            .unwrap();
        assert_eq!(plan_of(&s), plan, "{name}");
    }
    // a restricted replace parses its sources and stages them through insert_bulk
    for (n, plan) in [(E, Txn), (E + 1, Bulk)] {
        let s = seeded(2);
        s.replace_with(
            ReplaceTarget::Default,
            &[bytes(100, n)],
            CommitKind::GspPut,
            &restricted_opts(),
        )
        .unwrap();
        assert_eq!(plan_of(&s), plan, "restricted, {n} quads");
    }
}

#[test]
fn insert_bulk_plans() {
    for (n, plan) in [(E - 1, Txn), (E, Txn), (E + 1, Bulk)] {
        let s = seeded(2);
        let mut t = s.write();
        let p = t
            .intern(&oxrdf::Term::NamedNode(NamedNode::new_unchecked(
                "http://ex.org/b",
            )))
            .unwrap();
        let quads = (0..n)
            .map(|i| {
                [
                    Id::bnode(1000 + i),
                    p,
                    Id::from_i64(i as i64).unwrap(),
                    Id::DEFAULT_GRAPH,
                ]
            })
            .collect();
        t.insert_bulk(quads).unwrap();
        t.commit().unwrap();
        assert_eq!(plan_of(&s), plan, "{n} quads");
    }
}

#[test]
fn patch_plans() {
    let adds = |n: u64| {
        (0..n)
            .map(|i| format!("A <http://ex.org/s{i}> <http://ex.org/p> \"{i}\" .\n"))
            .collect::<String>()
    };
    let cases = [
        ("below the threshold", adds(E - 1), Txn),
        ("at the threshold", adds(E), Txn),
        ("above the threshold", adds(E + 1), Bulk),
        (
            "a delete row that removes nothing",
            format!(
                "D <http://ex.org/none> <http://ex.org/p> \"0\" .\n{}",
                adds(E + 1)
            ),
            Txn,
        ),
    ];
    for (name, patch, plan) in cases {
        let s = seeded(2);
        let out = s
            .apply_patch(patch.as_bytes(), &PatchOptions::default())
            .unwrap();
        assert!(out.receipt.committed, "{name}");
        assert_eq!(plan_of(&s), plan, "{name}");
    }
}

/// Times a graph-restricted replace and a reader load into a store that holds data, each
/// with the default threshold and with every batch kept transactional, to measure what
/// the plan of a restricted batch and the probe of a large stream decide. Run it with
/// `LOAD_PLAN_QUADS=1000000 mbx test --release -p sparkles-core --lib -- --ignored
/// load_plan_timing --nocapture`.
#[test]
#[ignore]
fn load_plan_timing() {
    let n: u64 = std::env::var("LOAD_PLAN_QUADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000_000);
    let data = ntriples(0, n);
    for (label, threshold) in [
        ("default threshold", StoreOptions::default().bulk_threshold),
        ("transactional", u64::MAX),
    ] {
        let open = || {
            let s = Store::in_memory(StoreOptions {
                bulk_threshold: threshold,
                ..StoreOptions::default()
            });
            s.load(&[bytes(10 * n, 1000)]).unwrap();
            s
        };
        let s = open();
        let t = std::time::Instant::now();
        s.replace_with(
            ReplaceTarget::Default,
            &[Source::from_bytes(data.clone(), RdfFormat::NTriples, None)],
            CommitKind::GspPut,
            &restricted_opts(),
        )
        .unwrap();
        let replace = t.elapsed();
        let s = open();
        let t = std::time::Instant::now();
        s.load_with(
            &[Source::from_reader(
                std::io::Cursor::new(data.clone()),
                RdfFormat::NTriples,
                None,
            )],
            CommitKind::GspPost,
            &WriteOptions::default(),
        )
        .unwrap();
        let load = t.elapsed();
        println!("{label}: {n} quads, restricted replace {replace:?}, stream load {load:?}");
    }
}
