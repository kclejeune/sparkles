//! Incremental maintenance: a report brought up to date from the net changes between its
//! commit and a later one, instead of being computed again.
//!
//! The observed layer is a sum over triples, so a change moves it by what that triple
//! adds or removes. For each triple a change touches, the update reads whether the
//! selection holds it in the new state (one SPO lookup), and derives whether it held it
//! before from the changes themselves: a triple was in the selection before when a
//! selected graph that still has it did not gain it, or when the change removed it from
//! a selected graph. Only the triples whose presence changed move the counts.
//!
//! - `triples` of a predicate moves by the sum of those changes.
//! - For each changed `(p, s)`, the subject's number of distinct objects is counted in
//!   the new state, and its old number follows from the changes. The report keeps how
//!   many subjects have each number of objects, so `maxPerSubject`,
//!   `subjectsWithMultiple` and `distinctSubjects` follow.
//! - For each changed `(p, o)`, the change in subjects is known. Whether the object was
//!   used before and is used now needs at most that many subjects more from `POS[p, o]`,
//!   which moves `distinctObjects` and the object's kind or datatype group. For
//!   `rdf:type`, the same change moves the instances of the class.
//!
//! The declared layer is read again from the declared graphs, which are small next to
//! the data, and labels, comments and version info are taken over from the old report
//! for every entry whose subject's literals did not change. The result equals a report
//! computed from scratch at the new commit, which `tests.rs` checks over random writes.

use super::{
    Budget, Kind, Observed, PredAcc, RDF_TYPE, RDFS_COMMENT, RDFS_LABEL, Reuse, SchemaError,
    SchemaOptions, SchemaReport, Selected, Src, assemble, in_phase, kind_of_key,
};
use crate::id::iri_key;
use crate::index::Perm;
use crate::store::{Diff, DiffOp, Snapshot, key_id};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

const OWL_VERSION_INFO: &str = "http://www.w3.org/2002/07/owl#versionInfo";

/// Which graph keys a selection accepts, by key, so that changes to graphs the new state
/// no longer has are judged as the old report judged them.
struct KeyFilter {
    /// `None`: every graph
    only: Option<Vec<Vec<u8>>>,
    /// a graph never accepted (the inferred graph left out)
    except: Option<Vec<u8>>,
}

impl KeyFilter {
    fn new(
        sel: &super::GraphSelection,
        inferred: Option<&str>,
        with_inferred: bool,
        union_default: bool,
    ) -> KeyFilter {
        let inferred = inferred.map(iri_key);
        let every = || KeyFilter {
            only: None,
            except: inferred.clone().filter(|_| !with_inferred),
        };
        match sel {
            super::GraphSelection::Union => every(),
            super::GraphSelection::Default if union_default => every(),
            super::GraphSelection::Default => {
                let mut only = vec![Vec::new()];
                if let Some(i) = inferred.clone().filter(|_| with_inferred) {
                    only.push(i);
                }
                KeyFilter {
                    only: Some(only),
                    except: None,
                }
            }
            super::GraphSelection::Named(n) => KeyFilter {
                only: Some(vec![iri_key(n.as_str())]),
                except: None,
            },
        }
    }

    fn accepts(&self, g: &[u8]) -> bool {
        if self.except.as_deref() == Some(g) {
            return false;
        }
        self.only
            .as_ref()
            .is_none_or(|o| o.iter().any(|k| k.as_slice() == g))
    }
}

/// Bring `old` up to date with `diff`, the net changes from its commit to `snap`'s.
///
/// Returns `Ok(None)` when it cannot, and the caller computes a new report: when `old`
/// keeps no [`Maintenance`] or `diff` does not start at its commit, when the options ask
/// for subject classes, distinct-term totals or a limited graph view, or when the counts
/// of `old` do not hold the changes (which would mean they were wrong). `opts` must
/// select the same graphs as the request that computed `old`.
pub fn update(
    old: &SchemaReport,
    snap: &Arc<Snapshot>,
    diff: &Diff,
    opts: &SchemaOptions,
) -> Result<Option<SchemaReport>, SchemaError> {
    let Some(m) = old.maintenance.as_deref() else {
        return Ok(None);
    };
    if opts.subject_classes
        || opts.term_totals
        || opts.graphs.as_ref().is_some_and(|a| !a.reads_all())
        || diff.from.commit.seq != m.commit
        || diff.to.commit.seq != snap.commit
    {
        return Ok(None);
    }
    let snap: &Snapshot = snap;
    let budget = Budget {
        deadline: opts.deadline,
        cancel: opts.cancel.as_deref(),
    };
    in_phase(budget.check(), || "starting".into())?;
    let sel = Selected::resolve(snap, opts)?;
    let declared_sel = opts.declared_graph.as_ref().unwrap_or(&opts.graph);
    if old.selection.graph != opts.graph.name()
        || old.selection.declared_graph != declared_sel.name()
        || old.selection.reasoning != opts.include_inferred
        || (old.selection.declared == "all") != opts.declared_from_inferred
    {
        return Ok(None);
    }
    let inferred = opts.inferred_graph.as_deref();
    let union_default = snap.union_default_graph;
    let obs_keys = KeyFilter::new(&opts.graph, inferred, opts.include_inferred, union_default);
    let decl_keys = KeyFilter::new(
        declared_sel,
        inferred,
        opts.declared_from_inferred,
        union_default,
    );
    let literal_preds = [
        iri_key(RDFS_LABEL),
        iri_key(RDFS_COMMENT),
        iri_key(OWL_VERSION_INFO),
    ];
    let rdf_type = iri_key(RDF_TYPE);

    // the changes to each triple of the selection, and the subjects whose labels changed
    type Triple<'d> = (&'d [u8], &'d [u8], &'d [u8]);
    let mut triples: FxHashMap<Triple, Vec<(bool, &[u8])>> = FxHashMap::default();
    let mut relabelled: FxHashSet<&[u8]> = FxHashSet::default();
    for (op, [g, s, p, o]) in diff.keys() {
        let add = op == DiffOp::Add;
        let (g, s, p, o): (&[u8], &[u8], &[u8], &[u8]) = (g, s, p, o);
        if decl_keys.accepts(g) && literal_preds.iter().any(|l| l.as_slice() == p) {
            relabelled.insert(s);
        }
        if obs_keys.accepts(g) {
            triples.entry((s, p, o)).or_default().push((add, g));
        }
    }

    // which triples entered or left the selection
    let mut d_pred: FxHashMap<&[u8], i64> = FxHashMap::default();
    let mut d_ps: FxHashMap<(&[u8], &[u8]), i64> = FxHashMap::default();
    let mut d_po: FxHashMap<(&[u8], &[u8]), i64> = FxHashMap::default();
    for (i, ((s, p, o), ops)) in triples.iter().enumerate() {
        if i % 1024 == 1023 {
            in_phase(budget.check(), || "reading the changes".into())?;
        }
        let mut now: Vec<u64> = Vec::new();
        if let (Some(s2), Some(p2), Some(o2)) = (key_id(snap, s), key_id(snap, p), key_id(snap, o))
        {
            let src = Src::direct(snap);
            in_phase(
                src.scan_until(Perm::Spo, &[s2.0, p2.0, o2.0], |k| {
                    if sel.observed.accepts(k[3]) {
                        now.push(k[3]);
                    }
                    true
                }),
                || "reading the changes".into(),
            )?;
        }
        let added: Vec<u64> = ops
            .iter()
            .filter(|(add, _)| *add)
            .filter_map(|(_, g)| key_id(snap, g).map(|i| i.0))
            .collect();
        let before = now.iter().any(|g| !added.contains(g)) || ops.iter().any(|(add, _)| !add);
        let delta = i64::from(!now.is_empty()) - i64::from(before);
        if delta == 0 {
            continue;
        }
        *d_pred.entry(p).or_default() += delta;
        *d_ps.entry((p, s)).or_default() += delta;
        *d_po.entry((p, o)).or_default() += delta;
    }

    let mut preds: FxHashMap<Vec<u8>, PredAcc> = m.preds.clone();
    let mut d_class: FxHashMap<&[u8], i64> = FxHashMap::default();
    let src = Src::direct(snap);
    let phase = || "applying the changes".to_string();
    macro_rules! held {
        ($e:expr) => {
            match $e {
                Some(v) => v,
                None => return Ok(None),
            }
        };
    }
    for (p, d) in &d_pred {
        let acc = preds.entry(p.to_vec()).or_default();
        acc.triples = held!(acc.triples.checked_add_signed(*d));
    }
    for ((p, s), d) in &d_ps {
        // the subject's distinct objects now, and before
        let mut n = 0u64;
        if let (Some(p2), Some(s2)) = (key_id(snap, p), key_id(snap, s)) {
            let mut prev = None;
            in_phase(
                src.for_each_key(Perm::Pso, &[p2.0, s2.0], &budget, |k| {
                    if sel.observed.accepts(k[3]) && prev != Some(k[2]) {
                        prev = Some(k[2]);
                        n += 1;
                    }
                }),
                phase,
            )?;
        }
        let was = held!(n.checked_add_signed(-d));
        let acc = held!(preds.get_mut(*p));
        held!(acc.subject_run(was, true));
        held!(acc.subject_run(n, false));
        let dd = i64::from(n > 0) - i64::from(was > 0);
        acc.distinct_subjects = held!(acc.distinct_subjects.checked_add_signed(dd));
    }
    for ((p, o), d) in &d_po {
        // the object is used now, and was before: at most `d + 1` subjects tell
        let need = if *d > 0 { d.unsigned_abs() + 1 } else { 1 };
        let mut n = 0u64;
        if let (Some(p2), Some(o2)) = (key_id(snap, p), key_id(snap, o)) {
            let mut prev = None;
            in_phase(
                src.scan_until(Perm::Pos, &[p2.0, o2.0], |k| {
                    if sel.observed.accepts(k[3]) && prev != Some(k[2]) {
                        prev = Some(k[2]);
                        n += 1;
                    }
                    n < need
                }),
                phase,
            )?;
        }
        let (used, was_used) = if *d > 0 {
            (true, n > d.unsigned_abs())
        } else {
            (n > 0, true)
        };
        let dd = i64::from(used) - i64::from(was_used);
        let acc = held!(preds.get_mut(*p));
        acc.distinct_objects = held!(acc.distinct_objects.checked_add_signed(dd));
        let kind = kind_of_key(o);
        if *p == rdf_type.as_slice() && matches!(kind, Kind::Iri) {
            *d_class.entry(*o).or_default() += d;
        }
        held!(acc.object(kind, *d, dd));
    }
    for acc in preds.values() {
        if acc.triples == 0 && !acc.is_empty() {
            return Ok(None);
        }
    }
    preds.retain(|_, a| a.triples > 0);

    // the observed layer by the new state's ids
    let mut by_id: FxHashMap<u64, PredAcc> = FxHashMap::default();
    for (p, acc) in preds {
        let id = held!(snap.lookup_key(&p));
        by_id.insert(id.0, acc);
    }
    let mut instances: FxHashMap<Vec<u8>, u64> = old
        .classes
        .iter()
        .filter(|c| c.observed.instances > 0)
        .map(|c| (iri_key(&c.iri), c.observed.instances))
        .collect();
    for (c, d) in d_class {
        let e = instances.entry(c.to_vec()).or_default();
        *e = held!(e.checked_add_signed(d));
    }
    let mut class_instances: FxHashMap<u64, u64> = FxHashMap::default();
    for (c, n) in instances {
        if n > 0 {
            class_instances.insert(held!(snap.lookup_key(&c)).0, n);
        }
    }
    let obs = Observed {
        preds: by_id,
        class_instances,
        joins: None,
        term_totals: None,
    };
    let reuse = Reuse {
        old,
        changed: relabelled
            .into_iter()
            .filter_map(|s| key_id(snap, s).map(|i| i.0))
            .collect(),
    };
    assemble(&src, opts, &sel, &budget, obs, Some(&reuse)).map(Some)
}

/// The number of changes past which a report is computed again rather than updated: one
/// per 500 triples of the report, and at least 256.
///
/// An update costs a few index lookups per changed triple, about 30 to 45 µs each on the
/// 1.05M-triple benchmark dataset, while a new report reads every selected triple twice,
/// at 40 to 140 ns per triple there. The two cost about the same near one change per
/// 500 triples.
pub fn max_changes(old: &SchemaReport) -> u64 {
    (old.totals.triples / 500).max(256)
}
