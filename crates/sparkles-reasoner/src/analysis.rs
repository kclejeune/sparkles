//! Request-local semantic witnesses and generalized diagnostics for isolated analyses.
//!
//! These controls are opt-in. Ordinary materializations keep their allocation and
//! caching policy. An analysis reserves one unused blank range in its private copies.

use crate::ParentHandle;
use crate::budget::{Budget, BudgetFailure, Held};
use crate::diagnostics::LiteralTypings;
use crate::terms::Terms;
use oxrdf::{Literal, Term};
use sparkles_core::id::Id;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug)]
struct Witnesses {
    keys: BTreeMap<(Arc<str>, Vec<String>), u64>,
    programs: BTreeSet<Arc<str>>,
    next: u64,
    /// Declared last so `keys` and `programs` drop before returning witness credit.
    held: Held,
}

/// The bounded witness namespace shared by every state of one analysis request.
#[derive(Debug)]
pub struct WitnessMap {
    ordinal: u16,
    state: Mutex<Witnesses>,
    now: Term,
    cancel: Arc<AtomicBool>,
    deadline: Option<Instant>,
    /// Declared last so `state` and its `Held` drop before parent credit returns.
    budget: Arc<Budget>,
}

impl WitnessMap {
    pub fn new(
        ordinal: u16,
        max_memory: Option<usize>,
        cancel: Arc<AtomicBool>,
        deadline: Option<Instant>,
    ) -> Arc<Self> {
        Self::with_memory_parent(ordinal, max_memory, None, cancel, deadline)
    }

    pub fn with_memory_parent(
        ordinal: u16,
        max_memory: Option<usize>,
        memory_parent: Option<ParentHandle>,
        cancel: Arc<AtomicBool>,
        deadline: Option<Instant>,
    ) -> Arc<Self> {
        let budget = Budget::with_parent(max_memory, memory_parent);
        let held = Held::new(&budget);
        Arc::new(Self {
            ordinal,
            state: Mutex::new(Witnesses {
                keys: BTreeMap::new(),
                programs: BTreeSet::new(),
                next: sparkles_core::branch::bnode_range_start(ordinal),
                held,
            }),
            now: Literal::new_typed_literal(
                oxsdatatypes::DateTime::now().to_string(),
                oxrdf::vocab::xsd::DATE_TIME,
            )
            .into(),
            cancel,
            deadline,
            budget,
        })
    }

    pub fn ordinal(&self) -> u16 {
        self.ordinal
    }
}

/// A single closure's bounded diagnostics, with the request's witness identities.
#[derive(Debug)]
pub struct AnalysisContext {
    witnesses: Arc<WitnessMap>,
    pub(crate) now: Term,
    program: Arc<str>,
    typings: Mutex<Option<(LiteralTypings, Held)>>,
    /// Declared last so `typings` and its `Held` drop before parent credit returns.
    typing_budget: Arc<Budget>,
}

impl AnalysisContext {
    pub fn new(
        witnesses: Arc<WitnessMap>,
        max_typing_memory: Option<usize>,
        program: String,
    ) -> anyhow::Result<Arc<Self>> {
        let program = {
            let mut state = witnesses.state.lock().unwrap();
            if let Some(program) = state.programs.get(program.as_str()) {
                program.clone()
            } else {
                state.held.try_grow(128 + program.capacity())?;
                let program: Arc<str> = program.into();
                state.programs.insert(program.clone());
                program
            }
        };
        let typing_budget = Budget::with_parent(max_typing_memory, witnesses.budget.parent());
        Ok(Arc::new(Self {
            now: witnesses.now.clone(),
            program,
            witnesses,
            typings: Mutex::new(None),
            typing_budget,
        }))
    }

    pub(crate) fn check(&self) -> Result<(), BudgetFailure> {
        self.witnesses.budget.check_peak()?;
        if self.witnesses.cancel.load(Ordering::Relaxed)
            || self
                .witnesses
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(BudgetFailure::local(
                self.witnesses.budget.peak(),
                " while stopping an entailment analysis".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn witness(&self, key: Vec<String>) -> Result<u64, BudgetFailure> {
        self.allocate(key)
    }

    pub(crate) fn allocate(&self, key: Vec<String>) -> Result<u64, BudgetFailure> {
        let fail = |message: &str| {
            BudgetFailure::local(self.witnesses.budget.peak(), format!(" while {message}"))
        };
        if self.witnesses.cancel.load(Ordering::Relaxed)
            || self
                .witnesses
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(fail("stopping an entailment analysis"));
        }
        let mut state = self.witnesses.state.lock().unwrap();
        let key = (self.program.clone(), key);
        if let Some(id) = state.keys.get(&key) {
            return Ok(*id);
        }
        if sparkles_core::branch::bnode_ordinal(state.next) != self.witnesses.ordinal as u64 {
            return Err(fail("exhausting the analysis witness range"));
        }
        let bytes = 192
            + key.1.capacity() * std::mem::size_of::<String>()
            + key.1.iter().map(String::capacity).sum::<usize>();
        state.held.try_grow(bytes)?;
        let id = Id::bnode(state.next).0;
        state.next += 1;
        state.keys.insert(key, id);
        Ok(id)
    }

    pub(crate) fn capture_typings(
        &self,
        terms: &Terms,
        facts: &rustc_hash::FxHashSet<[u64; 3]>,
    ) -> Result<(), BudgetFailure> {
        let mut held = Held::new(&self.typing_budget);
        let mut found = Vec::new();
        for triple in facts {
            if self.witnesses.cancel.load(Ordering::Relaxed)
                || self
                    .witnesses
                    .deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
            {
                return Err(BudgetFailure::local(
                    self.typing_budget.peak(),
                    " while stopping diagnostic capture".into(),
                ));
            }
            if let (Some(subject), Some(predicate), Some(object)) = (
                terms.term(triple[0]),
                terms.term(triple[1]),
                terms.term(triple[2]),
            ) && let Some((literal, datatype)) =
                LiteralTypings::violation(&subject, &predicate, &object)
            {
                held.try_grow(128 + literal.to_string().len() * 2 + datatype.as_str().len() * 2)?;
                found.push((literal, datatype));
            }
        }
        let previous = self.typings.lock().unwrap().replace((
            LiteralTypings {
                commit: 0,
                facts: found,
            },
            held,
        ));
        drop(previous);
        Ok(())
    }

    /// Consume the summary independently of optional closure caching.
    pub fn take_literal_typings(&self, commit: u64) -> Option<LiteralTypings> {
        self.take_literal_typings_guarded(commit)
            .map(crate::diagnostics::GuardedLiteralTypings::into_typings)
    }

    /// Consume the summary independently of optional closure caching, carrying its credit
    /// guard until the returned wrapper drops.
    pub fn take_literal_typings_guarded(
        &self,
        commit: u64,
    ) -> Option<crate::diagnostics::GuardedLiteralTypings> {
        let taken = self.typings.lock().unwrap().take();
        taken.map(|(mut typings, held)| {
            typings.commit = commit;
            crate::diagnostics::GuardedLiteralTypings::new(typings, Some(Arc::new(held)))
        })
    }
}
