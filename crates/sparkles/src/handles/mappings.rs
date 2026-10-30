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

    /// Run stored R2RML, RML or YARRRML mapping `name` (at `version`, or its latest) and
    /// commit once, then record the run. With `replace`, the run replaces every graph the
    /// mapping's graph maps write in that commit, which needs constant graph maps. `o`
    /// carries the commit message (by default `mapping NAME (version V)`), the author,
    /// the deadline and a precondition. CSVW metadata and templates map CSV files, which
    /// a load converts with them (see [`crate::tabular`]).
    pub fn run(
        &self,
        name: &str,
        version: Option<u64>,
        inputs: &Inputs,
        opts: &RunOptions,
        replace: bool,
        o: &crate::guard::WriteOptions,
    ) -> Result<MappingRun> {
        let stored = self
            .get(name, version)
            .ok_or_else(|| Error::NotFound(format!("no stored mapping '{name}'")))?;
        let kind = stored.definition.kind()?;
        if !matches!(kind, MappingKind::Rml | MappingKind::Yarrrml) {
            return Err(Error::invalid(format!(
                "mapping '{name}' is {} metadata and maps CSV files: load the tables with it",
                kind.name()
            )));
        }
        let m = mapping::parse(&stored.definition.text, kind, None)?;
        let graphs = if replace {
            Some(m.constant_graphs().ok_or_else(|| {
                Error::invalid(
                    "replace mode needs constant graph maps, since the graphs a template graph map would clear are not known before the run",
                )
            })?)
        } else {
            None
        };
        let (tmp, stats) = mapping::to_nquads_file(&m, inputs, opts, None)?;
        let mut o = o.clone();
        if o.message.is_none() {
            o.message = Some(format!("mapping {name} (version {})", stored.version.version).into());
        }
        let receipt = self.ds.load_mapping_output(&tmp, graphs.as_deref(), &o)?;
        self.record_run(
            name,
            LastRun {
                time: crate::builder::now_rfc3339(),
                version: stored.version.version,
                mode: if replace { "replace" } else { "add" }.into(),
                commit: receipt.committed.then_some(receipt.commit.seq),
                quads: stats.quads,
                rows: stats.rows,
                warnings: stats.warnings.clone(),
                author: o.author.as_ref().map(|a| a.to_string()),
            },
        )?;
        Ok(MappingRun { receipt, stats })
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
    /// Use [`Self::run_mapping_with`] for structured controls through the final load.
    pub fn run_mapping(
        &self,
        text: &str,
        kind: Option<MappingKind>,
        inputs: &Inputs,
        opts: &RunOptions,
        graph: Option<&str>,
    ) -> Result<MappingRun> {
        self.run_mapping_impl(text, kind, inputs, opts, graph, None)
    }

    /// Run a direct mapping with structured write controls. The conversion callback
    /// and write controls both apply during conversion, and the final load uses `o`.
    /// The original conversion callback remains conversion-only. Use `o.cancel`,
    /// `o.cancellation` and `o.deadline` for controls that also cover the writer wait.
    /// Once the loader publishes its commit, cancellation cannot retract success.
    pub fn run_mapping_with(
        &self,
        text: &str,
        kind: Option<MappingKind>,
        inputs: &Inputs,
        opts: &RunOptions,
        graph: Option<&str>,
        o: &crate::guard::WriteOptions,
    ) -> Result<MappingRun> {
        self.run_mapping_impl(text, kind, inputs, opts, graph, Some(o))
    }

    fn run_mapping_impl(
        &self,
        text: &str,
        kind: Option<MappingKind>,
        inputs: &Inputs,
        opts: &RunOptions,
        graph: Option<&str>,
        write: Option<&crate::guard::WriteOptions>,
    ) -> Result<MappingRun> {
        if let Some(write) = write {
            write.check()?;
        }
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
        // Compose once, preserving every structured flag and the independent
        // conversion callback. Sources keep their existing periodic checkpoints.
        let controlled = write.map(|write| {
            let mut conversion = opts.clone();
            let previous = opts.check.clone();
            let write = write.clone();
            conversion.check = Some(std::sync::Arc::new(move || {
                write.check()?;
                if let Some(check) = &previous {
                    check()?;
                }
                Ok(())
            }));
            conversion
        });
        let opts = controlled.as_ref().unwrap_or(opts);
        if controlled.is_some()
            && let Some(check) = &opts.check
        {
            check()?;
        }
        let (tmp, stats) = mapping::to_nquads_file(&m, inputs, opts, None)?;
        if controlled.is_some()
            && let Some(check) = &opts.check
        {
            check()?;
        }
        let sources = vec![mapping::source(&tmp, g, "mapping")];
        let receipt = match write {
            Some(write) => {
                self.store()
                    .load_with(&sources, crate::commit::CommitKind::Load, write)?
            }
            None => self.load_sources_receipt(sources)?,
        };
        // The loader owns the publication boundary. A later stop cannot turn a
        // published commit into an apparent refusal.
        Ok(MappingRun { receipt, stats })
    }

    /// Load the N-Quads file of a mapping run in one commit. With `graphs`, the run
    /// replaces those graphs (the default graph as `None`), and the quads the file lacks
    /// are removed from them in the same commit. `o` carries the commit message, the
    /// author, the deadline and a precondition.
    pub fn load_mapping_output(
        &self,
        file: &std::path::Path,
        graphs: Option<&[Option<oxrdf::NamedNode>]>,
        o: &crate::guard::WriteOptions,
    ) -> Result<Receipt> {
        let src = vec![mapping::source(file, None, "mapping")];
        match graphs {
            None => self
                .store()
                .load_with(&src, crate::commit::CommitKind::Load, o),
            Some(gs) => Ok(self
                .store()
                .replace_with(
                    crate::store::ReplaceTarget::Graphs(gs.to_vec()),
                    &src,
                    crate::commit::CommitKind::Load,
                    o,
                )?
                .1),
        }
    }
}
