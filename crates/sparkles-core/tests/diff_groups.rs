//! A diff grouped by class (`Diff::group_by_class`, U01 Phase 4): against the same
//! grouping done in the test from the materialized states, for random histories with
//! types that come and go; resources with several classes and with none; the group cap;
//! a view that hides a type triple; budgets and cancellation.

use oxrdf::{Quad, Term};
use sparkles_core::access::{GraphAccess, GraphRule, Graphs};
use sparkles_core::history::{At, HistoryOptions};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{ClassGroups, DiffOp, DiffOptions, ObjectGroup, Store, StoreOptions};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A quad of a small universe where subjects and objects are typed and untyped.
fn quad(r: &mut Rng) -> String {
    let s = format!("<urn:s{}>", r.below(6));
    let (p, o) = match r.below(4) {
        0 => (TYPE.to_string(), format!("<urn:C{}>", r.below(3))),
        1 => ("urn:p".to_string(), format!("<urn:s{}>", r.below(6))),
        2 => ("urn:p".to_string(), format!("<urn:o{}>", r.below(2))),
        _ => ("urn:v".to_string(), format!("{}", r.below(3))),
    };
    match r.below(3) {
        0 => format!("GRAPH <urn:g1> {{ {s} <{p}> {o} }}"),
        _ => format!("{s} <{p}> {o} ."),
    }
}

fn snapshot(s: &Store, seq: u64) -> Arc<sparkles_core::store::Snapshot> {
    s.snapshot_at(&At::Commit(seq), &HistoryOptions::default())
        .unwrap()
        .0
}

fn quads_of(s: &Store, seq: u64) -> Vec<Quad> {
    let snap = snapshot(s, seq);
    let mut out = Vec::new();
    snap.for_each_quad(|q| {
        out.push(snap.quad_to_terms(q).unwrap());
        Ok(())
    })
    .unwrap();
    out
}

fn iri(t: &Term) -> Option<String> {
    match t {
        Term::NamedNode(n) => Some(n.as_str().to_string()),
        _ => None,
    }
}

/// The grouping of one diff, done from the states: each side's resources, their classes
/// in that state, the class most of them share, then the counts.
fn reference(
    s: &Store,
    a: u64,
    b: u64,
    changes: &[(DiffOp, Quad)],
) -> BTreeMap<(Option<String>, String, ObjectGroup), (u64, u64)> {
    let classes_of = |seq: u64, op: DiffOp| -> HashMap<Term, String> {
        let mut resources: HashSet<Term> = HashSet::new();
        for (o, q) in changes.iter().filter(|(o, _)| *o == op) {
            let _ = o;
            resources.insert(q.subject.clone().into());
            if q.predicate.as_str() != TYPE && !matches!(q.object, Term::Literal(_)) {
                resources.insert(q.object.clone());
            }
        }
        let mut found: HashMap<Term, BTreeSet<String>> = HashMap::new();
        for q in quads_of(s, seq) {
            let subj: Term = q.subject.clone().into();
            if q.predicate.as_str() == TYPE
                && resources.contains(&subj)
                && let Some(c) = iri(&q.object)
            {
                found.entry(subj).or_default().insert(c);
            }
        }
        let mut freq: HashMap<String, u64> = HashMap::new();
        for cs in found.values() {
            for c in cs {
                *freq.entry(c.clone()).or_default() += 1;
            }
        }
        found
            .into_iter()
            .map(|(r, cs)| {
                let best = cs
                    .into_iter()
                    .max_by(|x, y| freq[x].cmp(&freq[y]).then_with(|| y.cmp(x)))
                    .unwrap();
                (r, best)
            })
            .collect()
    };
    let removed = classes_of(a, DiffOp::Remove);
    let added = classes_of(b, DiffOp::Add);
    let mut out = BTreeMap::new();
    for (op, q) in changes {
        let cl = if *op == DiffOp::Add { &added } else { &removed };
        let subj: Term = q.subject.clone().into();
        let object = match &q.object {
            Term::Literal(l) => ObjectGroup::Literal(l.datatype().as_str().to_string()),
            o if q.predicate.as_str() == TYPE => ObjectGroup::Class(iri(o)),
            o => match cl.get(o) {
                Some(c) => ObjectGroup::Class(Some(c.clone())),
                None if matches!(o, Term::NamedNode(_)) => ObjectGroup::Iri,
                None => ObjectGroup::Class(None),
            },
        };
        let e = out
            .entry((
                cl.get(&subj).cloned(),
                q.predicate.as_str().to_string(),
                object,
            ))
            .or_insert((0u64, 0u64));
        match op {
            DiffOp::Add => e.0 += 1,
            DiffOp::Remove => e.1 += 1,
        }
    }
    out
}

fn grouped(s: &Store, a: u64, b: u64, o: &DiffOptions, limit: usize) -> ClassGroups {
    let d = s.diff(&At::Commit(a), &At::Commit(b), o).unwrap();
    let (fa, fb) = (d.from.commit.seq, d.to.commit.seq);
    d.group_by_class(
        &mut |op| Ok(snapshot(s, if op == DiffOp::Add { fb } else { fa })),
        o,
        limit,
        &mut |_| Ok(()),
    )
    .unwrap()
}

fn as_map(g: &ClassGroups) -> BTreeMap<(Option<String>, String, ObjectGroup), (u64, u64)> {
    g.groups
        .iter()
        .map(|x| {
            (
                (
                    x.subject_class.clone(),
                    x.predicate.clone(),
                    x.object.clone(),
                ),
                (x.added, x.removed),
            )
        })
        .collect()
}

#[test]
fn groups_match_a_grouping_of_the_states() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    let mut r = Rng(11);
    for _ in 0..60 {
        let mut u = String::from("INSERT DATA { ");
        for _ in 0..3 {
            u.push_str(&quad(&mut r));
            u.push(' ');
        }
        u.push('}');
        update(&s, &u, &QueryOptions::default()).unwrap();
        let u = format!("DELETE DATA {{ {} }}", quad(&mut r));
        update(&s, &u, &QueryOptions::default()).unwrap();
    }
    let head = s.head_commit().seq;
    let o = DiffOptions::default();
    for _ in 0..40 {
        let (a, b) = (r.below(head + 1), r.below(head + 1));
        let d = s.diff(&At::Commit(a), &At::Commit(b), &o).unwrap();
        let changes: Vec<(DiffOp, Quad)> = d.iter().collect();
        let want = reference(&s, d.from.commit.seq, d.to.commit.seq, &changes);
        let got = grouped(&s, a, b, &o, 5_000);
        assert_eq!(as_map(&got), want, "{a}..{b}");
        assert_eq!(got.total, want.len());
        assert!(!got.truncated);
        // the largest groups first
        let sizes: Vec<u64> = got.groups.iter().map(|g| g.added + g.removed).collect();
        assert!(sizes.windows(2).all(|w| w[0] >= w[1]), "{sizes:?}");
    }
}

#[test]
fn several_classes_none_and_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    let up = |u: &str| update(&s, u, &QueryOptions::default()).unwrap();
    up("INSERT DATA { <urn:a> a <urn:B> , <urn:A> . <urn:b> a <urn:B> . }");
    up(
        "INSERT DATA { <urn:a> <urn:p> <urn:b> . <urn:b> <urn:p> <urn:a> . <urn:n> <urn:p> _:x . <urn:n> <urn:q> \"s\"@en }",
    );
    let g = grouped(&s, 1, 2, &DiffOptions::default(), 5_000);
    let m = as_map(&g);
    // a's classes are A and B; B is shared by more of the resources looked up
    let b = Some("urn:B".to_string());
    assert_eq!(
        m.get(&(b.clone(), "urn:p".into(), ObjectGroup::Class(b.clone()))),
        Some(&(2, 0)),
        "{m:?}"
    );
    // no class: a null subject class, and a blank node object without one
    assert_eq!(
        m.get(&(None, "urn:p".into(), ObjectGroup::Class(None))),
        Some(&(1, 0)),
        "{m:?}"
    );
    assert_eq!(
        m.get(&(
            None,
            "urn:q".into(),
            ObjectGroup::Literal("http://www.w3.org/1999/02/22-rdf-syntax-ns#langString".into())
        )),
        Some(&(1, 0)),
        "{m:?}"
    );
    // the cap keeps the largest groups and says so
    let capped = grouped(&s, 1, 2, &DiffOptions::default(), 1);
    assert_eq!(capped.groups.len(), 1);
    assert_eq!(capped.total, 3);
    assert!(capped.truncated);
    assert_eq!(capped.groups[0].added, 2);
}

#[test]
fn a_view_that_hides_a_type_triple_leaves_its_class_out() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    let up = |u: &str| update(&s, u, &QueryOptions::default()).unwrap();
    up(
        "INSERT DATA { GRAPH <urn:hidden> { <urn:a> a <urn:Secret> } GRAPH <urn:seen> { <urn:b> a <urn:Open> } }",
    );
    up("INSERT DATA { GRAPH <urn:seen> { <urn:a> <urn:p> <urn:b> } }");
    let all = grouped(&s, 1, 2, &DiffOptions::default(), 5_000);
    assert_eq!(all.groups[0].subject_class.as_deref(), Some("urn:Secret"));
    let view = GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:seen"], &[])),
        Graphs::none(),
    );
    let o = DiffOptions {
        graphs: Some(Arc::new(view)),
        ..Default::default()
    };
    let g = grouped(&s, 1, 2, &o, 5_000);
    assert_eq!(g.groups.len(), 1);
    assert_eq!(g.groups[0].subject_class, None);
    assert_eq!(
        g.groups[0].object,
        ObjectGroup::Class(Some("urn:Open".into()))
    );
}

#[test]
fn budgets_and_cancellation_stop_a_grouping() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    let mut u = String::from("INSERT DATA {");
    for i in 0..500 {
        u.push_str(&format!(" <urn:r{i}> a <urn:C> ; <urn:p> {i} ."));
    }
    u.push('}');
    update(&s, &u, &QueryOptions::default()).unwrap();
    let o = DiffOptions::default();
    let d = s.diff(&At::Commit(0), &At::Commit(1), &o).unwrap();
    // the charge refuses past a budget
    let mut held = 0u64;
    let r = d.group_by_class(&mut |_| Ok(snapshot(&s, 1)), &o, 5_000, &mut |n| {
        held += n;
        if held > 10_000 {
            Err(sparkles_core::Error::Timeout)
        } else {
            Ok(())
        }
    });
    assert!(r.is_err());
    // a cancelled request stops
    let cancelled = DiffOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..Default::default()
    };
    let r = d.group_by_class(&mut |_| Ok(snapshot(&s, 1)), &cancelled, 5_000, &mut |_| {
        Ok(())
    });
    assert!(matches!(r, Err(sparkles_core::Error::Cancelled)));
    // and the full grouping charges what it holds
    let mut charged = 0u64;
    let g = d
        .group_by_class(&mut |_| Ok(snapshot(&s, 1)), &o, 5_000, &mut |n| {
            charged += n;
            Ok(())
        })
        .unwrap();
    assert_eq!(g.total, 2);
    assert!(charged >= 500 * 64, "{charged}");
}

#[test]
fn the_charge_counts_the_strings_each_group_owns() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    let long = format!("urn:{}", "c".repeat(20_000));
    let mut u = format!("INSERT DATA {{ <urn:a> a <{long}> .");
    for i in 0..50 {
        u.push_str(&format!(" <urn:a> <urn:p{i}> {i} ."));
    }
    u.push('}');
    update(&s, &u, &QueryOptions::default()).unwrap();
    let o = DiffOptions::default();
    let d = s.diff(&At::Commit(0), &At::Commit(1), &o).unwrap();
    let mut charged = 0u64;
    let g = d
        .group_by_class(&mut |_| Ok(snapshot(&s, 1)), &o, 5_000, &mut |n| {
            charged += n;
            Ok(())
        })
        .unwrap();
    assert_eq!(g.total, 51);
    let strings: u64 = g
        .groups
        .iter()
        .map(|x| {
            let obj = match &x.object {
                ObjectGroup::Class(Some(c)) => c.len(),
                ObjectGroup::Literal(dt) => dt.len(),
                _ => 0,
            };
            (x.subject_class.as_ref().map_or(0, String::len) + x.predicate.len() + obj) as u64
        })
        .sum();
    // every group holds the long class name of its subject
    assert!(strings > 51 * 20_000, "{strings}");
    assert!(charged >= strings, "{charged} < {strings}");
}

/// Diff size against time, for the numbers in the U01 Phase 4 outcome. Run it with
/// `cargo test --release -p sparkles-core --test diff_groups -- --ignored --nocapture`.
#[test]
#[ignore]
fn measure_grouping_time_by_diff_size() {
    use sparkles_core::io::{RdfFormat, Source};
    use std::fmt::Write as _;
    use std::time::Instant;
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    let mut base = String::new();
    for i in 0..200_000 {
        writeln!(base, "<urn:r{i}> <{TYPE}> <urn:C{}> .", i % 20).unwrap();
        writeln!(base, "<urn:r{i}> <urn:p> <urn:r{}> .", (i * 7) % 200_000).unwrap();
    }
    s.load(&[Source::from_bytes(
        base.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let o = DiffOptions::default();
    let mut offset = 0;
    for n in [1_000usize, 10_000, 100_000] {
        let before = s.head_commit().seq;
        let mut add = String::new();
        for i in 0..n / 2 {
            let k = offset + i;
            writeln!(add, "<urn:n{k}> <{TYPE}> <urn:D{}> .", k % 10).unwrap();
            writeln!(add, "<urn:n{k}> <urn:q> <urn:r{}> .", k % 200_000).unwrap();
        }
        offset += n;
        s.load(&[Source::from_bytes(
            add.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let after = s.head_commit().seq;
        let mut best = (f64::MAX, f64::MAX);
        let mut groups = 0;
        for _ in 0..5 {
            let t = Instant::now();
            let d = s.diff(&At::Commit(before), &At::Commit(after), &o).unwrap();
            let diff_ms = t.elapsed().as_secs_f64() * 1e3;
            assert_eq!(d.len(), n);
            let t = Instant::now();
            let (fa, fb) = (d.from.commit.seq, d.to.commit.seq);
            let g = d
                .group_by_class(
                    &mut |op| Ok(snapshot(&s, if op == DiffOp::Add { fb } else { fa })),
                    &o,
                    5_000,
                    &mut |_| Ok(()),
                )
                .unwrap();
            let group_ms = t.elapsed().as_secs_f64() * 1e3;
            groups = g.total;
            best = (best.0.min(diff_ms), best.1.min(group_ms));
        }
        println!(
            "changes {n:>7}: diff {:8.2} ms, grouping {:8.2} ms, {groups} groups",
            best.0, best.1
        );
    }
}
