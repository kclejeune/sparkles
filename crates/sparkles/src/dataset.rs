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
use crate::index::{G, Key, O, P, Perm, S};
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

    /// A dataset around a store others share (a branch's, which its dataset keeps open).
    pub(crate) fn from_shared(store: Arc<Store>) -> Dataset {
        Dataset { store }
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
    /// the executed plan and timings). DESCRIBE follows the dataset's setting
    /// ([`Store::describe_settings`](crate::store::Store::describe_settings)).
    pub fn query(&self, query: &str) -> Result<QueryResult> {
        let opts = QueryOptions {
            describe: self.store.describe_settings(),
            ..Default::default()
        };
        self.query_with(query, &opts)
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

    /// Run a CONSTRUCT or DESCRIBE query: the triples of the default graph.
    pub fn construct(&self, query: &str) -> Result<Vec<Triple>> {
        let r = self.query(query)?;
        if !matches!(r.kind, QueryKind::Construct | QueryKind::Describe) {
            return Err(Error::invalid("not a CONSTRUCT or DESCRIBE query"));
        }
        Ok(r.triples)
    }

    /// Run a CONSTRUCT or DESCRIBE query as quads: the default graph's triples, then the
    /// quads of a CONSTRUCT template's `GRAPH` blocks (Jena ARQ's quad templates).
    pub fn construct_quads(&self, query: &str) -> Result<Vec<oxrdf::Quad>> {
        let r = self.query(query)?;
        if !matches!(r.kind, QueryKind::Construct | QueryKind::Describe) {
            return Err(Error::invalid("not a CONSTRUCT or DESCRIBE query"));
        }
        let mut out: Vec<oxrdf::Quad> = r
            .triples
            .into_iter()
            .map(|t| t.in_graph(oxrdf::GraphName::DefaultGraph))
            .collect();
        out.extend(r.quads);
        Ok(out)
    }

    /// Run a SPARQL Update request (all operations in one transaction).
    pub fn update(&self, update: &str) -> Result<UpdateStats> {
        crate::sparql::update::update(&self.store, update, &QueryOptions::default())
    }

    pub fn update_with(&self, update: &str, opts: &QueryOptions) -> Result<UpdateStats> {
        crate::sparql::update::update(&self.store, update, opts)
    }

    /// Apply an RDF Patch, in the text form or (`binary`) the RDF Thrift form, as one
    /// commit (see [`Store::apply_patch`]).
    pub fn apply_patch(
        &self,
        patch: impl std::io::Read,
        binary: bool,
    ) -> Result<crate::store::PatchOutcome> {
        self.store.apply_patch(
            patch,
            &crate::store::PatchOptions {
                binary,
                ..Default::default()
            },
        )
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

    /// The quads of [`find`](Self::find), read from the current snapshot in batches of
    /// keys and decoded as the iterator advances, so a large match is never collected.
    /// Later commits are not seen.
    pub fn quads(
        &self,
        graph: Option<GraphNameRef<'_>>,
        subject: Option<&NamedOrBlankNode>,
        predicate: Option<&NamedNode>,
        object: Option<&Term>,
    ) -> QuadIter {
        let snap = self.store.snapshot();
        let sel = match graph {
            None => GraphSel::Any,
            Some(GraphNameRef::DefaultGraph) => GraphSel::Default,
            Some(GraphNameRef::NamedNode(n)) => GraphSel::Named(n.into_owned()),
            Some(GraphNameRef::BlankNode(b)) => GraphSel::Blank(b.into_owned()),
        };
        let plan = scan_plan(&snap, &sel, subject, predicate, object, false);
        QuadIter::new(snap, plan)
    }

    /// [`quads`](Self::quads) over every graph, read from an index that orders the
    /// quads of one triple next to each other, so that a caller can group a triple's
    /// graphs without collecting the match.
    pub fn quads_by_triple(
        &self,
        subject: Option<&NamedOrBlankNode>,
        predicate: Option<&NamedNode>,
        object: Option<&Term>,
    ) -> QuadIter {
        let snap = self.store.snapshot();
        let plan = scan_plan(&snap, &GraphSel::Any, subject, predicate, object, true);
        QuadIter::new(snap, plan)
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
pub(crate) enum GraphSel {
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
    pub(crate) txn: WriteTxn<'s>,
    pub(crate) labels: HashMap<String, Id>,
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

    /// [`insert`](Self::insert), except that a blank node whose label names a stored
    /// node (`b…`, as [`find`](Self::find) and the dataset's reads hand it out) is that
    /// node rather than a new one. Other labels are scoped to the transaction as in
    /// `insert`, and a `b…` label that names no stored node is such a label. The labels
    /// inside a composite literal (`cdt:List`, `cdt:Map`) are linked the same way.
    pub fn insert_linked(&mut self, quad: QuadRef<'_>) -> Result<bool> {
        let mut link = |b: &str| {
            if self.labels.contains_key(b) {
                return;
            }
            if let Some(id) = crate::store::parse_bnode_label(b)
                && self.txn.bnode_allocated(id)
            {
                self.labels.insert(b.to_string(), id);
            }
        };
        if let oxrdf::NamedOrBlankNodeRef::BlankNode(b) = quad.subject {
            link(b.as_str());
        }
        each_blank_node(quad.object, &mut |b| link(b.as_str()));
        if let oxrdf::TermRef::Literal(l) = quad.object
            && crate::sparql::cdt::is_cdt(l.datatype().as_str())
        {
            crate::sparql::cdt::relabel_literal(&l.into_owned(), &mut |b| {
                link(b);
                b.to_string()
            });
        }
        if let GraphNameRef::BlankNode(b) = quad.graph_name {
            link(b.as_str());
        }
        self.insert(quad)
    }

    /// The stored blank node that `label` names in this transaction: the node an
    /// earlier insert made or linked for the label, or `None` when no insert has used
    /// it. Its label is the one later reads and writes use for the node.
    pub fn blank_node(&self, label: &str) -> Option<oxrdf::BlankNode> {
        self.labels
            .get(label)
            .map(|id| crate::store::bnode_for(*id))
    }

    /// Run a SPARQL query that sees this transaction's changes.
    pub fn query_with(&self, query: &str, opts: &QueryOptions) -> Result<QueryResult> {
        crate::sparql::query(Arc::new(self.txn.view()), query, opts)
    }

    /// Run a SPARQL Update request in this transaction. Its operations see the
    /// transaction's changes, and they commit or roll back with it.
    pub fn update_with(&mut self, update: &str, opts: &QueryOptions) -> Result<UpdateStats> {
        crate::sparql::update::update_in(&mut self.txn, update, opts)
    }
}

/// Call `f` on each blank node of a term, inside triple terms too.
fn each_blank_node<'a>(t: oxrdf::TermRef<'a>, f: &mut impl FnMut(oxrdf::BlankNodeRef<'a>)) {
    match t {
        oxrdf::TermRef::BlankNode(b) => f(b),
        oxrdf::TermRef::Triple(tr) => {
            if let oxrdf::NamedOrBlankNode::BlankNode(b) = &tr.subject {
                f(b.as_ref());
            }
            each_blank_node(tr.object.as_ref(), f);
        }
        _ => {}
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

pub(crate) fn quad_ids(snap: &Snapshot, q: QuadRef<'_>) -> Option<[Id; 4]> {
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

/// How to read the quads of a pattern: the permutation with the longest bound prefix,
/// the prefix, the bound components to check past it, and whether only named graphs
/// count (the union graph, whose triples are deduplicated).
pub(crate) struct ScanPlan {
    pub(crate) perm: Perm,
    pub(crate) prefix: Vec<u64>,
    pub(crate) bound: [Option<u64>; 4],
    pub(crate) named_only: bool,
}

impl ScanPlan {
    /// Whether a quad of the scan matches the components past the prefix.
    pub(crate) fn matches(&self, q: &[Id; 4]) -> bool {
        !(0..4).any(|c| self.bound[c].is_some_and(|b| b != q[c].0))
            && !(self.named_only && q[3] == Id::DEFAULT_GRAPH)
    }
}

/// The scan of a pattern, or `None` when a bound term is not in the store (no match).
/// With `graph_last`, the scan reads a permutation that ends with the graph, so the
/// quads of one triple are adjacent.
pub(crate) fn scan_plan(
    snap: &Snapshot,
    graph: &GraphSel,
    subject: Option<&NamedOrBlankNode>,
    predicate: Option<&NamedNode>,
    object: Option<&Term>,
    graph_last: bool,
) -> Option<ScanPlan> {
    let mut bound: [Option<u64>; 4] = [None; 4];
    if let Some(s) = subject {
        bound[S] = Some(subject_id(snap, s)?.0);
    }
    if let Some(p) = predicate {
        bound[P] = Some(snap.lookup_iri(p.as_str())?.0);
    }
    if let Some(o) = object {
        bound[O] = Some(snap.lookup_term(o)?.0);
    }
    let default_is_union = snap.union_default_graph;
    let (graph_eq, named_only) = match graph {
        GraphSel::Default if default_is_union => (None, true),
        GraphSel::Default => (Some(Id::DEFAULT_GRAPH.0), false),
        GraphSel::Named(n) => (Some(snap.lookup_iri(n.as_str())?.0), false),
        GraphSel::Blank(b) => (Some(crate::store::parse_bnode_label(b.as_str())?.0), false),
        GraphSel::Union => (None, true),
        GraphSel::Any => (None, false),
    };
    bound[G] = graph_eq;
    // permutation with the longest prefix of bound components
    let (perm, plen) = Perm::ALL
        .iter()
        .filter(|&&p| !(graph_last && p.order()[3] != G))
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
    Some(ScanPlan {
        perm,
        prefix,
        bound,
        named_only,
    })
}

/// Index-backed pattern matching: picks the permutation with the longest bound prefix.
fn find_quads(
    snap: &Snapshot,
    graph: &GraphSel,
    subject: Option<&NamedOrBlankNode>,
    predicate: Option<&NamedNode>,
    object: Option<&Term>,
) -> Result<Vec<Quad>> {
    let Some(plan) = scan_plan(snap, graph, subject, predicate, object, false) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut seen = FxHashSet::default();
    for k in snap.scan_keys(plan.perm, &plan.prefix)? {
        let q = plan.perm.to_quad(&k);
        if !plan.matches(&q) {
            continue;
        }
        if plan.named_only && !seen.insert([q[0], q[1], q[2]]) {
            continue;
        }
        if let Some(quad) = snap.quad_to_terms(&q) {
            out.push(quad);
        }
    }
    Ok(out)
}

/// Keys read per batch by [`QuadIter`].
const QUAD_BATCH: usize = 4096;

/// The quads of a pattern on one snapshot, from [`Dataset::quads`]. It reads up to 4096
/// keys at a time and decodes each quad when it is returned. An error ends the
/// iteration after it is returned.
pub struct QuadIter {
    snap: Arc<Snapshot>,
    pub(crate) plan: Option<ScanPlan>,
    /// where the next batch starts; `None` once the scan is done
    next: Option<Key>,
    hi: Key,
    pub(crate) batch: std::vec::IntoIter<Key>,
    /// triples already returned (union graph only)
    pub(crate) seen: FxHashSet<[Id; 3]>,
}

impl QuadIter {
    pub(crate) fn new(snap: Arc<Snapshot>, plan: Option<ScanPlan>) -> QuadIter {
        let pad = |v: u64| -> Key {
            let mut k = [v; 4];
            if let Some(p) = &plan {
                k[..p.prefix.len()].copy_from_slice(&p.prefix);
            }
            k
        };
        QuadIter {
            next: plan.as_ref().map(|_| pad(0)),
            hi: pad(u64::MAX),
            snap,
            plan,
            batch: Vec::new().into_iter(),
            seen: FxHashSet::default(),
        }
    }

    /// The snapshot the quads are read from.
    pub fn snapshot(&self) -> &Arc<Snapshot> {
        &self.snap
    }

    /// Read the next batch of keys; false when the scan is done.
    pub(crate) fn fill(&mut self) -> Result<bool> {
        let (Some(plan), Some(lo)) = (&self.plan, self.next) else {
            return Ok(false);
        };
        let mut keys = Vec::with_capacity(QUAD_BATCH);
        self.snap.scan_between(plan.perm, lo, self.hi, |c| {
            match c {
                crate::store::Chunk::Block(b, s, e) => {
                    let e = e.min(s + (QUAD_BATCH - keys.len()));
                    keys.extend((s..e).map(|i| b.key(i)));
                }
                crate::store::Chunk::Row(k) => keys.push(k),
            }
            Ok(keys.len() < QUAD_BATCH)
        })?;
        // a full batch continues after its last key
        self.next = if keys.len() < QUAD_BATCH {
            None
        } else {
            keys.last().and_then(successor)
        };
        let more = !keys.is_empty();
        self.batch = keys.into_iter();
        Ok(more)
    }
}

/// The key right after `k` in key order, if any.
fn successor(k: &Key) -> Option<Key> {
    let mut n = *k;
    for c in (0..4).rev() {
        if n[c] < u64::MAX {
            n[c] += 1;
            return Some(n);
        }
        n[c] = 0;
    }
    None
}

impl Iterator for QuadIter {
    type Item = Result<Quad>;

    fn next(&mut self) -> Option<Result<Quad>> {
        loop {
            let Some(k) = self.batch.next() else {
                match self.fill() {
                    Ok(true) => continue,
                    Ok(false) => return None,
                    Err(e) => {
                        self.next = None;
                        return Some(Err(e));
                    }
                }
            };
            let plan = self.plan.as_ref()?;
            let q = plan.perm.to_quad(&k);
            if !plan.matches(&q) {
                continue;
            }
            if plan.named_only && !self.seen.insert([q[0], q[1], q[2]]) {
                continue;
            }
            if let Some(quad) = self.snap.quad_to_terms(&q) {
                return Some(Ok(quad));
            }
        }
    }
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

    #[test]
    fn quads_iterator_matches_find_across_batches() {
        let ds = Dataset::memory();
        // more quads than one batch, spread over two graphs and two predicates
        let mut nt = String::new();
        for i in 0..(QUAD_BATCH * 2 + 17) {
            let p = if i % 3 == 0 { "q" } else { "p" };
            let g = if i % 5 == 0 { " <http://ex.org/g>" } else { "" };
            nt.push_str(&format!(
                "<http://ex.org/s{}> <http://ex.org/{p}> \"{i}\"{g} .\n",
                i % 50
            ));
        }
        ds.load_str(&nt, RdfFormat::NQuads).unwrap();
        let g = n("g");
        let check = |ds: &Dataset| {
            let patterns: Vec<(
                Option<GraphNameRef<'_>>,
                Option<NamedOrBlankNode>,
                Option<NamedNode>,
            )> = vec![
                (None, None, None),
                (Some(GraphNameRef::DefaultGraph), None, None),
                (Some(g.as_ref().into()), None, None),
                (None, Some(n("s7").into()), None),
                (None, None, Some(n("q"))),
                (
                    Some(GraphNameRef::DefaultGraph),
                    Some(n("s3").into()),
                    Some(n("p")),
                ),
                (None, Some(n("missing").into()), None),
            ];
            for (graph, s, p) in patterns {
                let want = ds.find(graph, s.as_ref(), p.as_ref(), None).unwrap();
                let got: Vec<Quad> = ds
                    .quads(graph, s.as_ref(), p.as_ref(), None)
                    .collect::<Result<_>>()
                    .unwrap();
                assert_eq!(got, want, "pattern {graph:?} {s:?} {p:?}");
            }
            assert_eq!(ds.quads(None, None, None, None).count() as u64, ds.len());
        };
        check(&ds);
        // the same after the pending changes are merged into a generation
        ds.compact().unwrap();
        check(&ds);
        // a later commit is not seen by an iterator made before it
        let it = ds.quads(None, None, None, None);
        ds.insert(QuadRef::new(
            &n("new"),
            &n("p"),
            &n("o"),
            GraphNameRef::DefaultGraph,
        ))
        .unwrap();
        assert_eq!(it.count() as u64, ds.len() - 1);
    }

    #[test]
    fn quads_by_triple_groups_a_triples_graphs() {
        let ds = Dataset::memory();
        let mut nq = String::new();
        for i in 0..300 {
            for g in ["", " <http://ex.org/g1>", " <http://ex.org/g2>"] {
                if (i + g.len()) % 2 == 0 || g.is_empty() {
                    nq.push_str(&format!(
                        "<http://ex.org/s{}> <http://ex.org/p> \"{i}\"{g} .\n",
                        i % 7
                    ));
                }
            }
        }
        ds.load_str(&nq, RdfFormat::NQuads).unwrap();
        for s in [None, Some(NamedOrBlankNode::from(n("s3")))] {
            let got: Vec<Quad> = ds
                .quads_by_triple(s.as_ref(), None, None)
                .collect::<Result<_>>()
                .unwrap();
            let mut want = ds.find(None, s.as_ref(), None, None).unwrap();
            assert_eq!(got.len(), want.len());
            // each triple's quads form one run
            let mut seen = FxHashSet::default();
            for (i, q) in got.iter().enumerate() {
                let t = (q.subject.clone(), q.predicate.clone(), q.object.clone());
                let same = i > 0 && {
                    let p = &got[i - 1];
                    (p.subject.clone(), p.predicate.clone(), p.object.clone()) == t
                };
                assert!(same || seen.insert(t), "a triple's quads are not adjacent");
            }
            let mut got = got;
            got.sort_by_key(|q| q.to_string());
            want.sort_by_key(|q| q.to_string());
            assert_eq!(got, want);
        }
    }

    /// The labels inside a composite literal name the nodes the transaction's labels
    /// name, a stored node's label included when the insert links stored nodes.
    #[test]
    fn transactions_scope_labels_inside_composite_literals() {
        let ds = Dataset::memory();
        ds.load_str("_:a <http://ex.org/p> \"1\" .", RdfFormat::NTriples)
            .unwrap();
        let NamedOrBlankNode::BlankNode(b) =
            ds.find(None, None, None, None).unwrap()[0].subject.clone()
        else {
            panic!("not a blank node")
        };
        let list = |label: &str| {
            Term::Literal(oxrdf::Literal::new_typed_literal(
                format!("[_:{label}]"),
                NamedNode::new_unchecked(crate::sparql::cdt::LIST),
            ))
        };
        let x = NamedOrBlankNode::from(oxrdf::BlankNode::new_unchecked("x"));
        ds.transaction(|tx| {
            tx.insert(Quad::new(x.clone(), n("l"), list("x"), GraphName::DefaultGraph).as_ref())?;
            tx.insert_linked(
                Quad::new(n("s"), n("m"), list(b.as_str()), GraphName::DefaultGraph).as_ref(),
            )?;
            Ok(())
        })
        .unwrap();
        let same = |q: &str| {
            let r = ds.query(q).unwrap();
            matches!(&r.rows()[0][0], Some(Term::Literal(l)) if l.value() == "true")
        };
        let cdt = "PREFIX cdt: <http://w3id.org/awslabs/neptune/SPARQL-CDTs/> ";
        assert!(same(&format!(
            "{cdt}SELECT (sameTerm(?s, cdt:head(?l)) AS ?x) {{ ?s <http://ex.org/l> ?l }}"
        )));
        assert!(same(&format!(
            "{cdt}SELECT (sameTerm(?s, cdt:head(?l)) AS ?x) {{ ?s <http://ex.org/p> ?o . <http://ex.org/s> <http://ex.org/m> ?l }}"
        )));
    }

    #[test]
    fn transactions_link_stored_blank_nodes_and_run_sparql() {
        let ds = Dataset::memory();
        ds.load_str("_:a <http://ex.org/p> \"1\" .", RdfFormat::NTriples)
            .unwrap();
        let stored = ds.find(None, None, None, None).unwrap()[0].subject.clone();
        let NamedOrBlankNode::BlankNode(b) = &stored else {
            panic!("not a blank node")
        };
        let q =
            |s: &NamedOrBlankNode| Quad::new(s.clone(), n("q"), n("o"), GraphName::DefaultGraph);
        let fresh = NamedOrBlankNode::from(oxrdf::BlankNode::new_unchecked("x"));
        let unallocated = NamedOrBlankNode::from(oxrdf::BlankNode::new_unchecked("bfffff"));
        let made = ds
            .transaction(|tx| {
                // a stored label names the stored node; others name new nodes
                assert!(tx.insert_linked(q(&stored).as_ref())?);
                assert!(tx.insert_linked(q(&fresh).as_ref())?);
                assert!(tx.insert_linked(q(&unallocated).as_ref())?);
                assert_eq!(tx.blank_node(b.as_str()).as_ref(), Some(b));
                let made = tx.blank_node("x").unwrap();
                assert_ne!(&made, b);
                assert_ne!(tx.blank_node("bfffff").unwrap().as_str(), "bfffff");
                // queries and updates in the transaction see its changes
                let r = tx.query_with(
                    "SELECT ?s WHERE { ?s <http://ex.org/q> ?o }",
                    &QueryOptions::default(),
                )?;
                assert_eq!(r.table.len(), 3);
                let st = tx.update_with(
                    "DELETE WHERE { ?s <http://ex.org/p> ?o }",
                    &QueryOptions::default(),
                )?;
                assert_eq!(st.deleted, 1);
                Ok(made)
            })
            .unwrap();
        let s_of = |p: &str| -> Vec<NamedOrBlankNode> {
            ds.find(None, None, Some(&n(p)), None)
                .unwrap()
                .into_iter()
                .map(|q| q.subject)
                .collect()
        };
        assert!(s_of("p").is_empty());
        let qs = s_of("q");
        assert_eq!(qs.len(), 3);
        assert!(qs.contains(&stored));
        assert!(qs.contains(&made.into()));
        // an update that fails leaves the transaction's earlier work to its caller
        let r = ds.transaction(|tx| {
            tx.insert(q(&n("t").into()).as_ref())?;
            tx.update_with("INSERT DATA { <a:x> <a:y> }", &QueryOptions::default())
        });
        assert!(r.is_err());
        assert!(s_of("q").len() == 3);
    }
}
