//! The data graph of a validation (SHACL, ShEx) over a store [`Snapshot`]: the graphs
//! that make it up, and index scans for `(s p ?)`, `(? p o)`, `(s ? ?)` and `(? p ?)`
//! restricted to them. A triple present in several graphs of the data graph is one
//! triple, as in the RDF merge.

use crate::error::{Budget, BudgetKind, Error, Result};
use crate::id::{Id, Tag};
use crate::index::{Key, Perm};
use crate::sparql::ctx::{DEFAULT_GRAPH_IRI, UNION_GRAPH_IRI};
use crate::store::{Chunk, Snapshot};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A byte allowance for retaining validation report entries. Workers can share one
/// allowance and reserve estimated bytes before moving each entry into a report.
#[derive(Debug)]
pub struct ReportBytes {
    limit: u64,
    retained: AtomicU64,
}

impl ReportBytes {
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            retained: AtomicU64::new(0),
        }
    }

    pub fn admit(&self, bytes: u64) -> Result<()> {
        self.retained
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |retained| {
                retained
                    .checked_add(bytes)
                    .filter(|&total| total <= self.limit)
            })
            .map_err(|retained| {
                Error::BudgetExceeded(Budget {
                    kind: BudgetKind::Memory,
                    limit: self.limit,
                    requested: retained.saturating_add(bytes),
                })
            })?;
        Ok(())
    }
}

/// Which graphs of the store form the data graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphSel {
    /// every graph (union default graph)
    All,
    /// these graph ids (sorted)
    Set(Vec<u64>),
    /// every graph but these (sorted)
    AllExcept(Vec<u64>),
}

impl GraphSel {
    #[inline]
    pub fn accepts(&self, g: u64) -> bool {
        match self {
            GraphSel::All => true,
            GraphSel::Set(gs) => gs.len() == 1 && gs[0] == g || gs.binary_search(&g).is_ok(),
            GraphSel::AllExcept(ex) => ex.binary_search(&g).is_err(),
        }
    }

    /// The graph ids, listing every graph of `snap` for `All` / `AllExcept`.
    pub fn ids(&self, snap: &Snapshot) -> Result<Vec<Id>> {
        Ok(match self {
            GraphSel::Set(gs) => gs.iter().map(|&g| Id(g)).collect(),
            _ => {
                let mut all = vec![Id::DEFAULT_GRAPH];
                all.extend(snap.graph_ids()?);
                all.retain(|g| self.accepts(g.0));
                all
            }
        })
    }
}

/// Resolve a graph IRI to its id (`None` if the graph does not exist).
pub fn graph_id(snap: &Snapshot, iri: &str) -> Option<Id> {
    if iri == DEFAULT_GRAPH_IRI {
        return Some(Id::DEFAULT_GRAPH);
    }
    snap.lookup_iri(iri)
}

/// The data graph: a snapshot and the graphs of it that are validated.
pub struct DataGraph {
    pub snap: Arc<Snapshot>,
    pub sel: GraphSel,
    /// the one graph of the data graph, when it is a single graph
    single: Option<u64>,
    /// the single graph holds every quad of the snapshot
    sole: bool,
}

impl DataGraph {
    /// The data graph of `snap`: `data_graph` is `None` for the store's default graph
    /// (the union of all graphs if the store uses a union default graph), a graph IRI,
    /// `urn:x-arq:DefaultGraph` or `urn:x-arq:UnionGraph`. `extra` graphs are merged
    /// into it (those that do not exist are ignored); `exclude` graphs are never part
    /// of it, even of the union of all graphs.
    pub fn new(
        snap: Arc<Snapshot>,
        data_graph: Option<&str>,
        extra: &[String],
        exclude: &[String],
    ) -> Result<DataGraph> {
        let mut excluded: Vec<u64> = exclude
            .iter()
            .filter_map(|g| graph_id(&snap, g))
            .map(|g| g.0)
            .collect();
        excluded.sort_unstable();
        excluded.dedup();
        let sel = match data_graph {
            Some(UNION_GRAPH_IRI) => GraphSel::All,
            None if snap.union_default_graph => GraphSel::All,
            _ => {
                let mut gs = vec![match data_graph {
                    None => Id::DEFAULT_GRAPH.0,
                    Some(iri) => match graph_id(&snap, iri) {
                        Some(g) => g.0,
                        None => {
                            return Err(Error::invalid(format!(
                                "data graph <{iri}> does not exist"
                            )));
                        }
                    },
                }];
                gs.extend(extra.iter().filter_map(|g| graph_id(&snap, g)).map(|g| g.0));
                gs.sort_unstable();
                gs.dedup();
                gs.retain(|g| excluded.binary_search(g).is_err());
                GraphSel::Set(gs)
            }
        };
        let sel = match sel {
            GraphSel::All if !excluded.is_empty() => GraphSel::AllExcept(excluded),
            s => s,
        };
        let single = match &sel {
            GraphSel::Set(gs) if gs.len() == 1 => Some(gs[0]),
            _ => None,
        };
        let sole = match single {
            Some(g) => snap.count(Perm::Gspo, &[g])? == snap.len(),
            None => false,
        };
        Ok(DataGraph {
            snap,
            sel,
            single,
            sole,
        })
    }

    /// Is the data graph one graph of the store? Then no triple occurs twice in it, and
    /// [`count_objects`](Self::count_objects) counts exactly.
    pub fn is_single_graph(&self) -> bool {
        self.single.is_some()
    }

    #[inline]
    fn stored(id: Id) -> bool {
        !matches!(id.tag(), Tag::Local | Tag::Undef)
    }

    fn scan_checked(
        &self,
        perm: Perm,
        prefix: &[u64],
        check: impl Fn() -> Result<()>,
        mut f: impl FnMut(&Key) -> bool,
    ) -> Result<()> {
        let sel = &self.sel;
        let gcol = 3; // graph is the last key column of every non-GSPO permutation
        self.snap.scan_checked(perm, prefix, &check, |c| {
            Ok(match c {
                Chunk::Block(b, s, e) => {
                    for (offset, &g) in b.cols[gcol][s..e].iter().enumerate() {
                        check()?;
                        if sel.accepts(g) && !f(&b.key(s + offset)) {
                            return Ok(false);
                        }
                    }
                    true
                }
                Chunk::Row(k) => !sel.accepts(k[gcol]) || f(&k),
            })
        })
    }

    fn next_distinct(last: &mut Option<Id>, id: Id) -> bool {
        last.replace(id) != Some(id)
    }

    /// Visit the distinct values of key column `col` under `prefix` (the column right
    /// after the prefix, so equal values are adjacent); `f` returns `false` to stop.
    fn scan_distinct(
        &self,
        perm: Perm,
        prefix: &[u64],
        col: usize,
        f: impl FnMut(Id) -> bool,
    ) -> Result<()> {
        self.scan_distinct_checked(perm, prefix, col, || Ok(()), f)
    }

    fn scan_distinct_checked(
        &self,
        perm: Perm,
        prefix: &[u64],
        col: usize,
        check: impl Fn() -> Result<()>,
        mut f: impl FnMut(Id) -> bool,
    ) -> Result<()> {
        let mut last = None;
        self.scan_checked(perm, prefix, check, |k| {
            let id = Id(k[col]);
            !Self::next_distinct(&mut last, id) || f(id)
        })
    }

    /// Stream a FOCUS pattern over the physical validation graph under its
    /// original preparation. The scanner polls rows excluded here as well.
    pub(crate) fn scan_focus<E: From<Error>>(
        &self,
        preparation: &crate::access::Preparation,
        predicate: Id,
        fixed: Option<Id>,
        focus_is_subject: bool,
        mut each: impl FnMut(Id) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        if !Self::stored(predicate) || fixed.is_some_and(|id| !Self::stored(id)) {
            return Ok(());
        }
        let (perm, prefix, len) = match (focus_is_subject, fixed) {
            (true, Some(object)) => (Perm::Pos, [predicate.0, object.0], 2),
            (false, Some(subject)) => (Perm::Spo, [subject.0, predicate.0], 2),
            (true, None) => (Perm::Pso, [predicate.0, 0], 1),
            (false, None) => (Perm::Pos, [predicate.0, 0], 1),
        };
        let column = perm.order()[len];
        let mut last = None;
        self.snap
            .scan_prepared(perm, &prefix[..len], preparation, |quad| {
                let id = quad[column];
                if self.sel.accepts(quad[crate::index::G].0) && Self::next_distinct(&mut last, id) {
                    each(id)?;
                }
                Ok(true)
            })
    }

    /// Stream the distinct objects of `(s, p, ?)`; `f` returns `false` to stop.
    pub fn scan_objects(&self, s: Id, p: Id, f: impl FnMut(Id) -> bool) -> Result<()> {
        if !Self::stored(s) || !Self::stored(p) {
            return Ok(());
        }
        self.scan_distinct(Perm::Spo, &[s.0, p.0], 2, f)
    }

    /// Stream the distinct subjects of `(?, p, o)`; `f` returns `false` to stop.
    pub fn scan_subjects(&self, p: Id, o: Id, f: impl FnMut(Id) -> bool) -> Result<()> {
        if !Self::stored(o) || !Self::stored(p) {
            return Ok(());
        }
        self.scan_distinct(Perm::Pos, &[p.0, o.0], 2, f)
    }

    /// Stream the distinct `(p, o)` pairs of the triples with subject `s`, in predicate
    /// order; `f` returns `false` to stop.
    pub fn scan_out_edges(&self, s: Id, f: impl FnMut(Id, Id) -> bool) -> Result<()> {
        self.scan_out_edges_checked(s, || Ok(()), f)
    }

    fn scan_out_edges_checked(
        &self,
        s: Id,
        check: impl Fn() -> Result<()>,
        mut f: impl FnMut(Id, Id) -> bool,
    ) -> Result<()> {
        if !Self::stored(s) {
            return Ok(());
        }
        let mut last = None;
        self.scan_checked(Perm::Spo, &[s.0], check, |k| {
            let e = (k[1], k[2]);
            if last == Some(e) {
                return true;
            }
            last = Some(e);
            f(Id(e.0), Id(e.1))
        })
    }

    /// Objects of `(s, p, ?)` (distinct).
    pub fn objects(&self, s: Id, p: Id) -> Result<Vec<Id>> {
        self.objects_checked(s, p, || Ok(()))
    }

    /// Collect distinct objects, polling even on rows outside the data graph.
    pub fn objects_checked(&self, s: Id, p: Id, check: impl Fn() -> Result<()>) -> Result<Vec<Id>> {
        self.collect_distinct(Perm::Spo, &[s, p], check)
    }

    /// Subjects of `(?, p, o)` (distinct).
    pub fn subjects(&self, p: Id, o: Id) -> Result<Vec<Id>> {
        self.subjects_checked(p, o, || Ok(()))
    }

    /// Collect distinct subjects, polling even on rows outside the data graph.
    pub fn subjects_checked(
        &self,
        p: Id,
        o: Id,
        check: impl Fn() -> Result<()>,
    ) -> Result<Vec<Id>> {
        self.collect_distinct(Perm::Pos, &[p, o], check)
    }

    /// `(p, o)` pairs of all triples with subject `s` (distinct).
    pub fn out_edges(&self, s: Id) -> Result<Vec<(Id, Id)>> {
        self.out_edges_checked(s, || Ok(()))
    }

    /// Collect distinct outbound edges with raw scan control.
    pub fn out_edges_checked(
        &self,
        s: Id,
        check: impl Fn() -> Result<()>,
    ) -> Result<Vec<(Id, Id)>> {
        let mut out = Vec::new();
        self.scan_out_edges_checked(s, check, |p, o| {
            out.push((p, o));
            true
        })?;
        Ok(out)
    }

    /// Distinct subjects of triples with predicate `p`.
    pub fn subjects_of(&self, p: Id) -> Result<Vec<Id>> {
        self.subjects_of_checked(p, || Ok(()))
    }

    /// Collect distinct subjects of a predicate with raw scan control.
    pub fn subjects_of_checked(&self, p: Id, check: impl Fn() -> Result<()>) -> Result<Vec<Id>> {
        self.collect_distinct(Perm::Pso, &[p], check)
    }

    /// Distinct objects of triples with predicate `p`.
    pub fn objects_of(&self, p: Id) -> Result<Vec<Id>> {
        self.objects_of_checked(p, || Ok(()))
    }

    /// Collect distinct objects of a predicate with raw scan control.
    pub fn objects_of_checked(&self, p: Id, check: impl Fn() -> Result<()>) -> Result<Vec<Id>> {
        self.collect_distinct(Perm::Pos, &[p], check)
    }

    /// Whether `o` is the object of some triple of the data graph.
    pub fn has_in_edge(&self, o: Id) -> Result<bool> {
        self.has_edge(Perm::Osp, o, || Ok(()))
    }

    fn has_edge(&self, perm: Perm, node: Id, check: impl Fn() -> Result<()>) -> Result<bool> {
        if !Self::stored(node) {
            return Ok(false);
        }
        let mut found = false;
        self.scan_checked(perm, &[node.0], check, |_| {
            found = true;
            false
        })?;
        Ok(found)
    }

    /// Whether a node is a subject or object in the data graph, with raw scan control.
    pub fn has_node_checked(&self, node: Id, check: impl Fn() -> Result<()>) -> Result<bool> {
        Ok(self.has_edge(Perm::Spo, node, &check)? || self.has_edge(Perm::Osp, node, check)?)
    }

    /// The nodes of the data graph: every distinct subject and object (RDF 1.2 Concepts
    /// §3.1), subjects first.
    pub fn nodes(&self) -> Result<Vec<Id>> {
        self.nodes_checked(|| Ok(()))
    }

    /// Collect all distinct subjects and objects with raw scan control.
    pub fn nodes_checked(&self, check: impl Fn() -> Result<()>) -> Result<Vec<Id>> {
        let mut out = Vec::new();
        let mut seen = rustc_hash::FxHashSet::default();
        for perm in [Perm::Spo, Perm::Osp] {
            self.scan_distinct_checked(perm, &[], 0, &check, |x| {
                if seen.insert(x) {
                    out.push(x);
                }
                true
            })?;
        }
        Ok(out)
    }

    fn collect_distinct<const N: usize>(
        &self,
        perm: Perm,
        prefix: &[Id; N],
        check: impl Fn() -> Result<()>,
    ) -> Result<Vec<Id>> {
        let mut out = Vec::new();
        if !prefix.iter().copied().all(Self::stored) {
            return Ok(out);
        }
        self.scan_distinct_checked(perm, &prefix.map(|id| id.0), N, check, |x| {
            out.push(x);
            true
        })?;
        Ok(out)
    }

    /// The number of objects of `(s, p, ?)` from the index counts, without reading
    /// them: exact when the data graph is a single graph, `None` otherwise.
    pub fn count_objects(&self, s: Id, p: Id) -> Result<Option<u64>> {
        let Some(g) = self.single else {
            return Ok(None);
        };
        if !Self::stored(s) || !Self::stored(p) {
            return Ok(Some(0));
        }
        self.snap.count(Perm::Gspo, &[g, s.0, p.0]).map(Some)
    }

    /// The number of subjects of `(?, p, o)` from the index counts, without reading
    /// them: exact when the data graph is a single graph holding every quad of the
    /// snapshot, `None` otherwise.
    pub fn count_subjects(&self, p: Id, o: Id) -> Result<Option<u64>> {
        if !self.sole {
            return Ok(None);
        }
        if !Self::stored(o) || !Self::stored(p) {
            return Ok(Some(0));
        }
        self.snap.count(Perm::Pos, &[p.0, o.0]).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::{RdfFormat, Source};
    use crate::store::{Store, StoreOptions};

    const TRIG: &str = r#"
@prefix ex: <http://ex.org/> .
ex:a ex:p 1, 2, 3 . ex:b ex:p 2 . ex:a ex:q ex:b .
ex:g1 { ex:a ex:p 3, 4 . ex:c ex:p 2 . }
ex:g2 { ex:a ex:p 4 . }
"#;

    fn store() -> Store {
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            TRIG.as_bytes().to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
        s
    }

    fn iri(snap: &Snapshot, local: &str) -> Id {
        snap.lookup_iri(&format!("http://ex.org/{local}")).unwrap()
    }

    fn int(n: i64) -> Id {
        crate::id::inline_id(&oxrdf::Literal::from(n).into()).unwrap()
    }

    fn data(snap: &Arc<Snapshot>, g: Option<&str>, extra: &[&str]) -> DataGraph {
        let extra: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
        DataGraph::new(snap.clone(), g, &extra, &[]).unwrap()
    }

    #[test]
    fn graph_selection_and_merge() {
        let store = store();
        let snap = store.snapshot();
        let (a, b, c, p) = (
            iri(&snap, "a"),
            iri(&snap, "b"),
            iri(&snap, "c"),
            iri(&snap, "p"),
        );
        let g1 = "http://ex.org/g1";
        let d = data(&snap, None, &[]);
        assert!(d.is_single_graph());
        assert_eq!(d.objects(a, p).unwrap(), vec![int(1), int(2), int(3)]);
        // 3 is in both graphs: one triple of the merge
        let m = data(&snap, None, &[g1]);
        assert!(!m.is_single_graph());
        assert_eq!(
            m.objects(a, p).unwrap(),
            vec![int(1), int(2), int(3), int(4)]
        );
        assert_eq!(m.subjects(p, int(2)).unwrap(), vec![a, b, c]);
        let u = data(&snap, Some(UNION_GRAPH_IRI), &[]);
        assert_eq!(u.sel, GraphSel::All);
        assert_eq!(
            u.objects_of(p).unwrap(),
            vec![int(1), int(2), int(3), int(4)]
        );
        assert_eq!(u.subjects_of(p).unwrap(), vec![a, b, c]);
        let ex = DataGraph::new(snap.clone(), Some(UNION_GRAPH_IRI), &[], &[g1.into()]).unwrap();
        assert!(matches!(ex.sel, GraphSel::AllExcept(_)));
        assert_eq!(ex.subjects(p, int(2)).unwrap(), vec![a, b]);
        assert_eq!(ex.sel.ids(&snap).unwrap().len(), 2);
        let err = DataGraph::new(snap.clone(), Some("http://ex.org/none"), &[], &[]);
        assert!(err.is_err());
    }

    #[test]
    fn controlled_focus_streams_the_same_distinct_physical_ids() {
        let store = Store::in_memory(StoreOptions::default());
        let trig = format!("{TRIG} _:g {{ <urn:blank-subject> <http://ex.org/p> 2 . }}");
        store
            .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
            .unwrap();
        let snap = store.snapshot();
        let a = iri(&snap, "a");
        let p = iri(&snap, "p");
        let preparation =
            crate::access::Preparation::new(crate::task::Control::none(), 4 << 20).unwrap();

        for (graph, excluded) in [
            (None, &[][..]),
            (Some("http://ex.org/g1"), &[][..]),
            (Some(UNION_GRAPH_IRI), &[][..]),
            (Some(UNION_GRAPH_IRI), &["http://ex.org/g1".to_owned()][..]),
        ] {
            let d = DataGraph::new(snap.clone(), graph, &[], excluded).unwrap();
            for (subject, fixed, expected) in [
                (true, Some(int(2)), d.subjects(p, int(2)).unwrap()),
                (false, Some(a), d.objects(a, p).unwrap()),
                (true, None, d.subjects_of(p).unwrap()),
                (false, None, d.objects_of(p).unwrap()),
                (true, Some(Id::UNDEF), Vec::new()),
            ] {
                let baseline = preparation.inner_used();
                let mut found = Vec::new();
                d.scan_focus(&preparation, p, fixed, subject, |id| -> Result<()> {
                    assert!(preparation.inner_used() > baseline);
                    found.push(id);
                    Ok(())
                })
                .unwrap();
                assert_eq!(found, expected);
                assert_eq!(preparation.inner_used(), baseline);
            }
        }
    }

    #[test]
    fn controlled_focus_polls_rows_that_emit_no_association() {
        use crate::task::{Control, Progress};
        use std::fmt::Write;

        let store = Store::in_memory(StoreOptions::default());
        let mut text = String::new();
        for n in 0..2048 {
            writeln!(text, "<urn:s> <urn:p> <urn:o{n:04}> <urn:excluded> .").unwrap();
        }
        store
            .load(&[Source::from_bytes(
                text.into_bytes(),
                RdfFormat::NQuads,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let p = snap.lookup_iri("urn:p").unwrap();
        for excluded in [Vec::new(), vec!["urn:excluded".to_owned()]] {
            let d = DataGraph::new(snap.clone(), Some(UNION_GRAPH_IRI), &[], &excluded).unwrap();
            let mut control = Control::none();
            let cancel = control.cancel.clone();
            control.progress = Progress::new(move |_, stage| {
                if stage == "scanning permission rows" {
                    cancel.cancel();
                }
            });
            let preparation = crate::access::Preparation::new(control, 4 << 20).unwrap();
            let mut called = 0;
            assert!(matches!(
                d.scan_focus(&preparation, p, None, true, |_| -> Result<()> {
                    called += 1;
                    Ok(())
                }),
                Err(Error::Cancelled)
            ));
            assert_eq!(called, usize::from(excluded.is_empty()));
        }

        let mut txn = store.write();
        snap.for_each_quad(|quad| {
            txn.delete(*quad)?;
            Ok(())
        })
        .unwrap();
        txn.commit().unwrap();
        assert_eq!(store.snapshot().len(), 0);
        let d = DataGraph::new(store.snapshot(), Some(UNION_GRAPH_IRI), &[], &[]).unwrap();
        let mut control = Control::none();
        let cancel = control.cancel.clone();
        control.progress = Progress::new(move |_, stage| {
            if stage == "scanning permission rows" {
                cancel.cancel();
            }
        });
        let preparation = crate::access::Preparation::new(control, 4 << 20).unwrap();
        assert!(matches!(
            d.scan_focus(&preparation, p, None, true, |_| -> Result<()> {
                panic!("a deleted row reached the callback")
            }),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn controlled_focus_stops_at_callback_refusal_and_returns_read_errors() {
        use crate::task::{Control, Progress};
        use std::fmt::Write;
        use std::sync::atomic::AtomicUsize;

        let store = Store::in_memory(StoreOptions::default());
        let mut text = String::new();
        for n in 0..2048 {
            writeln!(text, "<urn:s{n:04}> <urn:p> <urn:o> .").unwrap();
        }
        store
            .load(&[Source::from_bytes(
                text.into_bytes(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        let mut snap = store.snapshot();
        drop(store);
        let p = snap.lookup_iri("urn:p").unwrap();

        let checked = Arc::new(AtomicUsize::new(0));
        let mut control = Control::none();
        let count = checked.clone();
        control.progress = Progress::new(move |_, stage| {
            if stage == "scanning permission rows" {
                count.fetch_add(1, Ordering::Relaxed);
            }
        });
        let preparation = crate::access::Preparation::new(control, 4 << 20).unwrap();
        let baseline = preparation.inner_used();
        let d = DataGraph::new(snap.clone(), Some(UNION_GRAPH_IRI), &[], &[]).unwrap();
        for stop_at in [1, 2] {
            let mut visited = 0;
            assert!(matches!(
                d.scan_focus(&preparation, p, None, true, |_| -> Result<()> {
                    visited += 1;
                    if visited == stop_at {
                        return Err(Error::invalid("callback refused"));
                    }
                    Ok(())
                }),
                Err(Error::Invalid(message)) if message == "callback refused"
            ));
            assert_eq!(visited, stop_at);
            assert_eq!(checked.load(Ordering::Relaxed), 0);
            assert_eq!(preparation.inner_used(), baseline);
        }
        drop(d);

        let snapshot = Arc::get_mut(&mut snap).unwrap();
        let generation = Arc::get_mut(&mut snapshot.generation).unwrap();
        let index = &mut generation.perms[Perm::Pso.index()];
        let block = &mut Arc::make_mut(&mut index.blocks)[0];
        block.offset = u64::MAX;
        // A cold cache ensures the invalid frame address is reached on this scan.
        snapshot.cache = Arc::new(crate::index::BlockCache::new(0));
        let d = DataGraph::new(snap, Some(UNION_GRAPH_IRI), &[], &[]).unwrap();
        assert!(matches!(
            d.scan_focus(&preparation, p, None, true, |_| -> Result<()> {
                panic!("an unreadable block reached the callback")
            }),
            Err(Error::Corrupt(_))
        ));
        assert_eq!(preparation.inner_used(), baseline);
    }

    #[test]
    fn streaming_scans_stop_early() {
        let store = store();
        let snap = store.snapshot();
        let (a, b, p, q) = (
            iri(&snap, "a"),
            iri(&snap, "b"),
            iri(&snap, "p"),
            iri(&snap, "q"),
        );
        let d = data(&snap, None, &[]);
        let mut seen = Vec::new();
        d.scan_objects(a, p, |o| {
            seen.push(o);
            seen.len() < 2
        })
        .unwrap();
        assert_eq!(seen, vec![int(1), int(2)]);
        let mut subjects = Vec::new();
        d.scan_subjects(p, int(2), |s| {
            subjects.push(s);
            true
        })
        .unwrap();
        assert_eq!(subjects, vec![a, b]);
        let mut edges = Vec::new();
        d.scan_out_edges(a, |p, o| {
            edges.push((p, o));
            true
        })
        .unwrap();
        assert_eq!(edges, d.out_edges(a).unwrap());
        assert_eq!(edges.len(), 4);
        assert!(edges.contains(&(q, b)));
        // ids that are not in the store have no triples
        assert!(d.objects(Id::local(0), p).unwrap().is_empty());
    }

    #[test]
    fn counts_only_when_exact() {
        let store = store();
        let snap = store.snapshot();
        let (a, p) = (iri(&snap, "a"), iri(&snap, "p"));
        let d = data(&snap, None, &[]);
        assert_eq!(d.count_objects(a, p).unwrap(), Some(3));
        // other graphs hold quads: incoming counts would include them
        assert_eq!(d.count_subjects(p, int(2)).unwrap(), None);
        let g1 = data(&snap, Some("http://ex.org/g1"), &[]);
        assert_eq!(g1.count_objects(a, p).unwrap(), Some(2));
        let m = data(&snap, None, &["http://ex.org/g1"]);
        assert_eq!(m.count_objects(a, p).unwrap(), None);

        let one = Store::in_memory(StoreOptions::default());
        one.load(&[Source::from_bytes(
            b"<http://ex.org/a> <http://ex.org/p> 2 . <http://ex.org/b> <http://ex.org/p> 2 ."
                .to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        let snap = one.snapshot();
        let d = data(&snap, None, &[]);
        assert_eq!(d.count_subjects(iri(&snap, "p"), int(2)).unwrap(), Some(2));
    }
}
