//! Reversible close of the existing Own(Store)/Branch(Arc<Store>) graph.
use super::{Dataset, DatasetLifetime, DatasetState, RefusedClose, StoreHandle};
use crate::store::{
    Store, StoreOptions,
    lifecycle::{FamilyClose, FrozenWeak},
};
use std::{collections::BTreeMap, sync::Arc};

struct Shell {
    lifetime: Arc<DatasetLifetime>,
    alias: Option<Arc<str>>,
}
struct Node {
    state: Arc<DatasetState>,
    frozen: Option<FrozenWeak<DatasetState>>,
    store: Option<usize>,
}
struct Edge {
    parent: usize,
    name: String,
    child: usize,
    shell: Shell,
}
struct Closing {
    // Root remains present throughout reversible work, including unwinding.
    root: Option<Node>,
    root_address: usize,
    nodes: BTreeMap<usize, Node>,
    edges: Vec<Edge>,
    shell: Option<Shell>,
    stores: Vec<Arc<Store>>,
    parking: Option<Arc<Store>>,
    finished: bool,
}
impl Closing {
    fn new(dataset: Dataset) -> Self {
        let Dataset {
            _lifetime,
            inner,
            alias,
        } = dataset;
        let root_address = Arc::as_ptr(&inner) as usize;
        Self {
            root: Some(Node {
                state: inner,
                frozen: None,
                store: None,
            }),
            root_address,
            nodes: BTreeMap::new(),
            edges: Vec::new(),
            shell: Some(Shell {
                lifetime: _lifetime,
                alias,
            }),
            stores: Vec::new(),
            parking: None,
            finished: false,
        }
    }
    fn node(&self, address: usize) -> &Node {
        if address == self.root_address {
            self.root.as_ref().expect("reversible root")
        } else {
            &self.nodes[&address]
        }
    }
    fn node_mut(&mut self, address: usize) -> &mut Node {
        if address == self.root_address {
            self.root.as_mut().expect("reversible root")
        } else {
            self.nodes.get_mut(&address).expect("node")
        }
    }
    fn add(&mut self, dataset: Dataset) -> (usize, Shell) {
        let Dataset {
            _lifetime,
            inner,
            alias,
        } = dataset;
        let address = Arc::as_ptr(&inner) as usize;
        if address != self.root_address {
            self.nodes.entry(address).or_insert(Node {
                state: inner,
                frozen: None,
                store: None,
            });
        }
        (
            address,
            Shell {
                lifetime: _lifetime,
                alias,
            },
        )
    }
    fn prepare(&mut self) -> bool {
        if !matches!(
            &self.root.as_ref().expect("root").state.store,
            StoreHandle::Own(_)
        ) {
            return false;
        }
        let mut pending = vec![self.root_address];
        while let Some(address) = pending.pop() {
            if self.node(address).frozen.is_some() {
                continue;
            }
            let Ok(frozen) = self.node(address).state.reference.link.freeze() else {
                return false;
            };
            self.node_mut(address).frozen = Some(frozen);
            let branches = {
                let Some(mut cache) = self.node(address).state.branches.try_lock() else {
                    return false;
                };
                std::mem::take(&mut *cache)
            };
            for (name, dataset) in branches {
                let (child, shell) = self.add(dataset);
                self.edges.push(Edge {
                    parent: address,
                    name,
                    child,
                    shell,
                });
                pending.push(child);
            }
        }
        if Arc::get_mut(&mut self.root.as_mut().expect("root").state).is_none() {
            return false;
        }
        for node in self.nodes.values_mut() {
            let Some(state) = Arc::get_mut(&mut node.state) else {
                return false;
            };
            if matches!(&state.store, StoreHandle::Own(_)) {
                return false;
            }
        }
        if !self.nodes.is_empty() {
            // A genuine bounded Store used only as an inaccessible reservation.
            self.parking = Some(Arc::new(Store::in_memory(StoreOptions {
                cache_bytes: 0,
                result_cache_bytes: 0,
                service_cache_bytes: 0,
                history_cache_bytes: 0,
                memory_commit_ring: 1,
                ..Default::default()
            })));
        }
        for node in self.nodes.values_mut() {
            let state = Arc::get_mut(&mut node.state).expect("proven exclusive state");
            let StoreHandle::Branch(slot) = &mut state.store else {
                unreachable!("branch state")
            };
            let original = std::mem::replace(slot, self.parking.as_ref().expect("parking").clone());
            node.store = Some(Arc::as_ptr(&original) as usize);
            self.stores.push(original);
        }
        true
    }
    fn restore_slots(
        nodes: &mut BTreeMap<usize, Node>,
        stores: &[Arc<Store>],
        core: Option<&FamilyClose<'_>>,
    ) {
        for node in nodes.values_mut() {
            if let Some(address) = node.store.take() {
                let original = match core {
                    Some(core) => core.restored_store(address),
                    None => stores
                        .iter()
                        .find(|store| Arc::as_ptr(store) as usize == address)
                        .expect("original owner")
                        .clone(),
                };
                let state =
                    Arc::get_mut(&mut node.state).expect("reserved state remains exclusive");
                let StoreHandle::Branch(slot) = &mut state.store else {
                    unreachable!("original variant")
                };
                *slot = original;
            }
        }
    }
    fn rollback(&mut self) -> Dataset {
        Self::restore_slots(&mut self.nodes, &self.stores, None);
        self.stores.clear();
        for edge in std::mem::take(&mut self.edges) {
            let dataset = Dataset {
                _lifetime: edge.shell.lifetime,
                inner: self.node(edge.child).state.clone(),
                alias: edge.shell.alias,
            };
            self.node(edge.parent)
                .state
                .branches
                .lock()
                .insert(edge.name, dataset);
        }
        for node in self.nodes.values_mut() {
            if let Some(frozen) = node.frozen.take() {
                frozen.restore(&node.state);
            }
        }
        let root = self.root.as_mut().expect("reversible root");
        if let Some(frozen) = root.frozen.take() {
            frozen.restore(&root.state);
        }
        let shell = self.shell.take().expect("root shell");
        self.finished = true;
        Dataset {
            _lifetime: shell.lifetime,
            inner: root.state.clone(),
            alias: shell.alias,
        }
    }
    fn refused(&mut self, panic: Option<Box<dyn std::any::Any + Send>>) -> RefusedClose {
        RefusedClose {
            dataset: self.rollback(),
            panic,
        }
    }
    fn close(
        mut self,
        release_entries: impl FnOnce(),
    ) -> std::result::Result<Option<crate::reasoning::ReasoningRecord>, RefusedClose> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.prepare())) {
            Ok(true) => {}
            Ok(false) => return Err(self.refused(None)),
            Err(panic) => return Err(self.refused(Some(panic))),
        }
        let root = self.root.as_mut().expect("root stays present");
        let state = Arc::get_mut(&mut root.state).expect("exclusive root state");
        let reasoning = state.reasoning.get_mut().clone();
        let StoreHandle::Own(main) = &mut state.store else {
            unreachable!("main owns its Store")
        };
        let mut core = FamilyClose::new(main, std::mem::take(&mut self.stores));
        let reserved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| core.reserve()));
        if !matches!(&reserved, Ok(true)) {
            core.restore_core();
            Self::restore_slots(&mut self.nodes, &self.stores, Some(&core));
            core.finish_rollback();
            // Every canonical owner now has its actual normal owning edge.
            // Unmatched detached inventory was never upgraded into this bundle.
            drop(core);
            return Err(self.refused(reserved.err()));
        }
        core.commit();
        self.finished = true;
        root.frozen.take().expect("frozen root").close();
        for node in self.nodes.values_mut() {
            node.frozen.take().expect("frozen state").close();
        }
        Self::restore_slots(&mut self.nodes, &self.stores, Some(&core));
        release_entries();
        let (_lease, mut panic) = core.finish_commit();
        drop(self.edges.drain(..));
        drop(self.shell.take());
        for (_, node) in std::mem::take(&mut self.nodes) {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(node)));
            if panic.is_none() {
                panic = result.err();
            }
        }
        let root = self.root.take().expect("committed root");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(root)));
        if panic.is_none() {
            panic = result.err();
        }
        self.parking = None;
        if let Some(payload) = panic {
            std::panic::resume_unwind(payload);
        }
        Ok(reasoning)
    }
}
impl Drop for Closing {
    fn drop(&mut self) {
        if !self.finished {
            drop(self.rollback());
        }
    }
}
pub(super) fn close(
    dataset: Dataset,
    release_entries: impl FnOnce(),
) -> std::result::Result<Option<crate::reasoning::ReasoningRecord>, RefusedClose> {
    Closing::new(dataset).close(release_entries)
}
