//! Semantic actions: a registry of handlers by extension IRI. The Test extension
//! (`fail("msg")`, `print(s|p|o|"lit")`) is built in; actions of any other extension
//! succeed without running and are counted for a warning.

use crate::ast::SemAct;
use crate::error::todo;
use rustc_hash::FxHashMap;
use sparkles::id::Id;
use sparkles::store::Snapshot;

/// The extension IRI of the built-in Test extension.
pub const TEST_EXTENSION: &str = "http://shex.io/extensions/Test/";

/// The semantic-action handlers of a validation.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    /// keep the Test extension's `print` output
    pub trace: bool,
}

/// The state of semantic actions while matching: what `print` wrote and which unknown
/// extensions were skipped.
pub struct ActCtx<'a> {
    pub registry: &'a Registry,
    pub snap: &'a Snapshot,
    /// `print` output, in order (with [`Registry::trace`])
    pub prints: Vec<String>,
    /// actions not run, per unknown extension IRI
    pub unknown: FxHashMap<String, usize>,
}

impl<'a> ActCtx<'a> {
    pub fn new(registry: &'a Registry, snap: &'a Snapshot) -> ActCtx<'a> {
        ActCtx {
            registry,
            snap,
            prints: Vec::new(),
            unknown: FxHashMap::default(),
        }
    }
}

impl Registry {
    pub fn new(trace: bool) -> Registry {
        Registry { trace }
    }

    /// Can any of `acts` fail (so matching must enumerate assignments for them)?
    pub fn can_fail(&self, acts: &[SemAct]) -> bool {
        acts.iter().any(|a| a.name == TEST_EXTENSION)
    }

    /// Run the start actions, once per validation; `false`: an action failed.
    pub fn start(&self, acts: &[SemAct], cx: &mut ActCtx<'_>) -> anyhow::Result<bool> {
        let _ = (acts, cx);
        Err(todo("semantic actions"))
    }

    /// Run a shape's actions on a node that matches it.
    pub fn on_shape(
        &self,
        acts: &[SemAct],
        focus: Id,
        cx: &mut ActCtx<'_>,
    ) -> anyhow::Result<bool> {
        let _ = (acts, focus, cx);
        Err(todo("semantic actions"))
    }

    /// Run a triple constraint's actions on one triple matched to it.
    pub fn on_tc(
        &self,
        acts: &[SemAct],
        triple: [Id; 3],
        cx: &mut ActCtx<'_>,
    ) -> anyhow::Result<bool> {
        let _ = (acts, triple, cx);
        Err(todo("semantic actions"))
    }

    /// Run a group's (EachOf, OneOf) actions on the triples matched by the group.
    pub fn on_group(
        &self,
        acts: &[SemAct],
        triples: &[[Id; 3]],
        cx: &mut ActCtx<'_>,
    ) -> anyhow::Result<bool> {
        let _ = (acts, triples, cx);
        Err(todo("semantic actions"))
    }
}
