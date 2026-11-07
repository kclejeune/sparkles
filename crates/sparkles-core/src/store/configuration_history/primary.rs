//! Request-local authentication of primary references. Framing decides which
//! complete prefix survived recovery; an accepted seal cannot lose its target.
use super::*;
use crate::history::BranchConfigurationOrigin;
use crate::managed::ManagedState;

/// Scalar, request-local publication context. It holds no decoded generation or
/// coverage map. Cold readers charge this fixed value in their existing budget.
pub(crate) struct Header {
    dataset: uuid::Uuid,
    history: Option<managed_history::Root>,
    cutoff: Option<u64>,
    origin: Option<(u64, ConfigurationBinding)>,
    unreadable_history: bool,
    last: Option<ConfigurationIdentity>,
}

fn root_result(
    result: Result<Option<managed_history::Root>>,
    preparation: &Preparation,
) -> Result<(Option<managed_history::Root>, bool)> {
    match result {
        Ok(history) => Ok((history, false)),
        Err(error) => {
            preparation.check()?;
            if matches!(
                error,
                Error::Cancelled | Error::Timeout | Error::BudgetExceeded(_)
            ) {
                return Err(error);
            }
            // Preserve ordinary legacy RDF reads with an unavailable old managed
            // registry. Any sealed reference still refuses this unknown root.
            Ok((None, true))
        }
    }
}

impl Header {
    fn new(
        root: &Path,
        dataset: uuid::Uuid,
        branch: uuid::Uuid,
        origin: Option<(u64, ConfigurationBinding)>,
        history: Option<managed_history::Root>,
        unreadable_history: bool,
        preparation: &Preparation,
    ) -> Result<Self> {
        let origin_cutoff =
            if let Some((seq, binding)) = origin {
                binding.validate(dataset)?;
                Some(seq.checked_add(1).ok_or_else(|| {
                    managed_history::unavailable("primary branch boundary overflows")
                })?)
            } else {
                None
            };
        let coverage = history
            .as_ref()
            .map(|history| managed_history::read_coverage(root, dataset, history, preparation))
            .transpose()?;
        let cutoff = coverage
            .as_ref()
            .and_then(|coverage| coverage.value.branches.get(&branch))
            .map(|coverage| coverage.first_bound_own)
            .or(origin_cutoff);
        if history.is_some() && cutoff.is_none() {
            return Err(managed_history::unavailable(
                "primary branch has no configuration coverage",
            ));
        }
        Ok(Self {
            dataset,
            history,
            cutoff,
            origin,
            unreadable_history,
            last: None,
        })
    }

    pub(crate) fn from_state(
        root: &Path,
        dataset: uuid::Uuid,
        branch: uuid::Uuid,
        origin: Option<(u64, ConfigurationBinding)>,
        state: Option<&ManagedState>,
        preparation: &Preparation,
    ) -> Result<Self> {
        let (history, unreadable) = match state {
            Some(ManagedState::Installed(installed)) => {
                (installed.configuration_history.clone(), false)
            }
            Some(ManagedState::Unavailable { .. }) => {
                root_result(recorded_root(root, preparation), preparation)?
            }
            None => (None, false),
        };
        Self::new(
            root,
            dataset,
            branch,
            origin,
            history,
            unreadable,
            preparation,
        )
    }

    pub(crate) fn on_disk(
        root: &Path,
        dataset: uuid::Uuid,
        branch: uuid::Uuid,
        origin: Option<(u64, ConfigurationBinding)>,
        preparation: &Preparation,
    ) -> Result<Self> {
        let (history, unreadable) = root_result(recorded_root(root, preparation), preparation)?;
        Self::new(
            root,
            dataset,
            branch,
            origin,
            history,
            unreadable,
            preparation,
        )
    }

    pub(crate) fn generation(
        &mut self,
        root: &Path,
        preparation: &Preparation,
        seq: u64,
        binding: ConfigurationBinding,
    ) -> Result<()> {
        if self
            .origin
            .is_some_and(|(root_seq, origin)| root_seq == seq && binding != origin)
        {
            return Err(managed_history::unavailable(
                "branch root configuration differs from its primary origin",
            ));
        }
        self.binding(root, preparation, seq, binding)
    }

    fn raw_binding(
        &self,
        digest: Option<crate::evidence::Sha256Digest>,
    ) -> Result<ConfigurationBinding> {
        Ok(match digest {
            Some(generation) => {
                let history = self.history.as_ref().ok_or_else(|| {
                    managed_history::unavailable(
                        "sealed primary WAL reference has no published lineage",
                    )
                })?;
                managed_history::binding(ConfigurationIdentity {
                    dataset_id: self.dataset,
                    lineage_id: history.lineage_id,
                    generation,
                })
            }
            None => ConfigurationBinding::LegacyUnbound,
        })
    }

    /// The cold proof already polls controls. A scalar hit needs no child arena,
    /// decoding, file access or per-row allocation.
    pub(crate) fn cached_observation(
        &self,
        seq: u64,
        digest: Option<crate::evidence::Sha256Digest>,
    ) -> Result<bool> {
        Ok(self
            .identity(seq, self.raw_binding(digest)?)?
            .is_none_or(|identity| self.last == Some(identity)))
    }

    pub(crate) fn observation(
        &mut self,
        root: &Path,
        preparation: &Preparation,
        seq: u64,
        digest: Option<crate::evidence::Sha256Digest>,
    ) -> Result<()> {
        self.binding(root, preparation, seq, self.raw_binding(digest)?)
    }

    fn identity(
        &self,
        seq: u64,
        binding: ConfigurationBinding,
    ) -> Result<Option<ConfigurationIdentity>> {
        binding.validate(self.dataset)?;
        if self.cutoff.is_some_and(|cutoff| seq >= cutoff) && binding.configuration().is_none() {
            return Err(managed_history::unavailable(
                "covered primary commit is missing its configuration reference",
            ));
        }
        match binding {
            ConfigurationBinding::LegacyUnbound => Ok(None),
            ConfigurationBinding::ImportedUnbound { .. } => Err(managed_history::unavailable(
                "imported primary configuration needs its exact authenticated origin",
            )),
            ConfigurationBinding::Known { configuration } => {
                if self.cutoff.is_some_and(|cutoff| seq < cutoff)
                    && !self
                        .origin
                        .is_some_and(|(root_seq, origin)| root_seq == seq && origin == binding)
                {
                    return Err(managed_history::unavailable(
                        "pre-activation primary commit cannot acquire a native binding",
                    ));
                }
                let history = self.history.as_ref().ok_or_else(|| {
                    managed_history::unavailable(if self.unreadable_history {
                        "primary configuration publication is unreadable"
                    } else {
                        "primary configuration reference has no published lineage"
                    })
                })?;
                if configuration.lineage_id() != history.lineage_id {
                    return Err(managed_history::unavailable(
                        "primary reference belongs to another configuration lineage",
                    ));
                }
                Ok(Some(configuration))
            }
        }
    }

    fn binding(
        &mut self,
        root: &Path,
        preparation: &Preparation,
        seq: u64,
        binding: ConfigurationBinding,
    ) -> Result<()> {
        preparation.check()?;
        if let Some(configuration) = self.identity(seq, binding)?
            && self.last != Some(configuration)
        {
            let history = self
                .history
                .as_ref()
                .expect("identity checked the publication root");
            managed_history::resolve(root, self.dataset, history, configuration, preparation)?;
            self.last = Some(configuration);
        }
        Ok(())
    }
}

pub(crate) struct References<'a> {
    root: &'a Path,
    preparation: &'a Preparation,
    header: Header,
    _held: crate::access::Reservation,
}

impl<'a> References<'a> {
    pub(crate) fn new(
        root: &'a Path,
        dataset: uuid::Uuid,
        branch: uuid::Uuid,
        origin: Option<(u64, &'a BranchConfigurationOrigin)>,
        state: Option<&ManagedState>,
        preparation: &'a Preparation,
    ) -> Result<Self> {
        let held = preparation.reserve(512)?;
        if let Some((seq, origin)) = origin {
            origin.validate(dataset, seq)?;
        }
        let header = Header::from_state(
            root,
            dataset,
            branch,
            origin.map(|(seq, origin)| (seq, origin.binding)),
            state,
            preparation,
        )?;
        Ok(Self {
            root,
            preparation,
            header,
            _held: held,
        })
    }

    pub(crate) fn generation(&mut self, seq: u64, binding: ConfigurationBinding) -> Result<()> {
        self.header
            .generation(self.root, self.preparation, seq, binding)
    }

    /// Called by the existing replay only after its original complete-commit
    /// framing, term, sequence and annotation rules accept this observation.
    pub(crate) fn observation(
        &mut self,
        seq: u64,
        digest: Option<crate::evidence::Sha256Digest>,
    ) -> Result<()> {
        self.header
            .observation(self.root, self.preparation, seq, digest)
    }
}

pub(super) fn recorded_root(
    root: &Path,
    preparation: &Preparation,
) -> Result<Option<managed_history::Root>> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        format: Option<u64>,
        #[serde(rename = "configurationHistory")]
        history: Option<managed_history::Root>,
    }
    let bytes = match managed_history::read_bytes(&root.join(crate::managed::FILE), preparation) {
        Ok(bytes) => bytes,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    // Reuse the existing controlled metadata reader. This is a registry
    // envelope, not a format1 generation and not a canonical subset of it.
    let allowance = preparation.room();
    let _held = preparation.reserve(allowance)?;
    let options = HistoryOptions {
        cancel: Some(preparation.control().cancel.flag()),
        deadline: preparation.control().deadline,
    };
    let mut budget = cold::ColdBudget::new(ReadLimit {
        options: &options,
        memory: Some(allowance),
        held: 0,
    })?;
    let envelope = budget.json::<Envelope>(&bytes.value)?;
    let format = envelope.value.format.unwrap_or(1);
    if !matches!(format, 1..=3) {
        return Err(Error::Unsupported(format!(
            "primary configuration registry has format {format}; this release reads 1 through 3"
        )));
    }
    if (format == 3) != envelope.value.history.is_some() {
        return Err(Error::Unsupported(
            "inconsistent primary configuration root envelope".into(),
        ));
    }
    if let Some(history) = &envelope.value.history {
        history.validate()?;
    }
    Ok(envelope.value.history)
}
