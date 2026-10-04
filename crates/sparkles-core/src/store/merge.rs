//! Three-way merges of branches ([`Store::merge`]).
//!
//! A merge never materializes the base, ours (the target) and theirs (the source). It
//! works with *toggle sets*: `T(X→Y)` holds every quad whose presence differs between
//! states X and Y, with whether it was added. Toggle sets compose by symmetric
//! difference, so `T(B→O)` for a merge base B and a head O is the log walk from the
//! newest commit Z on both first-parent chains back to B, reversed, followed by the walk
//! from Z to O. Each walk is a sequence of [`Store::diff`]s, one per branch along the
//! chain.
//!
//! At the quad level the three-way rule never conflicts: the side that changed a quad
//! wins. Conflicts are defined on groups of quads, chosen by the [`ConflictScope`]: a
//! group conflicts when both sides changed it and their changes differ.

use super::branching::{BranchSet, MERGE_FAST_FORWARD, MergeRec};
use super::diff::{DiffOp, DiffOptions, QuadKey};
use super::*;
use crate::branch::{
    self, BranchError, BranchErrorKind, CommitRef, ConflictCell, ConflictReport, ConflictScope,
    GraphConflicts, MergeOptions, MergeOutcome, MergeReport, NamedCommitRef, Resolution, Take,
};
use crate::history::At;
use rustc_hash::{FxHashMap, FxHashSet};

/// The inferred graph that merges leave out unless asked to include it.
pub const INFERRED_GRAPH: &str = "urn:x-sparkles:inferred";

/// A toggle set: each quad whose presence differs, and whether it was added.
pub(crate) type Toggles = FxHashMap<QuadKey, bool>;

/// A group of quads that conflicts as one value: graph, subject and, for a cell, the
/// predicate.
type GroupKey = (Arc<[u8]>, Arc<[u8]>, Option<Arc<[u8]>>);

fn group_of(k: &QuadKey, scope: ConflictScope) -> GroupKey {
    match scope {
        ConflictScope::Subject => (k[0].clone(), k[1].clone(), None),
        _ => (k[0].clone(), k[1].clone(), Some(k[2].clone())),
    }
}

fn toggle(t: &mut Toggles, k: QuadKey, added: bool) {
    match t.entry(k) {
        std::collections::hash_map::Entry::Occupied(e) => {
            e.remove();
        }
        std::collections::hash_map::Entry::Vacant(e) => {
            e.insert(added);
        }
    }
}

fn gone(e: Error) -> Error {
    match e {
        Error::HistoryGone(g) => BranchError::error(
            BranchErrorKind::Gone,
            "merge-base-gone",
            format!("the merge base is no longer reconstructable: {g}"),
        ),
        e => e,
    }
}

/// The key of a term as stored: an inline literal in its canonical form.
pub(crate) fn term_key(t: &Term) -> Vec<u8> {
    if let Some(id) = crate::id::inline_id(t)
        && let Some(l) = crate::id::inline_to_literal(id)
    {
        let mut k = Vec::new();
        crate::id::write_literal_key(&l, &mut k);
        return k;
    }
    crate::id::term_key(t)
}

fn graph_key(g: &GraphName) -> Vec<u8> {
    match g {
        GraphName::DefaultGraph => Vec::new(),
        GraphName::NamedNode(n) => crate::id::iri_key(n.as_str()),
        GraphName::BlankNode(b) => crate::id::term_key(&Term::BlankNode(b.clone())),
    }
}

/// A key in N-Triples (`None` for the default graph's empty key).
fn nt(key: &[u8]) -> Option<String> {
    (!key.is_empty()).then(|| crate::id::key_to_term(key).to_string())
}

/// The id of a key in a write transaction, adding the term if `add`.
fn key_id(txn: &mut WriteTxn<'_>, key: &[u8], add: bool) -> Result<Option<Id>> {
    match key.first() {
        None => Ok(Some(Id::DEFAULT_GRAPH)),
        Some(b'_') => {
            let b: [u8; 8] = key
                .get(1..9)
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| Error::Corrupt("a blank node key of the wrong length".into()))?;
            Ok(Some(Id::new(crate::id::Tag::BNode, u64::from_be_bytes(b))))
        }
        Some(b'"') => {
            let t = crate::id::key_to_term(key);
            if let Some(id) = crate::id::inline_id(&t) {
                return Ok(Some(id));
            }
            if add {
                txn.intern_key(key).map(Some)
            } else {
                Ok(txn.lookup_key(key))
            }
        }
        Some(_) => {
            if add {
                txn.intern_key(key).map(Some)
            } else {
                Ok(txn.lookup_key(key))
            }
        }
    }
}

/// The quad ids of a quad key (`None` when deleting a quad whose terms the store lacks).
fn quad_ids(txn: &mut WriteTxn<'_>, k: &QuadKey, add: bool) -> Result<Option<[Id; 4]>> {
    let mut ids = [Id::UNDEF; 4];
    // the key is [g, s, p, o], ids are [s, p, o, g]
    for (i, slot) in [3usize, 0, 1, 2].into_iter().enumerate() {
        match key_id(txn, &k[i], add)? {
            Some(id) => ids[slot] = id,
            None => return Ok(None),
        }
    }
    Ok(Some(ids))
}

/// The toggles of ours and theirs in one group.
type Sides = (Vec<(QuadKey, bool)>, Vec<(QuadKey, bool)>);

/// What a merge will do: the changes to the target, and the conflicts.
struct Plan {
    /// the changes to apply to the target: quad → insert (true) or delete
    changes: FxHashMap<QuadKey, bool>,
    found: u64,
    resolved: u64,
    /// conflicting groups that remain, with the toggles of ours and theirs
    remaining: Vec<(GroupKey, Sides)>,
    excluded: Option<u64>,
}

/// A resolution with its terms as keys.
struct KeyedResolution {
    graph: Arc<[u8]>,
    subject: Option<Arc<[u8]>>,
    predicate: Option<Arc<[u8]>>,
    take: Take,
    objects: Vec<Arc<[u8]>>,
    used: bool,
}

impl Store {
    /// The net changes from commit `a` to commit `b`, any two commits of the dataset:
    /// walks of the logs along their first-parent chains from the newest commit both
    /// chains share.
    pub(crate) fn toggles(
        &self,
        set: &BranchSet,
        a: CommitRef,
        b: CommitRef,
        o: &DiffOptions,
    ) -> Result<Toggles> {
        let ca = set.chain(a);
        let cb = set.chain(b);
        let (mut zi, mut zseq) = (0usize, ca[0].2.min(cb[0].2));
        for i in 0..ca.len().min(cb.len()) {
            if ca[i].0 != cb[i].0 {
                break;
            }
            zi = i;
            zseq = ca[i].2.min(cb[i].2);
            if ca[i].2 != cb[i].2 {
                break;
            }
        }
        let mut t = Toggles::default();
        self.walk(&ca, zi, zseq, true, o, &mut t)?;
        self.walk(&cb, zi, zseq, false, o, &mut t)?;
        if o.max_quads > 0 && t.len() as u64 > o.max_quads {
            return Err(Error::BudgetExceeded(crate::Budget {
                kind: crate::BudgetKind::Rows,
                limit: o.max_quads,
                requested: t.len() as u64,
            }));
        }
        Ok(t)
    }

    fn walk(
        &self,
        chain: &[(uuid::Uuid, u64, u64)],
        zi: usize,
        zseq: u64,
        reverse: bool,
        o: &DiffOptions,
        t: &mut Toggles,
    ) -> Result<()> {
        for (j, &(bid, after, through)) in chain.iter().enumerate().skip(zi) {
            let lo = if j == zi { zseq } else { after };
            if lo >= through {
                continue;
            }
            let store = self.branch_by_id(bid)?;
            let d = store
                .diff(&At::Commit(lo), &At::Commit(through), o)
                .map_err(gone)?;
            for (op, k) in d.keys() {
                toggle(t, k.clone(), (op == DiffOp::Add) != reverse);
            }
        }
        Ok(())
    }

    /// The net difference between two commits of any branches of this dataset (a diff
    /// across branches). `from` and `to` resolve within their branches' histories.
    pub fn branch_diff(
        &self,
        from_branch: &str,
        from: &At,
        to_branch: &str,
        to: &At,
        o: &DiffOptions,
    ) -> Result<diff::Diff> {
        let set = self.owned_set()?.clone();
        let (rf, bf) = self.branch_resolve(from_branch, from)?;
        let (rt, bt) = self.branch_resolve(to_branch, to)?;
        let a = CommitRef {
            branch_id: bf,
            seq: rf.commit.seq,
        };
        let b = CommitRef {
            branch_id: bt,
            seq: rt.commit.seq,
        };
        let t = if set.normalize(a) == set.normalize(b) {
            Toggles::default()
        } else {
            self.toggles(&set, a, b, o)?
        };
        let mut changes: Vec<(QuadKey, DiffOp)> = t
            .into_iter()
            .filter(|(k, _)| {
                o.graph
                    .as_ref()
                    .is_none_or(|g| *k[0] == *graph_key(g).as_slice())
            })
            .filter(|(k, _)| {
                o.graphs.as_ref().is_none_or(|acc| {
                    acc.reads_everything() || diff::visible_key(acc, &mut Default::default(), k)
                })
            })
            .map(|(k, added)| (k, if added { DiffOp::Add } else { DiffOp::Remove }))
            .collect();
        changes
            .sort_unstable_by(|x, y| (x.1 == DiffOp::Add, &x.0).cmp(&(y.1 == DiffOp::Add, &y.0)));
        let added = changes.iter().filter(|c| c.1 == DiffOp::Add).count() as u64;
        Ok(diff::Diff {
            removed: changes.len() as u64 - added,
            added,
            from: rf,
            to: rt,
            method: diff::DiffMethod::Log,
            log_changes: 0,
            compared: 0,
            changes,
        })
    }

    /// Merge branch `source` into branch `target`: a fast-forward when the target has
    /// not moved since the merge base, a three-way merge of quad sets otherwise. One
    /// commit of kind `merge` on the target, even with no net change. Conflicts that
    /// `o`'s resolutions and rule leave stop the merge, and nothing is written.
    pub fn merge(&self, source: &str, target: &str, o: &MergeOptions) -> Result<MergeOutcome> {
        let t0 = std::time::Instant::now();
        let r = self.merge_inner(source, target, o, false);
        match &r {
            Ok(MergeOutcome::Merged(m)) => tracing::info!(target: "sparkles::store::merge",
                source,
                target,
                base = ?m.base.as_ref().map(|b| b.seq),
                inserted = m.inserted,
                deleted = m.deleted,
                conflicts = m.conflicts_found,
                fast_forward = m.fast_forward,
                ms = t0.elapsed().as_secs_f64() * 1e3,
                "merged"
            ),
            Ok(MergeOutcome::Conflicts(c)) => tracing::info!(target: "sparkles::store::merge",
                source,
                target,
                conflicts = c.conflicts,
                ms = t0.elapsed().as_secs_f64() * 1e3,
                "merge stopped by conflicts"
            ),
            _ => {}
        }
        r
    }

    /// What merging `source` into `target` would do, without writing or failing over
    /// conflicts: the report carries the conflicts that remain.
    pub fn preview_merge(
        &self,
        source: &str,
        target: &str,
        o: &MergeOptions,
    ) -> Result<MergeReport> {
        match self.merge_inner(source, target, o, true)? {
            MergeOutcome::UpToDate(r) | MergeOutcome::Merged(r) => Ok(r),
            MergeOutcome::Conflicts(_) => unreachable!("a preview reports conflicts"),
        }
    }

    fn merge_inner(
        &self,
        source: &str,
        target: &str,
        o: &MergeOptions,
        preview: bool,
    ) -> Result<MergeOutcome> {
        let set = self.owned_set()?.clone();
        if source == target {
            return Err(branch::invalid_merge(
                "a branch cannot be merged into itself",
            ));
        }
        if matches!(o.on_conflict, Some(Take::Objects(_))) {
            return Err(branch::invalid_merge(
                "onConflict takes ours, theirs, union or fail",
            ));
        }
        let src = self.branch(source)?;
        let tgt = self.branch(target)?;
        let (sid, tid) = (src.dataset_id, tgt.dataset_id);
        for _attempt in 0..3 {
            let s_head = src.head_commit().seq;
            let t_head = tgt.head_commit().seq;
            let moved = |which: &str, want: u64, now: u64| {
                branch::conflict(
                    "head-moved",
                    format!("the {which} head is commit {now}, not {want}; read the report again"),
                )
            };
            if let Some(e) = o.expect_source.filter(|e| *e != s_head) {
                return Err(moved("source", e, s_head));
            }
            if let Some(e) = o.expect_target.filter(|e| *e != t_head) {
                return Err(moved("target", e, t_head));
            }
            let sc = set.normalize(CommitRef {
                branch_id: sid,
                seq: s_head,
            });
            let tc = set.normalize(CommitRef {
                branch_id: tid,
                seq: t_head,
            });
            let bases = set.merge_bases(sc, tc)?;
            let base = match (o.base, bases.len()) {
                (Some(b), _) => {
                    let b = set.normalize(b);
                    if !bases.contains(&b) {
                        return Err(branch::invalid_merge(format!(
                            "commit {} of {} is not a merge base of these branches",
                            b.seq,
                            set.name_of(b.branch_id)
                                .unwrap_or_else(|| b.branch_id.to_string())
                        )));
                    }
                    b
                }
                (None, 1) => bases[0],
                (None, _) => {
                    let mut e = BranchError {
                        kind: BranchErrorKind::Conflict,
                        code: "ambiguous-merge-base",
                        message: format!(
                            "{source} and {target} have {} merge bases; choose one with base",
                            bases.len()
                        ),
                        conflicts: None,
                        candidates: Vec::new(),
                        inherited: None,
                    };
                    e.candidates = bases.iter().map(|b| set.named(*b)).collect();
                    return Err(Error::Branch(Box::new(e)));
                }
            };
            let named_src = NamedCommitRef {
                branch: Some(source.to_string()),
                branch_id: sid,
                seq: s_head,
            };
            let named_tgt = NamedCommitRef {
                branch: Some(target.to_string()),
                branch_id: tid,
                seq: t_head,
            };
            let mut report = MergeReport {
                merged: false,
                up_to_date: false,
                fast_forward: false,
                source: named_src.clone(),
                target: named_tgt.clone(),
                base: Some(set.named(base)),
                inserted: 0,
                deleted: 0,
                conflicts_found: 0,
                conflicts_resolved: 0,
                commit: None,
                inferences_excluded: None,
                conflicts: None,
            };
            if base == sc {
                report.up_to_date = true;
                return Ok(MergeOutcome::UpToDate(report));
            }
            let ff = base == tc;
            if o.ff_only && !ff {
                return Err(branch::conflict(
                    "not-fast-forward",
                    format!("{target} has moved since the merge base; this is not a fast-forward"),
                ));
            }
            report.fast_forward = ff;
            let dopts = DiffOptions {
                max_quads: o.max_quads,
                cancel: o.cancel.clone(),
                deadline: o.deadline,
                ..Default::default()
            };
            let t_o = if ff {
                Toggles::default()
            } else {
                self.toggles(&set, base, tc, &dopts)?
            };
            let t_t = self.toggles(&set, base, sc, &dopts)?;
            let snap = tgt.snapshot();
            let plan = plan_merge(&snap, t_o, t_t, o)?;
            report.inserted = plan.changes.values().filter(|i| **i).count() as u64;
            report.deleted = plan.changes.len() as u64 - report.inserted;
            report.conflicts_found = plan.found;
            report.conflicts_resolved = plan.resolved;
            report.inferences_excluded = plan.excluded;
            if !plan.remaining.is_empty() {
                let c = conflict_report(&snap, &plan, o, &named_src, &named_tgt, &report)?;
                if preview {
                    report.conflicts = Some(c);
                    return Ok(MergeOutcome::Merged(report));
                }
                return Ok(MergeOutcome::Conflicts(Box::new(c)));
            }
            if preview {
                return Ok(MergeOutcome::Merged(report));
            }
            let mut wo = o.write.clone();
            if wo.message.is_none() {
                wo.message = Some(format!("merge {source} (commit {s_head}) into {target}").into());
            }
            let mut txn = tgt.try_write_with(CommitKind::Merge, wo)?;
            if txn.guard.head.seq != t_head {
                drop(txn);
                if o.expect_target.is_some() {
                    return Err(branch::conflict(
                        "head-moved",
                        format!("the target head moved past commit {t_head}"),
                    ));
                }
                continue;
            }
            txn.force = true;
            txn.merge = Some(MergeRec {
                seq: 0,
                source: sc,
                resolved: plan.resolved,
                flags: if ff { MERGE_FAST_FORWARD } else { 0 },
            });
            let mut changes: Vec<(&QuadKey, &bool)> = plan.changes.iter().collect();
            // deletions first, each in key order, for a deterministic log
            changes.sort_unstable_by(|a, b| (a.1, a.0).cmp(&(b.1, b.0)));
            for (i, (k, insert)) in changes.into_iter().enumerate() {
                if i % 65_536 == 65_535 {
                    txn.opts.check()?;
                }
                match quad_ids(&mut txn, k, *insert)? {
                    Some(q) if *insert => {
                        txn.insert(q)?;
                    }
                    Some(q) => {
                        txn.delete(q)?;
                    }
                    None => {}
                }
            }
            let receipt = txn.commit()?;
            report.merged = true;
            report.commit = Some(receipt);
            return Ok(MergeOutcome::Merged(report));
        }
        Err(branch::conflict(
            "head-moved",
            format!("{target} kept moving during the merge; try again"),
        ))
    }
}

/// Group the toggles, find conflicts, apply resolutions and the rule, and keep blank
/// nodes whole.
fn plan_merge(
    snap: &Snapshot,
    mut t_o: Toggles,
    mut t_t: Toggles,
    o: &MergeOptions,
) -> Result<Plan> {
    let mut excluded = None;
    if !o.include_inferences {
        let inferred: Arc<[u8]> = crate::id::iri_key(INFERRED_GRAPH).into();
        t_o.retain(|k, _| k[0] != inferred);
        let before = t_t.len();
        t_t.retain(|k, _| k[0] != inferred);
        excluded = Some((before - t_t.len()) as u64);
    }
    let scope = o.scope;
    let mut resolutions: Vec<KeyedResolution> = o
        .resolutions
        .iter()
        .map(|r| keyed(r, scope))
        .collect::<Result<_>>()?;
    let mut groups: std::collections::BTreeMap<GroupKey, Sides> = Default::default();
    for (k, a) in &t_o {
        groups
            .entry(group_of(k, scope))
            .or_default()
            .0
            .push((k.clone(), *a));
    }
    for (k, a) in &t_t {
        groups
            .entry(group_of(k, scope))
            .or_default()
            .1
            .push((k.clone(), *a));
    }
    let mut plan = Plan {
        changes: Default::default(),
        found: 0,
        resolved: 0,
        remaining: Vec::new(),
        excluded,
    };
    // inserts a side made that the result leaves out, for the blank-node rule
    let mut dropped_theirs: Vec<QuadKey> = Vec::new();
    let mut dropped_ours: Vec<QuadKey> = Vec::new();
    for (gk, (mut ours, mut theirs)) in groups {
        let union = |plan: &mut Plan, ours: &[(QuadKey, bool)], theirs: &[(QuadKey, bool)]| {
            let o_keys: FxHashSet<&QuadKey> = ours.iter().map(|(k, _)| k).collect();
            for (k, a) in theirs {
                if !o_keys.contains(k) {
                    plan.changes.insert(k.clone(), *a);
                }
            }
        };
        if scope == ConflictScope::Quad || ours.is_empty() || theirs.is_empty() {
            union(&mut plan, &ours, &theirs);
            continue;
        }
        ours.sort();
        theirs.sort();
        if ours == theirs {
            continue;
        }
        plan.found += 1;
        let take =
            pick(&mut resolutions, &gk).or_else(|| o.on_conflict.clone().map(|t| (t, Vec::new())));
        let Some((take, objs)) = take else {
            plan.remaining.push((gk, (ours, theirs)));
            continue;
        };
        plan.resolved += 1;
        let o_map: FxHashMap<&QuadKey, bool> = ours.iter().map(|(k, a)| (k, *a)).collect();
        let t_map: FxHashMap<&QuadKey, bool> = theirs.iter().map(|(k, a)| (k, *a)).collect();
        match take {
            Take::Ours => {
                dropped_theirs.extend(theirs.iter().filter(|(_, a)| *a).map(|(k, _)| k.clone()));
            }
            Take::Theirs => {
                for (k, a) in &theirs {
                    if !o_map.contains_key(k) {
                        plan.changes.insert(k.clone(), *a);
                    }
                }
                for (k, a) in &ours {
                    if !t_map.contains_key(k) {
                        plan.changes.insert(k.clone(), !*a);
                        if *a {
                            dropped_ours.push(k.clone());
                        }
                    }
                }
            }
            Take::Base => {
                for (k, a) in &ours {
                    plan.changes.insert(k.clone(), !*a);
                    if *a {
                        dropped_ours.push(k.clone());
                    }
                }
                dropped_theirs.extend(theirs.iter().filter(|(_, a)| *a).map(|(k, _)| k.clone()));
            }
            Take::Union => union(&mut plan, &ours, &theirs),
            Take::Objects(_) => {
                let Some(p) = gk.2.clone() else {
                    return Err(branch::invalid_merge(
                        "take objects needs the cell scope and a subject and predicate",
                    ));
                };
                let current = group_quads(snap, &gk)?;
                let want: FxHashSet<QuadKey> = objs
                    .iter()
                    .map(|obj| [gk.0.clone(), gk.1.clone(), p.clone(), obj.clone()])
                    .collect();
                for k in current.iter().filter(|k| !want.contains(*k)) {
                    plan.changes.insert(k.clone(), false);
                    if o_map.get(k) == Some(&true) {
                        dropped_ours.push(k.clone());
                    }
                }
                for k in want.iter().filter(|k| !current.contains(*k)) {
                    plan.changes.insert(k.clone(), true);
                }
                dropped_theirs.extend(
                    theirs
                        .iter()
                        .filter(|(k, a)| *a && !want.contains(k))
                        .map(|(k, _)| k.clone()),
                );
            }
        }
    }
    if let Some(r) = resolutions.iter().find(|r| !r.used) {
        let what = match (&r.subject, &r.predicate) {
            (Some(s), Some(p)) => format!(
                "{} {}",
                nt(s).unwrap_or_default(),
                nt(p).unwrap_or_default()
            ),
            (Some(s), None) => nt(s).unwrap_or_default(),
            _ => nt(&r.graph).unwrap_or_else(|| "the default graph".into()),
        };
        return Err(branch::invalid_merge(format!(
            "the resolution for {what} covers no conflict; the report may be stale"
        )));
    }
    keep_bnodes_whole(snap, &mut plan, &t_o, &t_t, dropped_ours, dropped_theirs)?;
    Ok(plan)
}

/// The most specific resolution that covers group `gk`, marked used, with the keys of
/// its objects for `take: objects`.
fn pick(rs: &mut [KeyedResolution], gk: &GroupKey) -> Option<(Take, Vec<Arc<[u8]>>)> {
    let mut best: Option<(usize, u8)> = None;
    for (i, r) in rs.iter().enumerate() {
        if r.graph != gk.0 {
            continue;
        }
        let rank = match (&r.subject, &r.predicate, &gk.2) {
            (None, None, _) => 1,
            (Some(s), None, _) if *s == gk.1 => 2,
            (Some(s), Some(p), Some(gp)) if *s == gk.1 && p == gp => 3,
            _ => continue,
        };
        if best.is_none_or(|(_, b)| rank > b) {
            best = Some((i, rank));
        }
    }
    let (i, _) = best?;
    rs[i].used = true;
    Some((rs[i].take.clone(), rs[i].objects.clone()))
}

fn keyed(r: &Resolution, scope: ConflictScope) -> Result<KeyedResolution> {
    let objects = match &r.take {
        Take::Objects(objs) => {
            if scope != ConflictScope::Cell || r.subject.is_none() || r.predicate.is_none() {
                return Err(branch::invalid_merge(
                    "take objects needs the cell scope and a subject and predicate",
                ));
            }
            objs.iter().map(|t| term_key(t).into()).collect()
        }
        _ => Vec::new(),
    };
    if r.predicate.is_some() && r.subject.is_none() {
        return Err(branch::invalid_merge(
            "a resolution with a predicate needs a subject",
        ));
    }
    if r.predicate.is_some() && scope == ConflictScope::Subject {
        return Err(branch::invalid_merge(
            "with the subject scope, resolutions name a graph or a subject",
        ));
    }
    Ok(KeyedResolution {
        graph: graph_key(&r.graph).into(),
        subject: r.subject.as_ref().map(|s| term_key(s).into()),
        predicate: r
            .predicate
            .as_ref()
            .map(|p| crate::id::iri_key(p.as_str()).into()),
        take: r.take.clone(),
        objects,
        used: false,
    })
}

/// The quads of group `gk` in the target (as keys).
fn group_quads(snap: &Snapshot, gk: &GroupKey) -> Result<FxHashSet<QuadKey>> {
    let mut out = FxHashSet::default();
    let ids = [
        diff::key_id(snap, &gk.0),
        diff::key_id(snap, &gk.1),
        match &gk.2 {
            Some(p) => diff::key_id(snap, p),
            None => Some(Id::UNDEF),
        },
    ];
    let (Some(g), Some(s), Some(p)) = (ids[0], ids[1], ids[2]) else {
        return Ok(out);
    };
    let prefix: Vec<u64> = if gk.2.is_some() {
        vec![g.0, s.0, p.0]
    } else {
        vec![g.0, s.0]
    };
    let mut keys = diff::Keys::new(&snap.generation);
    for k in snap.scan_keys(Perm::Gspo, &prefix)? {
        out.insert(keys.quad(&Perm::Gspo.to_quad(&k))?);
    }
    Ok(out)
}

/// When a resolution leaves out a side's insert of a quad whose object is a blank node,
/// and nothing else refers to that node after the merge, the side's other inserts with
/// that node as subject go too, recursively. A list or a structured value thus stays
/// whole or goes as a whole.
fn keep_bnodes_whole(
    snap: &Snapshot,
    plan: &mut Plan,
    t_o: &Toggles,
    t_t: &Toggles,
    dropped_ours: Vec<QuadKey>,
    dropped_theirs: Vec<QuadKey>,
) -> Result<()> {
    let is_bnode = |k: &[u8]| k.first() == Some(&b'_');
    let mut todo: Vec<Arc<[u8]>> = dropped_ours
        .iter()
        .chain(dropped_theirs.iter())
        .filter(|k| is_bnode(&k[3]))
        .map(|k| k[3].clone())
        .collect();
    if todo.is_empty() {
        return Ok(());
    }
    let mut seen: FxHashSet<Arc<[u8]>> = FxHashSet::default();
    // the plan's net references to each blank node it names as an object
    let mut plan_refs: FxHashMap<Arc<[u8]>, i64> = FxHashMap::default();
    for (k, insert) in &plan.changes {
        if is_bnode(&k[3]) {
            *plan_refs.entry(k[3].clone()).or_default() += if *insert { 1 } else { -1 };
        }
    }
    // quads of each side's toggles by subject
    let mut theirs_by_subject: FxHashMap<Arc<[u8]>, Vec<QuadKey>> = FxHashMap::default();
    for (k, a) in t_t {
        if *a {
            theirs_by_subject
                .entry(k[1].clone())
                .or_default()
                .push(k.clone());
        }
    }
    let mut ours_by_subject: FxHashMap<Arc<[u8]>, Vec<QuadKey>> = FxHashMap::default();
    for (k, a) in t_o {
        if *a {
            ours_by_subject
                .entry(k[1].clone())
                .or_default()
                .push(k.clone());
        }
    }
    while let Some(x) = todo.pop() {
        if !seen.insert(x.clone()) {
            continue;
        }
        // references to x after the merge: in the target, plus the plan's inserts, less
        // its deletions
        let id = diff::key_id(snap, &x);
        let refs: i64 = match id {
            Some(id) => snap.count(Perm::Osp, &[id.0])? as i64,
            None => 0,
        } + plan_refs.get(&x).copied().unwrap_or(0);
        if refs > 0 {
            continue;
        }
        // theirs' inserts with x as subject that the plan would make
        for k in theirs_by_subject.get(&x).into_iter().flatten() {
            if plan.changes.get(k) == Some(&true) {
                plan.changes.remove(k);
                if is_bnode(&k[3]) {
                    *plan_refs.entry(k[3].clone()).or_default() -= 1;
                    todo.push(k[3].clone());
                }
            }
        }
        // ours' inserts with x as subject that the target holds
        for k in ours_by_subject.get(&x).into_iter().flatten() {
            if plan.changes.get(k) != Some(&false) {
                plan.changes.insert(k.clone(), false);
                if is_bnode(&k[3]) {
                    *plan_refs.entry(k[3].clone()).or_default() -= 1;
                    todo.push(k[3].clone());
                }
            }
        }
    }
    Ok(())
}

/// The report of the conflicts that remain.
fn conflict_report(
    snap: &Snapshot,
    plan: &Plan,
    o: &MergeOptions,
    source: &NamedCommitRef,
    target: &NamedCommitRef,
    report: &MergeReport,
) -> Result<ConflictReport> {
    let limit = match o.limit {
        0 => 100,
        n => n.min(10_000),
    };
    let mut by_graph: std::collections::BTreeMap<Option<String>, u64> = Default::default();
    for (gk, _) in &plan.remaining {
        *by_graph.entry(nt(&gk.0)).or_default() += 1;
    }
    let mut cells = Vec::new();
    for (gk, (ours, theirs)) in plan.remaining.iter().take(limit) {
        // the group in ours, then the base (ours with its toggles undone), then theirs
        let o_set = group_quads(snap, gk)?;
        let mut b_set = o_set.clone();
        for (k, a) in ours {
            if *a {
                b_set.remove(k);
            } else {
                b_set.insert(k.clone());
            }
        }
        let mut t_set = b_set.clone();
        for (k, a) in theirs {
            if *a {
                t_set.insert(k.clone());
            } else {
                t_set.remove(k);
            }
        }
        let show = |s: &FxHashSet<QuadKey>| -> Vec<String> {
            let mut v: Vec<String> = s
                .iter()
                .map(|k| match gk.2 {
                    Some(_) => nt(&k[3]).unwrap_or_default(),
                    None => format!(
                        "{} {}",
                        nt(&k[2]).unwrap_or_default(),
                        nt(&k[3]).unwrap_or_default()
                    ),
                })
                .collect();
            v.sort();
            v.truncate(100);
            v
        };
        cells.push(ConflictCell {
            graph: nt(&gk.0),
            subject: nt(&gk.1).unwrap_or_default(),
            predicate: gk.2.as_ref().and_then(|p| nt(p)),
            base: show(&b_set),
            ours: show(&o_set),
            theirs: show(&t_set),
        });
    }
    let n = plan.remaining.len() as u64;
    let sname = source.branch.clone().unwrap_or_default();
    let tname = target.branch.clone().unwrap_or_default();
    Ok(ConflictReport {
        error: format!(
            "{n} conflict{} merging {sname} (commit {}) into {tname} (commit {})",
            if n == 1 { "" } else { "s" },
            source.seq,
            target.seq
        ),
        code: "merge-conflict",
        source: source.clone(),
        target: target.clone(),
        base: report.base.clone(),
        scope: o.scope,
        conflicts: n,
        truncated: plan.remaining.len() > limit,
        graphs: by_graph
            .into_iter()
            .map(|(graph, conflicts)| GraphConflicts { graph, conflicts })
            .collect(),
        cells,
    })
}

/// The ids of a quad key, adding its terms (tests that write merges by hand).
#[cfg(test)]
pub(crate) fn tests_quad_ids(txn: &mut WriteTxn<'_>, k: &QuadKey) -> Result<[Id; 4]> {
    Ok(quad_ids(txn, k, true)?.expect("terms are added"))
}

/// The error a merge stopped by conflicts answers with.
pub fn conflict_error(c: ConflictReport) -> Error {
    Error::Branch(Box::new(BranchError {
        kind: BranchErrorKind::Conflict,
        code: "merge-conflict",
        message: c.error.clone(),
        conflicts: Some(Box::new(c)),
        candidates: Vec::new(),
        inherited: None,
    }))
}
