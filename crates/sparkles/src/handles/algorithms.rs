//! Graph algorithms: runs of PageRank, communities, similarity and embeddings on a
//! projection of the dataset, and the named projections (`algo.json`).

use crate::Dataset;
use crate::algo::catalog::{ProjectionDef, Stored};
use crate::error::{Error, Result};
use crate::sparql::algorun::{AlgoRequest, RunReport};
use crate::sparql::{QueryOptions, QueryResult};
use crate::task::Control;
use std::sync::Arc;

/// The dataset's graph algorithms (from [`Dataset::algorithms`]).
#[derive(Clone)]
pub struct Algorithms {
    pub(crate) ds: Dataset,
}

/// The named projections of the dataset (from [`Algorithms::projections`]).
#[derive(Clone)]
pub struct Projections {
    ds: Dataset,
}

impl Algorithms {
    /// The named projections, which `algo:projection "name"` and a request's
    /// `projection` refer to.
    pub fn projections(&self) -> Projections {
        Projections {
            ds: self.ds.clone(),
        }
    }

    /// The standard prefixes and the dataset's, for the prefixed names of a request.
    fn prefixes(&self) -> Vec<(String, String)> {
        let mut p = crate::io::standard_prefixes();
        p.extend(self.ds.store().prefixes());
        p.into_iter().collect()
    }

    /// Run a request and return its rows, one for each node (or node pair, or node and
    /// level), with a column for each output. The query it stands for runs with the
    /// dataset's query defaults and the budgets and access of `opts`.
    pub fn run(&self, req: &AlgoRequest, opts: &QueryOptions) -> Result<QueryResult> {
        if req.write.is_some() {
            return Err(Error::invalid(
                "the request writes its results; use run_write",
            ));
        }
        let (q, _) = crate::sparql::algorun::query_text(req, &self.prefixes(), false)?;
        self.ds.query_with(&q, opts)
    }

    /// Run a request and return its summary (the statistics of the run) as a JSON text.
    pub fn stats(&self, req: &AlgoRequest, opts: &QueryOptions) -> Result<String> {
        let (q, _) = crate::sparql::algorun::query_text(req, &self.prefixes(), true)?;
        let r = self.ds.query_with(&q, opts)?;
        r.rows()
            .into_iter()
            .next()
            .and_then(|row| row.into_iter().next().flatten())
            .map(|t| match t {
                oxrdf::Term::Literal(l) => l.value().to_string(),
                t => t.to_string(),
            })
            .ok_or_else(|| Error::invalid("the run has no nodes to report on"))
    }

    /// Run a request with a `write` part: compute at the head commit and write the rows
    /// to the graph in batched commits, with a run record (see
    /// [`algorun`](crate::sparql::algorun)). `author` is recorded with the commits.
    pub fn run_write(
        &self,
        req: &AlgoRequest,
        opts: &QueryOptions,
        author: Option<Arc<str>>,
        ctl: &Control,
    ) -> Result<RunReport> {
        let opts = self.ds.with_query_defaults(opts);
        crate::sparql::algorun::run_write(
            self.ds.store(),
            req,
            &self.prefixes(),
            &opts,
            author,
            ctl,
        )
    }
}

impl Projections {
    /// Every definition, by name.
    pub fn list(&self) -> Vec<(String, Stored)> {
        self.ds.state().algo.list()
    }

    /// A definition with its revision.
    pub fn get(&self, name: &str) -> Option<Stored> {
        self.ds.state().algo.get_stored(name)
    }

    /// Create or replace a definition, if its revision is `if_revision` when given (0:
    /// it must not exist); the stored definition and whether it is new. Prefixed names
    /// in the definition are written out in full with the standard prefixes and the
    /// dataset's.
    pub fn put(
        &self,
        name: &str,
        def: ProjectionDef,
        author: Option<String>,
        if_revision: Option<u64>,
    ) -> Result<(Stored, bool)> {
        let mut prefixes: Vec<(String, String)> =
            crate::io::standard_prefixes().into_iter().collect();
        prefixes.extend(self.ds.store().prefixes());
        let def = expand(def, &prefixes)?;
        self.ds.state().algo.put(name, def, author, if_revision)
    }

    /// Remove a definition (if its revision is `if_revision`, when given); whether it
    /// existed.
    pub fn delete(&self, name: &str, if_revision: Option<u64>) -> Result<bool> {
        self.ds.state().algo.delete(name, if_revision)
    }

    /// Why `algo.json` could not be read, if it could not. The catalog is then empty and
    /// refuses changes, so the file is never overwritten.
    pub fn error(&self) -> Option<String> {
        self.ds.state().algo.broken().map(str::to_string)
    }
}

/// `def` with its prefixed names written out in full.
fn expand(mut def: ProjectionDef, prefixes: &[(String, String)]) -> Result<ProjectionDef> {
    use crate::sparql::algorun::expand_iri;
    if let Some(ps) = &mut def.predicates {
        for p in ps {
            p.iri = expand_iri(&p.iri, prefixes)?;
        }
    }
    for c in &mut def.node_classes {
        *c = expand_iri(c, prefixes)?;
    }
    if let Some(w) = &mut def.weight {
        *w = expand_iri(w, prefixes)?;
    }
    Ok(def)
}

impl Dataset {
    /// The graph algorithms and the named projections.
    pub fn algorithms(&self) -> Algorithms {
        Algorithms { ds: self.clone() }
    }
}
