//! Embedded, term-level API (Jena `Dataset` / `DatasetGraph` / `Graph` /
//! `RDFConnection` equivalent): open or create a database, load data, run SPARQL, and
//! read or modify quads directly — no server involved.
//!
//! ```
//! use sparkles::Dataset;
//! use sparkles::io::RdfFormat;
//! use oxrdf::{NamedNode, Literal, Triple};
//!
//! let ds = Dataset::memory();
//! ds.load_str("@prefix ex: <http://ex.org/> . ex:alice ex:age 42 .", RdfFormat::Turtle)?;
//!
//! // SPARQL
//! let rows = ds.select("SELECT ?who ?age WHERE { ?who <http://ex.org/age> ?age }")?;
//! assert_eq!(rows.len(), 1);
//! assert_eq!(rows.iter().next().unwrap().get("age").unwrap().to_string(),
//!            "\"42\"^^<http://www.w3.org/2001/XMLSchema#integer>");
//!
//! // term-level access (Graph.find / add / delete)
//! let alice = NamedNode::new("http://ex.org/alice")?;
//! let name = NamedNode::new("http://ex.org/name")?;
//! ds.default_graph().insert(&Triple::new(alice.clone(), name.clone(), Literal::from("Alice")))?;
//! let found = ds.default_graph().find(Some(&alice.clone().into()), Some(&name), None)?;
//! assert_eq!(found.len(), 1);
//!
//! // transactions: all or nothing
//! ds.transaction(|tx| {
//!     tx.remove_triple(&found[0])?;
//!     tx.insert_triple(&Triple::new(alice, name, Literal::from("Alice A.")))?;
//!     Ok(())
//! })?;
//! # Ok::<_, Box<dyn std::error::Error>>(())
//! ```

use crate::commit::{CommitInfo, CommitKind, CommitPage, CommitRange, Receipt};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::{G, O, P, Perm, S};
use crate::io::{RdfFormat, Source};
use crate::sparql::update::UpdateStats;
use crate::sparql::{QueryKind, QueryOptions, QueryResult};
use crate::store::{Snapshot, Store, StoreOptions, WriteTxn};
use oxrdf::{
    GraphName, GraphNameRef, NamedNode, NamedOrBlankNode, Quad, QuadRef, Term, Triple, TripleRef,
};
use rustc_hash::FxHashSet;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// An RDF dataset (default graph + named graphs) backed by a Sparkles store.
///
/// Cheap to clone (shared handle). Reads work on consistent snapshots; writes are
/// serialized through a single-writer transaction (MR+SW, like TDB2).
#[derive(Clone)]
pub struct Dataset {
    store: Arc<Store>,
}

impl Dataset {
    /// A new, empty in-memory dataset (Jena `DatasetFactory.createTxnMem()`).
    pub fn memory() -> Dataset {
        Dataset::from_store(Store::in_memory(StoreOptions::default()))
    }

    /// Open (or create) a persistent database directory (Jena `TDB2Factory.connectDataset`).
    /// The directory is locked for the lifetime of the dataset.
    pub fn open(path: impl AsRef<Path>) -> Result<Dataset> {
        Dataset::open_with(path, StoreOptions::default())
    }

    pub fn open_with(path: impl AsRef<Path>, opts: StoreOptions) -> Result<Dataset> {
        Ok(Dataset::from_store(Store::open(path.as_ref(), opts)?))
    }

    pub fn from_store(store: Store) -> Dataset {
        Dataset {
            store: Arc::new(store),
        }
    }

    /// The underlying store (ids, snapshots, low-level scans).
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Current read snapshot.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.store.snapshot()
    }

    // ------------------------------------------------------------------ loading ------

    /// Load a file; the format is taken from the extension (`.ttl`, `.nt.gz`, `.trig`, …).
    /// Returns the number of new quads.
    pub fn load_file(&self, path: impl AsRef<Path>) -> Result<u64> {
        self.load_sources(vec![Source::from_path(path.as_ref(), None)?])
    }

    /// Load a file's triples into a named graph.
    pub fn load_file_into(&self, path: impl AsRef<Path>, graph: &str) -> Result<u64> {
        let g = NamedNode::new(graph).map_err(|e| Error::invalid(e.to_string()))?;
        self.load_sources(vec![Source::from_path(path.as_ref(), Some(g))?])
    }

    /// Load many files at once (parallel bulk path for large inputs).
    pub fn load_files(&self, paths: impl IntoIterator<Item = impl AsRef<Path>>) -> Result<u64> {
        let sources = paths
            .into_iter()
            .map(|p| Source::from_path(p.as_ref(), None))
            .collect::<Result<Vec<_>>>()?;
        self.load_sources(sources)
    }

    /// Load RDF text.
    pub fn load_str(&self, data: &str, format: RdfFormat) -> Result<u64> {
        self.load_sources(vec![Source::from_bytes(
            data.as_bytes().to_vec(),
            format,
            None,
        )])
    }

    /// Load RDF text into a named graph.
    pub fn load_str_into(&self, data: &str, format: RdfFormat, graph: &str) -> Result<u64> {
        let g = NamedNode::new(graph).map_err(|e| Error::invalid(e.to_string()))?;
        self.load_sources(vec![Source::from_bytes(
            data.as_bytes().to_vec(),
            format,
            Some(g),
        )])
    }

    fn load_sources(&self, sources: Vec<Source>) -> Result<u64> {
        self.store.load(&sources)
    }

    /// Load sources in one commit and return its receipt.
    pub fn load_sources_receipt(&self, sources: Vec<Source>) -> Result<Receipt> {
        self.store.load_as(&sources, CommitKind::Load)
    }

    // ------------------------------------------------------------------ commits ------

    /// The dataset id: a UUID created with the database, naming its commit history.
    pub fn dataset_id(&self) -> uuid::Uuid {
        self.store.dataset_id()
    }

    /// The latest commit.
    pub fn head_commit(&self) -> CommitInfo {
        self.store.head_commit()
    }

    /// A page of commit metadata (newest first for [`CommitRange::Latest`] and
    /// [`CommitRange::Before`], oldest first for [`CommitRange::After`]).
    pub fn commits(&self, range: CommitRange, limit: usize) -> CommitPage {
        self.store.commits(range, limit)
    }

    // ------------------------------------------------------------------- SPARQL ------

    /// Run any SPARQL query and get the full result (solutions, boolean or triples, plus
    /// the executed plan and timings).
    pub fn query(&self, query: &str) -> Result<QueryResult> {
        self.query_with(query, &QueryOptions::default())
    }

    pub fn query_with(&self, query: &str, opts: &QueryOptions) -> Result<QueryResult> {
        crate::sparql::query(self.store.snapshot(), query, opts)
    }

    /// Run a SELECT query and return its solutions as terms.
    pub fn select(&self, query: &str) -> Result<Solutions> {
        let r = self.query(query)?;
        if r.kind != QueryKind::Select {
            return Err(Error::invalid("not a SELECT query"));
        }
        Ok(Solutions::from_result(&r))
    }

    /// Run an ASK query.
    pub fn ask(&self, query: &str) -> Result<bool> {
        let r = self.query(query)?;
        if r.kind != QueryKind::Ask {
            return Err(Error::invalid("not an ASK query"));
        }
        Ok(r.boolean)
    }

    /// Run a CONSTRUCT or DESCRIBE query.
    pub fn construct(&self, query: &str) -> Result<Vec<Triple>> {
        let r = self.query(query)?;
        if !matches!(r.kind, QueryKind::Construct | QueryKind::Describe) {
            return Err(Error::invalid("not a CONSTRUCT or DESCRIBE query"));
        }
        Ok(r.triples)
    }

    /// Run a SPARQL Update request (all operations in one transaction).
    pub fn update(&self, update: &str) -> Result<UpdateStats> {
        crate::sparql::update::update(&self.store, update, &QueryOptions::default())
    }

    pub fn update_with(&self, update: &str, opts: &QueryOptions) -> Result<UpdateStats> {
        crate::sparql::update::update(&self.store, update, opts)
    }

    // --------------------------------------------------------------- quads/graphs ------

    /// The default graph.
    pub fn default_graph(&self) -> GraphView<'_> {
        GraphView {
            ds: self,
            graph: GraphSel::Default,
        }
    }

    /// A named graph (it need not exist yet; inserting creates it).
    pub fn named_graph(&self, iri: &str) -> Result<GraphView<'_>> {
        let n = NamedNode::new(iri).map_err(|e| Error::invalid(e.to_string()))?;
        Ok(GraphView {
            ds: self,
            graph: GraphSel::Named(n),
        })
    }

    /// The union of all named graphs (Jena `getUnionGraph`, read-only).
    pub fn union_graph(&self) -> GraphView<'_> {
        GraphView {
            ds: self,
            graph: GraphSel::Union,
        }
    }

    /// Names of the non-empty named graphs.
    pub fn graph_names(&self) -> Result<Vec<NamedOrBlankNode>> {
        let snap = self.store.snapshot();
        Ok(snap
            .graph_ids()?
            .into_iter()
            .filter_map(|g| match snap.term(g)? {
                Term::NamedNode(n) => Some(NamedOrBlankNode::NamedNode(n)),
                Term::BlankNode(b) => Some(NamedOrBlankNode::BlankNode(b)),
                _ => None,
            })
            .collect())
    }

    /// Quads matching a pattern (`None` = wildcard, Jena `Node.ANY`). `graph: None`
    /// matches every graph including the default graph.
    pub fn find(
        &self,
        graph: Option<GraphNameRef<'_>>,
        subject: Option<&NamedOrBlankNode>,
        predicate: Option<&NamedNode>,
        object: Option<&Term>,
    ) -> Result<Vec<Quad>> {
        let snap = self.store.snapshot();
        let sel = match graph {
            None => GraphSel::Any,
            Some(GraphNameRef::DefaultGraph) => GraphSel::Default,
            Some(GraphNameRef::NamedNode(n)) => GraphSel::Named(n.into_owned()),
            Some(GraphNameRef::BlankNode(b)) => GraphSel::Blank(b.into_owned()),
        };
        find_quads(&snap, &sel, subject, predicate, object)
    }

    pub fn contains(&self, quad: QuadRef<'_>) -> Result<bool> {
        let snap = self.store.snapshot();
        match quad_ids(&snap, quad) {
            Some(ids) => snap.contains(&ids),
            None => Ok(false),
        }
    }

    /// Number of quads in the dataset.
    pub fn len(&self) -> u64 {
        self.store.snapshot().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Insert a quad (auto-committed); returns true if it was new.
    pub fn insert(&self, quad: QuadRef<'_>) -> Result<bool> {
        self.transaction(|tx| tx.insert(quad))
    }

    /// Remove a quad (auto-committed); returns true if it was present.
    pub fn remove(&self, quad: QuadRef<'_>) -> Result<bool> {
        self.transaction(|tx| tx.remove(quad))
    }

    /// Insert many quads in one transaction (large batches use the bulk rebuild path).
    pub fn extend<'a>(&self, quads: impl IntoIterator<Item = QuadRef<'a>>) -> Result<u64> {
        self.transaction(|tx| {
            let mut n = 0;
            for q in quads {
                n += tx.insert(q)? as u64;
            }
            Ok(n)
        })
    }

    /// Run `f` in a write transaction: committed if `f` returns `Ok`, discarded otherwise
    /// (Jena `Txn.executeWrite`). Only one write transaction runs at a time.
    pub fn transaction<R>(&self, f: impl FnOnce(&mut Transaction<'_>) -> Result<R>) -> Result<R> {
        Ok(self.transaction_receipt(f)?.0)
    }

    /// [`transaction`](Self::transaction), also returning the commit receipt (whose
    /// `committed` is false when the transaction changed nothing).
    pub fn transaction_receipt<R>(
        &self,
        f: impl FnOnce(&mut Transaction<'_>) -> Result<R>,
    ) -> Result<(R, Receipt)> {
        let mut tx = Transaction {
            txn: self.store.write(),
            labels: HashMap::new(),
        };
        let r = f(&mut tx)?;
        let receipt = tx.txn.commit()?;
        Ok((r, receipt))
    }

    // --------------------------------------------------------------------- admin ------

    pub fn prefixes(&self) -> BTreeMap<String, String> {
        self.store.prefixes()
    }

    pub fn set_prefix(&self, prefix: &str, iri: &str) -> Result<()> {
        self.store.add_prefixes(
            [(prefix.to_string(), iri.to_string())]
                .into_iter()
                .collect(),
        )
    }

    /// Serialize the dataset. Quad formats (N-Quads, TriG) write every graph; triple
    /// formats write the default graph only.
    pub fn dump(&self, w: impl Write, format: RdfFormat) -> Result<u64> {
        let snap = self.store.snapshot();
        let quads_format = format.supports_datasets();
        let ser = crate::io::with_prefixes(
            oxrdfio::RdfSerializer::from_format(format),
            self.store.prefixes(),
        );
        let mut out = ser.for_writer(w);
        let mut n = 0;
        snap.for_each_quad(|q| {
            if !quads_format && q[3] != Id::DEFAULT_GRAPH {
                return Ok(());
            }
            if let Some(quad) = snap.quad_to_terms(q) {
                if quads_format {
                    out.serialize_quad(&quad)?;
                } else {
                    out.serialize_triple(TripleRef::new(
                        &quad.subject,
                        &quad.predicate,
                        &quad.object,
                    ))?;
                }
                n += 1;
            }
            Ok(())
        })?;
        out.finish()?;
        Ok(n)
    }

    /// Merge updates into a freshly built index generation (TDB2 compaction).
    pub fn compact(&self) -> Result<()> {
        self.store.compact()
    }

    /// Write a compressed N-Quads backup into `dir` (zstd, or gzip in builds without zstd);
    /// returns the file path.
    pub fn backup(&self, dir: impl AsRef<Path>) -> Result<PathBuf> {
        let name = self
            .store
            .root()
            .and_then(|r| r.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "dataset".into());
        self.store.backup(dir.as_ref(), &name)
    }
}

// -------------------------------------------------------------------------- graphs ----

#[derive(Clone, Debug)]
enum GraphSel {
    Default,
    Named(NamedNode),
    Blank(oxrdf::BlankNode),
    /// union of the named graphs (triples deduplicated)
    Union,
    /// every graph including the default graph
    Any,
}

/// One graph of a [`Dataset`] (Jena `Graph`): triple-level find / insert / remove.
pub struct GraphView<'a> {
    ds: &'a Dataset,
    graph: GraphSel,
}

impl GraphView<'_> {
    fn graph_name(&self) -> Result<GraphName> {
        match &self.graph {
            GraphSel::Default => Ok(GraphName::DefaultGraph),
            GraphSel::Named(n) => Ok(GraphName::NamedNode(n.clone())),
            GraphSel::Blank(b) => Ok(GraphName::BlankNode(b.clone())),
            GraphSel::Union | GraphSel::Any => Err(Error::invalid("the union graph is read-only")),
        }
    }

    /// Triples matching a pattern (`None` = wildcard).
    pub fn find(
        &self,
        subject: Option<&NamedOrBlankNode>,
        predicate: Option<&NamedNode>,
        object: Option<&Term>,
    ) -> Result<Vec<Triple>> {
        let snap = self.ds.store.snapshot();
        let quads = find_quads(&snap, &self.graph, subject, predicate, object)?;
        let mut seen = FxHashSet::default();
        Ok(quads
            .into_iter()
            .map(|q| Triple::new(q.subject, q.predicate, q.object))
            .filter(|t| seen.insert(t.clone()))
            .collect())
    }

    /// All triples of the graph.
    pub fn triples(&self) -> Result<Vec<Triple>> {
        self.find(None, None, None)
    }

    pub fn contains(&self, t: &Triple) -> Result<bool> {
        Ok(!self
            .find(Some(&t.subject), Some(&t.predicate), Some(&t.object))?
            .is_empty())
    }

    pub fn len(&self) -> Result<u64> {
        let snap = self.ds.store.snapshot();
        match &self.graph {
            GraphSel::Default => snap.count(Perm::Gspo, &[Id::DEFAULT_GRAPH.0]),
            GraphSel::Named(n) => match snap.lookup_iri(n.as_str()) {
                Some(g) => snap.count(Perm::Gspo, &[g.0]),
                None => Ok(0),
            },
            _ => Ok(self.triples()?.len() as u64),
        }
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    pub fn insert(&self, t: &Triple) -> Result<bool> {
        let g = self.graph_name()?;
        self.ds.insert(t.as_ref().in_graph(g.as_ref()))
    }

    pub fn remove(&self, t: &Triple) -> Result<bool> {
        let g = self.graph_name()?;
        self.ds.remove(t.as_ref().in_graph(g.as_ref()))
    }

    /// Remove every triple of the graph.
    pub fn clear(&self) -> Result<u64> {
        let target = match &self.graph {
            GraphSel::Default => "DEFAULT".to_string(),
            GraphSel::Named(n) => format!("GRAPH <{}>", n.as_str()),
            GraphSel::Union => "NAMED".to_string(),
            GraphSel::Any => "ALL".to_string(),
            GraphSel::Blank(_) => return Err(Error::unsupported("clearing a blank-node graph")),
        };
        Ok(self.ds.update(&format!("CLEAR SILENT {target}"))?.deleted)
    }
}

// --------------------------------------------------------------------- transactions ----

/// A write transaction over terms (Jena `Txn` + `DatasetGraph.add/delete`).
/// Blank node labels in inserted data are scoped to the transaction.
pub struct Transaction<'s> {
    txn: WriteTxn<'s>,
    labels: HashMap<String, Id>,
}

impl Transaction<'_> {
    pub fn insert(&mut self, quad: QuadRef<'_>) -> Result<bool> {
        let ids = self.txn.encode_quad(&quad.into_owned(), &mut self.labels)?;
        self.txn.insert(ids)
    }

    pub fn insert_triple(&mut self, t: &Triple) -> Result<bool> {
        self.insert(t.as_ref().in_graph(GraphNameRef::DefaultGraph))
    }

    /// Remove a quad; blank nodes are matched by the labels this store hands out
    /// (`find` results) or by labels inserted earlier in this transaction.
    pub fn remove(&mut self, quad: QuadRef<'_>) -> Result<bool> {
        let view = self.txn.view();
        let bn = |b: &oxrdf::BlankNode, labels: &HashMap<String, Id>| {
            labels
                .get(b.as_str())
                .copied()
                .or_else(|| crate::store::parse_bnode_label(b.as_str()))
        };
        let s = match quad.subject {
            oxrdf::NamedOrBlankNodeRef::NamedNode(n) => view.lookup_iri(n.as_str()),
            oxrdf::NamedOrBlankNodeRef::BlankNode(b) => bn(&b.into_owned(), &self.labels),
        };
        let p = view.lookup_iri(quad.predicate.as_str());
        let o = match quad.object {
            oxrdf::TermRef::BlankNode(b) => bn(&b.into_owned(), &self.labels),
            t => view.lookup_term(&t.into_owned()),
        };
        let g = match quad.graph_name {
            GraphNameRef::DefaultGraph => Some(Id::DEFAULT_GRAPH),
            GraphNameRef::NamedNode(n) => view.lookup_iri(n.as_str()),
            GraphNameRef::BlankNode(b) => bn(&b.into_owned(), &self.labels),
        };
        match (s, p, o, g) {
            (Some(s), Some(p), Some(o), Some(g)) => self.txn.delete([s, p, o, g]),
            _ => Ok(false),
        }
    }

    pub fn remove_triple(&mut self, t: &Triple) -> Result<bool> {
        self.remove(t.as_ref().in_graph(GraphNameRef::DefaultGraph))
    }

    /// Pattern match that sees this transaction's own changes.
    pub fn find(
        &self,
        graph: Option<GraphNameRef<'_>>,
        subject: Option<&NamedOrBlankNode>,
        predicate: Option<&NamedNode>,
        object: Option<&Term>,
    ) -> Result<Vec<Quad>> {
        let sel = match graph {
            None => GraphSel::Any,
            Some(GraphNameRef::DefaultGraph) => GraphSel::Default,
            Some(GraphNameRef::NamedNode(n)) => GraphSel::Named(n.into_owned()),
            Some(GraphNameRef::BlankNode(b)) => GraphSel::Blank(b.into_owned()),
        };
        find_quads(&self.txn.view(), &sel, subject, predicate, object)
    }
}

// ------------------------------------------------------------------------ solutions ----

/// SELECT results as terms.
#[derive(Clone, Debug)]
pub struct Solutions {
    pub vars: Vec<String>,
    rows: Vec<Solution>,
}

/// One solution: variable → term (unbound variables are absent).
#[derive(Clone, Debug)]
pub struct Solution {
    vars: Arc<[String]>,
    values: Vec<Option<Term>>,
}

impl Solutions {
    fn from_result(r: &QueryResult) -> Solutions {
        let vars: Arc<[String]> = r.vars.clone().into();
        let rows = r
            .rows()
            .into_iter()
            .map(|values| Solution {
                vars: vars.clone(),
                values,
            })
            .collect();
        Solutions {
            vars: r.vars.clone(),
            rows,
        }
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn iter(&self) -> std::slice::Iter<'_, Solution> {
        self.rows.iter()
    }
}

impl IntoIterator for Solutions {
    type Item = Solution;
    type IntoIter = std::vec::IntoIter<Solution>;
    fn into_iter(self) -> Self::IntoIter {
        self.rows.into_iter()
    }
}

impl<'a> IntoIterator for &'a Solutions {
    type Item = &'a Solution;
    type IntoIter = std::slice::Iter<'a, Solution>;
    fn into_iter(self) -> Self::IntoIter {
        self.rows.iter()
    }
}

impl Solution {
    /// Value of a variable (with or without the leading `?`).
    pub fn get(&self, var: &str) -> Option<&Term> {
        let var = var.trim_start_matches(['?', '$']);
        let i = self.vars.iter().position(|v| v == var)?;
        self.values[i].as_ref()
    }
    /// `(variable, term)` pairs of the bound variables.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Term)> {
        self.vars
            .iter()
            .zip(&self.values)
            .filter_map(|(v, t)| t.as_ref().map(|t| (v.as_str(), t)))
    }
    pub fn values(&self) -> &[Option<Term>] {
        &self.values
    }
}

// ---------------------------------------------------------------------- matching ----

fn subject_id(snap: &Snapshot, s: &NamedOrBlankNode) -> Option<Id> {
    match s {
        NamedOrBlankNode::NamedNode(n) => snap.lookup_iri(n.as_str()),
        NamedOrBlankNode::BlankNode(b) => crate::store::parse_bnode_label(b.as_str()),
    }
}

fn quad_ids(snap: &Snapshot, q: QuadRef<'_>) -> Option<[Id; 4]> {
    let q = q.into_owned();
    Some([
        subject_id(snap, &q.subject)?,
        snap.lookup_iri(q.predicate.as_str())?,
        snap.lookup_term(&q.object)?,
        match &q.graph_name {
            GraphName::DefaultGraph => Id::DEFAULT_GRAPH,
            GraphName::NamedNode(n) => snap.lookup_iri(n.as_str())?,
            GraphName::BlankNode(b) => crate::store::parse_bnode_label(b.as_str())?,
        },
    ])
}

/// Index-backed pattern matching: picks the permutation with the longest bound prefix.
fn find_quads(
    snap: &Snapshot,
    graph: &GraphSel,
    subject: Option<&NamedOrBlankNode>,
    predicate: Option<&NamedNode>,
    object: Option<&Term>,
) -> Result<Vec<Quad>> {
    let mut bound: [Option<u64>; 4] = [None; 4];
    if let Some(s) = subject {
        let Some(id) = subject_id(snap, s) else {
            return Ok(Vec::new());
        };
        bound[S] = Some(id.0);
    }
    if let Some(p) = predicate {
        let Some(id) = snap.lookup_iri(p.as_str()) else {
            return Ok(Vec::new());
        };
        bound[P] = Some(id.0);
    }
    if let Some(o) = object {
        let Some(id) = snap.lookup_term(o) else {
            return Ok(Vec::new());
        };
        bound[O] = Some(id.0);
    }
    let default_is_union = snap.union_default_graph;
    let (graph_eq, named_only) = match graph {
        GraphSel::Default if default_is_union => (None, true),
        GraphSel::Default => (Some(Id::DEFAULT_GRAPH.0), false),
        GraphSel::Named(n) => match snap.lookup_iri(n.as_str()) {
            Some(g) => (Some(g.0), false),
            None => return Ok(Vec::new()),
        },
        GraphSel::Blank(b) => match crate::store::parse_bnode_label(b.as_str()) {
            Some(g) => (Some(g.0), false),
            None => return Ok(Vec::new()),
        },
        GraphSel::Union => (None, true),
        GraphSel::Any => (None, false),
    };
    bound[G] = graph_eq;
    // permutation with the longest prefix of bound components
    let (perm, plen) = Perm::ALL
        .iter()
        .map(|&p| {
            (
                p,
                p.order()
                    .iter()
                    .take_while(|c| bound[**c].is_some())
                    .count(),
            )
        })
        .max_by_key(|&(_, n)| n)
        .unwrap();
    let order = perm.order();
    let prefix: Vec<u64> = order[..plen].iter().map(|c| bound[*c].unwrap()).collect();
    let mut out = Vec::new();
    let mut seen = FxHashSet::default();
    for k in snap.scan_keys(perm, &prefix)? {
        let q = perm.to_quad(&k);
        if (0..4).any(|c| bound[c].is_some_and(|b| b != q[c].0)) {
            continue;
        }
        if named_only && q[3] == Id::DEFAULT_GRAPH {
            continue;
        }
        if named_only && !seen.insert([q[0], q[1], q[2]]) {
            continue;
        }
        if let Some(quad) = snap.quad_to_terms(&q) {
            out.push(quad);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{Literal, NamedNodeRef};

    const EX: &str = "http://ex.org/";
    fn n(l: &str) -> NamedNode {
        NamedNode::new(format!("{EX}{l}")).unwrap()
    }

    #[test]
    fn embedded_api_roundtrip() {
        for persistent in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let ds = if persistent {
                Dataset::open(dir.path().join("db")).unwrap()
            } else {
                Dataset::memory()
            };
            assert!(ds.is_empty());
            ds.load_str(
                "@prefix ex: <http://ex.org/> . ex:a ex:p 1, 2 ; ex:q ex:b . ex:b ex:p 3 .",
                RdfFormat::Turtle,
            )
            .unwrap();
            ds.load_str_into(
                "<http://ex.org/a> <http://ex.org/p> 9 .",
                RdfFormat::Turtle,
                "http://ex.org/g",
            )
            .unwrap();
            assert_eq!(ds.len(), 5);
            let g = ds.default_graph();
            assert_eq!(g.len().unwrap(), 4);
            assert_eq!(
                g.find(Some(&n("a").into()), Some(&n("p")), None)
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(g.find(None, Some(&n("p")), None).unwrap().len(), 3);
            assert_eq!(g.find(None, None, Some(&n("b").into())).unwrap().len(), 1);
            assert!(
                g.find(Some(&n("zzz").into()), None, None)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(ds.named_graph("http://ex.org/g").unwrap().len().unwrap(), 1);
            assert_eq!(ds.union_graph().triples().unwrap().len(), 1);
            assert_eq!(ds.find(None, None, Some(&n("p")), None).unwrap().len(), 4);
            assert_eq!(ds.graph_names().unwrap().len(), 1);

            let t = Triple::new(n("c"), n("name"), Literal::from("C"));
            assert!(g.insert(&t).unwrap());
            assert!(!g.insert(&t).unwrap());
            assert!(g.contains(&t).unwrap());
            assert!(g.remove(&t).unwrap());
            assert!(!g.contains(&t).unwrap());

            // transaction: rolled back on error
            let r: Result<()> = ds.transaction(|tx| {
                tx.insert_triple(&Triple::new(n("x"), n("p"), Literal::from(1)))?;
                Err(Error::invalid("abort"))
            });
            assert!(r.is_err());
            assert!(!ds.ask("ASK { <http://ex.org/x> ?p ?o }").unwrap());
            // committed, blank nodes scoped to the transaction
            ds.transaction(|tx| {
                let b = oxrdf::BlankNode::new("n1").unwrap();
                tx.insert_triple(&Triple::new(b.clone(), n("p"), Literal::from(7)))?;
                tx.insert_triple(&Triple::new(b, n("q"), n("a")))?;
                assert_eq!(
                    tx.find(None, None, Some(&n("q")), Some(&n("a").into()))
                        .unwrap()
                        .len(),
                    1
                );
                Ok(())
            })
            .unwrap();
            let rows = ds.select("SELECT ?v WHERE { ?b <http://ex.org/q> <http://ex.org/a> ; <http://ex.org/p> ?v }").unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows.iter().next().unwrap().get("?v").unwrap(),
                &Term::Literal(Literal::from(7))
            );
            // removing a found blank-node triple round-trips its label
            let found = g.find(None, Some(&n("q")), Some(&n("a").into())).unwrap();
            assert!(ds.transaction(|tx| tx.remove_triple(&found[0])).unwrap());

            assert_eq!(
                ds.construct("CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }")
                    .unwrap()
                    .len(),
                5
            );
            ds.update("DELETE WHERE { <http://ex.org/b> ?p ?o }")
                .unwrap();
            assert_eq!(g.find(Some(&n("b").into()), None, None).unwrap().len(), 0);
            let mut buf = Vec::new();
            ds.dump(&mut buf, RdfFormat::NQuads).unwrap();
            assert!(
                String::from_utf8(buf)
                    .unwrap()
                    .contains("<http://ex.org/g>")
            );
            assert_eq!(
                ds.named_graph("http://ex.org/g").unwrap().clear().unwrap(),
                1
            );
            let _ = NamedNodeRef::new_unchecked("x");
        }
    }
}
