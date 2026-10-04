//! The blank node table of P04 §3.2. A stored blank node's own label is `b<hex>`, which
//! names it in every read and write. Any other label that Jena made is mapped here to
//! the stored node it became when the write that used it committed, in both directions,
//! so later patterns and writes with the label reach the node and reads return the node
//! with that label.

use oxrdf::BlankNode;
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use sparkles::id::Id;
use std::sync::Arc;

/// Labels to stored nodes and back.
#[derive(Default)]
pub struct LabelTable {
    by_label: FxHashMap<Box<str>, Id>,
    by_id: FxHashMap<u64, Arc<str>>,
}

impl LabelTable {
    pub fn insert(&mut self, label: &str, id: Id) {
        let l: Arc<str> = Arc::from(label);
        self.by_label.insert(Box::from(label), id);
        self.by_id.insert(id.0, l);
    }

    pub fn get(&self, label: &str) -> Option<Id> {
        self.by_label.get(label).copied()
    }

    pub fn label_of(&self, id: Id) -> Option<Arc<str>> {
        self.by_id.get(&id.0).cloned()
    }

    pub fn len(&self) -> usize {
        self.by_label.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_label.is_empty()
    }

    /// Move every entry of `other` into this table.
    pub fn merge(&mut self, other: &LabelTable) {
        for (l, id) in &other.by_label {
            self.insert(l, *id);
        }
    }
}

/// The tables a read or write consults: the dataset's (absent with
/// `BlankNodeLabels.TRANSACTION`) and the open write transaction's own, which holds the
/// labels its writes used and is merged into the dataset's when it commits.
#[derive(Clone, Default)]
pub struct Labels {
    pub dataset: Option<Arc<RwLock<LabelTable>>>,
    pub txn: Option<Arc<RwLock<LabelTable>>>,
}

impl Labels {
    /// Run `f` with the two lookups, under read locks of both tables: `resolve` maps a
    /// label from Jena to the stored node it names, and `label` maps a stored node's
    /// label to the label Jena knows it by.
    pub fn with<R>(
        &self,
        f: impl FnOnce(&dyn Fn(&str) -> Option<BlankNode>, &dyn Fn(&str) -> Option<Arc<str>>) -> R,
    ) -> R {
        let d = self.dataset.as_ref().map(|t| t.read());
        let t = self.txn.as_ref().map(|t| t.read());
        let empty =
            d.as_ref().is_none_or(|d| d.is_empty()) && t.as_ref().is_none_or(|t| t.is_empty());
        if empty {
            return f(&|_| None, &|_| None);
        }
        let resolve = |l: &str| -> Option<BlankNode> {
            t.as_ref()
                .and_then(|t| t.get(l))
                .or_else(|| d.as_ref().and_then(|d| d.get(l)))
                .map(sparkles::store::bnode_for)
        };
        let label = |stored: &str| -> Option<Arc<str>> {
            let id = sparkles::store::parse_bnode_label(stored)?;
            t.as_ref()
                .and_then(|t| t.label_of(id))
                .or_else(|| d.as_ref().and_then(|d| d.label_of(id)))
        };
        f(&resolve, &label)
    }
}
