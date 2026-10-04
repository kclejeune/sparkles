//! RDFS on read (Fuseki's `--rdfs FILE`, Jena's `ja:DatasetRDFS`): queries match the
//! RDFS closure of each graph with respect to a schema, computed at query time (see
//! [`sparkles::sparql::rdfs`](crate::sparql::rdfs)).
//!
//! The schema is a graph of the dataset, read in the state each query sees, or an RDF
//! document given once, whose closed schema triples are kept. A persistent dataset keeps
//! the setting in `rdfs.json`, with an uploaded schema in `rdfs-schema.nt`, and
//! [`Dataset::open`](crate::Dataset::open) loads it.

use crate::Dataset;
use crate::error::{Error, Result};
use crate::io::{RdfFormat, Source};
use crate::sparql::rdfs::{RdfsOnRead, RdfsSchema, SchemaSource};
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The setting of a persistent dataset.
pub const SETTING_FILE: &str = "rdfs.json";
/// The closed triples of an uploaded schema.
pub const SCHEMA_FILE: &str = "rdfs-schema.nt";

/// What `rdfs.json` holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Setting {
    #[serde(default)]
    rdfs_format: u32,
    /// the schema graph, `default` or an IRI; without it, the schema of `rdfs-schema.nt`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    graph: Option<String>,
}

/// A new setting.
#[derive(Clone, Debug)]
pub enum NewSchema {
    /// a graph of the dataset: `default` or an IRI
    Graph(String),
    /// a schema given as triples
    Triples(Vec<oxrdf::Triple>),
}

fn graph_source(g: &str) -> SchemaSource {
    SchemaSource::Graph((g != "default").then(|| g.to_string()))
}

/// The setting kept in a store's directory, if any. A setting that cannot be read is
/// logged and ignored, and queries run without it.
pub(crate) fn load(store: &Store) -> Option<Arc<RdfsOnRead>> {
    let root = store.root()?;
    let bytes = std::fs::read(root.join(SETTING_FILE)).ok()?;
    let open = || -> Result<RdfsOnRead> {
        let s: Setting = serde_json::from_slice(&bytes)
            .map_err(|e| Error::invalid(format!("{SETTING_FILE}: {e}")))?;
        Ok(match s.graph {
            Some(g) => RdfsOnRead::new(graph_source(&g)),
            None => {
                let nt = std::fs::read(root.join(SCHEMA_FILE)).map_err(|e| {
                    Error::Io(std::io::Error::new(
                        e.kind(),
                        format!("reading {SCHEMA_FILE}: {e}"),
                    ))
                })?;
                let (quads, _) =
                    crate::io::parse_to_vec(&Source::from_bytes(nt, RdfFormat::NTriples, None))?;
                let triples: Vec<oxrdf::Triple> =
                    quads.into_iter().map(oxrdf::Triple::from).collect();
                RdfsOnRead::fixed(RdfsSchema::from_triples(&triples))
            }
        })
    };
    match open() {
        Ok(r) => Some(Arc::new(r)),
        Err(e) => {
            tracing::error!(
                "RDFS on read of {}: {e}; queries run without it",
                root.display()
            );
            None
        }
    }
}

/// Set the dataset's RDFS on read, or with `None` remove it: in its directory first,
/// for a persistent dataset, then in memory.
pub fn set(ds: &Dataset, new: Option<NewSchema>) -> Result<()> {
    let state = ds.state();
    let root = state.store.root();
    let json = |s: &Setting| {
        serde_json::to_vec_pretty(s).map_err(|e| Error::invalid(format!("{SETTING_FILE}: {e}")))
    };
    let r = match new {
        None => {
            if let Some(root) = root {
                for f in [SETTING_FILE, SCHEMA_FILE] {
                    match std::fs::remove_file(root.join(f)) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            None
        }
        Some(NewSchema::Graph(g)) => {
            if let Some(root) = root {
                let s = Setting {
                    rdfs_format: 1,
                    graph: Some(g.clone()),
                };
                crate::guard::config::write_atomic(&root.join(SETTING_FILE), &json(&s)?)?;
                let _ = std::fs::remove_file(root.join(SCHEMA_FILE));
            }
            Some(RdfsOnRead::new(graph_source(&g)))
        }
        Some(NewSchema::Triples(t)) => {
            let schema = RdfsSchema::from_triples(&t);
            if let Some(root) = root {
                let mut nt = String::new();
                for t in schema.triples() {
                    nt.push_str(&format!("{t} .\n"));
                }
                // the schema before the setting that names it
                crate::guard::config::write_atomic(&root.join(SCHEMA_FILE), nt.as_bytes())?;
                let s = Setting {
                    rdfs_format: 1,
                    graph: None,
                };
                crate::guard::config::write_atomic(&root.join(SETTING_FILE), &json(&s)?)?;
            }
            Some(RdfsOnRead::fixed(schema))
        }
    };
    *state.rdfs.write() = r.map(Arc::new);
    Ok(())
}
