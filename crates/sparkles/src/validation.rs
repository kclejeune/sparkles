//! The data graph of a validation (SHACL, ShEx) over a store [`Snapshot`]: the graphs
//! that make it up, and index scans for `(s p ?)`, `(? p o)`, `(s ? ?)` and `(? p ?)`
//! restricted to them. A triple present in several graphs of the data graph is one
//! triple, as in the RDF merge.

use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::{Key, Perm};
use crate::sparql::ctx::{DEFAULT_GRAPH_IRI, UNION_GRAPH_IRI};
use crate::store::{Chunk, Snapshot};
use std::sync::Arc;

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

    /// Visit the keys of `perm` with `prefix` in the data graph; `f` returns `false` to
    /// stop.
    fn scan(&self, perm: Perm, prefix: &[u64], mut f: impl FnMut(&Key) -> bool) -> Result<()> {
        let sel = &self.sel;
        let gcol = 3; // graph is the last key column of every non-GSPO permutation
        self.snap.scan(perm, prefix, |c| {
            Ok(match c {
                Chunk::Block(b, s, e) => {
                    let gs = &b.cols[gcol];
                    (s..e).all(|i| !sel.accepts(gs[i]) || f(&b.key(i)))
                }
                Chunk::Row(k) => !sel.accepts(k[gcol]) || f(&k),
            })
        })
    }

    /// Visit the distinct values of key column `col` under `prefix` (the column right
    /// after the prefix, so equal values are adjacent); `f` returns `false` to stop.
    fn scan_distinct(
        &self,
        perm: Perm,
        prefix: &[u64],
        col: usize,
        mut f: impl FnMut(Id) -> bool,
    ) -> Result<()> {
        let mut last = None;
        self.scan(perm, prefix, |k| {
            if last == Some(k[col]) {
                return true;
            }
            last = Some(k[col]);
            f(Id(k[col]))
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
    pub fn scan_out_edges(&self, s: Id, mut f: impl FnMut(Id, Id) -> bool) -> Result<()> {
        if !Self::stored(s) {
            return Ok(());
        }
        let mut last = None;
        self.scan(Perm::Spo, &[s.0], |k| {
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
        let mut out = Vec::new();
        self.scan_objects(s, p, |o| {
            out.push(o);
            true
        })?;
        Ok(out)
    }

    /// Subjects of `(?, p, o)` (distinct).
    pub fn subjects(&self, p: Id, o: Id) -> Result<Vec<Id>> {
        let mut out = Vec::new();
        self.scan_subjects(p, o, |s| {
            out.push(s);
            true
        })?;
        Ok(out)
    }

    /// `(p, o)` pairs of all triples with subject `s` (distinct).
    pub fn out_edges(&self, s: Id) -> Result<Vec<(Id, Id)>> {
        let mut out = Vec::new();
        self.scan_out_edges(s, |p, o| {
            out.push((p, o));
            true
        })?;
        Ok(out)
    }

    /// Distinct subjects of triples with predicate `p`.
    pub fn subjects_of(&self, p: Id) -> Result<Vec<Id>> {
        self.distinct_second(Perm::Pso, p)
    }

    /// Distinct objects of triples with predicate `p`.
    pub fn objects_of(&self, p: Id) -> Result<Vec<Id>> {
        self.distinct_second(Perm::Pos, p)
    }

    fn distinct_second(&self, perm: Perm, p: Id) -> Result<Vec<Id>> {
        let mut out = Vec::new();
        if !Self::stored(p) {
            return Ok(out);
        }
        self.scan_distinct(perm, &[p.0], 1, |x| {
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
