//! The write guard, and SHACL and ShEx validation of the dataset's current state.

use crate::Dataset;
use crate::error::Result;
use crate::guard::ValidationSummary;
use crate::write_guard::WriteGuard;

/// Validation of the dataset (from [`Dataset::validation`]).
#[derive(Clone)]
pub struct Validation {
    pub(crate) ds: Dataset,
}

impl Validation {
    /// The write guard: write-time validation of every commit.
    pub fn guard(&self) -> GuardSetting {
        GuardSetting {
            ds: self.ds.clone(),
        }
    }

    /// Validate the current state against SHACL shapes. The dataset's hidden control
    /// graphs (spec C26 §2.3) are never read, and a data graph that is one of them
    /// conforms as a graph without quads does.
    #[cfg(feature = "shacl")]
    pub fn shacl(
        &self,
        shapes: &sparkles_shacl::Shapes,
        opts: &sparkles_shacl::ValidateOptions,
    ) -> Result<sparkles_shacl::ValidationReport> {
        let mut o = opts.clone();
        if !self.ds.hide_validation_graphs(
            &o.data_graph,
            &mut o.extra_graphs,
            &mut o.exclude_graphs,
        ) {
            return Ok(sparkles_shacl::ValidationReport {
                conforms: true,
                ..Default::default()
            });
        }
        sparkles_shacl::validate(&self.ds.snapshot(), shapes, &o).map_err(super::from_anyhow)
    }

    /// Validate the current state against a ShEx schema and shape map, without the
    /// dataset's hidden control graphs, as [`shacl`](Self::shacl) does.
    #[cfg(feature = "shex")]
    pub fn shex(
        &self,
        schema: &sparkles_shex::CompiledSchema,
        map: &sparkles_shex::ShapeMap,
        opts: &sparkles_shex::ValidateOptions,
    ) -> Result<sparkles_shex::ResultMap> {
        let mut o = opts.clone();
        if !self.ds.hide_validation_graphs(
            &o.data_graph,
            &mut o.extra_graphs,
            &mut o.exclude_graphs,
        ) {
            return Ok(sparkles_shex::ResultMap {
                conforms: true,
                ..Default::default()
            });
        }
        sparkles_shex::validate(&self.ds.snapshot(), schema, map, &o).map_err(super::from_anyhow)
    }
}

/// What setting a write guard's configuration did.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum GuardOutcome {
    /// installed, with the validation of the current state
    Installed(ValidationSummary),
    /// mode `reject` was refused: the current state does not pass
    NotConforming(ValidationSummary),
    /// validation is off (mode `off`)
    Removed,
}

/// The write guard's configuration (`validation.json`) and the installed guard.
#[derive(Clone)]
pub struct GuardSetting {
    ds: Dataset,
}

impl GuardSetting {
    /// Refuse a configuration that names one of the dataset's hidden control graphs
    /// (spec C26 §2.3) as a source of its shapes or schema, or as a data graph, as for a
    /// graph that does not exist, so that installing it never reads one.
    #[cfg(any(feature = "shacl", feature = "shex"))]
    fn refuse_hidden(
        &self,
        sources: Option<&[String]>,
        data: &crate::guard::config::DataGraphSel,
    ) -> Result<()> {
        let hidden = self.ds.store().hidden_graphs();
        let named = match data {
            crate::guard::config::DataGraphSel::Graphs(gs) => gs.as_slice(),
            crate::guard::config::DataGraphSel::Named(_) => &[],
        };
        match sources
            .into_iter()
            .flatten()
            .chain(named)
            .find(|g| hidden.contains(g))
        {
            Some(g) => Err(crate::Error::NotFound(format!("no such graph: <{g}>"))),
            None => Ok(()),
        }
    }

    /// The installed guard, whose `json()` is its configuration and status.
    pub fn get(&self) -> Option<WriteGuard> {
        self.ds.write_guard()
    }

    /// Validate the current state with a SHACL configuration, and install it unless mode
    /// `reject` finds blocking results.
    #[cfg(feature = "shacl")]
    pub fn set_shacl(&self, cfg: sparkles_shacl::guard::ValidationConfig) -> Result<GuardOutcome> {
        use sparkles_shacl::guard::{SetOutcome, set_config};
        self.refuse_hidden(cfg.shapes.graphs.as_deref(), &cfg.data_graph)?;
        Ok(
            match set_config(self.ds.store(), Some(cfg)).map_err(super::from_anyhow)? {
                SetOutcome::Installed(g, s) => {
                    self.ds.set_write_guard(Some(WriteGuard::Shacl(g)));
                    GuardOutcome::Installed(s)
                }
                SetOutcome::NotConforming(s) => GuardOutcome::NotConforming(s),
                SetOutcome::Removed => {
                    self.ds.set_write_guard(None);
                    GuardOutcome::Removed
                }
            },
        )
    }

    /// Validate the current state with a ShEx configuration, and install it unless mode
    /// `reject` finds nonconformant associations. `resolver` reads the schema's imports.
    #[cfg(feature = "shex")]
    pub fn set_shex(
        &self,
        cfg: sparkles_shex::guard::ShexValidationConfig,
        resolver: &dyn sparkles_shex::Resolver,
    ) -> Result<GuardOutcome> {
        use sparkles_shex::guard::{SetOutcome, set_config};
        self.refuse_hidden(cfg.schema.graphs.as_deref(), &cfg.data_graph)?;
        Ok(
            match set_config(self.ds.store(), Some(cfg), resolver).map_err(super::from_anyhow)? {
                SetOutcome::Installed(g, s) => {
                    self.ds.set_write_guard(Some(WriteGuard::Shex(g)));
                    GuardOutcome::Installed(s)
                }
                SetOutcome::NotConforming(s) => GuardOutcome::NotConforming(s),
                SetOutcome::Removed => {
                    self.ds.set_write_guard(None);
                    GuardOutcome::Removed
                }
            },
        )
    }

    /// Remove the configuration and the guard. Either language's removal takes every
    /// validation file away.
    pub fn reset(&self) -> Result<()> {
        #[cfg(feature = "shacl")]
        sparkles_shacl::guard::set_config(self.ds.store(), None).map_err(super::from_anyhow)?;
        #[cfg(all(feature = "shex", not(feature = "shacl")))]
        sparkles_shex::guard::set_config(self.ds.store(), None, &sparkles_shex::NoImports)
            .map_err(super::from_anyhow)?;
        #[cfg(not(any(feature = "shacl", feature = "shex")))]
        return Err(crate::Error::unsupported(
            "write-time validation needs the `shacl` or `shex` feature",
        ));
        #[cfg(any(feature = "shacl", feature = "shex"))]
        {
            self.ds.set_write_guard(None);
            Ok(())
        }
    }
}
