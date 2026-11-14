//! Authentication at the cold physical-proof boundary. Only a fixed scalar
//! publication header survives each synchronous metadata preparation.
use super::*;
use crate::access::Preparation;
use crate::store::configuration_history::primary::Header;

pub(super) struct PrimaryReader {
    root: std::path::PathBuf,
    header: Header,
}

/// The caller's cold buffers remain charged and unchanged during this loan.
/// No payload, allocation or reservation escapes the synchronous check. The
/// scalar header itself is charged separately in the parent ColdBudget.
fn preparation(budget: &ColdBudget<'_>) -> Result<Preparation> {
    budget.owned_limit()?.options.temporary(budget.available())
}

impl PrimaryReader {
    pub(super) fn for_store(store: &Store, budget: &mut ColdBudget<'_>) -> Result<Self> {
        let before = budget.used();
        let length = store
            .root
            .as_ref()
            .map_or(0, |root| root.as_os_str().len() as u64);
        budget.charge(length.saturating_mul(8).saturating_add(2048))?;
        let result = (|| {
            let root = store.managed_root().ok_or_else(|| {
                Error::HistoryUnsupported("primary history needs its persistent family".into())
            })?;
            let state = store.managed_state_shared();
            let origin = store.branching.ident.as_ref().and_then(|identity| {
                identity
                    .configuration_origin
                    .as_ref()
                    .map(|origin| (identity.from.seq, origin))
            });
            if let Some((seq, origin)) = origin {
                origin.validate(store.owner_dataset_id(), seq)?;
            }
            let temporary = preparation(budget)?;
            let header = Header::from_state(
                &root,
                store.owner_dataset_id(),
                store.dataset_id,
                origin.map(|(seq, origin)| (seq, origin.binding)),
                state.as_deref(),
                &temporary,
            )?;
            let retained = (root.as_os_str().len() as u64)
                .saturating_mul(4)
                .saturating_add(std::mem::size_of::<Self>() as u64)
                .saturating_add(256);
            Ok((Self { root, header }, retained))
        })();
        match result {
            Ok((value, retained)) => {
                budget.release(budget.used() - before - retained);
                Ok(value)
            }
            Err(error) => {
                budget.release(budget.used() - before);
                Err(error)
            }
        }
    }

    pub(super) fn load(root: &Path, id: uuid::Uuid, budget: &mut ColdBudget<'_>) -> Result<Self> {
        let before = budget.used();
        budget.charge(
            (root.as_os_str().len() as u64)
                .saturating_mul(8)
                .saturating_add(2048),
        )?;
        let result = (|| {
            let family_root = super::super::super::link::dataset_root_of(root)?;
            let family = commit::read_dataset_file_controlled(&family_root, budget)?
                .ok_or_else(|| Error::Corrupt("primary family identity is missing".into()))?;
            let bytes = budget.read_file(&root.join(BRANCH_FILE))?;
            let mut raw: Loaded<serde_json::Value> = budget.json(&bytes)?;
            crate::formats::check_settings(BRANCH_FILE, &raw.value)?;
            if let Some(object) = raw.value.as_object_mut() {
                object.entry("format").or_insert(1.into());
            }
            let descriptor: BranchFile = budget.value(raw.value)?;
            descriptor.check_configuration(&bytes)?;
            if descriptor.dataset_id != family || descriptor.id != id {
                return Err(Error::Corrupt(
                    "primary branch/family identity differs".into(),
                ));
            }
            let origin = descriptor
                .configuration_origin
                .as_ref()
                .map(|origin| (descriptor.from.seq, origin.binding));
            let temporary = preparation(budget)?;
            let header = Header::on_disk(&family_root, family, id, origin, &temporary)?;
            drop(temporary);
            let retained = (family_root.as_os_str().len() as u64)
                .saturating_mul(4)
                .saturating_add(std::mem::size_of::<Self>() as u64)
                .saturating_add(256);
            Ok((
                Self {
                    root: family_root,
                    header,
                },
                retained,
            ))
        })();
        match result {
            Ok((value, retained)) => {
                budget.release(budget.used() - before - retained);
                Ok(value)
            }
            Err(error) => {
                budget.release(budget.used() - before);
                Err(error)
            }
        }
    }

    pub(super) fn generation(
        &mut self,
        generation: &Current,
        budget: &ColdBudget<'_>,
    ) -> Result<()> {
        let temporary = preparation(budget)?;
        self.header.generation(
            &self.root,
            &temporary,
            generation.base.seq,
            generation.configuration,
        )
    }

    pub(super) fn observation(
        &mut self,
        observation: wal::probe::WalCommitObservation,
        budget: &ColdBudget<'_>,
    ) -> Result<()> {
        budget.check()?;
        if self.header.cached_observation(observation)? {
            return Ok(());
        }
        let temporary = preparation(budget)?;
        self.header.observation(&self.root, &temporary, observation)
    }
}
