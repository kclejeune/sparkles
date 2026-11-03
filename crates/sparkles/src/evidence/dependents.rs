//! The dependents of changed evidence records (spec C25 §6 and F22 §5).
//!
//! An assessment depends on the record it evaluates, its `ev:target`, and on every
//! record it names as `prov:used`. When a claim is withdrawn, a decision is superseded,
//! a support ends or a resolution map gets a new revision, the operation answers the
//! identities it changed. [`dependents`] finds the authenticated assessments that depend
//! on them, so that a maintained analysis can be marked for review or recomputation. It
//! decides nothing about the assessments' conclusions, which may well still hold.

use super::{State, caller_view, check_store, readable};
use crate::access::GraphAccess;
use crate::evidence::propose::{READS_BYTES, settings_of};
use crate::task::Control;
use crate::{Dataset, Error, Result};
use oxrdf::{NamedNode, Term};
use sparkles_core::evidence::RecordKind;
use sparkles_core::evidence::review::MAX_EVIDENCE;
use sparkles_core::evidence::write::Reads;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

const PROV_USED: &str = "http://www.w3.org/ns/prov#used";

/// The most identities one request names.
pub const MAX_CHANGED: usize = 1000;

/// How an assessment depends on a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dependence {
    /// The record is the assessment's `ev:target`.
    Target,
    /// The assessment names the record as `prov:used`.
    Used,
}

impl Dependence {
    pub fn name(self) -> &'static str {
        match self {
            Dependence::Target => "target",
            Dependence::Used => "used",
        }
    }
}

/// One assessment that depends on a changed record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependent {
    pub assessment: NamedNode,
    pub graph: NamedNode,
    /// The record it depends on: a changed identity, or for a transitive dependent the
    /// assessment it depends on.
    pub on: NamedNode,
    pub dependence: Dependence,
    /// How many assessments lie between it and a changed identity, 0 for a direct one.
    pub depth: u32,
}

/// The outcome of [`dependents`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependents {
    /// The commit the records were read at.
    pub commit: u64,
    pub dependents: Vec<Dependent>,
}

/// The authenticated assessments in the proposal graphs the caller can read that
/// depend on any of `changed`, at the head of `logical`, which is `main` or one of its
/// branches. With `transitive`, assessments that depend on a dependent are found too.
/// `access` is what the caller may read there, `None` for everything. A graph the
/// caller cannot read is not searched, and a record whose triples it cannot see is not
/// reported, so the answer discloses nothing hidden. Each dependent is reported once,
/// at its smallest depth, sorted by depth and IRI.
///
/// Refusals: invalid input, `unsupported-profile` when the profile is not enabled, and
/// `incomplete-evidence` when more than [`MAX_EVIDENCE`] candidates would be read.
pub fn dependents(
    main: &Dataset,
    logical: &Dataset,
    changed: &[NamedNode],
    transitive: bool,
    access: Option<Arc<GraphAccess>>,
    control: &Control,
) -> Result<Dependents> {
    control.check()?;
    check_store(main, logical)?;
    if changed.is_empty() || changed.len() > MAX_CHANGED {
        return Err(Error::invalid(format!(
            "changed: a request names 1 to {MAX_CHANGED} identities"
        )));
    }
    let (settings, _) = settings_of(main)?;
    let registry = main.store().managed_snapshot()?;
    let nonce = |g: &str| {
        registry.as_ref().and_then(|s| {
            s.graphs
                .owners
                .get(&crate::managed::OwnerKind::Evidence)
                .filter(|e| e.graphs.iter().any(|x| x == g))
                .and_then(|e| e.protections.get(g).copied())
        })
    };
    let access = access.or_else(|| Some(Arc::new(GraphAccess::all())));
    let graphs: Vec<(NamedNode, Option<uuid::Uuid>)> = settings
        .proposals
        .iter()
        .filter_map(|g| NamedNode::new(g.as_str()).ok())
        .filter(|g| readable(access.as_ref(), g).is_ok())
        .map(|g| {
            let n = nonce(g.as_str());
            (g, n)
        })
        .collect();
    let head = logical.store().snapshot();
    let preparation = crate::access::Preparation::new(control.clone(), READS_BYTES)?;
    let view = caller_view(access.as_ref(), &head, &preparation)?;
    let snap = view.as_ref().map_or(&*head, |v| v.snapshot());
    let mut state = State {
        reads: Reads::new(&preparation, snap),
        seen: None,
        claim_graphs: Vec::new(),
        superseded: BTreeSet::new(),
        withdrawn: BTreeSet::new(),
    };
    let mut found: BTreeMap<NamedNode, Dependent> = BTreeMap::new();
    let mut queue: VecDeque<(NamedNode, u32)> = changed.iter().map(|c| (c.clone(), 0)).collect();
    let mut asked: BTreeSet<NamedNode> = changed.iter().cloned().collect();
    let mut read = 0usize;
    while let Some((on, depth)) = queue.pop_front() {
        control.check()?;
        for g in &graphs {
            for (p, dependence) in [
                (super::ev("target"), Dependence::Target),
                (PROV_USED.to_string(), Dependence::Used),
            ] {
                let (candidates, more) = state.candidates(&p, &Term::NamedNode(on.clone()), g)?;
                read += candidates.len();
                if more || read > MAX_EVIDENCE {
                    return Err(super::incomplete("the dependents of the changed records"));
                }
                for a in candidates {
                    if found.contains_key(&a) || !state.typed(&a, g, RecordKind::Assessment)? {
                        continue;
                    }
                    found.insert(
                        a.clone(),
                        Dependent {
                            assessment: a.clone(),
                            graph: g.0.clone(),
                            on: on.clone(),
                            dependence,
                            depth,
                        },
                    );
                    if transitive && asked.insert(a.clone()) {
                        queue.push_back((a, depth + 1));
                    }
                }
            }
        }
    }
    let mut dependents: Vec<Dependent> = found.into_values().collect();
    dependents.sort_by(|a, b| (a.depth, &a.assessment).cmp(&(b.depth, &b.assessment)));
    Ok(Dependents {
        commit: head.commit,
        dependents,
    })
}
