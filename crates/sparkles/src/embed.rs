//! Building blocks for language bindings that embed the engine: quad patterns read from
//! a given snapshot, scans that return ids rather than terms, and [`TxnWorker`], the
//! thread that owns a write transaction on behalf of a caller whose threads cannot hold
//! the writer lock.
//!
//! The JVM bindings (`crates/sparkles-ffi`, spec P04) use all of it. The Python bindings
//! keep their own worker until they next change.

use crate::Dataset;
use crate::commit::Receipt;
use crate::dataset::{GraphSel, QuadIter, ScanPlan, Transaction, scan_plan};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::{G, O, P, S};
use crate::store::Snapshot;
use oxrdf::{BlankNode, NamedNode, NamedOrBlankNode, Term};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

/// Which graphs a [`QuadPattern`] matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphMatch {
    /// every graph, the default graph included (Jena `Node.ANY`)
    Any,
    /// the default graph (the union of the named graphs when the snapshot's store has
    /// the union default graph setting)
    Default,
    /// the union of the named graphs, each triple once (Jena `Quad.unionGraph`)
    Union,
    /// one named graph
    Named(NamedOrBlankNode),
}

/// A quad pattern: `None` is a wildcard. A blank node is a stored one, named by the
/// label that reads hand out (`b<hex>`); any other label matches nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuadPattern {
    pub graph: GraphMatch,
    pub subject: Option<NamedOrBlankNode>,
    pub predicate: Option<NamedNode>,
    pub object: Option<Term>,
}

impl QuadPattern {
    /// The pattern that matches every quad.
    pub fn any() -> QuadPattern {
        QuadPattern {
            graph: GraphMatch::Any,
            subject: None,
            predicate: None,
            object: None,
        }
    }

    fn sel(&self) -> GraphSel {
        match &self.graph {
            GraphMatch::Any => GraphSel::Any,
            GraphMatch::Default => GraphSel::Default,
            GraphMatch::Union => GraphSel::Union,
            GraphMatch::Named(NamedOrBlankNode::NamedNode(n)) => GraphSel::Named(n.clone()),
            GraphMatch::Named(NamedOrBlankNode::BlankNode(b)) => GraphSel::Blank(b.clone()),
        }
    }

    /// The scan of this pattern on `snap`, or `None` when it cannot match.
    fn plan(&self, snap: &Snapshot) -> Option<ScanPlan> {
        scan_plan(
            snap,
            &self.sel(),
            self.subject.as_ref(),
            self.predicate.as_ref(),
            self.object.as_ref(),
            false,
        )
    }
}

impl Dataset {
    /// The quads of a pattern, read in batches from `snap` rather than from the current
    /// snapshot, as a read transaction of a binding needs. `snap` is usually one this
    /// dataset handed out, or a [`Transaction::snapshot`].
    pub fn quads_in(&self, snap: Arc<Snapshot>, pattern: &QuadPattern) -> QuadIter {
        quads_in(snap, pattern)
    }
}

/// [`Dataset::quads_in`] without the dataset.
pub fn quads_in(snap: Arc<Snapshot>, pattern: &QuadPattern) -> QuadIter {
    let plan = pattern.plan(&snap);
    QuadIter::new(snap, plan)
}

/// The number of quads that match a pattern on `snap`. A pattern whose bound terms form
/// a prefix of an index is counted from the index without reading its quads.
pub fn count_in(snap: &Arc<Snapshot>, pattern: &QuadPattern) -> Result<u64> {
    let Some(plan) = pattern.plan(snap) else {
        return Ok(0);
    };
    let bound = plan.bound.iter().filter(|b| b.is_some()).count();
    if !plan.named_only && plan.prefix.len() == bound {
        return snap.count(plan.perm, &plan.prefix);
    }
    let mut it = QuadIter::new(snap.clone(), Some(plan));
    let mut n = 0;
    while let Some(q) = it.next_ids() {
        q?;
        n += 1;
    }
    Ok(n)
}

impl QuadIter {
    /// The next matching quad as ids (subject, predicate, object, graph), without
    /// decoding its terms. A binding that sends terms once per batch decodes each
    /// distinct id once with [`Snapshot::term`].
    pub fn next_ids(&mut self) -> Option<Result<[Id; 4]>> {
        loop {
            let Some(k) = self.batch.next() else {
                match self.fill() {
                    Ok(true) => continue,
                    Ok(false) => return None,
                    Err(e) => {
                        self.plan = None;
                        return Some(Err(e));
                    }
                }
            };
            let plan = self.plan.as_ref()?;
            let q = plan.perm.to_quad(&k);
            if !plan.matches(&q) {
                continue;
            }
            if plan.named_only && !self.seen.insert([q[S], q[P], q[O]]) {
                continue;
            }
            return Some(Ok(q));
        }
    }
}

impl Transaction<'_> {
    /// The state this transaction's reads see, with its own changes. It is a copy:
    /// later changes in the transaction do not reach it.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::new(self.txn.view())
    }

    /// The commit this transaction started from.
    pub fn base_commit(&self) -> u64 {
        self.txn.base().commit
    }

    /// Remove every quad that matches a pattern, without returning them; the number
    /// removed. Blank node labels that this transaction's inserts used name the nodes
    /// they made, as in [`Transaction::remove`]. A pattern on the union graph removes the
    /// matching quads of every named graph.
    pub fn remove_matching(&mut self, pattern: &QuadPattern) -> Result<u64> {
        let pattern = self.resolve_labels(pattern);
        let view = self.txn.view();
        let Some(mut plan) = pattern.plan(&view) else {
            return Ok(0);
        };
        // every quad of every named graph, not each triple once
        let named_only = plan.named_only;
        plan.named_only = false;
        let mut it = QuadIter::new(Arc::new(view), Some(plan));
        let mut matched = Vec::new();
        while let Some(q) = it.next_ids() {
            let q = q?;
            if named_only && q[G] == Id::DEFAULT_GRAPH {
                continue;
            }
            matched.push(q);
        }
        let mut n = 0;
        for q in matched {
            n += self.txn.delete(q)? as u64;
        }
        Ok(n)
    }

    /// `pattern` with the blank node labels of this transaction's inserts replaced by
    /// the labels of the nodes they made.
    fn resolve_labels(&self, pattern: &QuadPattern) -> QuadPattern {
        if self.labels.is_empty() {
            return pattern.clone();
        }
        let node = |b: &BlankNode| -> BlankNode {
            match self.labels.get(b.as_str()) {
                Some(id) => crate::store::bnode_for(*id),
                None => b.clone(),
            }
        };
        let subject = |s: &NamedOrBlankNode| match s {
            NamedOrBlankNode::BlankNode(b) => NamedOrBlankNode::BlankNode(node(b)),
            n => n.clone(),
        };
        QuadPattern {
            graph: match &pattern.graph {
                GraphMatch::Named(g) => GraphMatch::Named(subject(g)),
                g => g.clone(),
            },
            subject: pattern.subject.as_ref().map(subject),
            predicate: pattern.predicate.clone(),
            object: pattern.object.as_ref().map(|o| match o {
                Term::BlankNode(b) => Term::BlankNode(node(b)),
                t => t.clone(),
            }),
        }
    }
}

// ------------------------------------------------------------------------ the worker ----

type Job = Box<dyn for<'a, 'b> FnOnce(&'a mut Transaction<'b>) + Send>;

enum Msg {
    Job(Job),
    Commit(Sender<Result<Receipt>>),
}

/// A write transaction that runs on a thread of its own.
///
/// The store's writer lock must be released by the thread that took it, and callers
/// such as the JVM run one transaction across several threads (a virtual thread moves
/// between carriers). The worker thread takes the lock, begins the transaction, and
/// runs the closures given to [`run`](Self::run) in order until [`commit`](Self::commit)
/// or [`abort`](Self::abort). Dropping the worker aborts the transaction.
pub struct TxnWorker {
    jobs: Option<Sender<Msg>>,
    base: u64,
    thread: Option<JoinHandle<()>>,
}

fn ended() -> Error {
    Error::invalid("the write transaction has ended")
}

impl TxnWorker {
    /// Start a worker, wait until it holds the writer lock, and begin the transaction.
    ///
    /// With `expect_commit`, this is a promotion: the transaction begins only if the
    /// head is still that commit once the lock is held, and otherwise the lock is
    /// released and the result is `Ok(None)`. Since no commit can land while the lock is
    /// held, the check is final.
    pub fn begin(ds: &Dataset, expect_commit: Option<u64>) -> Result<Option<TxnWorker>> {
        if let Some(c) = expect_commit
            && ds.snapshot().commit != c
        {
            // a commit landed already: fail without waiting for the lock
            return Ok(None);
        }
        let (jobs_tx, jobs_rx) = channel::<Msg>();
        let (started_tx, started_rx) = channel::<Result<Option<u64>>>();
        let ds = ds.clone();
        let thread = std::thread::Builder::new()
            .name("sparkles-txn".into())
            .spawn(move || work(ds, expect_commit, jobs_rx, started_tx))?;
        match started_rx.recv() {
            Ok(Ok(Some(base))) => Ok(Some(TxnWorker {
                jobs: Some(jobs_tx),
                base,
                thread: Some(thread),
            })),
            Ok(Ok(None)) => {
                let _ = thread.join();
                Ok(None)
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::invalid("the write transaction could not start")),
        }
    }

    /// The commit the transaction started from.
    pub fn base_commit(&self) -> u64 {
        self.base
    }

    /// Run `f` in the transaction, on the worker thread, and wait for its result. A
    /// panic in `f` ends the transaction without committing it.
    pub fn run<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Transaction<'_>) -> Result<R> + Send + 'static,
    ) -> Result<R> {
        let (tx, rx) = channel();
        let job: Job = Box::new(move |t| {
            let _ = tx.send(f(t));
        });
        self.jobs
            .as_ref()
            .ok_or_else(ended)?
            .send(Msg::Job(job))
            .map_err(|_| ended())?;
        rx.recv().map_err(|_| ended())?
    }

    /// Whether the worker still runs a transaction.
    pub fn is_open(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Commit and wait for the receipt. The writer lock is released when this returns.
    pub fn commit(mut self) -> Result<Receipt> {
        let (tx, rx) = channel();
        let sent = self
            .jobs
            .take()
            .ok_or_else(ended)?
            .send(Msg::Commit(tx))
            .map_err(|_| ended());
        let r = sent.and_then(|()| rx.recv().map_err(|_| ended()));
        self.join();
        r?
    }

    /// Abort, and wait until the writer lock is released.
    pub fn abort(mut self) {
        self.jobs = None;
        self.join();
    }

    fn join(&mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for TxnWorker {
    fn drop(&mut self) {
        // closing the channel makes the worker roll back; it is not waited for, so a
        // drop on a finalizer thread does not block on a running request
        self.jobs = None;
    }
}

fn work(
    ds: Dataset,
    expect: Option<u64>,
    jobs: Receiver<Msg>,
    started: Sender<Result<Option<u64>>>,
) {
    let mut reply: Option<Sender<Result<Receipt>>> = None;
    let mut moved = false;
    let r = ds.transaction_receipt(|tx| {
        let base = tx.base_commit();
        if expect.is_some_and(|c| c != base) {
            moved = true;
            return Err(Error::Cancelled);
        }
        let _ = started.send(Ok(Some(base)));
        loop {
            match jobs.recv() {
                Ok(Msg::Job(f)) => {
                    let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(tx)));
                    if ok.is_err() {
                        return Err(Error::invalid("a request in the transaction panicked"));
                    }
                }
                Ok(Msg::Commit(r)) => {
                    reply = Some(r);
                    return Ok(());
                }
                // the caller is gone, or aborted
                Err(_) => return Err(Error::Cancelled),
            }
        }
    });
    if moved {
        let _ = started.send(Ok(None));
    }
    if let Some(reply) = reply {
        let _ = reply.send(r.map(|(_, receipt)| receipt));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::RdfFormat;
    use oxrdf::{GraphName, Literal, Quad, QuadRef};

    fn n(l: &str) -> NamedNode {
        NamedNode::new(format!("http://ex.org/{l}")).unwrap()
    }

    fn data() -> Dataset {
        let ds = Dataset::memory();
        ds.load_str(
            r#"<http://ex.org/a> <http://ex.org/p> "1" .
               <http://ex.org/a> <http://ex.org/q> "2" .
               <http://ex.org/b> <http://ex.org/p> "3" <http://ex.org/g1> .
               <http://ex.org/b> <http://ex.org/p> "3" <http://ex.org/g2> .
               <http://ex.org/c> <http://ex.org/p> "4" <http://ex.org/g2> ."#,
            RdfFormat::NQuads,
        )
        .unwrap();
        ds
    }

    fn pat(graph: GraphMatch, s: Option<&str>, p: Option<&str>) -> QuadPattern {
        QuadPattern {
            graph,
            subject: s.map(|s| n(s).into()),
            predicate: p.map(n),
            object: None,
        }
    }

    #[test]
    fn patterns_on_a_snapshot() {
        let ds = data();
        let snap = ds.snapshot();
        let g2 = GraphMatch::Named(n("g2").into());
        let cases = [
            (QuadPattern::any(), 5),
            (pat(GraphMatch::Default, None, None), 2),
            (pat(GraphMatch::Union, None, None), 2),
            (pat(g2.clone(), None, None), 2),
            (pat(GraphMatch::Any, Some("b"), None), 2),
            (pat(GraphMatch::Union, Some("b"), Some("p")), 1),
            (pat(g2, Some("c"), Some("p")), 1),
            (pat(GraphMatch::Any, Some("zzz"), None), 0),
            (pat(GraphMatch::Any, None, Some("p")), 4),
        ];
        // a later commit is not seen through the snapshot
        ds.insert(QuadRef::new(
            &n("d"),
            &n("p"),
            &n("o"),
            oxrdf::GraphNameRef::DefaultGraph,
        ))
        .unwrap();
        for (p, want) in cases {
            let quads: Vec<Quad> = ds
                .quads_in(snap.clone(), &p)
                .collect::<Result<_>>()
                .unwrap();
            assert_eq!(quads.len(), want, "{p:?}");
            assert_eq!(count_in(&snap, &p).unwrap(), want as u64, "{p:?}");
            let mut it = quads_in(snap.clone(), &p);
            let mut ids = 0;
            while let Some(q) = it.next_ids() {
                let q = q.unwrap();
                assert!(snap.quad_to_terms(&q).is_some());
                ids += 1;
            }
            assert_eq!(ids, want);
        }
    }

    #[test]
    fn remove_matching_in_a_transaction() {
        let ds = data();
        let removed = ds
            .transaction(|tx| {
                let b = BlankNode::new("x").unwrap();
                tx.insert(QuadRef::new(
                    &b,
                    &n("p"),
                    &n("o"),
                    GraphName::DefaultGraph.as_ref(),
                ))?;
                // a label the transaction used names the node it made
                let mut p = QuadPattern::any();
                p.subject = Some(b.into());
                assert_eq!(tx.remove_matching(&p)?, 1);
                // the union graph removes the quads of every named graph
                let n2 = tx.remove_matching(&pat(GraphMatch::Union, Some("b"), None))?;
                assert_eq!(count_in(&tx.snapshot(), &QuadPattern::any())?, 3);
                Ok(n2)
            })
            .unwrap();
        assert_eq!(removed, 2);
        assert_eq!(ds.len(), 3);
    }

    #[test]
    fn worker_commits_aborts_and_promotes() {
        let ds = data();
        let head = ds.snapshot().commit;
        let w = TxnWorker::begin(&ds, None).unwrap().unwrap();
        assert_eq!(w.base_commit(), head);
        let quad = Quad::new(n("w"), n("p"), Literal::from(1), GraphName::DefaultGraph);
        let q = quad.clone();
        assert!(w.run(move |tx| tx.insert(q.as_ref())).unwrap());
        // the change is not visible outside until the commit
        assert!(!ds.contains(quad.as_ref()).unwrap());
        let r = w.commit().unwrap();
        assert!(r.committed);
        assert!(ds.contains(quad.as_ref()).unwrap());

        // an aborted worker leaves nothing and releases the lock
        let w = TxnWorker::begin(&ds, None).unwrap().unwrap();
        w.run(|tx| tx.remove_matching(&QuadPattern::any())).unwrap();
        w.abort();
        assert_eq!(ds.len(), 6);
        // so does a dropped one
        let w = TxnWorker::begin(&ds, None).unwrap().unwrap();
        drop(w);
        let w = TxnWorker::begin(&ds, None).unwrap().unwrap();
        w.abort();

        // promotion from the head succeeds, from an older commit fails
        let now = ds.snapshot().commit;
        assert!(now > head);
        assert!(TxnWorker::begin(&ds, Some(head)).unwrap().is_none());
        let w = TxnWorker::begin(&ds, Some(now)).unwrap().unwrap();
        // a request that fails leaves the transaction open
        assert!(
            w.run(|tx| tx.update_with("NOT SPARQL", &Default::default()))
                .is_err()
        );
        assert!(w.is_open());
        w.commit().unwrap();

        // a promotion that waits for the lock sees the commit that held it
        let w = TxnWorker::begin(&ds, None).unwrap().unwrap();
        let base = ds.snapshot().commit;
        let ds2 = ds.clone();
        let waiter =
            std::thread::spawn(move || TxnWorker::begin(&ds2, Some(base)).map(|w| w.is_some()));
        std::thread::sleep(std::time::Duration::from_millis(50));
        let q = Quad::new(n("w2"), n("p"), Literal::from(2), GraphName::DefaultGraph);
        w.run(move |tx| tx.insert(q.as_ref())).unwrap();
        w.commit().unwrap();
        assert!(!waiter.join().unwrap().unwrap());
    }
}
