//! Request-local semantic witnesses and generalized diagnostics for isolated analyses.
//!
//! These controls are opt-in. Ordinary materializations keep their allocation and
//! caching policy. An analysis reserves one unused blank range in its private copies.

use crate::budget::{Budget, MemoryExceeded};
use crate::diagnostics::LiteralTypings;
use crate::terms::Terms;
use oxrdf::{Literal, Term};
use sparkles_core::id::Id;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

#[derive(Debug)]
struct Witnesses {
    keys: BTreeMap<(Arc<str>, Vec<String>), u64>,
    programs: BTreeSet<Arc<str>>,
    next: u64,
}

/// The bounded witness namespace shared by every state of one analysis request.
#[derive(Debug)]
pub struct WitnessMap {
    ordinal: u16,
    state: Mutex<Witnesses>,
    budget: Arc<Budget>,
    now: Term,
    cancel: Arc<AtomicBool>,
    deadline: Option<Instant>,
}

impl WitnessMap {
    pub fn new(
        ordinal: u16,
        max_memory: Option<usize>,
        cancel: Arc<AtomicBool>,
        deadline: Option<Instant>,
    ) -> Arc<Self> {
        Arc::new(Self {
            ordinal,
            state: Mutex::new(Witnesses {
                keys: BTreeMap::new(),
                programs: BTreeSet::new(),
                next: sparkles_core::branch::bnode_range_start(ordinal),
            }),
            budget: Budget::new(max_memory),
            now: Literal::new_typed_literal(
                oxsdatatypes::DateTime::now().to_string(),
                oxrdf::vocab::xsd::DATE_TIME,
            )
            .into(),
            cancel,
            deadline,
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
    typings: Mutex<Option<LiteralTypings>>,
    typing_budget: Arc<Budget>,
    failed: OnceLock<MemoryExceeded>,
}

impl AnalysisContext {
    pub fn new(
        witnesses: Arc<WitnessMap>,
        max_typing_memory: Option<usize>,
        program: String,
    ) -> Result<Arc<Self>, MemoryExceeded> {
        let program = {
            let mut state = witnesses.state.lock().unwrap();
            if let Some(program) = state.programs.get(program.as_str()) {
                program.clone()
            } else {
                witnesses.budget.try_charge(128 + program.capacity())?;
                let program: Arc<str> = program.into();
                state.programs.insert(program.clone());
                program
            }
        };
        Ok(Arc::new(Self {
            now: witnesses.now.clone(),
            program,
            witnesses,
            typings: Mutex::new(None),
            typing_budget: Budget::new(max_typing_memory),
            failed: OnceLock::new(),
        }))
    }

    pub(crate) fn check(&self) -> Result<(), MemoryExceeded> {
        if let Some(error) = self.failed.get() {
            return Err(error.clone());
        }
        self.witnesses.budget.check_peak()?;
        if self.witnesses.cancel.load(Ordering::Relaxed)
            || self
                .witnesses
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(MemoryExceeded {
                limit: self.witnesses.budget.peak(),
                context: " while stopping an entailment analysis".into(),
            });
        }
        Ok(())
    }

    pub(crate) fn fail(&self, error: MemoryExceeded) {
        let _ = self.failed.set(error);
    }

    pub(crate) fn witness(&self, key: Vec<String>) -> Result<u64, MemoryExceeded> {
        self.allocate(key)
    }

    pub(crate) fn allocate(&self, key: Vec<String>) -> Result<u64, MemoryExceeded> {
        let fail = |message: &str| MemoryExceeded {
            limit: self.witnesses.budget.peak(),
            context: format!(" while {message}"),
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
        let bytes = 192
            + key.1.capacity() * std::mem::size_of::<String>()
            + key.1.iter().map(String::capacity).sum::<usize>();
        self.witnesses.budget.try_charge(bytes)?;
        if sparkles_core::branch::bnode_ordinal(state.next) != self.witnesses.ordinal as u64 {
            return Err(fail("exhausting the analysis witness range"));
        }
        let id = Id::bnode(state.next).0;
        state.next += 1;
        state.keys.insert(key, id);
        Ok(id)
    }

    pub(crate) fn capture_typings(
        &self,
        terms: &Terms,
        facts: &rustc_hash::FxHashSet<[u64; 3]>,
    ) -> Result<(), MemoryExceeded> {
        let mut found = Vec::new();
        for triple in facts {
            if self.witnesses.cancel.load(Ordering::Relaxed)
                || self
                    .witnesses
                    .deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
            {
                return Err(MemoryExceeded {
                    limit: self.typing_budget.peak(),
                    context: " while stopping diagnostic capture".into(),
                });
            }
            if let (Some(subject), Some(predicate), Some(object)) = (
                terms.term(triple[0]),
                terms.term(triple[1]),
                terms.term(triple[2]),
            ) && let Some((literal, datatype)) =
                LiteralTypings::violation(&subject, &predicate, &object)
            {
                self.typing_budget.try_charge(
                    128 + literal.to_string().len() * 2 + datatype.as_str().len() * 2,
                )?;
                found.push((literal, datatype));
            }
        }
        *self.typings.lock().unwrap() = Some(LiteralTypings {
            commit: 0,
            facts: found,
        });
        Ok(())
    }

    /// Consume the summary independently of optional closure caching.
    pub fn take_literal_typings(&self, commit: u64) -> Option<LiteralTypings> {
        self.typings.lock().unwrap().take().map(|mut typings| {
            typings.commit = commit;
            typings
        })
    }
}
