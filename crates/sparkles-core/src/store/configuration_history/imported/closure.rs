//! Genuine prior-source metadata selection. A complete/hash-checked graph is
//! not a proof of physical base/ranges or current caller permission.
use super::*;
use crate::access::Reservation;
use crate::managed::ManagedState;
use std::sync::Arc;

pub(in crate::store) struct Sources {
    encoded: managed_history::Bytes,
    pub(super) origin: managed_history::Owned<Origin>,
    pub(super) nodes: Vec<Node>,
    _held: Reservation,
}
pub(super) struct Node {
    pub(super) digest: Sha256Digest,
    encoded: managed_history::Bytes,
    pub(super) descriptor: managed_history::Owned<Descriptor>,
    pub(super) directory: documents::Opened,
}

impl Sources {
    pub(in crate::store) fn capture(
        store: &Store,
        writer: &crate::store::WriterGuard<'_>,
        state: &Option<Arc<ManagedState>>,
        preparation: &Preparation,
    ) -> Result<Option<Self>> {
        preparation.check()?;
        if !std::ptr::eq(&**writer, store.writer.data_ptr()) {
            return Err(Error::invalid(
                "prior sources need this Store's actual writer",
            ));
        }
        if writer.poisoned || writer.staged.is_some() {
            return Err(Error::Poisoned);
        }
        if !store.same_managed_state(state)
            || !matches!(state.as_deref(), Some(ManagedState::Installed(installed)) if installed.configuration_history.is_some())
        {
            return Err(managed_history::unavailable(
                "prior source configuration changed",
            ));
        }
        // There is no prior import namespace in today's genuine memory state.
        if store.root.is_none() {
            store.family.check_origin_selection(None)?;
            return Ok(None);
        }
        let options = crate::guard::WriteOptions {
            cancel: Some(preparation.control().cancel.flag()),
            deadline: preparation.control().deadline,
            ..Default::default()
        };
        let set = store
            .try_branch_set(&options)?
            .ok_or_else(|| managed_history::unavailable("prior source family is unavailable"))?;
        if set.is_memory() {
            return Err(managed_history::unavailable(
                "persistent prior sources have a memory family",
            ));
        }
        let sources = Self::load(set.root(), &store.family, preparation)?;
        if sources
            .as_ref()
            .is_some_and(|sources| sources.origin.value.dataset_id != store.owner_dataset_id())
        {
            return Err(managed_history::unavailable(
                "prior origin belongs to another live family",
            ));
        }
        if !store.same_managed_state(state) {
            return Err(Error::Conflict(
                "configuration changed while selecting prior sources".into(),
            ));
        }
        preparation.check()?;
        Ok(sources)
    }

    /// The root path and metadata cannot establish opening authority. The real
    /// Family must still own this exact process lock throughout selection.
    pub(super) fn opening(
        root: &Path,
        family: &crate::store::family::Family,
        lock: &Arc<crate::store::ProcessLock>,
        preparation: &Preparation,
    ) -> Result<Option<Self>> {
        family.verify_origin_lock(root, lock)?;
        let sources = Self::load(root, family, preparation)?;
        family.verify_origin_lock(root, lock)?;
        preparation.check()?;
        Ok(sources)
    }

    fn load(
        root: &Path,
        actual_family: &crate::store::family::Family,
        preparation: &Preparation,
    ) -> Result<Option<Self>> {
        let selected = opening_origin_selection(root, preparation)?;
        actual_family.check_origin_selection(selected)?;
        if selected.is_none() {
            preparation.check()?;
            return Ok(None);
        }
        let mut held = preparation.reserve(std::mem::size_of::<Self>() as u64 + 512)?;
        let _paths = preparation.reserve(
            (root.as_os_str().len() as u64)
                .checked_mul(8)
                .and_then(|n| n.checked_add(4096))
                .ok_or_else(|| preparation.exhausted())?,
        )?;
        let family = documents::Opened::root(root, preparation)?;
        let history = family.child("evidence-history", true, preparation)?;
        let root = match history.child("origin.json", false, preparation) {
            Ok(root) => root,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                match history.child("imports", true, preparation) {
                    Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                        actual_family.check_origin_selection(None)?;
                        preparation.check()?;
                        return Ok(None);
                    }
                    Err(error) => return Err(error),
                    Ok(_) => {
                        return Err(managed_history::unavailable(
                            "prior imports survive without their origin root",
                        ));
                    }
                }
            }
            Err(error) => return Err(error),
        };
        let encoded = root.bytes(None, preparation)?;
        let digest = Sha256Digest::of_bytes(&encoded.value, preparation.control())?;
        if selected != Some(digest) {
            return Err(managed_history::unavailable(
                "origin changed during original source selection",
            ));
        }
        actual_family.check_origin_selection(selected)?;
        let origin = Origin::decode(&encoded.value, preparation)?;
        let imports = history.child("imports", true, preparation)?;
        let count = origin.value.sources.len();
        let bytes = count
            .checked_mul(std::mem::size_of::<Node>() + 128)
            .and_then(|n| n.checked_mul(2))
            .ok_or_else(|| preparation.exhausted())?;
        held.grow_to(
            (std::mem::size_of::<Self>() as u64 + 512)
                .checked_add(bytes as u64)
                .ok_or_else(|| preparation.exhausted())?,
        )?;
        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(count)
            .map_err(|_| preparation.exhausted())?;
        for digest in &origin.value.sources {
            preparation.check()?;
            let text = digest.to_string();
            let name = text.strip_prefix("sha256:").expect("canonical digest");
            let directory = imports.child(name, true, preparation)?;
            let encoded = directory
                .child("origin.json", false, preparation)?
                .bytes(None, preparation)?;
            if Sha256Digest::of_bytes(&encoded.value, preparation.control())? != *digest {
                return Err(managed_history::unavailable(
                    "prior source descriptor digest differs",
                ));
            }
            let descriptor = Descriptor::decode(&encoded.value, preparation)?;
            for path in descriptor.value.files.keys() {
                drop(documents::read_member(
                    &directory,
                    &descriptor.value,
                    path,
                    preparation,
                )?);
            }
            documents::source_identities(&directory, &descriptor.value, preparation)?;
            let registry = documents::read_member(
                &directory,
                &descriptor.value,
                crate::managed::FILE,
                preparation,
            )?;
            let publication = primary::recorded_root_bytes(&registry.value, preparation)?
                .ok_or_else(|| {
                    managed_history::unavailable("prior source publication is missing")
                })?;
            drop(registry);
            drop(documents::source_publication(
                &directory,
                &descriptor.value,
                publication.identity(descriptor.value.family_id),
                preparation,
            )?);
            nodes.push(Node {
                digest: *digest,
                encoded,
                descriptor,
                directory,
            });
        }
        acyclic(
            &origin.value.sources,
            |index| &nodes[index].descriptor.value.sources,
            preparation,
        )?;
        actual_family.check_origin_selection(selected)?;
        preparation.check()?;
        Ok(Some(Self {
            encoded,
            origin,
            nodes,
            _held: held,
        }))
    }

    pub(in crate::store) fn source_count(&self) -> usize {
        self.nodes.len()
    }

    /// Only a declared, hash-pinned original small member, never a raw RDF or
    /// snapshot extractor. The copied bytes have the receiving request's owner.
    pub(in crate::store) fn read(
        &self,
        source: Sha256Digest,
        path: &str,
        preparation: &Preparation,
    ) -> Result<managed_history::Bytes> {
        preparation.check()?;
        let index = self
            .nodes
            .binary_search_by(|node| node.digest.as_bytes().cmp(source.as_bytes()))
            .map_err(|_| {
                managed_history::unavailable("original source is not in the captured graph")
            })?;
        let node = &self.nodes[index];
        documents::read_member(&node.directory, &node.descriptor.value, path, preparation)
    }
}

fn acyclic<'a>(
    sources: &[Sha256Digest],
    dependencies: impl Fn(usize) -> &'a [Sha256Digest],
    preparation: &Preparation,
) -> Result<()> {
    preparation.check()?;
    if !sorted_sources(sources) {
        return Err(managed_history::unavailable(
            "prior sources are not unique and sorted",
        ));
    }
    let count = sources.len();
    let bytes = count
        .checked_mul(std::mem::size_of::<(usize, usize)>() + 1)
        .and_then(|n| n.checked_mul(2))
        .and_then(|n| n.checked_add(1024))
        .ok_or_else(|| preparation.exhausted())?;
    let _held = preparation.reserve(bytes as u64)?;
    let mut colors = Vec::new();
    colors
        .try_reserve_exact(count)
        .map_err(|_| preparation.exhausted())?;
    colors.resize(count, 0u8);
    let mut stack = Vec::new();
    stack
        .try_reserve_exact(count)
        .map_err(|_| preparation.exhausted())?;
    for start in 0..count {
        preparation.check()?;
        if colors[start] == 2 {
            continue;
        }
        colors[start] = 1;
        stack.push((start, 0usize));
        while let Some((index, next)) = stack.last_mut() {
            preparation.check()?;
            let edges = dependencies(*index);
            if *next == edges.len() {
                colors[*index] = 2;
                stack.pop();
                continue;
            }
            let digest = edges[*next];
            *next += 1;
            let target = sources
                .binary_search_by(|id| id.as_bytes().cmp(digest.as_bytes()))
                .map_err(|_| managed_history::unavailable("prior source dependency is missing"))?;
            match colors[target] {
                1 => {
                    return Err(managed_history::unavailable(
                        "prior source dependency cycle",
                    ));
                }
                0 => {
                    colors[target] = 1;
                    stack.push((target, 0));
                }
                _ => {}
            }
        }
    }
    preparation.check()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ids() -> Vec<Sha256Digest> {
        (0..4)
            .map(|n| Sha256Digest::from_digest_bytes([n; 32]))
            .collect()
    }
    #[test]
    fn prior_source_graph_requires_complete_unique_acyclic_dependencies() {
        let preparation = Preparation::new(crate::task::Control::none(), 64 << 10).unwrap();
        let before = preparation.room();
        let ids = ids();
        let edges = [vec![ids[1], ids[2]], vec![ids[3]], vec![ids[3]], vec![]];
        acyclic(&ids, |n| &edges[n], &preparation).unwrap();
        assert_eq!(preparation.room(), before);
        for edges in [
            [vec![ids[0]], vec![], vec![], vec![]],
            [vec![ids[1]], vec![ids[2]], vec![ids[0]], vec![]],
            [
                vec![Sha256Digest::from_digest_bytes([9; 32])],
                vec![],
                vec![],
                vec![],
            ],
        ] {
            assert!(acyclic(&ids, |n| &edges[n], &preparation).is_err());
            assert_eq!(preparation.room(), before);
        }
        assert!(acyclic(&[ids[0], ids[0]], |_| &[], &preparation).is_err());
        assert!(acyclic(&[ids[1], ids[0]], |_| &[], &preparation).is_err());
        assert_eq!(preparation.room(), before);
    }
    #[test]
    fn prior_source_graph_admits_work_storage_and_preserves_typed_controls() {
        let ids = ids();
        let preparation = Preparation::new(crate::task::Control::none(), 512).unwrap();
        let before = preparation.room();
        assert!(matches!(
            acyclic(
                &ids,
                |_| panic!("graph storage must be admitted first"),
                &preparation
            ),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(preparation.room(), before);
        let control = crate::task::Control::none();
        let preparation = Preparation::new(control.clone(), 64 << 10).unwrap();
        let before = preparation.room();
        control.cancel.cancel();
        assert!(matches!(
            acyclic(
                &ids,
                |_| panic!("cancel before graph traversal"),
                &preparation
            ),
            Err(Error::Cancelled)
        ));
        assert_eq!(preparation.room(), before);
    }
}
