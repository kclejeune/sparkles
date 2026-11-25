//! `reasoning=auto` (spec F17 §4): a query reads the materialized inferences for the
//! patterns they can answer cheaply and derives the others on demand.
//!
//! [`AutoReasoner`] wraps the dataset's query-time reasoner. When a query starts, it
//! decides for the snapshot the query reads whether the materialized inferences can
//! answer at all: the materialization must have the query-time profile, rules and input
//! graphs, be fresh at the snapshot's commit (C08's freshness), and be readable by the
//! caller. A pattern then reads the materialization unless a backward rule lies in its
//! cone, and derives otherwise (`sparkles_core::sparql::reasoning::choose`). With no
//! backward rule in the program, as under `rdfs` and `owl-rl`, the query starts no
//! session at all ([`ReasoningProvider::reads_all`]). A stale materialization is never read
//! unless the dataset's setting says `"stale": "allow"`, and then the response carries
//! `Sparkles-Inferences`.
//!
//! [`Limited`] wraps either reasoner with the setting's `maxMillis` budget.

use crate::Dataset;
use crate::reasoning::freshness;
use sparkles_core::error::{ComponentError, Error, Result};
use sparkles_core::sparql::reasoning::{
    Demand, DemandInfo, Derived, Materialization, ReasoningProvider, ReasoningSession, Report,
    SessionRequest,
};
use sparkles_reasoner::query::QueryReasoner;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The provider of `reasoning=auto` for one dataset.
pub struct AutoReasoner {
    inner: QueryReasoner,
    dataset: Dataset,
    /// read a stale materialization and say so (spec F17 §4.2)
    stale_allow: bool,
}

impl AutoReasoner {
    /// `auto` over `inner`, the dataset's query-time reasoner, for `dataset`, whose
    /// materialization record and freshness decide what the inferred graph can answer.
    pub fn new(inner: QueryReasoner, dataset: Dataset, stale_allow: bool) -> AutoReasoner {
        AutoReasoner {
            inner,
            dataset,
            stale_allow,
        }
    }

    /// The query-time reasoner that derives what is not read.
    pub fn reasoner(&self) -> &QueryReasoner {
        &self.inner
    }

    /// Whether the materialized inferences can answer the query that `req` starts (spec
    /// F17 §4.1 and §4.2).
    fn materialization(&self, req: &SessionRequest) -> Materialization {
        let derive = |reason: String| Materialization::Derive { reason };
        let Some(record) = self.dataset.reasoning_record() else {
            return derive("no materialization".to_string());
        };
        let profile = self.inner.profile().name();
        if record.profile != profile {
            return derive(format!(
                "the materialization has the profile {} and the query {profile}",
                record.profile
            ));
        }
        let same_rules = match (record.profile(), record.extras()) {
            (Ok(p), Ok(e)) => sparkles_reasoner::rules_digest(&p, &e) == self.inner.digest(),
            _ => false,
        };
        if !same_rules {
            return derive("the materialization's rules differ from the query's".to_string());
        }
        if record.run_inputs().ok().as_ref() != Some(self.inner.inputs()) {
            return derive(
                "the materialization's input graphs differ from the query's".to_string(),
            );
        }
        // backward rules beside negation or fresh terms: the two sources do not combine
        if let Some(why) = self.inner.mixing_hazard() {
            return derive(why);
        }
        // a caller whose protections hide the inferences cannot read the graph (C12b)
        if let Some(readable) = &req.readable
            && !readable
                .read
                .allows_named_iri(crate::reasoning::INFERRED_GRAPH)
        {
            return derive("the caller cannot read the inferred graph".to_string());
        }
        let f = freshness(&record, self.dataset.store(), req.snap.commit);
        match f.stale {
            Some(false) => Materialization::Fresh,
            Some(true) | None => {
                let mut reason = if f.stale.is_some() {
                    "the materialization is stale".to_string()
                } else {
                    "the freshness of the materialization is unknown".to_string()
                };
                if let Some(why) = &f.reason {
                    reason.push_str(&format!(" ({why})"));
                }
                if self.stale_allow {
                    Materialization::StaleRead { reason }
                } else {
                    Materialization::Derive { reason }
                }
            }
        }
    }
}

impl ReasoningProvider for AutoReasoner {
    fn label(&self) -> String {
        format!("auto, profile {}", self.inner.profile().name())
    }

    fn auto(&self) -> bool {
        true
    }

    /// A materialization that can answer answers every pattern when the rules hold no
    /// backward rule, so the query starts no session.
    fn reads_all(&self, req: &SessionRequest) -> Option<Materialization> {
        if self.inner.has_backward_rules() {
            return None;
        }
        let m = self.materialization(req);
        matches!(
            m,
            Materialization::Fresh | Materialization::StaleRead { .. }
        )
        .then_some(m)
    }

    fn rewrites(&self) -> bool {
        self.inner.rewrites()
    }

    fn rewrite(
        &self,
        query: &spargebra::Query,
        bound: &[(String, oxrdf::Term)],
        req: &SessionRequest,
    ) -> Result<Option<sparkles_core::sparql::reasoning::Rewritten>> {
        self.inner.rewrite(query, bound, req)
    }

    fn rewrite_guarded(
        &self,
        query: &spargebra::Query,
        bound: &[(String, oxrdf::Term)],
        req: &SessionRequest,
    ) -> Result<Option<sparkles_core::sparql::reasoning::GuardedRewritten>> {
        self.inner.rewrite_guarded(query, bound, req)
    }

    fn start(&self, req: SessionRequest) -> Result<Box<dyn ReasoningSession>> {
        let materialization = self.materialization(&req);
        let inner = self.inner.start(req)?;
        Ok(Box::new(AutoSession {
            inner,
            materialization,
        }))
    }
}

/// The session of [`AutoReasoner`]: the query-time reasoner's, with the decision about
/// the materialized inferences.
struct AutoSession {
    inner: Box<dyn ReasoningSession>,
    materialization: Materialization,
}

impl ReasoningSession for AutoSession {
    fn intensional(
        &self,
        s: Option<&oxrdf::Term>,
        p: Option<&oxrdf::Term>,
        o: Option<&oxrdf::Term>,
    ) -> Result<bool> {
        self.inner.intensional(s, p, o)
    }

    fn prepare(&mut self, demands: &[Demand], estimates: bool) -> Result<Vec<DemandInfo>> {
        self.inner.prepare(demands, estimates)
    }

    fn derive(&mut self, seeds: Vec<(usize, Vec<u64>)>) -> Result<Derived> {
        self.inner.derive(seeds)
    }

    fn report(&self) -> Report {
        self.inner.report()
    }

    fn materialization(&self) -> Materialization {
        self.materialization.clone()
    }
}

/// A provider under the dataset setting's `budget.maxMillis` (spec F17 §2.2): a
/// derivation that runs longer fails with `422` and the code `reasoning-budget`, as a
/// derivation past the other limits does (§8). The clock starts when the query's
/// session starts.
pub struct Limited {
    inner: Arc<dyn ReasoningProvider>,
    max: Duration,
}

impl Limited {
    /// `inner` with at most `max_millis` of derivation per query.
    pub fn new(inner: Arc<dyn ReasoningProvider>, max_millis: u64) -> Limited {
        Limited {
            inner,
            max: Duration::from_millis(max_millis),
        }
    }
}

impl Limited {
    /// `stop`, which also fails once `max` has passed from now.
    fn limit(
        &self,
        stop: Arc<dyn Fn() -> Result<()> + Send + Sync>,
    ) -> Arc<dyn Fn() -> Result<()> + Send + Sync> {
        let started = Instant::now();
        let max = self.max;
        Arc::new(move || -> Result<()> {
            stop()?;
            if started.elapsed() > max {
                return Err(Error::Component(Box::new(ComponentError::new(
                    "reasoner",
                    sparkles_reasoner::query::BUDGET_CODE,
                    format!(
                        "query-time reasoning passed the dataset's budget of {} ms of derivation",
                        max.as_millis()
                    ),
                ))));
            }
            Ok(())
        })
    }
}

impl ReasoningProvider for Limited {
    fn label(&self) -> String {
        self.inner.label()
    }

    fn auto(&self) -> bool {
        self.inner.auto()
    }

    fn reads_all(&self, req: &SessionRequest) -> Option<Materialization> {
        self.inner.reads_all(req)
    }

    fn rewrites(&self) -> bool {
        self.inner.rewrites()
    }

    fn rewrite(
        &self,
        query: &spargebra::Query,
        bound: &[(String, oxrdf::Term)],
        req: &SessionRequest,
    ) -> Result<Option<sparkles_core::sparql::reasoning::Rewritten>> {
        // the OWL 2 QL rewriting reads the terminology and can grow, so it has the same
        // allowance as a derivation
        let req = SessionRequest {
            snap: req.snap.clone(),
            readable: req.readable.clone(),
            stop: self.limit(req.stop.clone()),
            max_memory: req.max_memory,
            memory_parent: req.memory_parent.clone(),
            no_cache: req.no_cache,
        };
        let rewritten = self.inner.rewrite(query, bound, &req)?;
        // the terminology's parsing has no checks of its own, so the allowance is checked
        // once more before the rewritten query is planned
        (req.stop)()?;
        Ok(rewritten)
    }

    fn rewrite_guarded(
        &self,
        query: &spargebra::Query,
        bound: &[(String, oxrdf::Term)],
        req: &SessionRequest,
    ) -> Result<Option<sparkles_core::sparql::reasoning::GuardedRewritten>> {
        let req = SessionRequest {
            snap: req.snap.clone(),
            readable: req.readable.clone(),
            stop: self.limit(req.stop.clone()),
            max_memory: req.max_memory,
            memory_parent: req.memory_parent.clone(),
            no_cache: req.no_cache,
        };
        let rewritten = self.inner.rewrite_guarded(query, bound, &req)?;
        (req.stop)()?;
        Ok(rewritten)
    }

    fn start(&self, mut req: SessionRequest) -> Result<Box<dyn ReasoningSession>> {
        req.stop = self.limit(req.stop.clone());
        self.inner.start(req)
    }
}
