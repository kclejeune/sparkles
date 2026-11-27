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

use Entry::{InsertBulk, Load, Patch, Replace, SparqlLoad};
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
    let files = matches!(entry, Load | Replace | SparqlLoad) as usize;
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
        // sparql LOAD (single-operation request)
        row("sparql_load: empty head, a small file", empty(sized(SparqlLoad, 1)), 0, Txn, &[]),
        row("sparql_load: empty head, a file at the threshold", empty(sized(SparqlLoad, T)), 0, Txn, &[]),
        row("sparql_load: empty head, a file above the threshold", empty(sized(SparqlLoad, T + 1)), 0, Bulk, &[]),
        row("sparql_load: empty head, stream at the large bound", empty(stream(SparqlLoad)), LARGE, Txn, &[LARGE]),
        row("sparql_load: empty head, stream above the large bound", empty(stream(SparqlLoad)), LARGE + 1, Bulk, &[LARGE]),
        row("sparql_load: estimate at the threshold", sized(SparqlLoad, T), 0, Txn, &[]),
        row("sparql_load: estimate above the threshold", sized(SparqlLoad, T + 1), 0, Bulk, &[]),
        row("sparql_load: stream at the large bound", stream(SparqlLoad), LARGE, Txn, &[LARGE]),
        row("sparql_load: stream above the large bound", stream(SparqlLoad), LARGE + 1, Bulk, &[LARGE]),
        row("sparql_load: restricted, empty head, above the threshold", restricted(empty(sized(SparqlLoad, T + 1))), 0, Txn, &[]),
        row("sparql_load: restricted, above the threshold", restricted(sized(SparqlLoad, T + 1)), 0, Txn, &[]),
        row("sparql_load: restricted, large stream", restricted(stream(SparqlLoad)), LARGE + 1, Txn, &[]),
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
        let sources = matches!(r.inputs.entry, Load | Replace | SparqlLoad);
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
    // name, seeded, write, source, rows a dry run's commit preview inserts
    type Case<'a> = (&'a str, bool, Write<'a>, fn() -> Source, Option<u64>);
    let cases: Vec<Case> = vec![
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

#[test]
fn sparql_load_plans_and_semantics() {
    use crate::error::Error;
    use crate::outbound::OutboundPolicy;
    use crate::sparql::{FileLoads, QueryOptions, query, update::update};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let tmp = tempfile::tempdir().unwrap();
    let write_nt = |name: &str, from: u64, n: u64| {
        let p = tmp.path().join(name);
        std::fs::write(&p, ntriples(from, n)).unwrap();
        p
    };
    let file_url = |p: &std::path::Path| format!("file://{}", p.display());
    let ask = |s: &Store, q: &str| {
        query(s.snapshot(), q, &QueryOptions::default())
            .unwrap()
            .boolean
    };

    let small_file = write_nt("small.nt", 100, E);
    let large_file = write_nt("large.nt", 200, E + 1);
    let extra_file = write_nt("extra.nt", 300, E + 1);

    // 1. Single-operation large LOAD (default graph & INTO GRAPH) takes Bulk on both empty
    // and seeded stores; small LOAD stays Transactional on both.
    let file_cases = [
        ("empty head, at threshold", store(), &small_file, None, Txn),
        (
            "empty head, above threshold (default)",
            store(),
            &large_file,
            None,
            Bulk,
        ),
        (
            "empty head, above threshold (INTO GRAPH)",
            store(),
            &large_file,
            Some("http://ex.org/g"),
            Bulk,
        ),
        ("seeded, at threshold", seeded(2), &small_file, None, Txn),
        (
            "seeded, above threshold (default)",
            seeded(2),
            &large_file,
            None,
            Bulk,
        ),
        (
            "seeded, above threshold (INTO GRAPH)",
            seeded(2),
            &large_file,
            Some("http://ex.org/g"),
            Bulk,
        ),
    ];
    for (name, s, path, graph, expected) in file_cases {
        let before = s.snapshot().len();
        let stmt = match graph {
            Some(g) => format!("LOAD <{}> INTO GRAPH <{g}>", file_url(path)),
            None => format!("LOAD <{}>", file_url(path)),
        };
        let stats = update(&s, &stmt, &QueryOptions::default()).unwrap();
        let receipt = stats.commit.as_ref().expect("receipt");
        assert_eq!(plan_of(&s), expected, "{name}");
        assert_eq!(receipt.commit.bulk, expected == Bulk, "{name}");
        let want_inserted = if expected == Bulk { E + 1 } else { E };
        assert_eq!(stats.inserted, want_inserted, "{name}");
        assert_eq!(s.snapshot().len(), before + want_inserted, "{name}");
        if let Some(g) = graph {
            assert!(
                ask(
                    &s,
                    &format!("ASK {{ GRAPH <{g}> {{ <http://ex.org/s200> ?p ?o }} }}")
                ),
                "{name}"
            );
            assert!(
                !ask(&s, "ASK { <http://ex.org/s200> ?p ?o }"),
                "{name}: default graph untouched"
            );
        }
    }

    // 2. Single-operation HTTP LOAD (plain and gzipped) follows the stream probe boundary.
    let serve = |body: Vec<u8>, ct: &'static str| -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: {ct}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });
        format!("http://127.0.0.1:{port}/data.nt")
    };
    let http_opts = QueryOptions {
        outbound: OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
        ..Default::default()
    };
    for (name, s, n, gzip, expected) in [
        ("http empty, at bound", store(), E, false, Txn),
        ("http empty, above bound", store(), E + 1, false, Bulk),
        ("http seeded, at bound", seeded(2), E, false, Txn),
        ("http seeded, above bound", seeded(2), E + 1, false, Bulk),
        (
            "http seeded gzip, above bound",
            seeded(2),
            E + 1,
            true,
            Bulk,
        ),
    ] {
        let raw = ntriples(400, n);
        let body = if gzip {
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            gz.write_all(&raw).unwrap();
            gz.finish().unwrap()
        } else {
            raw
        };
        let url = serve(body, "application/n-triples");
        let stats = update(
            &s,
            &format!("LOAD <{url}> INTO GRAPH <http://ex.org/h>"),
            &http_opts,
        )
        .unwrap();
        assert_eq!(plan_of(&s), expected, "{name}");
        assert_eq!(
            stats.commit.unwrap().commit.bulk,
            expected == Bulk,
            "{name}"
        );
        assert_eq!(stats.inserted, n, "{name}");
        assert!(
            ask(
                &s,
                "ASK { GRAPH <http://ex.org/h> { <http://ex.org/s400> ?p ?o } }"
            ),
            "{name}"
        );
    }

    // 3. Multi-operation request containing a large LOAD stays transactional and atomic.
    let s = seeded(2);
    let multi = format!(
        "INSERT DATA {{ <http://ex.org/m> <http://ex.org/p> 1 }} ; LOAD <{}>",
        file_url(&large_file)
    );
    let stats = update(&s, &multi, &QueryOptions::default()).unwrap();
    assert_eq!(plan_of(&s), Txn, "multi-op stays transactional");
    assert!(!stats.commit.unwrap().commit.bulk);
    assert_eq!(stats.inserted, E + 2);
    assert_eq!(stats.operations, 2);

    // Multi-op atomicity: if the second operation fails, the first large LOAD is rolled back.
    let s = seeded(2);
    let bad = tmp.path().join("broken.nt");
    std::fs::write(&bad, "<urn:broken \n").unwrap();
    let multi_fail = format!(
        "LOAD <{}> ; LOAD <{}>",
        file_url(&large_file),
        file_url(&bad)
    );
    assert!(update(&s, &multi_fail, &QueryOptions::default()).is_err());
    assert_eq!(
        s.snapshot().len(),
        2,
        "failed multi-op leaves store unchanged"
    );

    // 4. Restricted graph access keeps a large LOAD transactional and enforces rules.
    let s = seeded(2);
    let restricted_q = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(
            Graphs::All,
            Graphs::Only(GraphRule::new(["default"], &[])),
        ))),
        ..Default::default()
    };
    let stats = update(
        &s,
        &format!("LOAD <{}>", file_url(&large_file)),
        &restricted_q,
    )
    .unwrap();
    assert_eq!(plan_of(&s), Txn, "restricted stays transactional");
    assert!(!stats.commit.unwrap().commit.bulk);
    assert_eq!(stats.inserted, E + 1);
    // Disallowed target graph is refused even under LOAD SILENT.
    let err = update(
        &s,
        &format!(
            "LOAD SILENT <{}> INTO GRAPH <http://ex.org/forbidden>",
            file_url(&extra_file)
        ),
        &restricted_q,
    )
    .unwrap_err();
    assert!(matches!(err, Error::NotPermitted(_)), "{err:?}");

    // 5. LOAD SILENT suppresses source/parse errors (including large malformed files) and
    // returns an unchanged-head receipt, while still surfacing refusals, preconditions,
    // dry runs and managed-graph boundaries.
    let big_bad = tmp.path().join("big_broken.nt");
    let mut bad_bytes = ntriples(500, E + 5);
    bad_bytes.extend_from_slice(b"<urn:broken \n");
    std::fs::write(&big_bad, bad_bytes).unwrap();
    let s = seeded(2);
    let stats = update(
        &s,
        &format!("LOAD SILENT <{}>", file_url(&big_bad)),
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(stats.inserted, 0);
    assert!(!stats.commit.as_ref().unwrap().committed);
    assert_eq!(s.snapshot().len(), 2);

    // Precondition failure is NOT hidden by LOAD SILENT.
    let mut pre_opts = QueryOptions::default();
    pre_opts.write.precondition = Some(crate::guard::Precondition::new(|_| {
        Err(Error::PreconditionFailed("stale".into()))
    }));
    let err = update(
        &s,
        &format!("LOAD SILENT <{}>", file_url(&big_bad)),
        &pre_opts,
    )
    .unwrap_err();
    assert!(matches!(err, Error::PreconditionFailed(_)), "{err:?}");

    // Dry run is NOT hidden by LOAD SILENT and previews a bulk LOAD cleanly.
    let mut dry_opts = QueryOptions::default();
    dry_opts.write.dry_run = Some(Default::default());
    let p = crate::preview::catch(
        update(&s, &format!("LOAD <{}>", file_url(&large_file)), &dry_opts)
            .map(|st| st.commit.unwrap()),
    )
    .unwrap();
    assert!(p.commit.as_ref().unwrap().bulk);
    assert_eq!(s.snapshot().len(), 2, "dry run writes nothing");

    // FileLoads::Disabled is NOT hidden by LOAD SILENT.
    let disabled = QueryOptions {
        file_loads: FileLoads::Disabled,
        ..Default::default()
    };
    let err = update(
        &s,
        &format!("LOAD SILENT <{}>", file_url(&large_file)),
        &disabled,
    )
    .unwrap_err();
    assert!(matches!(err, Error::NotPermitted(_)), "{err:?}");

    // Managed graph fence blocks a bulk LOAD targeting a managed graph even under SILENT.
    let s = seeded(2);
    let managed_graph = "http://ex.org/managed";
    let settings = br#"{"format":1,"profile":1}"#;
    let owner =
        crate::managed::OwnerEntry::for_settings(1, vec![managed_graph.to_string()], settings);
    let _managed = s
        .enable_managed(crate::managed::OwnerKind::Evidence, owner, settings)
        .unwrap();
    let err = update(
        &s,
        &format!(
            "LOAD SILENT <{}> INTO GRAPH <{managed_graph}>",
            file_url(&large_file)
        ),
        &QueryOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Managed(_)), "{err:?}");

    // Cancellation stops a single-op LOAD before committing.
    let s = store();
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let cancelled = QueryOptions {
        cancel: Some(cancel),
        ..Default::default()
    };
    let err = update(&s, &format!("LOAD <{}>", file_url(&large_file)), &cancelled).unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err:?}");
    assert_eq!(s.snapshot().len(), 0);
}

#[test]
fn sparql_load_silent_provenance_and_write_errors() {
    use crate::error::Error;
    use crate::guard::{
        Candidate, CommitGuard, GuardMode, GuardStatus, PublishHook, Severity, ValidationSummary,
    };
    use crate::sparql::{QueryOptions, update::update};

    struct InvalidGuard;
    impl CommitGuard for InvalidGuard {
        fn check(&self, _: &Candidate<'_>) -> crate::error::Result<ValidationSummary> {
            Err(Error::invalid("guard configuration invalid"))
        }
        fn describe(&self) -> String {
            "invalid-guard".into()
        }
    }

    struct RejectGuard;
    impl CommitGuard for RejectGuard {
        fn check(&self, _: &Candidate<'_>) -> crate::error::Result<ValidationSummary> {
            Ok(ValidationSummary::empty(
                GuardStatus::Rejected,
                GuardMode::Reject,
                Severity::Violation,
            ))
        }
        fn describe(&self) -> String {
            "reject-guard".into()
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let small_file = tmp.path().join("small.nt");
    let large_file = tmp.path().join("large.nt");
    std::fs::write(&small_file, ntriples(100, E)).unwrap();
    std::fs::write(&large_file, ntriples(200, E + 1)).unwrap();
    let file_url = |p: &std::path::Path| format!("file://{}", p.display());

    // 1. A write guard returning Error::Invalid MUST NOT be suppressed by LOAD SILENT on
    // either the transactional or bulk plan.
    for (label, path) in [("txn", &small_file), ("bulk", &large_file)] {
        let s = seeded(2);
        s.set_guard(Some(Arc::new(InvalidGuard)));
        let err = update(
            &s,
            &format!("LOAD SILENT <{}>", file_url(path)),
            &QueryOptions::default(),
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::Invalid(ref m) if m.contains("guard configuration invalid")),
            "{label}: {err:?}"
        );
        assert_eq!(s.snapshot().len(), 2, "{label}");
    }

    // 2. A write guard returning Rejected MUST NOT be suppressed by LOAD SILENT on bulk.
    let s = seeded(2);
    s.set_guard(Some(Arc::new(RejectGuard)));
    let err = update(
        &s,
        &format!("LOAD SILENT <{}>", file_url(&large_file)),
        &QueryOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Rejected(_)), "{err:?}");
    assert_eq!(s.snapshot().len(), 2);

    // 3. A before_publish hook returning Error::Invalid MUST NOT be suppressed by LOAD
    // SILENT on either the transactional or bulk plan.
    for (label, path) in [("txn", &small_file), ("bulk", &large_file)] {
        let s = seeded(2);
        let mut opts = QueryOptions::default();
        opts.write.before_publish = Some(PublishHook::new(|| {
            Err(Error::invalid("publish hook refused"))
        }));
        let err = update(&s, &format!("LOAD SILENT <{}>", file_url(path)), &opts).unwrap_err();
        assert!(
            matches!(err, Error::Invalid(ref m) if m.contains("publish hook refused")),
            "{label}: {err:?}"
        );
        assert_eq!(s.snapshot().len(), 2, "{label}");
    }

    // 4. Corrupt compressed file (.nt.zst with valid magic header and corrupt payload,
    // which zstd reports as io::ErrorKind::Other) fails on LOAD and IS suppressed by
    // LOAD SILENT on both small and large files.
    #[cfg(feature = "zstd")]
    {
        for (label, pad) in [("small", 32usize), ("large", (E as usize + 10) * 80)] {
            let corrupt = tmp.path().join(format!("corrupt_{label}.nt.zst"));
            let mut data = vec![0x28u8, 0xb5, 0x2f, 0xfd]; // valid zstd frame magic
            data.extend(std::iter::repeat_n(0xffu8, pad));
            std::fs::write(&corrupt, data).unwrap();
            let s = seeded(2);
            let err = update(
                &s,
                &format!("LOAD <{}>", file_url(&corrupt)),
                &QueryOptions::default(),
            )
            .unwrap_err();
            assert!(matches!(err, Error::Io(_)), "{label} LOAD: {err:?}");
            let st = update(
                &s,
                &format!("LOAD SILENT <{}>", file_url(&corrupt)),
                &QueryOptions::default(),
            )
            .unwrap();
            assert_eq!(st.inserted, 0, "{label} SILENT");
            assert!(!st.commit.as_ref().unwrap().committed, "{label} SILENT");
            assert_eq!(s.snapshot().len(), 2, "{label} SILENT");
        }
    }

    // 5. LOAD SILENT with dry_run on a broken source preserves Error::DryRun via noop.
    let broken = tmp.path().join("broken.nt");
    std::fs::write(&broken, "<urn:broken \n").unwrap();
    let s = seeded(2);
    let mut dry = QueryOptions::default();
    dry.write.dry_run = Some(Default::default());
    let preview = crate::preview::catch(
        update(&s, &format!("LOAD SILENT <{}>", file_url(&broken)), &dry)
            .map(|st| st.commit.unwrap()),
    )
    .unwrap();
    assert!(preview.commit.is_none());
    assert_eq!(s.snapshot().len(), 2);
}

#[test]
fn sparql_load_silent_preserves_outbound_limits_and_write_controls() {
    use crate::error::Error;
    use crate::outbound::OutboundPolicy;
    use crate::sparql::{QueryOptions, update::update};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    type Handler = Arc<dyn Fn(&str, &mut std::net::TcpStream) + Send + Sync>;
    let spawn_server = |handler: Handler| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let conns = Arc::new(AtomicUsize::new(0));
        let conns_bg = Arc::clone(&conns);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                conns_bg.fetch_add(1, Ordering::SeqCst);
                let h = Arc::clone(&handler);
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let mut head = Vec::new();
                    let mut b = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut b).unwrap_or(0) == 1 {
                        head.push(b[0]);
                    }
                    let req = String::from_utf8_lossy(&head);
                    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                    h(&path, &mut stream);
                });
            }
        });
        (port, conns)
    };

    let big_nt = ntriples(0, 200); // 16,000 bytes
    let mut gz_enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz_enc.write_all(&big_nt).unwrap();
    let big_gz = gz_enc.finish().unwrap();

    let big_nt_srv = big_nt.clone();
    let big_gz_srv = big_gz.clone();
    let (port, conns) = spawn_server(Arc::new(move |path, stream| match path {
        "/declared.nt" => {
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/n-triples\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                big_nt_srv.len()
            );
            let _ = stream.write_all(&big_nt_srv);
        }
        "/streamed.nt" => {
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/n-triples\r\nconnection: close\r\n\r\n"
            );
            let _ = stream.write_all(&big_nt_srv);
        }
        "/big.nt.gz" => {
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/n-triples\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                big_gz_srv.len()
            );
            let _ = stream.write_all(&big_gz_srv);
        }
        "/slow-headers.nt" => {
            std::thread::sleep(Duration::from_millis(700));
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/n-triples\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
        }
        "/trickle.nt" => {
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/n-triples\r\nconnection: close\r\n\r\n"
            );
            for _ in 0..20 {
                if stream.write_all(b" ").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(60));
            }
        }
        "/trickle.nt.gz" => {
            // Send valid gzip header bytes immediately, then trickle trailing bytes slowly
            // so the gzip decoder blocks on raw HTTP body reads.
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/n-triples\r\nconnection: close\r\n\r\n"
            );
            let _ = stream.write_all(&big_gz_srv[..10.min(big_gz_srv.len())]);
            for chunk in big_gz_srv[10.min(big_gz_srv.len())..].chunks(1) {
                std::thread::sleep(Duration::from_millis(50));
                if stream.write_all(chunk).is_err() {
                    break;
                }
            }
        }
        _ => {
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/n-triples\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                big_nt_srv.len()
            );
            let _ = stream.write_all(&big_nt_srv);
        }
    }));
    let url = |p: &str| format!("http://127.0.0.1:{port}{p}");

    // 1. Outbound raw response ceiling (both declared Content-Length and streamed) is NOT
    // suppressed by LOAD SILENT.
    for path in ["/declared.nt", "/streamed.nt"] {
        let s = seeded(2);
        let opts = QueryOptions {
            outbound: OutboundPolicy {
                allow_private: true,
                max_response_bytes: 2048,
                ..Default::default()
            },
            ..Default::default()
        };
        let err = update(&s, &format!("LOAD SILENT <{}>", url(path)), &opts).unwrap_err();
        assert!(
            matches!(err, Error::Invalid(ref m) if m.contains("larger than the outbound limit")),
            "{path}: {err:?}"
        );
        assert_eq!(s.snapshot().len(), 2, "{path}");
    }

    // 2. Outbound timeouts (headers and body trickle) are NOT suppressed by LOAD SILENT.
    for path in ["/slow-headers.nt", "/trickle.nt"] {
        let s = seeded(2);
        let opts = QueryOptions {
            outbound: OutboundPolicy {
                allow_private: true,
                timeout: Duration::from_millis(200),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = update(&s, &format!("LOAD SILENT <{}>", url(path)), &opts).unwrap_err();
        assert!(
            matches!(err, Error::Invalid(ref m) if m.contains("no complete response within")),
            "{path}: {err:?}"
        );
        assert_eq!(s.snapshot().len(), 2, "{path}");
    }

    // 3. Compressed decompression ceiling & request byte budget are NOT suppressed by LOAD SILENT.
    let s = seeded(2);
    let decomp_opts = QueryOptions {
        outbound: OutboundPolicy {
            allow_private: true,
            max_response_bytes: (big_gz.len() as u64) + 64,
            ..Default::default()
        },
        ..Default::default()
    };
    let err = update(
        &s,
        &format!("LOAD SILENT <{}>", url("/big.nt.gz")),
        &decomp_opts,
    )
    .unwrap_err();
    assert!(matches!(err, Error::BudgetExceeded(_)), "decomp: {err:?}");

    let budget_opts = QueryOptions {
        outbound: OutboundPolicy {
            allow_private: true,
            max_request_bytes: 2048,
            ..Default::default()
        },
        ..Default::default()
    };
    let err = update(
        &s,
        &format!("LOAD SILENT <{}>", url("/streamed.nt")),
        &budget_opts,
    )
    .unwrap_err();
    assert!(matches!(err, Error::BudgetExceeded(_)), "budget: {err:?}");

    // 4. Write-only cancel, cancellation and expired deadline stop LOAD before contacting
    // the HTTP server (0 new connections).
    let before = conns.load(Ordering::SeqCst);
    let mut w_cancel = QueryOptions {
        outbound: OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
        ..Default::default()
    };
    w_cancel.write.cancel = Some(Arc::new(AtomicBool::new(true)));
    let err = update(
        &s,
        &format!("LOAD SILENT <{}>", url("/declared.nt")),
        &w_cancel,
    )
    .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "write.cancel: {err:?}");

    let mut w_deadline = QueryOptions {
        outbound: OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
        ..Default::default()
    };
    w_deadline.write.deadline = Some(Instant::now() - Duration::from_secs(1));
    let err = update(
        &s,
        &format!("LOAD SILENT <{}>", url("/declared.nt")),
        &w_deadline,
    )
    .unwrap_err();
    assert!(matches!(err, Error::Timeout), "write.deadline: {err:?}");

    // 5. Stricter opts.write.graphs when opts.graphs is ALSO set rejects before fetching.
    let mut stricter = QueryOptions {
        outbound: OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
        graphs: Some(Arc::new(GraphAccess::graphs(
            Graphs::All,
            Graphs::Only(GraphRule::new(["http://ex.org/g"], &[])),
        ))),
        ..Default::default()
    };
    stricter.write.graphs = Some(Arc::new(GraphAccess::graphs(
        Graphs::All,
        Graphs::Only(GraphRule::new(["default"], &[])),
    )));
    let err = update(
        &s,
        &format!(
            "LOAD SILENT <{}> INTO GRAPH <http://ex.org/g>",
            url("/declared.nt")
        ),
        &stricter,
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::NotPermitted(_)),
        "stricter write.graphs: {err:?}"
    );
    assert_eq!(
        conns.load(Ordering::SeqCst),
        before,
        "pre-lock write controls and stricter write.graphs must reject before HTTP fetch"
    );

    // 6. A write-only deadline bounds slow HTTP headers AND a trickling compressed stream
    // beneath the gzip decoder, returning Error::Timeout even under LOAD SILENT.
    for path in ["/slow-headers.nt", "/trickle.nt.gz"] {
        let mut opts = QueryOptions {
            outbound: OutboundPolicy {
                allow_private: true,
                timeout: Duration::from_secs(10),
                ..Default::default()
            },
            ..Default::default()
        };
        opts.write.deadline = Some(Instant::now() + Duration::from_millis(180));
        let t0 = Instant::now();
        let err = update(&s, &format!("LOAD SILENT <{}>", url(path)), &opts).unwrap_err();
        assert!(matches!(err, Error::Timeout), "{path}: {err:?}");
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "{path}: {:?}",
            t0.elapsed()
        );
    }
}

#[test]
fn sparql_load_repeat_ground_default_graph_flag_and_protected_preview() {
    use crate::error::Error;
    use crate::sparql::{
        QueryOptions, query,
        update::{update, update_in},
    };

    let tmp = tempfile::tempdir().unwrap();
    let file_url = |p: &std::path::Path| format!("file://{}", p.display());
    let ask = |s: &Store, q: &str| {
        query(s.snapshot(), q, &QueryOptions::default())
            .unwrap()
            .boolean
    };

    // 1. Repeat above-threshold ground LOAD preserves head commit and returns committed=false.
    let large_nt = tmp.path().join("large.nt");
    std::fs::write(&large_nt, ntriples(0, E + 2)).unwrap();
    let s = store();
    let first = update(
        &s,
        &format!("LOAD <{}>", file_url(&large_nt)),
        &QueryOptions::default(),
    )
    .unwrap();
    let first_r = first.commit.as_ref().unwrap();
    assert!(first_r.committed);
    assert!(first_r.commit.bulk);
    assert_eq!(first.inserted, E + 2);
    let head_after_first = first_r.commit.seq;

    let repeat = update(
        &s,
        &format!("LOAD <{}>", file_url(&large_nt)),
        &QueryOptions::default(),
    )
    .unwrap();
    let repeat_r = repeat.commit.as_ref().unwrap();
    assert!(
        !repeat_r.committed,
        "repeat ground LOAD must not allocate a commit"
    );
    assert_eq!(repeat_r.commit.seq, head_after_first);
    assert_eq!(repeat.inserted, 0);
    assert_eq!(s.snapshot().len(), E + 2);

    // Dry run of the repeat duplicate-only LOAD reports commit: None.
    let mut dry = QueryOptions::default();
    dry.write.dry_run = Some(Default::default());
    let p = crate::preview::catch(
        update(&s, &format!("LOAD <{}>", file_url(&large_nt)), &dry).map(|st| st.commit.unwrap()),
    )
    .unwrap();
    assert!(p.commit.is_none());
    assert!(p.graphs.is_empty());

    // 2. Bulk LOAD of N-Quads with default-graph AND named-graph quads INTO GRAPH <target>
    // remaps default quads to <target>, keeps named quads in their graph, and sets
    // commit.default_graph == false (and store.default_graph_changed == false).
    let mut nq = String::new();
    for i in 0..(E + 2) {
        nq.push_str(&format!(
            "<http://ex.org/def{i}> <http://ex.org/p> \"{i:0>40}\" .\n"
        ));
    }
    nq.push_str("<http://ex.org/ns0> <http://ex.org/p> \"named\" <http://ex.org/named> .\n");
    let nq_path = tmp.path().join("dataset.nq");
    std::fs::write(&nq_path, &nq).unwrap();

    let s = seeded(2);
    let before_seq = s.snapshot().commit;
    let st = update(
        &s,
        &format!(
            "LOAD <{}> INTO GRAPH <http://ex.org/target>",
            file_url(&nq_path)
        ),
        &QueryOptions::default(),
    )
    .unwrap();
    let r = st.commit.as_ref().unwrap();
    assert!(r.committed);
    assert!(r.commit.bulk);
    assert!(
        !r.commit.default_graph,
        "bulk LOAD INTO GRAPH must keep default_graph flag false"
    );
    assert!(!s.default_graph_changed(before_seq, r.commit.seq));
    assert!(ask(
        &s,
        "ASK { GRAPH <http://ex.org/target> { <http://ex.org/def0> ?p ?o } }"
    ));
    assert!(ask(
        &s,
        "ASK { GRAPH <http://ex.org/named> { <http://ex.org/ns0> ?p \"named\" } }"
    ));
    assert!(!ask(&s, "ASK { <http://ex.org/def0> ?p ?o }"));

    // 3. Bulk LOAD <file.nq> (without INTO GRAPH) containing a managed named graph is refused
    // by Builder::with_graph_fence even under LOAD SILENT.
    let s = seeded(2);
    let managed_graph = "http://ex.org/named";
    let settings = br#"{"format":1,"profile":1}"#;
    let owner =
        crate::managed::OwnerEntry::for_settings(1, vec![managed_graph.to_string()], settings);
    let _managed = s
        .enable_managed(crate::managed::OwnerKind::Evidence, owner, settings)
        .unwrap();
    let err = update(
        &s,
        &format!("LOAD SILENT <{}>", file_url(&nq_path)),
        &QueryOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Managed(_)), "{err:?}");

    // 4. Bulk dry-run LOAD with preview_as_new_branch succeeds on a protected persistent head.
    let db_dir = tempfile::tempdir().unwrap();
    let persistent = Store::open(
        &db_dir.path().join("db"),
        StoreOptions {
            bulk_threshold: E,
            ..StoreOptions::default()
        },
    )
    .unwrap();
    persistent.load(&[bytes(1000, 2)]).unwrap();
    persistent.set_branch_protected("main", true).unwrap();
    let mut blocked_dry = QueryOptions::default();
    blocked_dry.write.dry_run = Some(Default::default());
    let err = update(
        &persistent,
        &format!("LOAD <{}>", file_url(&large_nt)),
        &blocked_dry,
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::Branch(_)),
        "protected dry run without flag: {err:?}"
    );

    let mut preview_branch = QueryOptions::default();
    preview_branch.write.dry_run = Some(Default::default());
    preview_branch.write.preview_as_new_branch = true;
    let p = crate::preview::catch(
        update(
            &persistent,
            &format!("LOAD <{}>", file_url(&large_nt)),
            &preview_branch,
        )
        .map(|st| st.commit.unwrap()),
    )
    .unwrap();
    assert!(p.commit.as_ref().unwrap().bulk);
    assert_eq!(persistent.snapshot().len(), 2);

    // 5. Caller-owned WriteTxn via update_in keeps a large LOAD transactional and rolls back
    // when dropped uncommitted.
    let s = seeded(2);
    {
        let mut txn = s.write();
        let st = update_in(
            &mut txn,
            &format!("LOAD <{}>", file_url(&large_nt)),
            &QueryOptions::default(),
        )
        .unwrap();
        assert_eq!(st.inserted, E + 2);
        // Drop txn uncommitted.
    }
    assert_eq!(s.snapshot().len(), 2);
}

#[test]
fn reader_source_read_ahead_caps_allocation_capacity() {
    let data = vec![b'a'; 300_000];
    let src = crate::io::ReaderSource::new(std::io::Cursor::new(data));
    let want = 200_001;
    let held = src.read_ahead(want, &|| Ok(())).unwrap();
    assert_eq!(held, want);
    assert!(
        src.head_capacity() <= want,
        "probe buffer capacity {} exceeded requested bound {want}",
        src.head_capacity()
    );
}

#[test]
fn sparql_load_transactional_refusal_drains_group_commit() {
    use crate::error::Error;
    use crate::sparql::{QueryOptions, update::update};
    use std::sync::mpsc;

    let tmp = tempfile::tempdir().unwrap();
    let small_file = tmp.path().join("small.nt");
    std::fs::write(&small_file, ntriples(100, 2)).unwrap();
    let file_url = format!("file://{}", small_file.display());

    let db_dir = tempfile::tempdir().unwrap();
    let s = Arc::new(
        Store::open(
            &db_dir.path().join("db"),
            StoreOptions {
                bulk_threshold: E,
                group_commit: true,
                ..StoreOptions::default()
            },
        )
        .unwrap(),
    );
    s.load(&[bytes(0, 1)]).unwrap();

    // Hold a group-commit fence in flight while a single-op transactional LOAD starts from
    // the speculative head and fails on a graph restriction; when the fence sync fails,
    // after_refusal must surface Error::Poisoned instead of the speculative NotPermitted.
    let (sealed_tx, sealed_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let go_rx = Arc::new(parking_lot::Mutex::new(go_rx));
    s.set_failpoint(
        "group-sealed",
        Some(Arc::new(move |_| {
            let _ = sealed_tx.send(());
            let _ = go_rx.lock().recv();
        })),
    );
    let s_bg = Arc::clone(&s);
    let first = std::thread::spawn(move || {
        update(
            &s_bg,
            "INSERT DATA { <http://ex.org/gc1> <http://ex.org/p> 1 }",
            &QueryOptions::default(),
        )
    });
    sealed_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("predecessor did not seal its group commit");

    let (draining_tx, draining_rx) = mpsc::channel();
    s.set_failpoint(
        "group-draining",
        Some(Arc::new(move |_| {
            let _ = draining_tx.send(());
        })),
    );
    let s_load = Arc::clone(&s);
    let load_url = file_url.clone();
    let restricted_load = std::thread::spawn(move || {
        let mut opts = QueryOptions::default();
        // Allow only named graph <http://ex.org/ok>, so LOAD into default graph fails inside
        // transactional txn.insert() after taking the speculative head.
        opts.write.graphs = Some(Arc::new(GraphAccess::graphs(
            Graphs::All,
            Graphs::Only(GraphRule::new(["http://ex.org/ok"], &[])),
        )));
        update(&s_load, &format!("LOAD <{load_url}>"), &opts)
    });
    // Wait until the load thread has entered the refusal drain, then fail the fence.
    draining_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("transactional load refusal did not enter group-draining");
    s.fail_next_group_sync();
    let _ = go_tx.send(());
    s.set_failpoint("group-draining", None);
    s.set_failpoint("group-sealed", None);

    let first_res = first.join().unwrap();
    let load_res = restricted_load.join().unwrap();
    assert!(
        matches!(first_res, Err(Error::Poisoned)),
        "first: {first_res:?}"
    );
    assert!(
        matches!(load_res, Err(Error::Poisoned)),
        "transactional load refusal must drain via after_refusal and report Poisoned: {load_res:?}"
    );
}

#[test]
fn sparql_load_silent_http_limit_preserves_group_poisoned_and_writer_busy() {
    use crate::error::Error;
    use crate::outbound::OutboundPolicy;
    use crate::sparql::{QueryOptions, update::update};
    use crate::store::SparqlLoadError;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration;

    // 1. Force the transactional plan, start HTTP LOAD SILENT from a speculative group-commit
    // head, exceed max_response_bytes after sniffing, and fail the predecessor's fence during
    // the refusal drain. Store(Poisoned) must take precedence over LoadHttpBody's recorded limit.
    let big_nt = ntriples(0, 200); // ~16 KiB, exceeds max_response_bytes = 2048 after sniffing
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let body = big_nt.clone();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut b).unwrap_or(0) == 1 {
                head.push(b[0]);
            }
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/n-triples\r\nconnection: close\r\n\r\n"
            );
            let _ = stream.write_all(&body);
        }
    });

    let db_dir = tempfile::tempdir().unwrap();
    let s = Arc::new(
        Store::open(
            &db_dir.path().join("db-poison"),
            StoreOptions {
                bulk_threshold: u64::MAX,
                group_commit: true,
                ..StoreOptions::default()
            },
        )
        .unwrap(),
    );
    s.load(&[bytes(0, 1)]).unwrap();

    let (sealed_tx, sealed_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let go_rx = Arc::new(parking_lot::Mutex::new(go_rx));
    s.set_failpoint(
        "group-sealed",
        Some(Arc::new(move |_| {
            let _ = sealed_tx.send(());
            let _ = go_rx.lock().recv();
        })),
    );
    let s_bg = Arc::clone(&s);
    let first = std::thread::spawn(move || {
        update(
            &s_bg,
            "INSERT DATA { <http://ex.org/gc-http> <http://ex.org/p> 1 }",
            &QueryOptions::default(),
        )
    });
    sealed_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let (draining_tx, draining_rx) = mpsc::channel();
    s.set_failpoint(
        "group-draining",
        Some(Arc::new(move |_| {
            let _ = draining_tx.send(());
        })),
    );
    let s_load = Arc::clone(&s);
    let http_load = std::thread::spawn(move || {
        let opts = QueryOptions {
            outbound: OutboundPolicy {
                allow_private: true,
                max_response_bytes: 2048,
                ..OutboundPolicy::default()
            },
            ..QueryOptions::default()
        };
        update(
            &s_load,
            &format!("LOAD SILENT <http://127.0.0.1:{port}/streamed.nt>"),
            &opts,
        )
    });
    draining_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("HTTP LOAD SILENT refusal did not enter group-draining");
    s.fail_next_group_sync();
    let _ = go_tx.send(());
    s.set_failpoint("group-draining", None);
    s.set_failpoint("group-sealed", None);

    let first_res = first.join().unwrap();
    let load_res = http_load.join().unwrap();
    assert!(
        matches!(first_res, Err(Error::Poisoned)),
        "first: {first_res:?}"
    );
    assert!(
        matches!(load_res, Err(Error::Poisoned)),
        "Store(Poisoned) must take precedence over LoadHttpBody's recorded limit error: {load_res:?}"
    );

    // 2. Under no_wait=true while a group-commit fence is in flight, writer admission
    // refuses the load before parsing with Store(WriterBusy) (which LOAD SILENT surfaces),
    // and the post-refusal drain classifier itself also keeps a WriterBusy drain result as
    // SparqlLoadError::Store rather than downgrading it to Source.
    let tmp = tempfile::tempdir().unwrap();
    let bad_nt = tmp.path().join("bad.nt");
    std::fs::write(&bad_nt, "not valid ntriples\n").unwrap();
    let s2 = Arc::new(
        Store::open(
            &db_dir.path().join("db-busy"),
            StoreOptions {
                bulk_threshold: E,
                group_commit: true,
                ..StoreOptions::default()
            },
        )
        .unwrap(),
    );
    s2.load(&[bytes(0, 1)]).unwrap();

    let (sealed2_tx, sealed2_rx) = mpsc::channel();
    let (go2_tx, go2_rx) = mpsc::channel();
    let go2_rx = Arc::new(parking_lot::Mutex::new(go2_rx));
    s2.set_failpoint(
        "group-sealed",
        Some(Arc::new(move |_| {
            let _ = sealed2_tx.send(());
            let _ = go2_rx.lock().recv();
        })),
    );
    let s2_bg = Arc::clone(&s2);
    let first2 = std::thread::spawn(move || {
        update(
            &s2_bg,
            "INSERT DATA { <http://ex.org/gc-busy> <http://ex.org/p> 1 }",
            &QueryOptions::default(),
        )
    });
    sealed2_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let src = Source::from_path(&bad_nt, None).unwrap();
    let no_wait = WriteOptions {
        no_wait: true,
        ..WriteOptions::default()
    };
    // Direct check of refusal-drain classification: after_refusal returns Error::WriterBusy
    // for an in-flight group under no_wait=true, and the classifier must keep it Store.
    let classified = s2.classify_refusal_after_drain(
        true,
        Error::RdfParse("not valid ntriples".into()),
        &no_wait,
    );
    assert!(
        matches!(classified, SparqlLoadError::Store(Error::WriterBusy)),
        "after_refusal WriterBusy must stay SparqlLoadError::Store: {}",
        match classified {
            SparqlLoadError::Source(e) => format!("Source({e:?})"),
            SparqlLoadError::Store(e) => format!("Store({e:?})"),
        }
    );
    // Writer-entry refusal check: try_write_with refuses before parsing under no_wait=true.
    let res = s2.load_sparql(&src, CommitKind::Update, &no_wait, None);
    assert!(
        matches!(res, Err(SparqlLoadError::Store(Error::WriterBusy))),
        "writer-entry WriterBusy must stay SparqlLoadError::Store: {}",
        match res {
            Ok(_) => "Ok".to_string(),
            Err(SparqlLoadError::Source(e)) => format!("Source({e:?})"),
            Err(SparqlLoadError::Store(e)) => format!("Store({e:?})"),
        }
    );
    let mut silent_opts = QueryOptions::default();
    silent_opts.write.no_wait = true;
    let err = update(
        &s2,
        &format!("LOAD SILENT <file://{}>", bad_nt.display()),
        &silent_opts,
    )
    .unwrap_err();
    assert!(matches!(err, Error::WriterBusy), "{err:?}");

    let _ = go2_tx.send(());
    s2.set_failpoint("group-sealed", None);
    first2.join().unwrap().unwrap();

    // Once the group is idle, the same refusal classifier preserves a source parse error.
    let classified_idle = s2.classify_refusal_after_drain(
        true,
        Error::RdfParse("not valid ntriples".into()),
        &no_wait,
    );
    assert!(
        matches!(classified_idle, SparqlLoadError::Source(Error::RdfParse(_))),
        "idle source error must stay SparqlLoadError::Source: {}",
        match classified_idle {
            SparqlLoadError::Source(e) => format!("Source({e:?})"),
            SparqlLoadError::Store(e) => format!("Store({e:?})"),
        }
    );
}

#[test]
fn sparql_load_bulk_unchanged_protected_quota_and_prefixes() {
    use crate::error::Error;
    use crate::sparql::{QueryOptions, update::update};

    let tmp = tempfile::tempdir().unwrap();
    let file_url = |p: &std::path::Path| format!("file://{}", p.display());

    // Build two distinct above-threshold N-Triples files and a Turtle file restating the
    // first file's ground triples under a fresh prefix.
    let large_bytes = ntriples(0, E + 2);
    let large_nt = tmp.path().join("large.nt");
    std::fs::write(&large_nt, &large_bytes).unwrap();
    let mut extra_buf = String::new();
    for i in 0..200u64 {
        extra_buf.push_str(&format!(
            "<http://ex.org/extra/{i}> <http://ex.org/p> \"unique-payload-{i:0>60}\" .\n"
        ));
    }
    let extra_nt = tmp.path().join("extra.nt");
    std::fs::write(&extra_nt, &extra_buf).unwrap();

    let large_str = std::str::from_utf8(&large_bytes).unwrap();
    // Same ground triples as large.nt, plus enough long prefix declarations that the
    // scratch generation's meta.json is strictly larger on disk than the existing one.
    let mut padded_ttl = String::new();
    for i in 0..40 {
        padded_ttl.push_str(&format!(
            "@prefix qpad{i}: <http://ex.org/quota-pad-{i}/{}> .\n",
            "a".repeat(160)
        ));
    }
    padded_ttl.push_str(large_str);
    let padded_ttl_path = tmp.path().join("same_padded.ttl");
    std::fs::write(&padded_ttl_path, &padded_ttl).unwrap();

    let same_ttl = format!("@prefix fresh: <http://ex.org/fresh/> .\n{large_str}");
    let same_ttl_path = tmp.path().join("same_prefix.ttl");
    std::fs::write(&same_ttl_path, &same_ttl).unwrap();

    let same_ttl2 = format!("@prefix fresh2: <http://ex.org/fresh2/> .\n{large_str}");
    let same_ttl2_path = tmp.path().join("same_prefix2.ttl");
    std::fs::write(&same_ttl2_path, &same_ttl2).unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("db");
    let persistent = Store::open(
        &db_path,
        StoreOptions {
            bulk_threshold: E,
            ..StoreOptions::default()
        },
    )
    .unwrap();
    let first = update(
        &persistent,
        &format!("LOAD <{}>", file_url(&large_nt)),
        &QueryOptions::default(),
    )
    .unwrap();
    let first_r = first.commit.as_ref().unwrap();
    assert!(first_r.committed && first_r.commit.bulk);
    let head_seq = first_r.commit.seq;

    // 1. Protected branch: repeat ground bulk LOAD succeeds with unchanged head (live and
    // ordinary dry-run), while a load adding new quads is still refused with Error::Branch.
    persistent.set_branch_protected("main", true).unwrap();
    let repeat = update(
        &persistent,
        &format!("LOAD <{}>", file_url(&large_nt)),
        &QueryOptions::default(),
    )
    .unwrap();
    let repeat_r = repeat.commit.as_ref().unwrap();
    assert!(!repeat_r.committed);
    assert_eq!(repeat_r.commit.seq, head_seq);
    assert_eq!(repeat.inserted, 0);

    let mut dry = QueryOptions::default();
    dry.write.dry_run = Some(Default::default());
    let p = crate::preview::catch(
        update(
            &persistent,
            &format!("LOAD <{}>", file_url(&large_nt)),
            &dry,
        )
        .map(|st| st.commit.unwrap()),
    )
    .unwrap();
    assert!(p.commit.is_none());
    assert!(p.graphs.is_empty());

    let err = update(
        &persistent,
        &format!("LOAD <{}>", file_url(&extra_nt)),
        &QueryOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Branch(_)), "{err:?}");

    let err = update(
        &persistent,
        &format!("LOAD <{}>", file_url(&extra_nt)),
        &dry,
    )
    .unwrap_err();
    assert!(matches!(err, Error::Branch(_)), "{err:?}");

    let mut preview_branch = dry.clone();
    preview_branch.write.preview_as_new_branch = true;
    let p = crate::preview::catch(
        update(
            &persistent,
            &format!("LOAD <{}>", file_url(&extra_nt)),
            &preview_branch,
        )
        .map(|st| st.commit.unwrap()),
    )
    .unwrap();
    assert!(p.commit.as_ref().unwrap().bulk);

    // 2. Lowered quota: duplicate-only bulk LOAD discards the scratch generation without
    // tripping publication-fit checks even when the scratch generation is larger on disk
    // than the current one, while a changing load still fails with BudgetExceeded(DatasetBytes)
    // (or records storage.refused on dry run).
    persistent.set_branch_protected("main", false).unwrap();
    persistent.set_quota(Some(1)).unwrap();

    let repeat_q = update(
        &persistent,
        &format!("LOAD <{}>", file_url(&padded_ttl_path)),
        &QueryOptions::default(),
    )
    .unwrap();
    assert!(!repeat_q.commit.as_ref().unwrap().committed);
    assert_eq!(repeat_q.commit.as_ref().unwrap().commit.seq, head_seq);

    let p_q = crate::preview::catch(
        update(
            &persistent,
            &format!("LOAD <{}>", file_url(&padded_ttl_path)),
            &dry,
        )
        .map(|st| st.commit.unwrap()),
    )
    .unwrap();
    assert!(p_q.commit.is_none());
    assert!(p_q.storage.refused.is_none());

    let err = update(
        &persistent,
        &format!("LOAD <{}>", file_url(&extra_nt)),
        &QueryOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            Error::BudgetExceeded(b) if b.kind == crate::error::BudgetKind::DatasetBytes
        ),
        "{err:?}"
    );

    let p_over = crate::preview::catch(
        update(
            &persistent,
            &format!("LOAD <{}>", file_url(&extra_nt)),
            &dry,
        )
        .map(|st| st.commit.unwrap()),
    )
    .unwrap();
    assert!(p_over.commit.is_some());
    assert!(p_over.storage.refused.is_some());

    // 3. Prefix registration on duplicate-only bulk LOAD: dry run skips registration, live
    // load registers and persists the fresh prefix without allocating a quad commit, and a
    // metadata write failure surfaces as a store error even under LOAD SILENT.
    persistent.set_quota(None).unwrap();
    assert_eq!(
        persistent
            .source_plan(
                SparqlLoad,
                &[Source::from_path(&same_ttl_path, None).unwrap()],
                &WriteOptions::default()
            )
            .unwrap(),
        Bulk
    );
    let p_ttl = crate::preview::catch(
        update(
            &persistent,
            &format!("LOAD <{}>", file_url(&same_ttl_path)),
            &dry,
        )
        .map(|st| st.commit.unwrap()),
    )
    .unwrap();
    assert!(p_ttl.commit.is_none());
    assert!(!persistent.prefixes().contains_key("fresh"));

    let st_ttl = update(
        &persistent,
        &format!("LOAD <{}>", file_url(&same_ttl_path)),
        &QueryOptions::default(),
    )
    .unwrap();
    let r_ttl = st_ttl.commit.as_ref().unwrap();
    assert!(!r_ttl.committed);
    assert_eq!(r_ttl.commit.seq, head_seq);
    assert_eq!(
        persistent.prefixes().get("fresh").map(String::as_str),
        Some("http://ex.org/fresh/")
    );

    // Reopen to confirm the prefix persisted without allocating a quad commit, then inject
    // a failure before rename (preserving prefixes.json) so persisting a second fresh prefix
    // fails under SILENT without polluting memory, retries cleanly once repaired, and
    // survives reopen.
    drop(persistent);
    let persistent = Store::open(
        &db_path,
        StoreOptions {
            bulk_threshold: E,
            ..StoreOptions::default()
        },
    )
    .unwrap();
    assert_eq!(persistent.snapshot().commit, head_seq);
    assert_eq!(
        persistent.prefixes().get("fresh").map(String::as_str),
        Some("http://ex.org/fresh/")
    );

    let prefixes_tmp = db_path.join("prefixes.tmp");
    std::fs::create_dir(&prefixes_tmp).unwrap();
    let err = update(
        &persistent,
        &format!("LOAD SILENT <{}>", file_url(&same_ttl2_path)),
        &QueryOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err:?}");
    assert!(
        !persistent.prefixes().contains_key("fresh2"),
        "failed prefix persistence must not leave the prefix in memory"
    );
    assert_eq!(
        persistent.prefixes().get("fresh").map(String::as_str),
        Some("http://ex.org/fresh/")
    );

    std::fs::remove_dir(&prefixes_tmp).unwrap();
    let retry = update(
        &persistent,
        &format!("LOAD SILENT <{}>", file_url(&same_ttl2_path)),
        &QueryOptions::default(),
    )
    .unwrap();
    let retry_r = retry.commit.as_ref().unwrap();
    assert!(!retry_r.committed);
    assert_eq!(retry_r.commit.seq, head_seq);
    assert_eq!(
        persistent.prefixes().get("fresh2").map(String::as_str),
        Some("http://ex.org/fresh2/")
    );

    drop(persistent);
    let reopened = Store::open(
        &db_path,
        StoreOptions {
            bulk_threshold: E,
            ..StoreOptions::default()
        },
    )
    .unwrap();
    assert_eq!(reopened.snapshot().commit, head_seq);
    assert_eq!(
        reopened.prefixes().get("fresh").map(String::as_str),
        Some("http://ex.org/fresh/")
    );
    assert_eq!(
        reopened.prefixes().get("fresh2").map(String::as_str),
        Some("http://ex.org/fresh2/")
    );
}

#[test]
fn sparql_load_bulk_refusal_and_duplicate_preserve_active_compaction() {
    use crate::error::Error;
    use crate::sparql::{QueryOptions, update::update};
    use crate::store::CompactOptions;
    use std::sync::atomic::{AtomicBool, Ordering};

    let tmp = tempfile::tempdir().unwrap();
    let file_url = |p: &std::path::Path| format!("file://{}", p.display());

    let large_bytes = ntriples(0, E + 2);
    let large_nt = tmp.path().join("large.nt");
    std::fs::write(&large_nt, &large_bytes).unwrap();

    let extra_nt = tmp.path().join("extra.nt");
    std::fs::write(&extra_nt, ntriples(10_000, E + 2)).unwrap();

    let large_str = std::str::from_utf8(&large_bytes).unwrap();
    let same_ttl_path = tmp.path().join("same_prefix.ttl");
    std::fs::write(
        &same_ttl_path,
        format!("@prefix fresh: <http://ex.org/fresh/> .\n{large_str}"),
    )
    .unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("db");
    let persistent = Store::open(
        &db_path,
        StoreOptions {
            bulk_threshold: E,
            ..StoreOptions::default()
        },
    )
    .unwrap();
    let first = update(
        &persistent,
        &format!("LOAD <{}>", file_url(&large_nt)),
        &QueryOptions::default(),
    )
    .unwrap();
    assert!(first.commit.as_ref().unwrap().committed && first.commit.as_ref().unwrap().commit.bulk);
    update(
        &persistent,
        "INSERT DATA { <http://ex.org/delta1> <http://ex.org/p> 1 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let head_seq = persistent.snapshot().commit;

    // 1. On a protected branch, a refused changing bulk LOAD (live or dry-run) and a
    // duplicate-only live bulk LOAD must both leave an active compaction's tap intact.
    persistent.set_branch_protected("main", true).unwrap();
    let ran = Arc::new(AtomicBool::new(false));
    let extra_url = file_url(&extra_nt);
    let same_url = file_url(&same_ttl_path);
    persistent.set_failpoint(
        "compact-built",
        Some(Arc::new({
            let ran = Arc::clone(&ran);
            let extra_url = extra_url.clone();
            let same_url = same_url.clone();
            move |st: &Store| {
                let err = update(st, &format!("LOAD <{extra_url}>"), &QueryOptions::default())
                    .unwrap_err();
                assert!(matches!(err, Error::Branch(_)), "{err:?}");

                let mut dry = QueryOptions::default();
                dry.write.dry_run = Some(Default::default());
                let err = update(st, &format!("LOAD <{extra_url}>"), &dry).unwrap_err();
                assert!(matches!(err, Error::Branch(_)), "{err:?}");

                let dup =
                    update(st, &format!("LOAD <{same_url}>"), &QueryOptions::default()).unwrap();
                let dup_r = dup.commit.as_ref().unwrap();
                assert!(!dup_r.committed);
                assert_eq!(dup_r.commit.seq, head_seq);
                ran.store(true, Ordering::Release);
            }
        })),
    );
    let report = persistent.compact_with(&CompactOptions::default()).unwrap();
    persistent.set_failpoint("compact-built", None);
    assert!(ran.load(Ordering::Acquire));
    assert_eq!(
        report.abandoned, None,
        "protected refusal and duplicate-only bulk LOAD must not abort compaction: {report:?}"
    );
    assert_eq!(persistent.snapshot().commit, head_seq);
    assert_eq!(
        persistent.prefixes().get("fresh").map(String::as_str),
        Some("http://ex.org/fresh/")
    );

    // 2. Once unprotected, a changing live bulk LOAD during compaction commits a new
    // generation and abandons the active compaction as before.
    persistent.set_branch_protected("main", false).unwrap();
    update(
        &persistent,
        "INSERT DATA { <http://ex.org/delta2> <http://ex.org/p> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let ran2 = Arc::new(AtomicBool::new(false));
    persistent.set_failpoint(
        "compact-built",
        Some(Arc::new({
            let ran2 = Arc::clone(&ran2);
            let extra_url = extra_url.clone();
            move |st: &Store| {
                let committed =
                    update(st, &format!("LOAD <{extra_url}>"), &QueryOptions::default()).unwrap();
                let r = committed.commit.as_ref().unwrap();
                assert!(r.committed && r.commit.bulk);
                ran2.store(true, Ordering::Release);
            }
        })),
    );
    let report2 = persistent.compact_with(&CompactOptions::default()).unwrap();
    persistent.set_failpoint("compact-built", None);
    assert!(ran2.load(Ordering::Acquire));
    assert!(
        report2.abandoned.is_some(),
        "changing bulk LOAD must supersede active compaction: {report2:?}"
    );
    let final_seq = persistent.snapshot().commit;
    assert!(final_seq > head_seq + 1);

    drop(persistent);
    let reopened = Store::open(
        &db_path,
        StoreOptions {
            bulk_threshold: E,
            ..StoreOptions::default()
        },
    )
    .unwrap();
    assert_eq!(reopened.snapshot().commit, final_seq);
    assert_eq!(
        reopened.prefixes().get("fresh").map(String::as_str),
        Some("http://ex.org/fresh/")
    );
}

/// Measures wall time, 1 ms lock-probe samples (`first_busy_ms` estimating pre-lock time
/// and `busy_span_ms` spanning first-to-last observed `WriterBusy`), concurrent reader
/// latency, and peak RSS (`VmHWM`) for a large single-operation SPARQL `LOAD` (`file:` and
/// `http:`) under the default bulk threshold versus forced transactional (`bulk_threshold =
/// u64::MAX`). Filter with `SPARQL_LOAD_MODE=bulk|txn`, `SPARQL_LOAD_KIND=persistent|memory`,
/// and `SPARQL_LOAD_SOURCE=file|http` so each arm can run in its own process for an exact
/// `VmHWM` reading.
/// Run with:
/// `SPARQL_LOAD_QUADS=300000 mbx test --release -p sparkles-core --lib -- --ignored sparql_load_profile --nocapture`
#[test]
#[ignore]
fn sparql_load_profile() {
    use crate::outbound::OutboundPolicy;
    use crate::sparql::{QueryOptions, update::update};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    let env_u64 = |k: &str, d: u64| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let n = env_u64("SPARQL_LOAD_QUADS", 300_000);
    let rounds = env_u64("SPARQL_LOAD_ROUNDS", 3);
    let mode_filter = std::env::var("SPARQL_LOAD_MODE").unwrap_or_else(|_| "both".into());
    let kind_filter = std::env::var("SPARQL_LOAD_KIND").unwrap_or_else(|_| "both".into());
    let source_filter = std::env::var("SPARQL_LOAD_SOURCE").unwrap_or_else(|_| "both".into());

    let data = ntriples(0, n);
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("big.nt");
    std::fs::write(&file_path, &data).unwrap();
    let file_url = format!("file://{}", file_path.display());

    let vm_hwm_kib = || -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmHWM:"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(0)
    };

    let run_with_probes =
        |s: &Arc<Store>, f: &dyn Fn(&Store) -> crate::sparql::update::UpdateStats| {
            let done = Arc::new(AtomicBool::new(false));
            let s_reader = Arc::clone(s);
            let done_r = Arc::clone(&done);
            let reader_handle = std::thread::spawn(move || {
                let mut reads = 0u64;
                let mut max_us = 0u128;
                while !done_r.load(Ordering::Relaxed) {
                    let rt = Instant::now();
                    let _ = s_reader.snapshot().len();
                    max_us = max_us.max(rt.elapsed().as_micros());
                    reads += 1;
                    std::thread::sleep(Duration::from_micros(250));
                }
                (reads, max_us)
            });
            let s_lock = Arc::clone(s);
            let done_l = Arc::clone(&done);
            let t_start = Instant::now();
            let lock_handle = std::thread::spawn(move || {
                let mut first_busy: Option<Duration> = None;
                let mut last_busy: Option<Duration> = None;
                let mut busy_samples = 0u64;
                let mut total_samples = 0u64;
                let probe_opts = WriteOptions {
                    no_wait: true,
                    ..Default::default()
                };
                while !done_l.load(Ordering::Relaxed) {
                    total_samples += 1;
                    match s_lock.try_write_with(CommitKind::Update, probe_opts.clone()) {
                        Err(crate::error::Error::WriterBusy) => {
                            let now = t_start.elapsed();
                            if first_busy.is_none() {
                                first_busy = Some(now);
                            }
                            last_busy = Some(now);
                            busy_samples += 1;
                        }
                        Ok(txn) => drop(txn),
                        Err(_) => {}
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                (first_busy, last_busy, busy_samples, total_samples)
            });

            let t0 = Instant::now();
            let st = f(s);
            let elapsed = t0.elapsed();
            done.store(true, Ordering::Relaxed);
            let (reads, max_read_us) = reader_handle.join().unwrap();
            let (first_busy, last_busy, busy_samples, total_samples) = lock_handle.join().unwrap();
            let busy_span_ms = match (first_busy, last_busy) {
                (Some(a), Some(b)) => b.saturating_sub(a).as_secs_f64() * 1000.0,
                _ => 0.0,
            };
            let bulk = st.commit.as_ref().unwrap().commit.bulk;
            (
                elapsed,
                bulk,
                reads,
                max_read_us,
                first_busy.map_or(0.0, |d| d.as_secs_f64() * 1000.0),
                busy_span_ms,
                busy_samples,
                total_samples,
            )
        };

    let kinds: Vec<bool> = match kind_filter.as_str() {
        "persistent" => vec![true],
        "memory" => vec![false],
        _ => vec![true, false],
    };
    let modes: Vec<(&str, u64)> = match mode_filter.as_str() {
        "bulk" => vec![("bulk", StoreOptions::default().bulk_threshold)],
        "txn" => vec![("txn", u64::MAX)],
        _ => vec![
            ("bulk", StoreOptions::default().bulk_threshold),
            ("txn", u64::MAX),
        ],
    };
    let sources: Vec<&str> = match source_filter.as_str() {
        "file" => vec!["file"],
        "http" => vec!["http"],
        _ => vec!["file", "http"],
    };

    for round in 0..rounds {
        for &persistent in &kinds {
            let kind = if persistent {
                "persistent"
            } else {
                "in-memory"
            };
            for &(label, threshold) in &modes {
                for &src_kind in &sources {
                    let db_dir = tempfile::tempdir().unwrap();
                    let opts = StoreOptions {
                        bulk_threshold: threshold,
                        ..StoreOptions::default()
                    };
                    let s = Arc::new(if persistent {
                        Store::open(&db_dir.path().join("db"), opts).unwrap()
                    } else {
                        Store::in_memory(opts)
                    });
                    s.load(&[bytes(10 * n, 1000)]).unwrap();

                    let (
                        elapsed,
                        bulk,
                        reads,
                        max_read_us,
                        first_busy_ms,
                        busy_span_ms,
                        busy,
                        total,
                    ) = if src_kind == "file" {
                        run_with_probes(&s, &|st| {
                            update(st, &format!("LOAD <{file_url}>"), &QueryOptions::default())
                                .unwrap()
                        })
                    } else {
                        let payload = data.clone();
                        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                        let port = listener.local_addr().unwrap().port();
                        let srv = std::thread::spawn(move || {
                            if let Ok((mut stream, _)) = listener.accept() {
                                let mut head = Vec::new();
                                let mut b = [0u8; 1];
                                while !head.ends_with(b"\r\n\r\n")
                                    && stream.read(&mut b).unwrap_or(0) == 1
                                {
                                    head.push(b[0]);
                                }
                                let _ = write!(
                                    stream,
                                    "HTTP/1.1 200 OK\r\ncontent-type: application/n-triples\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                                    payload.len()
                                );
                                let _ = stream.write_all(&payload);
                            }
                        });
                        let http_opts = QueryOptions {
                            outbound: OutboundPolicy {
                                allow_private: true,
                                max_response_bytes: (data.len() as u64) + (1 << 20),
                                max_request_bytes: (data.len() as u64) + (1 << 20),
                                ..Default::default()
                            },
                            ..Default::default()
                        };
                        let res = run_with_probes(&s, &|st| {
                            update(
                                st,
                                &format!("LOAD <http://127.0.0.1:{port}/big.nt>"),
                                &http_opts,
                            )
                            .unwrap()
                        });
                        let _ = srv.join();
                        res
                    };
                    drop(s);
                    println!(
                        "round {round} | {kind:10} | {src_kind:4} | {label:4} | {n} quads | wall {:.3}s | bulk={bulk} | first_busy={first_busy_ms:.1}ms | busy_span={busy_span_ms:.1}ms | lock_busy={busy}/{total} | reads={reads} max={max_read_us}us | VmHWM={} MiB",
                        elapsed.as_secs_f64(),
                        vm_hwm_kib() / 1024
                    );
                }
            }
        }
    }
}

/// A tracked LOAD that is refused after its intent was persisted retires the intent
/// and leaves the writer usable for the same sequence, on the bulk, WAL and in-memory
/// paths. The bulk path also removes its unpublished generation. A failed retirement
/// keeps the writer poisoned.
#[test]
fn a_refusal_after_the_commit_intent_retires_it_on_every_path() {
    use crate::error::Error;
    use crate::guard::CommitIntent;
    use crate::sparql::{QueryOptions, update::update, update::update_with_commit_hook};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};

    fn generations(root: &std::path::Path) -> usize {
        std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("gen-"))
            .count()
    }
    struct Intent {
        root: Option<std::path::PathBuf>,
        cancel: Arc<AtomicBool>,
        retire_fails: bool,
        persisted: AtomicU64,
        retired: AtomicU64,
        generations: AtomicU64,
    }
    impl CommitIntent for Intent {
        fn persist(&self, seq: u64) -> crate::Result<()> {
            self.persisted.store(seq, SeqCst);
            if let Some(root) = &self.root {
                self.generations.store(generations(root) as u64, SeqCst);
            }
            // the cancellation arrives while the intent is being synced
            self.cancel.store(true, SeqCst);
            Ok(())
        }
        fn retire(&self, seq: u64) -> crate::Result<()> {
            self.retired.store(seq, SeqCst);
            if self.retire_fails {
                return Err(Error::invalid("the retirement failed"));
            }
            Ok(())
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("load.nt");
    std::fs::write(&source, ntriples(0, E + 1)).unwrap();
    let load = format!("LOAD <file://{}>", source.display());
    for (name, persistent, threshold, retire_fails, bulk) in [
        ("bulk", true, E, false, true),
        ("wal", true, 1_000, false, false),
        ("memory", false, 1_000, false, false),
        ("bulk, retirement fails", true, E, true, true),
        ("wal, retirement fails", true, 1_000, true, false),
    ] {
        let opts = StoreOptions {
            bulk_threshold: threshold,
            ..StoreOptions::default()
        };
        let root = tmp.path().join(name.replace([',', ' '], "-"));
        let s = if persistent {
            Store::open(&root, opts).unwrap()
        } else {
            Store::in_memory(opts)
        };
        update(
            &s,
            "INSERT DATA { <urn:a> <urn:p> <urn:o> }",
            &QueryOptions::default(),
        )
        .unwrap();
        let head = s.snapshot().commit;
        let before = persistent.then(|| generations(&root));
        let cancel = Arc::new(AtomicBool::new(false));
        let intent = Intent {
            root: persistent.then(|| root.clone()),
            cancel: cancel.clone(),
            retire_fails,
            persisted: AtomicU64::new(0),
            retired: AtomicU64::new(0),
            generations: AtomicU64::new(0),
        };
        let mut q = QueryOptions::default();
        q.write.cancel = Some(cancel);
        let refused = update_with_commit_hook(&s, &load, &q, &intent);
        assert!(
            matches!(refused, Err(Error::Cancelled)),
            "{name}: {refused:?}"
        );
        assert_eq!(intent.persisted.load(SeqCst), head + 1, "{name}");
        assert_eq!(intent.retired.load(SeqCst), head + 1, "{name}");
        assert_eq!(s.snapshot().commit, head, "{name}");
        assert_eq!(s.snapshot().len(), 1, "{name}");
        if let Some(before) = before {
            // the candidate generation existed when the intent was persisted
            assert_eq!(
                intent.generations.load(SeqCst) as usize,
                before + usize::from(bulk),
                "{name}"
            );
            assert_eq!(generations(&root), before, "{name}");
        }
        let next = update(
            &s,
            "INSERT DATA { <urn:b> <urn:p> <urn:o> }",
            &QueryOptions::default(),
        );
        if retire_fails {
            assert!(matches!(next, Err(Error::Poisoned)), "{name}: {next:?}");
            continue;
        }
        let receipt = next.unwrap().commit.expect("receipt");
        assert_eq!(receipt.commit.seq, head + 1, "{name}");
        drop(s);
        if persistent {
            let s = Store::open(&root, StoreOptions::default()).unwrap();
            assert_eq!(s.snapshot().commit, head + 1, "{name}");
            assert_eq!(s.snapshot().len(), 2, "{name}");
        }
    }
}
