//! Write previews (dry runs) of the store: what a commit through the WAL or a bulk
//! commit would be, assembled where the write would have committed. See
//! [`crate::preview`] and `docs/specs/C15-write-previews.md`.

use super::*;
use crate::guard::{Changes, ValidationSummary, WriteOptions};
use crate::preview::{DryRun, GraphChange, Preview, StorageCheck};
use rustc_hash::FxHashMap;

/// The name of graph `g` in `snap`.
fn graph_name(snap: &Snapshot, g: Id) -> Option<GraphName> {
    if g == Id::DEFAULT_GRAPH {
        return Some(GraphName::DefaultGraph);
    }
    match snap.term(g)? {
        Term::NamedNode(n) => Some(GraphName::NamedNode(n)),
        Term::BlankNode(b) => Some(GraphName::BlankNode(b)),
        _ => None,
    }
}

/// The default graph first, then named graphs by IRI, then blank-node graphs by label.
fn graph_order(g: &GraphName) -> (u8, &str) {
    match g {
        GraphName::DefaultGraph => (0, ""),
        GraphName::NamedNode(n) => (1, n.as_str()),
        GraphName::BlankNode(b) => (2, b.as_str()),
    }
}

/// The id of graph `g` in `snap`, if it has one.
fn graph_id(snap: &Snapshot, g: &GraphName) -> Option<Id> {
    match g {
        GraphName::DefaultGraph => Some(Id::DEFAULT_GRAPH),
        GraphName::NamedNode(n) => snap.lookup_iri(n.as_str()),
        GraphName::BlankNode(b) => parse_bnode_label(b.as_str()),
    }
}

/// The quads of graph `g` in `snap`.
fn graph_len(snap: &Snapshot, g: &GraphName) -> Result<u64> {
    match graph_id(snap, g) {
        Some(id) => snap.count(Perm::Gspo, &[id.0]),
        None => Ok(0),
    }
}

/// The net changes of a WAL path change log: each quad whose presence differs at the
/// end, with whether it was added, removals first. The log holds effective changes only,
/// so a quad was present at the start iff its first change is a delete, and is present
/// at the end iff its last change is an insert.
fn net_log(log: &[(u8, [Id; 4])]) -> Vec<([Id; 4], bool)> {
    let mut ends: FxHashMap<[Id; 4], (u8, u8)> = FxHashMap::default();
    for (op, q) in log {
        ends.entry(*q).or_insert((*op, *op)).1 = *op;
    }
    let mut net: Vec<([Id; 4], bool)> = ends
        .into_iter()
        .filter_map(|(q, (first, last))| {
            let (was, is) = (first == WAL_DELETE, last == WAL_INSERT);
            (was != is).then_some((q, is))
        })
        .collect();
    net.sort_unstable_by_key(|&(q, added)| (added, q));
    net
}

/// The outcome of a write's precondition, checked on the head (the writer lock is held,
/// so this is the head the write would see).
fn precondition(store: &Store, o: &WriteOptions) -> Option<std::result::Result<(), String>> {
    o.precondition
        .as_ref()
        .map(|p| match p.check(&store.snapshot()) {
            Ok(()) => Ok(()),
            Err(Error::PreconditionFailed(m)) => Err(m),
            Err(e) => Err(e.to_string()),
        })
}

/// `rows` budget error past `max` changes (0: no limit).
fn over(n: u64, max: u64) -> Result<()> {
    if max > 0 && n > max {
        return Err(Error::BudgetExceeded(crate::Budget {
            kind: crate::BudgetKind::Rows,
            limit: max,
            requested: n,
        }));
    }
    Ok(())
}

/// What a bulk commit's preview is made of.
pub(super) struct BulkPreview<'a> {
    /// the head the write ran against
    pub head: CommitInfo,
    /// the state the rebuild started from: the head, or a write transaction's view
    pub view: &'a Snapshot,
    /// the state on the built generation
    pub candidate: &'a Snapshot,
    pub commit: CommitInfo,
    pub changes: Changes<'a>,
    /// graphs the rebuild left out (a replace)
    pub drop_graphs: &'a [Id],
    pub opts: &'a WriteOptions,
    pub dr: &'a DryRun,
    pub validation: Option<Arc<ValidationSummary>>,
    pub storage: StorageCheck,
    pub message: Option<Arc<str>>,
}

impl Store {
    /// The storage numbers of a rebuild into `new` that replaces `old`: the quota's, or
    /// an in-memory store's size limit.
    pub(super) fn rebuild_storage(&self, old: Option<&Path>, new: &Path) -> StorageCheck {
        if self.root.is_some() {
            return match self.quota.project_rebuild(old, new) {
                Some((limit, used, projected)) => StorageCheck {
                    refused: None,
                    limit: Some(limit),
                    used: Some(used),
                    projected: Some(projected),
                },
                None => StorageCheck::default(),
            };
        }
        match self.opts.max_memory_bytes {
            Some(max) => StorageCheck {
                refused: None,
                limit: Some(max),
                used: None,
                projected: Some(dir_size(new)),
            },
            None => StorageCheck::default(),
        }
    }

    /// The preview of a bulk commit, made after its generation was built and validated.
    pub(super) fn preview_bulk(&self, b: BulkPreview<'_>) -> Result<Preview> {
        let head_snap = self.snapshot();
        // the graphs the write may have changed
        let mut names: Vec<GraphName> = match b.changes {
            Changes::Rebuilt { log, bulk } => {
                let mut ids: Vec<Id> = log.iter().map(|(_, q)| q[3]).collect();
                ids.extend(bulk.iter().map(|q| q[3]));
                ids.sort_unstable();
                ids.dedup();
                ids.into_iter()
                    .filter_map(|g| graph_name(b.view, g))
                    .collect()
            }
            _ => {
                let mut v = vec![GraphName::DefaultGraph];
                for s in [&*head_snap, b.candidate] {
                    for g in s.graph_ids()? {
                        v.extend(graph_name(s, g));
                    }
                }
                v
            }
        };
        names.sort_by(|x, y| graph_order(x).cmp(&graph_order(y)));
        names.dedup();
        // a transaction's own deletions, per graph
        let mut log_deleted: FxHashMap<Id, u64> = FxHashMap::default();
        if let Changes::Rebuilt { log, .. } = b.changes {
            for (q, added) in net_log(log) {
                if !added {
                    *log_deleted.entry(q[3]).or_default() += 1;
                }
            }
        }
        let mut graphs = Vec::new();
        for name in names {
            let old = graph_len(&head_snap, &name)?;
            let new = graph_len(b.candidate, &name)?;
            let dropped = graph_id(&head_snap, &name).is_some_and(|g| b.drop_graphs.contains(&g));
            let deleted = if dropped {
                old
            } else {
                graph_id(b.view, &name)
                    .and_then(|g| log_deleted.get(&g).copied())
                    .unwrap_or(0)
            };
            let inserted = (new + deleted).saturating_sub(old);
            if inserted > 0 || deleted > 0 {
                graphs.push(GraphChange {
                    graph: name,
                    inserted,
                    deleted,
                });
            }
        }
        let (changes, changes_total) = if b.dr.changes > 0 || b.dr.all_changes {
            let o = DiffOptions {
                max_quads: b.dr.max_changes,
                cancel: b.opts.cancel.clone(),
                deadline: b.opts.deadline,
                ..Default::default()
            };
            let mut all = diff::compare_changes(&head_snap, b.candidate, &o)?;
            let total = all.len() as u64;
            if !b.dr.all_changes {
                all.truncate(b.dr.changes);
            }
            (all, Some(total))
        } else {
            (Vec::new(), None)
        };
        Ok(Preview {
            dataset_id: self.owner_dataset_id(),
            head: b.head,
            commit: Some(b.commit),
            kind: b.commit.kind,
            message: b.message,
            graphs,
            changes,
            changes_total,
            validation: b.validation,
            precondition: precondition(self, b.opts),
            storage: b.storage,
        })
    }
}

impl WriteTxn<'_> {
    /// The preview of a commit through the WAL, made where `publish_log` would write it.
    pub(super) fn preview_log(&mut self, dr: &DryRun) -> Result<Preview> {
        let store = self.store;
        let head = self.guard.head;
        let pre = precondition(store, &self.opts);
        let changed = self.net_ins != 0 || self.net_del != 0;
        let (commit, validation, storage, message) = if changed {
            let validation = match store.run_guard(
                &self.base,
                || Arc::new(self.view()),
                self.kind,
                Changes::Log(&self.log),
                &self.opts,
                head.seq,
            ) {
                Ok(v) => v,
                Err(Error::Rejected(r)) => Some(Arc::new(r.summary)),
                Err(e) => return Err(e),
            };
            let message = match &self.opts.message {
                Some(m) => crate::annotations::validate_message(m)?,
                None => None,
            };
            let mut storage = match (&store.root, store.opts.max_memory_bytes) {
                (Some(_), _) => match store.quota.project_commit(self.wal_bytes()) {
                    Some((limit, used, projected)) => StorageCheck {
                        refused: None,
                        limit: Some(limit),
                        used: Some(used),
                        projected: Some(projected),
                    },
                    None => StorageCheck::default(),
                },
                (None, Some(max)) => StorageCheck {
                    refused: None,
                    limit: Some(max),
                    used: None,
                    projected: Some(self.memory_size()),
                },
                (None, None) => StorageCheck::default(),
            };
            storage.refused = self.check_storage().err();
            let commit = self.next_commit(validation.as_deref());
            (Some(commit), validation, storage, message)
        } else {
            (None, None, StorageCheck::default(), None)
        };
        let view = self.view();
        let net = net_log(&self.log);
        let mut per: FxHashMap<Id, (u64, u64)> = FxHashMap::default();
        for (q, added) in &net {
            let e = per.entry(q[3]).or_default();
            if *added {
                e.0 += 1;
            } else {
                e.1 += 1;
            }
        }
        let mut graphs: Vec<GraphChange> = per
            .into_iter()
            .filter_map(|(g, (inserted, deleted))| {
                graph_name(&view, g).map(|graph| GraphChange {
                    graph,
                    inserted,
                    deleted,
                })
            })
            .collect();
        graphs.sort_by(|x, y| graph_order(&x.graph).cmp(&graph_order(&y.graph)));
        let (changes, changes_total) = if dr.changes > 0 || dr.all_changes {
            let total = net.len() as u64;
            let n = if dr.all_changes {
                over(total, dr.max_changes)?;
                net.len()
            } else {
                dr.changes.min(net.len())
            };
            let listed = net[..n]
                .iter()
                .filter_map(|(q, added)| {
                    let op = if *added { DiffOp::Add } else { DiffOp::Remove };
                    view.quad_to_terms(q).map(|t| (op, t))
                })
                .collect();
            (listed, Some(total))
        } else {
            (Vec::new(), None)
        };
        Ok(Preview {
            dataset_id: store.owner_dataset_id(),
            head,
            commit,
            kind: self.kind,
            message,
            graphs,
            changes,
            changes_total,
            validation,
            precondition: pre,
            storage,
        })
    }
}
