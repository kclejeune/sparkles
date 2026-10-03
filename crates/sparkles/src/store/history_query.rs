//! History queries: the recorded changes of a range of commits, filtered by subject,
//! predicate, object, graph and operation ([`Store::history_changes`]), read from the
//! change log ([`ChangeLog`]).
//!
//! The model follows a system-versioned table (SQL:2011) and an append-only log of
//! assertions and retractions: each row is one net change a commit made, an addition or
//! a removal of a quad, with the commit's number, time, kind, author and message. A
//! query names the commits by number or time, and its terms are looked up in the log's
//! index of subjects, predicates, objects and graphs, so it reads only the records that
//! may match.

use super::changelog::{LogFilter, Unrecorded};
use super::diff::{QuadKey, key_quad, visible_key};
use super::*;
use rustc_hash::FxHashMap;
use std::time::Instant;

/// One end of the range of commits a history query reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryBound {
    /// a commit number
    Commit(u64),
    /// the commits made at or after (for `from`) or at or before (for `to`) this time,
    /// in milliseconds since the Unix epoch
    Time(i64),
    /// a selector of [`At`](crate::history::At) (`head`, `commit:N`, `time:…`,
    /// `snapshot:NAME`)
    At(crate::history::At),
}

/// What a history query asks for. Empty term lists match anything.
#[derive(Clone, Default)]
pub struct HistoryQuery {
    pub subjects: Vec<Term>,
    pub predicates: Vec<NamedNode>,
    pub objects: Vec<Term>,
    /// the graphs (the default graph included) whose changes are listed
    pub graphs: Vec<GraphName>,
    /// the first commit whose changes are listed (default: the first)
    pub from: Option<HistoryBound>,
    /// the last commit whose changes are listed (default: the head)
    pub to: Option<HistoryBound>,
    /// only additions or only removals
    pub op: Option<DiffOp>,
    /// the most changes listed (0: no limit)
    pub limit: usize,
    /// newest commits first
    pub descending: bool,
    /// the graphs and triples the caller may read (`None`: everything)
    pub access: Option<Arc<crate::access::GraphAccess>>,
    pub cancel: Option<Arc<AtomicBool>>,
    pub deadline: Option<Instant>,
}

/// One recorded change.
#[derive(Clone, Debug)]
pub struct HistoryChange {
    pub commit: Arc<ChangeCommit>,
    pub op: DiffOp,
    pub quad: Quad,
}

/// The changes a history query found.
#[derive(Clone, Debug)]
pub struct HistoryResult {
    /// the range of commits read (`from > to` when it is empty)
    pub from: u64,
    pub to: u64,
    /// the newest commit the query could see
    pub head: u64,
    pub changes: Vec<HistoryChange>,
    /// commits in the range whose changes are not recorded
    pub unrecorded: Vec<Unrecorded>,
    /// the limit cut the list short
    pub truncated: bool,
}

/// The vocabulary key a term has in a recorded change. A literal with an inline id is
/// recorded in its canonical form.
fn filter_key(t: &Term) -> Vec<u8> {
    if let Term::Literal(_) = t
        && let Some(l) = id::inline_id(t).and_then(id::inline_to_literal)
    {
        let mut k = Vec::new();
        id::write_literal_key(&l, &mut k);
        return k;
    }
    id::term_key(t)
}

fn graph_filter_key(g: &GraphName) -> Vec<u8> {
    match g {
        GraphName::DefaultGraph => Vec::new(),
        GraphName::NamedNode(n) => id::iri_key(n.as_str()),
        GraphName::BlankNode(b) => id::term_key(&Term::BlankNode(b.clone())),
    }
}

impl HistoryQuery {
    fn filter(&self) -> LogFilter {
        let list = |v: Vec<Vec<u8>>| (!v.is_empty()).then_some(v);
        LogFilter {
            keys: [
                list(self.graphs.iter().map(graph_filter_key).collect()),
                list(self.subjects.iter().map(filter_key).collect()),
                list(
                    self.predicates
                        .iter()
                        .map(|p| id::iri_key(p.as_str()))
                        .collect(),
                ),
                list(self.objects.iter().map(filter_key).collect()),
            ],
        }
    }

    fn check(&self) -> Result<()> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            return Err(Error::Cancelled);
        }
        if self.deadline.is_some_and(|t| Instant::now() > t) {
            return Err(Error::Timeout);
        }
        Ok(())
    }
}

/// Refuse a caller whose triple protections depend on the data (classes or patterns):
/// which of a past triple's changes it may see would need the state of every commit.
pub(crate) fn check_history_access(a: Option<&Arc<crate::access::GraphAccess>>) -> Result<()> {
    if a.and_then(|a| a.triples.as_ref())
        .is_some_and(|t| t.hides() && !t.state_independent())
    {
        return Err(Error::NotPermitted(
            "history queries are not available to a caller whose protections depend on the \
             data (classes or patterns)"
                .into(),
        ));
    }
    Ok(())
}

impl ChangeLog {
    /// Run a history query against the commits up to `upto` (the state the caller
    /// reads). Bounds given as [`HistoryBound::At`] must already be resolved.
    pub fn query(&self, q: &HistoryQuery, upto: u64) -> Result<HistoryResult> {
        if !self.is_enabled() {
            return Err(Error::HistoryUnsupported(
                "the change log of this dataset is off; history queries need it".into(),
            ));
        }
        check_history_access(q.access.as_ref())?;
        // what the commits up to now queued
        self.flush(false)?;
        let from = match &q.from {
            None => 1,
            Some(HistoryBound::Commit(n)) => (*n).max(1),
            Some(HistoryBound::Time(ms)) => self
                .commit_at_time(ms.saturating_sub(1))?
                .map_or(1, |c| c + 1),
            Some(HistoryBound::At(a)) => {
                return Err(Error::Invalid(format!(
                    "the selector {a} must be resolved by the store"
                )));
            }
        };
        let to = match &q.to {
            None => upto,
            Some(HistoryBound::Commit(n)) => (*n).min(upto),
            Some(HistoryBound::Time(ms)) => self.commit_at_time(*ms)?.unwrap_or(0).min(upto),
            Some(HistoryBound::At(a)) => {
                return Err(Error::Invalid(format!(
                    "the selector {a} must be resolved by the store"
                )));
            }
        };
        let filter = q.filter();
        let access = q.access.as_ref().filter(|a| !a.reads_everything());
        let mut readable: FxHashMap<Arc<[u8]>, bool> = FxHashMap::default();
        let mut changes = Vec::new();
        let mut truncated = false;
        let mut n = 0u64;
        let mut check = || {
            n += 1;
            if n & 0xFF == 0 { q.check() } else { Ok(()) }
        };
        let unrecorded = self.scan(
            from,
            to,
            &filter,
            q.descending,
            &mut check,
            &mut |c, add, k: &QuadKey| {
                let op = if add { DiffOp::Add } else { DiffOp::Remove };
                if q.op.is_some_and(|o| o != op) {
                    return Ok(true);
                }
                if access.is_some_and(|a| !visible_key(a, &mut readable, k)) {
                    return Ok(true);
                }
                if q.limit > 0 && changes.len() >= q.limit {
                    truncated = true;
                    return Ok(false);
                }
                if let Some(quad) = key_quad(k) {
                    changes.push(HistoryChange {
                        commit: c.clone(),
                        op,
                        quad,
                    });
                }
                Ok(true)
            },
        )?;
        Ok(HistoryResult {
            from,
            to,
            head: upto,
            changes,
            unrecorded,
            truncated,
        })
    }
}

impl Store {
    /// The recorded changes that match `q`, read from the change log (see
    /// [`HistoryQuery`]). Changes in graphs or of triples the caller may not read are
    /// left out. Commits whose changes the log does not hold are listed in
    /// [`HistoryResult::unrecorded`].
    pub fn history_changes(&self, q: &HistoryQuery) -> Result<HistoryResult> {
        let Some(log) = &self.changelog else {
            return Err(Error::HistoryUnsupported(
                "this store has no change log".into(),
            ));
        };
        let head = self.head_commit();
        let resolve = |b: &Option<HistoryBound>| -> Result<Option<HistoryBound>> {
            Ok(match b {
                Some(HistoryBound::At(a)) => Some(match a {
                    crate::history::At::Time(ms) => HistoryBound::Time(*ms),
                    a => HistoryBound::Commit(self.resolve_seq(a, head)?),
                }),
                b => b.clone(),
            })
        };
        let q = HistoryQuery {
            from: resolve(&q.from)?,
            to: resolve(&q.to)?,
            ..q.clone()
        };
        log.query(&q, head.seq)
    }

    /// The commit a selector names, without asking that its state be readable.
    fn resolve_seq(&self, a: &crate::history::At, head: CommitInfo) -> Result<u64> {
        use crate::history::At;
        match a {
            At::Head => Ok(head.seq),
            At::Commit(n) => Ok(*n),
            At::Time(ms) => Ok(self.catalog.lock().at_time(*ms).map_or(0, |c| c.seq)),
            At::Snapshot(_) => Ok(self.resolve_with(a, head)?.commit.seq),
        }
    }
}
