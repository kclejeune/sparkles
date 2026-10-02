//! Write previews (dry runs): a dry run reports the commit the write would make and
//! changes nothing (docs/specs/C15-write-previews.md).

use sparkles::Error;
use sparkles::commit::{CommitInfo, CommitKind, Receipt};
use sparkles::guard::{
    Candidate, CommitGuard, GuardMode, GuardStatus, Precondition, Severity, Strategy,
    ValidationSummary, WriteOptions,
};
use sparkles::io::{RdfFormat, Source};
use sparkles::preview::{self, DryRun, Outcome, Preview};
use sparkles::sparql::QueryOptions;
use sparkles::sparql::update::update_as;
use sparkles::store::{DiffOp, ReplaceTarget, Store, StoreOptions, named};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn dry(changes: usize) -> WriteOptions {
    WriteOptions {
        dry_run: Some(DryRun {
            changes,
            all_changes: false,
            max_changes: 0,
        }),
        ..Default::default()
    }
}

fn all_changes() -> WriteOptions {
    WriteOptions {
        dry_run: Some(DryRun {
            changes: 0,
            all_changes: true,
            max_changes: 0,
        }),
        ..Default::default()
    }
}

fn upd(s: &Store, text: &str, w: WriteOptions) -> sparkles::Result<Receipt> {
    let opts = QueryOptions {
        write: w,
        ..Default::default()
    };
    Ok(update_as(s, text, &opts, CommitKind::Update)?
        .commit
        .unwrap())
}

fn preview_upd(s: &Store, text: &str, w: WriteOptions) -> sparkles::Result<Preview> {
    preview::catch(upd(s, text, w))
}

fn nt(text: &str) -> Source {
    Source::from_bytes(text.as_bytes().to_vec(), RdfFormat::NQuads, None)
}

/// Every quad of the head, as N-Quads lines.
fn state(s: &Store) -> BTreeSet<String> {
    let snap = s.snapshot();
    let mut out = BTreeSet::new();
    snap.for_each_quad(|q| {
        out.insert(snap.quad_to_terms(q).unwrap().to_string());
        Ok(())
    })
    .unwrap();
    out
}

/// Every file of a directory with its bytes.
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

/// The members a receipt and a preview share: everything but the time.
fn same_commit(p: &CommitInfo, r: &CommitInfo) {
    let strip = |c: &CommitInfo| CommitInfo {
        timestamp_ms: 0,
        ..*c
    };
    assert_eq!(strip(p), strip(r));
}

/// The preview must describe the receipt and the change of the real write.
fn check_against_real(s: &Store, p: &Preview, r: &Receipt, before: &BTreeSet<String>) {
    assert_eq!(p.commit.is_some(), r.committed, "{p:?} vs {r:?}");
    same_commit(p.receipt_commit(), &r.commit);
    assert_eq!(
        p.validation.as_deref().map(strip_ms),
        r.validation.as_deref().map(strip_ms)
    );
    if r.committed {
        let ins: u64 = p.graphs.iter().map(|g| g.inserted).sum();
        let del: u64 = p.graphs.iter().map(|g| g.deleted).sum();
        assert_eq!((ins, del), (r.commit.inserted, r.commit.deleted));
    } else {
        assert!(p.graphs.is_empty());
    }
    // every change, against the states before and after
    let after = state(s);
    let added: BTreeSet<String> = after.difference(before).cloned().collect();
    let removed: BTreeSet<String> = before.difference(&after).cloned().collect();
    // the listing is exact also where a bulk commit's counts are not
    if let Some(total) = p.changes_total {
        let mut pa = BTreeSet::new();
        let mut pr = BTreeSet::new();
        for (op, q) in &p.changes {
            match op {
                DiffOp::Add => pa.insert(q.to_string()),
                DiffOp::Remove => pr.insert(q.to_string()),
            };
        }
        if p.changes.len() as u64 == total {
            assert_eq!(pa, added);
            assert_eq!(pr, removed);
        } else {
            assert!(pa.is_subset(&added) && pr.is_subset(&removed));
        }
        assert_eq!(total, (added.len() + removed.len()) as u64);
    }
}

fn strip_ms(v: &ValidationSummary) -> ValidationSummary {
    ValidationSummary {
        millis: 0,
        ..v.clone()
    }
}

/// A small deterministic random source.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A random update over a few subjects, predicates, graphs and fresh terms.
fn random_update(r: &mut Rng, fresh: &mut u64) -> String {
    let term = |r: &mut Rng, fresh: &mut u64| match r.below(5) {
        0 => {
            *fresh += 1;
            format!("<urn:new:{}>", *fresh)
        }
        1 => format!("\"lit{}\"", r.below(4)),
        2 => format!("{}", r.below(4)),
        _ => format!("<urn:o{}>", r.below(4)),
    };
    let triple = |r: &mut Rng, fresh: &mut u64| {
        format!(
            "<urn:s{}> <urn:p{}> {}",
            r.below(4),
            r.below(3),
            term(r, fresh)
        )
    };
    let graph = |r: &mut Rng| match r.below(3) {
        0 => None,
        n => Some(format!("<urn:g{n}>")),
    };
    let wrap = |g: Option<String>, body: String| match g {
        Some(g) => format!("GRAPH {g} {{ {body} }}"),
        None => body,
    };
    let mut ops = Vec::new();
    for _ in 0..1 + r.below(3) {
        let op = match r.below(9) {
            0..=2 => {
                let n = 1 + r.below(4);
                let body: Vec<String> = (0..n)
                    .map(|_| {
                        let t = triple(r, fresh);
                        wrap(graph(r), t)
                    })
                    .collect();
                format!("INSERT DATA {{ {} }}", body.join(" . "))
            }
            3 => {
                let t = triple(r, fresh);
                format!("DELETE DATA {{ {} }}", wrap(graph(r), t))
            }
            4 => format!(
                "DELETE {{ ?s <urn:p{p}> ?o }} INSERT {{ ?s <urn:p{p}> \"moved\" }} WHERE {{ ?s <urn:p{p}> ?o }}",
                p = r.below(3)
            ),
            5 => format!(
                "DELETE WHERE {{ {} }}",
                wrap(graph(r), format!("<urn:s{}> ?p ?o", r.below(4)))
            ),
            6 => match graph(r) {
                Some(g) => format!("CLEAR SILENT GRAPH {g}"),
                None => "CLEAR DEFAULT".into(),
            },
            7 => format!(
                "INSERT {{ GRAPH <urn:g{}> {{ ?s ?p ?o }} }} WHERE {{ ?s ?p ?o }}",
                1 + r.below(2)
            ),
            _ => "INSERT { ?s <urn:link> _:b } WHERE { ?s <urn:p0> ?o }".into(),
        };
        ops.push(op);
    }
    ops.join(" ;\n")
}

/// A guard that rejects any state with `<urn:bad>` as a predicate, and counts its calls.
#[derive(Default)]
struct BadGuard {
    checks: AtomicUsize,
    dry_checks: AtomicUsize,
    committed: AtomicUsize,
    warn: bool,
}

impl CommitGuard for BadGuard {
    fn check(&self, c: &Candidate<'_>) -> sparkles::Result<ValidationSummary> {
        if c.opts.dry_run.is_some() {
            self.dry_checks.fetch_add(1, Ordering::Relaxed);
        } else {
            self.checks.fetch_add(1, Ordering::Relaxed);
        }
        let bad = c
            .view
            .lookup_iri("urn:bad")
            .map(|p| c.view.count(sparkles::index::Perm::Pso, &[p.0]).unwrap())
            .unwrap_or(0);
        let mode = if self.warn {
            GuardMode::Warn
        } else {
            GuardMode::Reject
        };
        let status = match (bad > 0, self.warn) {
            (false, _) => GuardStatus::Passed,
            (true, true) => GuardStatus::Warned,
            (true, false) => GuardStatus::Rejected,
        };
        let mut s = ValidationSummary::empty(status, mode, Severity::Violation);
        s.strategy = Strategy::Full;
        s.blocking = bad;
        s.total = bad;
        s.conforms = bad == 0;
        s.by_severity.violation = bad;
        Ok(s)
    }
    fn committed(&self, _seq: u64) {
        self.committed.fetch_add(1, Ordering::Relaxed);
    }
    fn describe(&self) -> String {
        "bad".into()
    }
}

fn seed(s: &Store) {
    upd(
        s,
        "INSERT DATA { <urn:s0> <urn:p0> 1 . <urn:s1> <urn:p1> \"lit0\" . GRAPH <urn:g1> { <urn:s2> <urn:p2> <urn:o1> } }",
        Default::default(),
    )
    .unwrap();
}

/// Random updates: each dry run equals the receipt of the same update made right after
/// it, and changes nothing before that.
/// Returns how many updates committed, changed nothing and were rejected.
fn random_updates(
    s: &Store,
    seed_value: u64,
    rounds: usize,
    guard: Option<&BadGuard>,
) -> [usize; 3] {
    let mut r = Rng(seed_value);
    let mut fresh = 0u64;
    let mut seen = [0; 3];
    for i in 0..rounds {
        let mut text = random_update(&mut r, &mut fresh);
        if guard.is_some() && r.below(4) == 0 {
            text = format!("{text} ; INSERT DATA {{ <urn:s9> <urn:bad> {i} }}");
        }
        if guard.is_some() && r.below(4) == 0 {
            text = format!("{text} ; DELETE WHERE {{ ?s <urn:bad> ?o }}");
        }
        let before = state(s);
        let head = s.head_commit();
        let snap = s.snapshot();
        let listed = if r.below(2) == 0 { 3 } else { 1000 };
        let p = preview_upd(s, &text, dry(listed)).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(s.head_commit(), head, "{text}");
        assert!(Arc::ptr_eq(&snap, &s.snapshot()), "{text}");
        assert_eq!(state(s), before, "{text}");
        assert_eq!(p.head, head);
        let real = upd(s, &text, Default::default());
        match real {
            Ok(rec) => {
                assert_ne!(p.outcome(), Outcome::Rejected, "{text}");
                check_against_real(s, &p, &rec, &before);
                seen[if rec.committed { 0 } else { 1 }] += 1;
            }
            Err(Error::Rejected(rej)) => {
                assert_eq!(p.outcome(), Outcome::Rejected, "{text}");
                assert_eq!(
                    strip_ms(&rej.summary),
                    strip_ms(p.validation.as_deref().unwrap())
                );
                assert_eq!(s.head_commit(), head);
                seen[2] += 1;
            }
            Err(e) => panic!("{text}: {e}"),
        }
    }
    seen
}

#[test]
fn previews_equal_receipts_in_memory() {
    let s = Store::in_memory(StoreOptions::default());
    seed(&s);
    let seen = random_updates(&s, 0x9e37_79b9_7f4a_7c15, 300, None);
    assert!(seen[0] > 100 && seen[1] > 5, "{seen:?}");
}

#[test]
fn previews_equal_receipts_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    seed(&s);
    random_updates(&s, 0x2545_f491_4f6c_dd1d, 200, None);
    // compacted, so the base generation holds data too
    s.compact().unwrap();
    random_updates(&s, 0x1234_5678_9abc_def1, 100, None);
}

#[test]
fn previews_equal_receipts_with_a_guard() {
    for warn in [false, true] {
        let s = Store::in_memory(StoreOptions::default());
        seed(&s);
        let g = Arc::new(BadGuard {
            warn,
            ..Default::default()
        });
        s.set_guard(Some(g.clone()));
        let seen = random_updates(&s, 0x0dd0_f00d_cafe_beef ^ warn as u64, 200, Some(&g));
        assert_eq!(seen[2] > 10, !warn, "{seen:?}");
        // the guard saw each dry run as one, and was told of real commits only
        let (checks, dry_checks, committed) = (
            g.checks.load(Ordering::Relaxed),
            g.dry_checks.load(Ordering::Relaxed),
            g.committed.load(Ordering::Relaxed),
        );
        assert!(dry_checks > 100 && checks > 100, "{dry_checks} {checks}");
        assert!(committed <= checks);
    }
}

#[test]
fn a_dry_run_changes_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let opts = StoreOptions {
        bulk_threshold: 50,
        ..Default::default()
    };
    let s = Store::open(&root, opts).unwrap();
    seed(&s);
    let before = files(&root);
    let head = s.head_commit();
    let mut r = Rng(7);
    let mut fresh = 0;
    for _ in 0..200 {
        let text = random_update(&mut r, &mut fresh);
        preview_upd(&s, &text, dry(5)).unwrap();
    }
    // enough new terms to spill the vocabulary file's write buffer
    let big: Vec<String> = (0..2000)
        .map(|i| format!("<urn:s{i}> <urn:p> \"a fairly long literal to fill the buffer {i}\""))
        .collect();
    let p = preview_upd(
        &s,
        &format!("INSERT DATA {{ {} }}", big.join(" . ")),
        dry(0),
    )
    .unwrap();
    assert_eq!(p.commit.unwrap().inserted, 2000);
    // the bulk path: a replace and a load built into a generation that is then removed
    let body: String = (0..120)
        .map(|i| format!("<urn:x{i}> <urn:p> \"v{i}\" <urn:g9> .\n"))
        .collect();
    let p = preview::catch(s.replace_with(
        ReplaceTarget::Named(named("urn:g1")),
        &[nt(&body)],
        CommitKind::GspPut,
        &dry(3),
    ))
    .unwrap();
    assert!(p.commit.unwrap().bulk);
    let p = preview::catch(s.load_with(&[nt(&body)], CommitKind::GspPost, &dry(0))).unwrap();
    assert!(p.commit.unwrap().bulk);
    assert_eq!(files(&root), before);
    assert_eq!(s.head_commit(), head);
    // later writes and a reopen see a sound vocabulary
    upd(
        &s,
        "INSERT DATA { <urn:after> <urn:p> \"after\" }",
        Default::default(),
    )
    .unwrap();
    let want = state(&s);
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(state(&s), want);
    assert_eq!(s.head_commit().seq, head.seq + 1);
    drop(s);
    let report = sparkles::check::check(&root, &Default::default()).unwrap();
    assert_eq!(report.exit_code(), 0, "{report:?}");
}

#[test]
fn terms_and_blank_nodes_are_rolled_back() {
    let a = Store::in_memory(StoreOptions::default());
    let b = Store::in_memory(StoreOptions::default());
    for s in [&a, &b] {
        seed(s);
    }
    // a dry run that adds terms and blank nodes, on `a` only
    let p = preview_upd(
        &a,
        "INSERT DATA { _:x <urn:q> <urn:fresh1> . _:y <urn:q> \"fresh2\" }",
        dry(10),
    )
    .unwrap();
    assert_eq!(p.changes.len(), 2);
    // the next write gets the ids it gets without the dry run
    for s in [&a, &b] {
        upd(
            s,
            "INSERT DATA { _:z <urn:q> <urn:other> }",
            Default::default(),
        )
        .unwrap();
    }
    assert_eq!(state(&a), state(&b));
    assert!(a.snapshot().lookup_iri("urn:fresh1").is_none());
}

#[test]
fn a_rejection_is_reported_with_the_other_checks() {
    let s = Store::in_memory(StoreOptions::default());
    seed(&s);
    let g = Arc::new(BadGuard::default());
    s.set_guard(Some(g.clone()));
    let head = s.head_commit();
    let mut w = dry(5);
    w.precondition = Some(Precondition::new(|_| {
        Err(Error::PreconditionFailed("stale".into()))
    }));
    let p = preview_upd(&s, "INSERT DATA { <urn:a> <urn:bad> 1 }", w).unwrap();
    // every check is evaluated; the precondition comes first
    assert_eq!(p.outcome(), Outcome::PreconditionFailed);
    assert_eq!(p.precondition, Some(Err("stale".into())));
    assert!(p.rejected());
    assert_eq!(p.commit.unwrap().seq, head.seq + 1);
    assert_eq!(p.error().as_deref(), Some("stale"));
    // without it, the rejection
    let p = preview_upd(&s, "INSERT DATA { <urn:a> <urn:bad> 1 }", dry(0)).unwrap();
    assert_eq!(p.outcome(), Outcome::Rejected);
    assert!(p.error().unwrap().contains("1 blocking result"));
    assert_eq!(g.committed.load(Ordering::Relaxed), 0);
    assert_eq!(g.checks.load(Ordering::Relaxed), 0);
    // a bypass is reported, and the guard is not told
    let mut w = dry(0);
    w.bypass_validation = true;
    let p = preview_upd(&s, "INSERT DATA { <urn:a> <urn:bad> 1 }", w).unwrap();
    assert_eq!(p.validation.as_ref().unwrap().status, GuardStatus::Bypassed);
    assert!(p.commit.unwrap().unvalidated);
    assert_eq!(p.outcome(), Outcome::Commit);
}

#[test]
fn the_quota_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    seed(&s);
    let used = s.disk_usage();
    s.set_quota(Some(used + 10)).unwrap();
    let p = preview_upd(&s, "INSERT DATA { <urn:a> <urn:p> 2 }", dry(0)).unwrap();
    assert_eq!(p.outcome(), Outcome::StorageRefused);
    assert_eq!(p.storage.limit, Some(used + 10));
    assert!(p.storage.projected.unwrap() > used + 10);
    let real = upd(&s, "INSERT DATA { <urn:a> <urn:p> 2 }", Default::default()).unwrap_err();
    assert_eq!(p.error().unwrap(), real.to_string());
    // deletes always fit
    let p = preview_upd(&s, "DELETE DATA { <urn:s0> <urn:p0> 1 }", dry(0)).unwrap();
    assert_eq!(p.outcome(), Outcome::Commit);
    assert!(p.storage.refused.is_none());
}

#[test]
fn bulk_previews_equal_bulk_receipts() {
    for persistent in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let opts = StoreOptions {
            bulk_threshold: 20,
            ..Default::default()
        };
        let s = if persistent {
            Store::open(&dir.path().join("db"), opts).unwrap()
        } else {
            Store::in_memory(opts)
        };
        let body = |n: usize, g: &str| -> String {
            (0..n)
                .map(|i| format!("<urn:x{i}> <urn:p> \"v{i}\" {g} .\n"))
                .collect()
        };
        // a load into an empty store, a replace that overlaps, a replace of everything,
        // and a transaction with a bulk batch
        type Step<'a> = Box<dyn Fn(&WriteOptions) -> sparkles::Result<Receipt> + 'a>;
        let steps: Vec<Step> = vec![
            Box::new(|w| s.load_with(&[nt(&body(60, "<urn:g1>"))], CommitKind::Load, w)),
            Box::new(|w| {
                s.replace_with(
                    ReplaceTarget::Named(named("urn:g1")),
                    &[nt(&body(40, ""))],
                    CommitKind::GspPut,
                    w,
                )
                .map(|r| r.1)
            }),
            Box::new(|w| {
                s.replace_with(
                    ReplaceTarget::Default,
                    &[nt(&body(30, ""))],
                    CommitKind::GspPut,
                    w,
                )
                .map(|r| r.1)
            }),
            Box::new(|w| {
                let mut t = s.write_with(CommitKind::Transaction, w.clone());
                let mut labels = Default::default();
                let mut ids = Vec::new();
                for i in 0..50 {
                    let q = oxrdf::Quad::new(
                        oxrdf::NamedNode::new_unchecked(format!("urn:y{i}")),
                        oxrdf::NamedNode::new_unchecked("urn:p"),
                        oxrdf::Literal::from(i as i64),
                        oxrdf::NamedNode::new_unchecked("urn:g2"),
                    );
                    ids.push(t.encode_quad(&q, &mut labels)?);
                }
                let old = oxrdf::Quad::new(
                    oxrdf::NamedNode::new_unchecked("urn:x1"),
                    oxrdf::NamedNode::new_unchecked("urn:p"),
                    oxrdf::Literal::from("v1"),
                    oxrdf::GraphName::DefaultGraph,
                );
                let old = t.encode_quad(&old, &mut labels)?;
                t.delete(old)?;
                t.insert_bulk(ids)?;
                t.commit()
            }),
        ];
        for step in &steps {
            let before = state(&s);
            let head = s.head_commit();
            let p = preview::catch(step(&all_changes())).unwrap();
            assert_eq!(state(&s), before);
            assert_eq!(s.head_commit(), head);
            let c = p.commit.unwrap();
            assert!(c.bulk);
            let r = step(&Default::default()).unwrap();
            same_commit(&c, &r.commit);
            check_against_real(&s, &p, &r, &before);
            if persistent {
                // only the published generation and none of the candidates
                let gens: Vec<_> = std::fs::read_dir(dir.path().join("db"))
                    .unwrap()
                    .filter_map(|e| {
                        let n = e.unwrap().file_name().to_string_lossy().into_owned();
                        n.starts_with("gen-").then_some(n)
                    })
                    .collect();
                assert_eq!(gens.len(), 1, "{gens:?}");
            }
        }
    }
}

#[test]
fn a_full_listing_is_bounded() {
    let s = Store::in_memory(StoreOptions::default());
    seed(&s);
    let w = WriteOptions {
        dry_run: Some(DryRun {
            changes: 0,
            all_changes: true,
            max_changes: 2,
        }),
        ..Default::default()
    };
    let e = preview_upd(&s, "INSERT DATA { <urn:a> <urn:p> 1, 2, 3 }", w).unwrap_err();
    assert!(matches!(e, Error::BudgetExceeded(b) if b.kind == sparkles::BudgetKind::Rows));
    // a listing that is cut short says so through its total
    let p = preview_upd(&s, "INSERT DATA { <urn:a> <urn:p> 1, 2, 3 }", dry(2)).unwrap();
    assert_eq!((p.changes.len(), p.changes_total), (2, Some(3)));
}

#[test]
fn a_write_without_net_effect_previews_no_commit() {
    let s = Store::in_memory(StoreOptions::default());
    seed(&s);
    let p = preview_upd(&s, "INSERT DATA { <urn:s0> <urn:p0> 1 }", dry(5)).unwrap();
    assert_eq!(p.outcome(), Outcome::NoChange);
    assert!(p.commit.is_none() && p.graphs.is_empty() && p.changes.is_empty());
    assert_eq!(p.receipt_commit(), &s.head_commit());
}

#[test]
fn a_transaction_previews_itself() {
    let s = Store::in_memory(StoreOptions::default());
    seed(&s);
    let mut t = s.write();
    let q = oxrdf::Quad::new(
        oxrdf::NamedNode::new_unchecked("urn:t"),
        oxrdf::NamedNode::new_unchecked("urn:p"),
        oxrdf::Literal::from("t"),
        oxrdf::GraphName::DefaultGraph,
    );
    let id = t.encode_quad(&q, &mut Default::default()).unwrap();
    t.insert(id).unwrap();
    let p = t.preview(DryRun::default()).unwrap();
    assert_eq!(p.commit.unwrap().inserted, 1);
    assert_eq!(s.head_commit().seq, 1);
    assert!(s.snapshot().lookup_iri("urn:t").is_none());
}

#[cfg(feature = "text")]
#[test]
fn the_full_text_index_sees_nothing() {
    let s = Store::in_memory(StoreOptions::default());
    seed(&s);
    s.enable_text(Default::default()).unwrap();
    let before = s.text_status().unwrap();
    preview_upd(
        &s,
        "INSERT DATA { <urn:t> <urn:p> \"searchable words\" }",
        dry(0),
    )
    .unwrap();
    let after = s.text_status().unwrap();
    assert_eq!((before.docs, before.seq), (after.docs, after.seq));
}

/// A Graph Store `PUT` through the WAL logs only the difference between the graph and
/// its new content (C15 §6): its receipt is unchanged, and the WAL grows by the quads
/// that changed.
#[test]
fn a_put_logs_only_the_difference() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    let body = |n: usize| -> String {
        (0..n)
            .map(|i| format!("<urn:x{i}> <urn:p> \"v{i}\" .\n"))
            .collect()
    };
    let put = |text: String| {
        s.replace_with(
            ReplaceTarget::Named(named("urn:g")),
            &[Source::from_bytes(
                text.into_bytes(),
                RdfFormat::NTriples,
                Some(named("urn:g")),
            )],
            CommitKind::GspPut,
            &Default::default(),
        )
        .unwrap()
        .1
    };
    put(body(100));
    let before = s.wal_bytes();
    // the same content and one more quad: one insert record and the commit record
    let r = put(body(101));
    assert_eq!((r.commit.inserted, r.commit.deleted), (1, 0));
    assert_eq!(s.wal_bytes() - before, 2 * 33);
    // a quad less: one delete record
    let before = s.wal_bytes();
    let r = put(body(100));
    assert_eq!((r.commit.inserted, r.commit.deleted), (0, 1));
    assert_eq!(s.wal_bytes() - before, 2 * 33);
    // the same content: no commit
    let r = put(body(100));
    assert!(!r.committed);
    assert_eq!(state(&s).len(), 100);
}
