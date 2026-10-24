//! Stored mappings (`mappings.json`, spec F19 §8) and runs of R2RML, RML and YARRRML
//! mappings into the dataset.

use crate::Dataset;
use crate::commit::Receipt;
use crate::error::{Error, Result};
use crate::mapping::{self, Inputs, MappingKind, RunOptions, Stats};
use crate::stored::Version;
use crate::stored_mappings::{Change, Definition, LastRun, Saved, Stored};

/// The dataset's stored mappings (from [`Dataset::mappings`]).
#[derive(Clone)]
pub struct StoredMappings {
    pub(crate) ds: Dataset,
}

impl StoredMappings {
    /// Every stored mapping at its latest version, with its last run.
    pub fn list(&self) -> Vec<(String, Stored, Option<LastRun>)> {
        self.ds.state().mappings.list()
    }

    /// Mapping `name` at `version`, or its latest version.
    pub fn get(&self, name: &str, version: Option<u64>) -> Option<Stored> {
        self.ds.state().mappings.get(name, version)
    }

    /// The versions of mapping `name`, newest first.
    pub fn versions(&self, name: &str) -> Option<Vec<Version>> {
        self.ds.state().mappings.versions(name)
    }

    /// Create or replace mapping `name` as a new version. The mapping is parsed and
    /// checked first.
    pub fn put(&self, name: &str, def: Definition, change: Change) -> Result<Saved> {
        self.ds.state().mappings.put(name, def, change)
    }

    /// Remove mapping `name`; whether it existed.
    pub fn delete(&self, name: &str, if_version: Option<u64>) -> Result<bool> {
        self.ds.state().mappings.delete(name, if_version)
    }

    /// The last run of mapping `name`.
    pub fn last_run(&self, name: &str) -> Option<LastRun> {
        self.ds.state().mappings.last_run(name)
    }

    /// Record the last run of mapping `name`.
    pub fn record_run(&self, name: &str, run: LastRun) -> Result<()> {
        self.ds.state().mappings.record_run(name, run)
    }

    /// Why `mappings.json` could not be read, if it could not.
    pub fn error(&self) -> Option<String> {
        self.ds.state().mappings.broken().map(str::to_string)
    }
}

/// What [`Dataset::run_mapping`] did: the commit's receipt and the run's counts.
#[derive(Debug)]
pub struct MappingRun {
    pub receipt: Receipt,
    pub stats: Stats,
}

impl Dataset {
    /// The stored mappings (`mappings.json`).
    pub fn mappings(&self) -> StoredMappings {
        StoredMappings { ds: self.clone() }
    }

    /// Run an R2RML, RML or YARRRML mapping and load its quads in one commit, with the
    /// default graph's triples in `graph` when one is given. The kind is read from the
    /// text when `kind` is `None`. Sources are read from `inputs`, one at a time, and
    /// the quads go through a temporary N-Quads file, so a failed run commits nothing.
    pub fn run_mapping(
        &self,
        text: &str,
        kind: Option<MappingKind>,
        inputs: &Inputs,
        opts: &RunOptions,
        graph: Option<&str>,
    ) -> Result<MappingRun> {
        let kind = match kind.or_else(|| mapping::detect(text)) {
            Some(k @ (MappingKind::Rml | MappingKind::Yarrrml)) => k,
            Some(_) => {
                return Err(Error::invalid(
                    "CSVW metadata and CONSTRUCT templates map CSV files: load the table with the mapping or template instead",
                ));
            }
            None => {
                return Err(Error::invalid(
                    "the text is not an R2RML, RML or YARRRML mapping",
                ));
            }
        };
        let g = graph
            .map(oxrdf::NamedNode::new)
            .transpose()
            .map_err(|e| Error::invalid(e.to_string()))?;
        let m = mapping::parse(text, kind, None)?;
        let (tmp, stats) = mapping::to_nquads_file(&m, inputs, opts, None)?;
        let receipt = self.load_sources_receipt(vec![mapping::source(&tmp, g, "mapping")])?;
        Ok(MappingRun { receipt, stats })
    }
}
