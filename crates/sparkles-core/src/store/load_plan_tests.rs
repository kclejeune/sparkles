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
        row("replace: empty head, a small file", empty(sized(Replace, 1)), 0, Txn, &[]),
        row("replace: empty head, no sources", empty(LoadInputs::new(Replace, T)), 0, Txn, &[]),
        row("replace: empty head, a file above the threshold", empty(sized(Replace, T + 1)), 0, Bulk, &[]),
        row("replace: empty head, stream at the large bound", empty(stream(Replace)), LARGE, Txn, &[LARGE]),
        row("replace: empty head, stream above the large bound", empty(stream(Replace)), LARGE + 1, Bulk, &[LARGE]),
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

/// The sizes of a row: its quads as the estimate, and its stream bytes.
struct Stub<'a> {
    quads: u64,
    stream_bytes: u64,
    probes: &'a mut Vec<u64>,
    estimated: &'a mut bool,
}

impl super::Sizes for Stub<'_> {
    fn estimate(&mut self) -> u64 {
        *self.estimated = true;
        self.quads
    }

    fn exceeds(&mut self, limit: u64) -> crate::error::Result<bool> {
        self.probes.push(limit);
        Ok(self.stream_bytes > limit)
    }
}

#[test]
fn plan_table() {
    let mut failures = Vec::new();
    for r in rows() {
        let (mut probes, mut estimated) = (Vec::new(), false);
        let plan = LoadPlan::choose(
            &r.inputs,
            &mut Stub {
                quads: r.inputs.quads,
                stream_bytes: r.stream_bytes,
                probes: &mut probes,
                estimated: &mut estimated,
            },
        )
        .unwrap();
        if plan != r.plan || probes != r.probes {
            failures.push(format!(
                "{}: chose {plan:?} probing {probes:?}, expected {:?} probing {:?}",
                r.name, r.plan, r.probes
            ));
        }
        // the estimate reads file sources, so a plan that it does not decide skips it
        let sources = matches!(r.inputs.entry, Load | Replace);
        let needed =
            sources && !r.inputs.restricted && !(r.inputs.empty_head && r.inputs.entry == Load);
        if estimated != needed {
            failures.push(format!(
                "{}: estimated {estimated}, expected {needed}",
                r.name
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A threshold whose byte bound passes the largest byte count probes no stream.
#[test]
fn saturated_bound() {
    let i = LoadInputs {
        threshold: u64::MAX,
        ..stream(Load)
    };
    let (mut probes, mut estimated) = (Vec::new(), false);
    let mut stub = Stub {
        quads: 0,
        stream_bytes: u64::MAX,
        probes: &mut probes,
        estimated: &mut estimated,
    };
    assert_eq!(LoadPlan::choose(&i, &mut stub).unwrap(), Txn);
    assert!(probes.is_empty());
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
        ("empty head, small bytes", store(), bytes(0, 1), Txn),
        (
            "empty head, bytes above the threshold",
            store(),
            bytes(0, E + 1),
            Bulk,
        ),
        ("empty head, small stream", store(), reader(0, 3), Txn),
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
/// the plan of a restricted batch and the probe of a large stream decide. The stores are
/// persistent, under `LOAD_PLAN_DIR` or a temporary directory, and in memory, and the
/// arms alternate over `LOAD_PLAN_ROUNDS` rounds. Run it with `LOAD_PLAN_QUADS=1000000
/// mbx test --release -p sparkles-core --lib -- --ignored load_plan_timing --nocapture`.
#[test]
#[ignore]
fn load_plan_timing() {
    let env = |k: &str, d: u64| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let (n, rounds) = (
        env("LOAD_PLAN_QUADS", 1_000_000),
        env("LOAD_PLAN_ROUNDS", 3),
    );
    let data = ntriples(0, n);
    let parent = std::env::var_os("LOAD_PLAN_DIR").map(std::path::PathBuf::from);
    for round in 0..rounds {
        for persistent in [true, false] {
            for (label, threshold) in [
                ("default threshold", StoreOptions::default().bulk_threshold),
                ("transactional", u64::MAX),
            ] {
                let mut dirs = Vec::new();
                let mut open = || {
                    let opts = StoreOptions {
                        bulk_threshold: threshold,
                        ..StoreOptions::default()
                    };
                    let s = if persistent {
                        let d = match &parent {
                            Some(p) => tempfile::tempdir_in(p).unwrap(),
                            None => tempfile::tempdir().unwrap(),
                        };
                        let s = Store::open(&d.path().join("db"), opts).unwrap();
                        dirs.push(d);
                        s
                    } else {
                        Store::in_memory(opts)
                    };
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
                drop(s);
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
                drop(s);
                let kind = if persistent {
                    "persistent"
                } else {
                    "in memory"
                };
                println!(
                    "round {round}, {kind}, {label}: {n} quads, restricted replace {:.3} s, stream load {:.3} s",
                    replace.as_secs_f64(),
                    load.as_secs_f64()
                );
            }
        }
    }
}

/// A replace counts the quads it parsed, duplicates included, whichever plan it takes.
#[test]
fn replace_counts_parsed_quads_on_both_plans() {
    let twice = |from, n| {
        let mut v = ntriples(from, n);
        v.extend(ntriples(from, n));
        v
    };
    let o = WriteOptions::default();
    let cases: Vec<(&str, Store, Source, u64, LoadPlan)> = vec![
        (
            "empty head, small bytes",
            store(),
            Source::from_bytes(twice(0, 2), RdfFormat::NTriples, None),
            4,
            Txn,
        ),
        (
            "empty head, bytes above the threshold",
            store(),
            Source::from_bytes(twice(0, E), RdfFormat::NTriples, None),
            2 * E,
            Bulk,
        ),
        (
            "empty head, a small stream",
            store(),
            Source::from_reader(std::io::Cursor::new(twice(0, 2)), RdfFormat::NTriples, None),
            4,
            Txn,
        ),
        (
            "above the threshold",
            seeded(2),
            Source::from_bytes(twice(100, E), RdfFormat::NTriples, None),
            2 * E,
            Bulk,
        ),
        (
            "at the threshold",
            seeded(2),
            Source::from_bytes(twice(100, E / 2), RdfFormat::NTriples, None),
            E,
            Txn,
        ),
    ];
    for (name, s, src, n, plan) in cases {
        let (count, _) = s
            .replace_with(ReplaceTarget::Default, &[src], CommitKind::GspPut, &o)
            .unwrap();
        assert_eq!(count, n, "{name}");
        assert_eq!(plan_of(&s), plan, "{name}");
    }
}

/// An empty source written into an empty store. A replace takes a transaction and makes
/// no commit. A load of a stream does the same, and a load of other sources takes the bulk
/// plan, which commits an empty generation.
#[test]
fn empty_sources_into_an_empty_store() {
    let o = WriteOptions::default();
    for (stream, committed) in [(false, true), (true, false)] {
        let src = || if stream { reader(0, 0) } else { bytes(0, 0) };
        let s = store();
        let (count, r) = s
            .replace_with(ReplaceTarget::Default, &[src()], CommitKind::GspPut, &o)
            .unwrap();
        assert_eq!((count, r.committed), (0, false), "replace, stream {stream}");
        let s = store();
        let r = s.load_with(&[src()], CommitKind::Load, &o).unwrap();
        assert_eq!(r.committed, committed, "load, stream {stream}");
        assert_eq!(s.snapshot().len(), 0);
    }
}

/// A dry run of a transactional load or replace into an in-memory store over its limit is
/// previewed as the storage refusal the write itself gets, not refused outright, and
/// leaves the store as it was. Copies of one quad are refused while they are parsed though
/// the commit would hold one quad, or none when the graph already holds it.
#[test]
fn a_dry_run_over_the_memory_limit_is_previewed() {
    use crate::preview::{Outcome, catch};
    const HELD: &[u8] = b"<urn:s> <urn:p> <urn:o> .\n";
    let limited = |seed: bool| {
        let s = Store::in_memory(StoreOptions {
            bulk_threshold: 1_000,
            max_memory_bytes: Some(16 << 10),
            ..StoreOptions::default()
        });
        if seed {
            let src = Source::from_bytes(HELD.to_vec(), RdfFormat::NTriples, None);
            s.load(&[src]).unwrap();
        }
        s
    };
    let copies = || Source::from_bytes(HELD.repeat(500), RdfFormat::NTriples, None);
    let dry = WriteOptions {
        dry_run: Some(Default::default()),
        ..Default::default()
    };
    let real = WriteOptions::default();
    let replace = |s: &Store, src: Source, o: &WriteOptions| {
        s.replace_with(ReplaceTarget::Default, &[src], CommitKind::GspPut, o)
            .map(|(_, r)| r)
    };
    let load = |s: &Store, src: Source, o: &WriteOptions| s.load_with(&[src], CommitKind::Load, o);
    type Write<'a> =
        &'a dyn Fn(&Store, Source, &WriteOptions) -> crate::error::Result<crate::commit::Receipt>;
    let cases: Vec<(&str, bool, Write, fn() -> Source, Option<u64>)> = vec![
        (
            "load into a store with data",
            true,
            &load,
            || bytes(0, 500),
            Some(500),
        ),
        (
            "first replace",
            false,
            &replace,
            || bytes(0, 500),
            Some(500),
        ),
        ("copies loaded", true, &load, copies, None),
        ("copies replaced", false, &replace, copies, Some(1)),
        ("held copies replaced", true, &replace, copies, None),
    ];
    for (name, seed, write, src, inserted) in cases {
        let s = limited(seed);
        let before = s.snapshot().len();
        let e = write(&s, src(), &real).unwrap_err();
        assert!(
            matches!(e, crate::error::Error::StorageFull(_)),
            "{name}: {e}"
        );
        let p = catch(write(&s, src(), &dry)).unwrap();
        assert_eq!(p.outcome(), Outcome::StorageRefused, "{name}");
        assert_eq!(p.error(), Some(e.to_string()), "{name}");
        assert_eq!(p.commit.as_ref().map(|c| c.inserted), inserted, "{name}");
        assert!(p.commit.as_ref().is_none_or(|c| !c.bulk), "{name}");
        // the sizes are those of the check that refused it, with or without a commit
        let (projected, limit) = (p.storage.projected.unwrap(), p.storage.limit.unwrap());
        assert!(projected > limit, "{name}: {projected} <= {limit}");
        assert_eq!(s.snapshot().len(), before, "{name}");
    }
}
