//! The typed transitions of the ledger: submission, claim, renewal, publication,
//! failure, cancellation, retry, inspection and the boot-epoch recovery.
//!
//! Each transition opens one transaction under the jobs owner's capability, reads its
//! preconditions from the transaction's base snapshot, stages its changes and commits.
//! See C26 §7.1 for why this ordering gives the acceptance cases.

use super::ledger::{
    self, Attempt, AttemptState, Changes, Ledger, Run, RunView, State, attempt_iri, epoch_iri,
    integer, ms, now_ms, operation_iri, publication_iri, run_iri, string,
};
use super::{Caller, Jobs, managed};
use crate::access::Preparation;
use crate::branch::CommitRef;
use crate::managed::{ManagedErrorKind, OwnerKind};
use crate::store::WriteTxn;
use crate::task::Control;
use crate::{Error, Result};
use oxrdf::{GraphName, Quad, Term};
use sparkles_core::jobs::{
    Definition, LEDGER_GRAPH, MAX_IDEMPOTENCY_KEY_BYTES, Settings, backoff_ms, canonical_json,
    digest_hex, new_token, operation_id, token_matches,
};
use std::sync::Arc;
use uuid::Uuid;

/// The longest retry delay, one hour.
const MAX_BACKOFF_MS: u64 = 3_600_000;

/// The default and largest page of [`Jobs::list`].
pub const DEFAULT_LIST_LIMIT: usize = 100;
pub const MAX_LIST_LIMIT: usize = 1000;

/// A run to submit (spec C26 §3).
#[derive(Clone, Debug)]
pub struct Submission {
    /// The registered definition's name.
    pub definition: String,
    /// The typed payload the worker receives at its claim. It must hold no secret.
    pub payload: serde_json::Value,
    /// Scoped to the dataset, the branch, the principal and the definition revision.
    pub idempotency_key: String,
    /// The source checkpoint the run starts from, as the worker understands it.
    pub checkpoint: Option<String>,
    /// Commits the run reads, pinned by a source hold until the run is terminal.
    pub inputs: Vec<CommitRef>,
    /// The commit message of the submission.
    pub message: Option<String>,
    /// The terminal run this submission retries, set by [`Jobs::retry`].
    pub retry_of: Option<Uuid>,
}

impl Submission {
    pub fn new(definition: &str, payload: serde_json::Value, idempotency_key: &str) -> Submission {
        Submission {
            definition: definition.into(),
            payload,
            idempotency_key: idempotency_key.into(),
            checkpoint: None,
            inputs: Vec::new(),
            message: None,
            retry_of: None,
        }
    }
}

/// The durable receipt of a submission.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Submitted {
    pub run: Uuid,
    pub version: u64,
    /// The commit that recorded the run, or the head when the key was replayed.
    pub commit: u64,
    /// Whether the key was known with the same payload, so no run was queued again.
    pub replayed: bool,
    pub state: State,
}

/// A claimed run and its lease (spec C26 §4.2). The token is returned once and never
/// recorded.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Claimed {
    pub run: Uuid,
    pub attempt: Uuid,
    pub number: u32,
    pub generation: u64,
    pub version: u64,
    pub token: String,
    pub lease_expires_ms: i64,
    pub definition: String,
    pub kind: String,
    pub kind_version: u32,
    pub payload: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
    pub inputs: Vec<CommitRef>,
    pub graphs: Vec<String>,
}

/// A renewed lease.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Renewed {
    pub run: Uuid,
    pub version: u64,
    pub lease_expires_ms: i64,
}

/// The typed result of a `quads` run (spec C26 §4.3).
#[derive(Clone, Debug)]
pub struct Publication {
    /// The publication operation id, chosen by the worker and kept across retries.
    pub operation: Uuid,
    pub expected_version: Option<u64>,
    pub insert: Vec<Quad>,
    pub delete: Vec<Quad>,
    pub checkpoint: Option<String>,
    pub message: Option<String>,
}

impl Publication {
    pub fn new(operation: Uuid) -> Publication {
        Publication {
            operation,
            expected_version: None,
            insert: Vec::new(),
            delete: Vec::new(),
            checkpoint: None,
            message: None,
        }
    }

    /// `sha256:` and the digest of the publication's content: its sorted insert and
    /// delete quads and its checkpoint. Order does not change it.
    pub fn digest(&self) -> String {
        let mut lines: Vec<String> = self
            .insert
            .iter()
            .map(|q| format!("+{q}"))
            .chain(self.delete.iter().map(|q| format!("-{q}")))
            .collect();
        lines.sort();
        let mut bytes = lines.join("\n").into_bytes();
        bytes.push(0);
        if let Some(c) = &self.checkpoint {
            bytes.extend_from_slice(c.as_bytes());
        }
        digest_hex(&bytes)
    }
}

/// The receipt of a publication.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Published {
    pub run: Uuid,
    pub publication: Uuid,
    pub commit: u64,
    pub version: u64,
    pub inserted: u64,
    pub deleted: u64,
    /// Whether the operation id was completed before with the same digest, so this is
    /// the original receipt.
    pub replayed: bool,
}

/// A typed failure of an attempt (spec C26 §4.4).
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    /// Whether another attempt may succeed. A permanent failure ends the run.
    #[serde(default)]
    pub transient: bool,
    /// A diagnostic the worker has redacted. It is cut to 512 characters.
    #[serde(default)]
    pub reason: String,
    /// The least delay before the next attempt, such as a source's `Retry-After`.
    #[serde(default)]
    pub retry_after_ms: Option<u64>,
}

/// A filter of [`Jobs::list`].
#[derive(Clone, Debug, Default)]
pub struct ListFilter {
    pub state: Option<State>,
    pub definition: Option<String>,
    pub limit: Option<usize>,
}

/// What a boot-epoch recovery did.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recovered {
    pub epoch: Uuid,
    /// Whether this call recorded the epoch, or an earlier call of this process had.
    pub installed: bool,
    /// The runs whose attempts of an earlier epoch were interrupted.
    pub interrupted: Vec<Uuid>,
}

fn lease_lost(why: &str) -> Error {
    managed(ManagedErrorKind::LeaseLost, format!("lease-lost: {why}"))
}

fn not_found(id: Uuid) -> Error {
    Error::NotFound(format!("run {id} is not known or not visible"))
}

fn preparation() -> Result<Preparation> {
    Preparation::new(Control::none(), ledger::READS_BYTES)
}

/// The retry decision after a failed or interrupted attempt.
fn after_failure(
    run: &Run,
    def: Option<&Definition>,
    now: i64,
    transient: bool,
    retry_after_ms: Option<u64>,
) -> (State, Option<i64>, Option<String>) {
    if !transient {
        return (State::Failed, None, None);
    }
    let Some(def) = def else {
        return (
            State::Failed,
            None,
            Some("the definition is no longer registered".into()),
        );
    };
    if run.attempts >= def.attempts {
        return (
            State::Failed,
            None,
            Some(format!("{} attempts were exhausted", def.attempts)),
        );
    }
    let delay = backoff_ms(run.attempts, MAX_BACKOFF_MS).max(retry_after_ms.unwrap_or(0));
    let next = now.saturating_add(delay as i64);
    let horizon = run
        .submitted_ms
        .saturating_add(def.retry_horizon_ms.min(i64::MAX as u64) as i64);
    if next > horizon {
        return (State::Failed, None, Some("the retry horizon passed".into()));
    }
    (State::RetryWait, Some(next), None)
}

/// A transaction under the jobs capability, with the recorded settings and the
/// ledger graph's protection nonce.
struct Begun<'s> {
    t: WriteTxn<'s>,
    settings: Settings,
    protection: Uuid,
}

impl Jobs {
    fn begin<'s>(
        &'s self,
        operation: Uuid,
        write: Option<Arc<crate::access::GraphAccess>>,
        message: Option<String>,
    ) -> Result<Begun<'s>> {
        let ds = self.main()?;
        let (settings, bytes) = self.enabled_settings()?;
        let store = ds.store();
        let capability = store.managed_writer(OwnerKind::Jobs, operation)?;
        if capability.settings().as_deref() != Some(&bytes[..]) {
            return Err(Error::Conflict(
                "the jobs settings changed while the operation was prepared; retry".into(),
            ));
        }
        let protection = capability.protection(LEDGER_GRAPH).ok_or_else(|| {
            managed(
                ManagedErrorKind::UnsupportedProfile,
                "the job ledger graph is not protected; record the jobs settings again",
            )
        })?;
        let opts = crate::guard::WriteOptions {
            message: message.map(Arc::from),
            graphs: write,
            ..Default::default()
        };
        let mut t = store.try_write_with(crate::commit::CommitKind::Transaction, opts)?;
        t.set_managed(capability)?;
        Ok(Begun {
            t,
            settings,
            protection,
        })
    }

    fn authentic(&self, run: &Run, protection: Uuid) -> Result<()> {
        if run.protection != Some(protection) {
            return Err(Error::Corrupt(format!(
                "run {} was not written under the ledger's current protection; dispatch is refused",
                run.id
            )));
        }
        Ok(())
    }

    /// Record this process's boot epoch and interrupt the attempts of earlier epochs,
    /// once per open of the dataset, before any claim, renewal or publication of this
    /// process is admitted (spec C26 §4.2 and §7).
    pub fn recover(&self) -> Result<Recovered> {
        let rt = self.runtime();
        if rt.installed() {
            return Ok(Recovered {
                epoch: rt.epoch(),
                installed: false,
                interrupted: Vec::new(),
            });
        }
        let _installing = rt.installing.lock();
        if rt.installed() {
            return Ok(Recovered {
                epoch: rt.epoch(),
                installed: false,
                interrupted: Vec::new(),
            });
        }
        let begun = self.begin(Uuid::new_v4(), None, Some("jobs: boot epoch".into()))?;
        let Begun {
            mut t,
            settings,
            protection,
        } = begun;
        let base = t.base().clone();
        let prep = preparation()?;
        let mut l = Ledger::new(&prep, &base);
        if l.epoch()? == Some(rt.epoch()) {
            rt.mark_installed();
            return Ok(Recovered {
                epoch: rt.epoch(),
                installed: false,
                interrupted: Vec::new(),
            });
        }
        let now = now_ms();
        let mut ch = Changes::default();
        let mut interrupted = Vec::new();
        for id in l.runs_in(State::Leased)? {
            let Some(run) = l.run(id)? else { continue };
            self.authentic(&run, protection)?;
            let Some(attempt) = run.current_attempt else {
                continue;
            };
            let Some(attempt) = l.attempt(attempt)? else {
                continue;
            };
            if attempt.epoch == rt.epoch() {
                continue;
            }
            let a = attempt_iri(attempt.id);
            ch.set(
                &mut l,
                &a,
                "attemptState",
                Some(string(AttemptState::Interrupted.as_str())),
            )?;
            ch.set(
                &mut l,
                &a,
                "diagnostic",
                Some(string("interrupted by a restart of the dataset's process")),
            )?;
            let def = settings.definitions.get(&run.definition);
            let (state, next, reason) = after_failure(&run, def, now, true, None);
            self.transition(&mut ch, &mut l, &run, state, next, reason)?;
            rt.revoke(run.id);
            interrupted.push(run.id);
        }
        let e = epoch_iri();
        ch.set(&mut l, &e, "epoch", Some(string(&rt.epoch().to_string())))?;
        ch.set(&mut l, &e, "installedAtMs", Some(ms(now)))?;
        drop(l);
        ch.apply(&mut t)?;
        t.commit()?;
        rt.mark_installed();
        for id in &interrupted {
            self.maybe_release_hold(*id);
        }
        Ok(Recovered {
            epoch: rt.epoch(),
            installed: true,
            interrupted,
        })
    }

    fn ensure_epoch(&self) -> Result<()> {
        self.recover().map(|_| ())
    }

    /// Stage a run's state change: state, version, next eligible time and reason.
    fn transition(
        &self,
        ch: &mut Changes,
        l: &mut Ledger<'_>,
        run: &Run,
        state: State,
        next_eligible_ms: Option<i64>,
        reason: Option<String>,
    ) -> Result<()> {
        let r = run_iri(run.id);
        ch.set(l, &r, "state", Some(string(state.as_str())))?;
        ch.set(l, &r, "version", Some(integer(run.version + 1)))?;
        ch.set(l, &r, "nextEligibleAtMs", next_eligible_ms.map(ms))?;
        if let Some(reason) = reason {
            ch.set(l, &r, "reason", Some(string(&ledger::redacted(&reason))))?;
        }
        Ok(())
    }

    fn maybe_release_hold(&self, run: Uuid) {
        if let Err(e) = self.ds.store().release_holds(OwnerKind::Jobs, &[run]) {
            tracing::warn!(target: "sparkles::jobs", "the source hold of run {run} was not released: {e}");
        }
    }

    /// Whether any run is queued, leased or waiting to retry.
    pub(super) fn has_active_runs(&self) -> Result<bool> {
        let snap = self.main()?.store().snapshot();
        let prep = preparation()?;
        let mut l = Ledger::new(&prep, &snap);
        for st in [State::Queued, State::Leased, State::RetryWait] {
            if !l.runs_in(st)?.is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Submit a run. The same key with the same payload returns the existing run, a
    /// different payload is `idempotency-conflict`, and admission past the dataset's
    /// bound on queued runs is `jobs-overloaded` without storing a run.
    pub fn submit(&self, s: &Submission, caller: &Caller) -> Result<Submitted> {
        let ds = self.main()?;
        self.ensure_epoch()?;
        let key = s.idempotency_key.as_str();
        if key.is_empty() || key.len() > MAX_IDEMPOTENCY_KEY_BYTES {
            return Err(Error::invalid(format!(
                "idempotencyKey is empty or longer than {MAX_IDEMPOTENCY_KEY_BYTES} bytes"
            )));
        }
        let (settings, _) = self.enabled_settings()?;
        let def = settings.definitions.get(&s.definition).ok_or_else(|| {
            Error::NotFound(format!("definition {:?} is not registered", s.definition))
        })?;
        if !def.enabled {
            return Err(Error::Conflict(format!(
                "definition {:?} is disabled",
                s.definition
            )));
        }
        if !def.submitters.is_empty()
            && !def
                .submitters
                .iter()
                .any(|p| p == caller.principal.as_str())
        {
            return Err(Error::NotPermitted(format!(
                "{} may not submit runs of {:?}",
                caller.principal.as_str(),
                s.definition
            )));
        }
        let payload = canonical_json(&s.payload);
        if payload.len() as u64 > def.max_input_bytes {
            return Err(Error::invalid(format!(
                "the payload is larger than {} bytes",
                def.max_input_bytes
            )));
        }
        let payload = String::from_utf8(payload).expect("JSON is UTF-8");
        let digest = digest_hex(payload.as_bytes());
        let revision = def.revision();
        let store = ds.store();
        let op = operation_id(
            ds.dataset_id(),
            store.branch_id(),
            caller.principal.as_str(),
            &revision,
            key,
        );
        // A repeated key answers before anything is held or locked.
        {
            let snap = store.snapshot();
            let prep = preparation()?;
            let mut l = Ledger::new(&prep, &snap);
            if let Some(found) = l.operation(op)? {
                return self.replay(&mut l, found, &digest, snap.commit);
            }
        }
        let run = Uuid::new_v4();
        if !s.inputs.is_empty() {
            store
                .acquire_hold(OwnerKind::Jobs, run, &s.inputs)
                .map_err(|e| match e {
                    Error::HistoryGone(_) => Error::Conflict(
                        "an input commit is no longer retained, so the run cannot pin it".into(),
                    ),
                    e => e,
                })?;
            store.failpoint("jobs-held");
        }
        let Begun {
            mut t,
            settings,
            protection,
        } = self.begin(op, None, s.message.clone())?;
        let base = t.base().clone();
        let prep = preparation()?;
        let mut l = Ledger::new(&prep, &base);
        if let Some(found) = l.operation(op)? {
            return self.replay(&mut l, found, &digest, base.commit);
        }
        let mut waiting = l.runs_in(State::Queued)?.len();
        waiting += l.runs_in(State::RetryWait)?.len();
        if waiting >= settings.max_queued as usize {
            return Err(managed(
                ManagedErrorKind::JobsOverloaded,
                format!(
                    "{waiting} runs are waiting, the dataset's bound of {}; no run was stored",
                    settings.max_queued
                ),
            ));
        }
        let (seq, time_ms) = t.managed_stamp()?;
        let mut ch = Changes::default();
        let o = operation_iri(op);
        ch.add_type(&o, "Operation");
        ch.add(&o, "run", string(&run.to_string()));
        ch.add(&o, "payloadDigest", string(&digest));
        ch.add(&o, "idempotencyKey", string(key));
        ch.add(&o, "principal", Term::NamedNode(caller.principal.clone()));
        ch.add(&o, "definitionRevision", string(&revision));
        ch.add(&o, "graphProtection", string(&protection.to_string()));
        let r = run_iri(run);
        ch.add_type(&r, "Run");
        ch.add(&r, "definition", string(&s.definition));
        ch.add(&r, "definitionRevision", string(&revision));
        ch.add(&r, "kind", string(&def.kind));
        ch.add(&r, "kindVersion", integer(u64::from(def.version)));
        ch.add(&r, "principal", Term::NamedNode(caller.principal.clone()));
        ch.add(&r, "datasetId", string(&ds.dataset_id().to_string()));
        ch.add(&r, "branchId", string(&store.branch_id().to_string()));
        ch.add(&r, "payload", string(&payload));
        ch.add(&r, "payloadDigest", string(&digest));
        if let Some(c) = &s.checkpoint {
            ch.add(&r, "sourceCheckpoint", string(c));
        }
        for i in &s.inputs {
            ch.add(&r, "input", string(&format!("{}@{}", i.branch_id, i.seq)));
        }
        ch.add(&r, "submittedAtMs", ms(time_ms));
        ch.add(&r, "submittedCommit", integer(seq));
        ch.add(&r, "state", string(State::Queued.as_str()));
        ch.add(&r, "version", integer(1));
        ch.add(&r, "attempts", integer(0));
        ch.add(&r, "generation", integer(0));
        ch.add(&r, "operation", string(&op.to_string()));
        if let Some(of) = s.retry_of {
            ch.add(&r, "retryOf", string(&of.to_string()));
        }
        ch.add(&r, "graphProtection", string(&protection.to_string()));
        drop(l);
        ch.apply(&mut t)?;
        store.failpoint("jobs-before-commit");
        let receipt = t.commit()?;
        store.failpoint("jobs-committed");
        Ok(Submitted {
            run,
            version: 1,
            commit: receipt.commit.seq,
            replayed: false,
            state: State::Queued,
        })
    }

    fn replay(
        &self,
        l: &mut Ledger<'_>,
        (run, recorded): (Uuid, String),
        digest: &str,
        head: u64,
    ) -> Result<Submitted> {
        if recorded != digest {
            return Err(managed(
                ManagedErrorKind::IdempotencyConflict,
                "the idempotency key was used before with another payload",
            ));
        }
        let Some(run) = l.run(run)? else {
            return Err(Error::Corrupt(format!(
                "the submission operation names run {run}, which the ledger does not hold"
            )));
        };
        Ok(Submitted {
            run: run.id,
            version: run.version,
            commit: head,
            replayed: true,
            state: run.state,
        })
    }

    /// Claim one eligible run of the caller's permitted definitions, `definitions`
    /// restricting them further when it is not empty. Eligible runs are taken first
    /// submitted first. A run whose lease expired is taken over with a later
    /// generation, and one whose attempts are exhausted by that fails instead.
    pub fn claim(&self, definitions: &[String], caller: &Caller) -> Result<Option<Claimed>> {
        let ds = self.main()?;
        self.ensure_epoch()?;
        let rt = self.runtime();
        let Begun {
            mut t,
            settings,
            protection,
        } = self.begin(Uuid::new_v4(), None, Some("jobs: claim".into()))?;
        let base = t.base().clone();
        let prep = preparation()?;
        let mut l = Ledger::new(&prep, &base);
        let now = now_ms();
        let permitted = |def: &Definition, name: &str| {
            def.enabled
                && (definitions.is_empty() || definitions.iter().any(|d| d == name))
                && (def.workers.is_empty()
                    || def.workers.iter().any(|w| w == caller.principal.as_str()))
        };
        let mut candidates: Vec<Run> = Vec::new();
        let mut leased_by_definition: std::collections::HashMap<String, u32> = Default::default();
        for st in [State::Queued, State::RetryWait, State::Leased] {
            for id in l.runs_in(st)? {
                let Some(run) = l.run(id)? else { continue };
                self.authentic(&run, protection)?;
                let eligible = match run.state {
                    State::Queued => true,
                    State::RetryWait => run.next_eligible_ms.is_none_or(|n| n <= now),
                    State::Leased => {
                        if rt.lease_expired(run.id) {
                            true
                        } else {
                            *leased_by_definition
                                .entry(run.definition.clone())
                                .or_default() += 1;
                            false
                        }
                    }
                    _ => false,
                };
                if !eligible {
                    continue;
                }
                let Some(def) = settings.definitions.get(&run.definition) else {
                    continue;
                };
                if def.revision() != run.revision || !permitted(def, &run.definition) {
                    continue;
                }
                candidates.push(run);
            }
        }
        candidates.sort_by_key(|r| r.submitted_commit);
        let mut ch = Changes::default();
        let mut chosen = None;
        for run in candidates {
            let def = &settings.definitions[&run.definition];
            if run.attempts >= def.attempts {
                // an expired lease or a stale retry whose attempts ran out ends here
                if let Some(a) = run.current_attempt
                    && let Some(a) = l.attempt(a)?
                    && a.state == AttemptState::Active
                {
                    ch.set(
                        &mut l,
                        &attempt_iri(a.id),
                        "attemptState",
                        Some(string(AttemptState::Expired.as_str())),
                    )?;
                }
                self.transition(
                    &mut ch,
                    &mut l,
                    &run,
                    State::Failed,
                    None,
                    Some(format!("{} attempts were exhausted", def.attempts)),
                )?;
                rt.revoke(run.id);
                continue;
            }
            let leased = leased_by_definition
                .get(&run.definition)
                .copied()
                .unwrap_or(0);
            if leased >= def.concurrency {
                continue;
            }
            chosen = Some((run, def.clone()));
            break;
        }
        let Some((run, def)) = chosen else {
            drop(l);
            if !ch.is_empty() {
                ch.apply(&mut t)?;
                t.commit()?;
            }
            return Ok(None);
        };
        if let Some(previous) = run.current_attempt
            && let Some(previous) = l.attempt(previous)?
            && previous.state == AttemptState::Active
        {
            ch.set(
                &mut l,
                &attempt_iri(previous.id),
                "attemptState",
                Some(string(AttemptState::Expired.as_str())),
            )?;
        }
        let attempt = Uuid::new_v4();
        let (token, verifier) = new_token();
        let generation = run.generation + 1;
        let number = run.attempts + 1;
        let expires = now.saturating_add(def.lease_ms as i64);
        let r = run_iri(run.id);
        ch.set(&mut l, &r, "state", Some(string(State::Leased.as_str())))?;
        ch.set(&mut l, &r, "version", Some(integer(run.version + 1)))?;
        ch.set(&mut l, &r, "generation", Some(integer(generation)))?;
        ch.set(&mut l, &r, "attempts", Some(integer(u64::from(number))))?;
        ch.set(
            &mut l,
            &r,
            "currentAttempt",
            Some(string(&attempt.to_string())),
        )?;
        ch.set(&mut l, &r, "nextEligibleAtMs", None)?;
        let a = attempt_iri(attempt);
        ch.add_type(&a, "Attempt");
        ch.add(&a, "run", string(&run.id.to_string()));
        ch.add(&a, "worker", Term::NamedNode(caller.principal.clone()));
        ch.add(&a, "number", integer(u64::from(number)));
        ch.add(&a, "generation", integer(generation));
        ch.add(&a, "epoch", string(&rt.epoch().to_string()));
        ch.add(&a, "tokenVerifier", string(&verifier));
        ch.add(&a, "claimedAtMs", ms(now));
        ch.add(&a, "leaseExpiresAtMs", ms(expires));
        ch.add(&a, "attemptState", string(AttemptState::Active.as_str()));
        ch.add(&a, "graphProtection", string(&protection.to_string()));
        drop(l);
        ch.apply(&mut t)?;
        ds.store().failpoint("jobs-before-claim-commit");
        t.commit()?;
        rt.grant(run.id, def.lease_ms);
        let payload: serde_json::Value = serde_json::from_str(&run.payload).map_err(|e| {
            Error::Corrupt(format!("the payload of run {} is not JSON: {e}", run.id))
        })?;
        Ok(Some(Claimed {
            run: run.id,
            attempt,
            number,
            generation,
            version: run.version + 1,
            token,
            lease_expires_ms: expires,
            definition: run.definition,
            kind: run.kind,
            kind_version: run.kind_version,
            payload,
            checkpoint: run.checkpoint,
            inputs: run.inputs,
            graphs: def.graphs,
        }))
    }

    /// The run and its active attempt when `token` is the live lease of `run` held by
    /// the caller, or `lease-lost`.
    fn check_lease(
        &self,
        l: &mut Ledger<'_>,
        run: Uuid,
        token: &str,
        caller: &Caller,
        protection: Uuid,
    ) -> Result<(Run, Attempt)> {
        let rt = self.runtime();
        let Some(run) = l.run(run)? else {
            return Err(not_found(run));
        };
        self.authentic(&run, protection)?;
        if run.state != State::Leased {
            return Err(lease_lost(&format!("the run is {}", run.state.as_str())));
        }
        let Some(attempt) = run.current_attempt.and_then(|a| l.attempt(a).transpose()) else {
            return Err(lease_lost("the run has no active attempt"));
        };
        let attempt = attempt?;
        if attempt.run != run.id {
            return Err(Error::Corrupt(format!(
                "attempt {} names run {}, not run {}; dispatch is refused",
                attempt.id, attempt.run, run.id
            )));
        }
        if attempt.state != AttemptState::Active {
            return Err(lease_lost(&format!(
                "the attempt is {}",
                attempt.state.as_str()
            )));
        }
        if !token_matches(token, &attempt.verifier) || attempt.generation != run.generation {
            return Err(lease_lost("the token does not match the current attempt"));
        }
        if attempt.epoch != rt.epoch() {
            return Err(lease_lost("the lease was granted by an earlier process"));
        }
        if attempt.worker != caller.principal {
            return Err(lease_lost("the lease belongs to another worker"));
        }
        if rt.lease_expired(run.id) {
            return Err(lease_lost("the lease expired"));
        }
        Ok((run, attempt))
    }

    fn expect_version(run: &Run, expected: Option<u64>) -> Result<()> {
        if let Some(v) = expected
            && v != run.version
        {
            return Err(Error::PreconditionFailed(format!(
                "the run is at version {}, not {v}",
                run.version
            )));
        }
        Ok(())
    }

    /// Renew the lease of `run` by `extend_ms`, at most the definition's largest
    /// extension and by its lease length when `None`.
    pub fn renew(
        &self,
        run: Uuid,
        token: &str,
        extend_ms: Option<u64>,
        expected_version: Option<u64>,
        caller: &Caller,
    ) -> Result<Renewed> {
        self.ensure_epoch()?;
        let Begun {
            mut t,
            settings,
            protection,
        } = self.begin(Uuid::new_v4(), None, Some("jobs: renew".into()))?;
        let base = t.base().clone();
        let prep = preparation()?;
        let mut l = Ledger::new(&prep, &base);
        let (run, attempt) = self.check_lease(&mut l, run, token, caller, protection)?;
        Self::expect_version(&run, expected_version)?;
        let def = settings
            .definitions
            .get(&run.definition)
            .ok_or_else(|| lease_lost("the definition is no longer registered"))?;
        let extend = extend_ms
            .unwrap_or(def.lease_ms)
            .min(def.max_lease_extension_ms)
            .max(1);
        let now = now_ms();
        let expires = now.saturating_add(extend as i64);
        let mut ch = Changes::default();
        ch.set(
            &mut l,
            &attempt_iri(attempt.id),
            "leaseExpiresAtMs",
            Some(ms(expires)),
        )?;
        ch.set(
            &mut l,
            &run_iri(run.id),
            "version",
            Some(integer(run.version + 1)),
        )?;
        drop(l);
        ch.apply(&mut t)?;
        t.commit()?;
        self.runtime().grant(run.id, extend);
        Ok(Renewed {
            run: run.id,
            version: run.version + 1,
            lease_expires_ms: expires,
        })
    }

    /// Publish the typed result of `run` under its lease: the business quads, the
    /// publication record, the checkpoint and the completion state in one commit. A
    /// repeated operation id with the same digest returns the original receipt, even
    /// after the lease expired, and a different digest is `publication-conflict`.
    pub fn publish(
        &self,
        run_id: Uuid,
        token: &str,
        p: &Publication,
        caller: &Caller,
    ) -> Result<Published> {
        let ds = self.main()?;
        self.ensure_epoch()?;
        let digest = p.digest();
        {
            let snap = ds.store().snapshot();
            let prep = preparation()?;
            let mut l = Ledger::new(&prep, &snap);
            if let Some(found) = l.publication(p.operation)? {
                return Self::replay_publication(&mut l, &found, &digest);
            }
        }
        let Begun {
            mut t,
            settings,
            protection,
        } = self.begin(p.operation, caller.write.clone(), p.message.clone())?;
        let base = t.base().clone();
        let prep = preparation()?;
        let mut l = Ledger::new(&prep, &base);
        if let Some(found) = l.publication(p.operation)? {
            return Self::replay_publication(&mut l, &found, &digest);
        }
        let (run, attempt) = self.check_lease(&mut l, run_id, token, caller, protection)?;
        Self::expect_version(&run, p.expected_version)?;
        let def = settings
            .definitions
            .get(&run.definition)
            .ok_or_else(|| lease_lost("the definition is no longer registered"))?;
        let count = (p.insert.len() + p.delete.len()) as u64;
        if count > def.max_output_quads {
            return Err(Error::invalid(format!(
                "the result holds {count} quads, more than the definition's {}",
                def.max_output_quads
            )));
        }
        {
            let capability = t.managed().expect("capability set");
            for q in p.insert.iter().chain(&p.delete) {
                let GraphName::NamedNode(g) = &q.graph_name else {
                    return Err(Error::invalid(
                        "a publication's quads lie in named graphs of the definition",
                    ));
                };
                if !def.graphs.iter().any(|d| d == g.as_str()) {
                    return Err(Error::NotPermitted(format!(
                        "graph <{}> is not among the definition's graphs",
                        g.as_str()
                    )));
                }
                if capability.managed_by_any(g.as_str()) {
                    return Err(managed(
                        ManagedErrorKind::Protected,
                        format!(
                            "graph <{}> is managed by a profile; a publication cannot write it",
                            g.as_str()
                        ),
                    ));
                }
                if let Some(access) = &caller.write {
                    let term = Term::NamedNode(g.clone());
                    if !access.writable(Some(&term)) {
                        return Err(crate::access::GraphAccess::refused(Some(&term)));
                    }
                }
            }
        }
        let mut labels = std::collections::HashMap::new();
        for q in &p.delete {
            let ids = t.encode_quad(q, &mut labels)?;
            t.delete(ids)?;
        }
        for q in &p.insert {
            let ids = t.encode_quad(q, &mut labels)?;
            t.insert(ids)?;
        }
        let (seq, time_ms) = t.managed_stamp()?;
        let mut ch = Changes::default();
        let pb = publication_iri(p.operation);
        ch.add_type(&pb, "Publication");
        ch.add(&pb, "run", string(&run.id.to_string()));
        ch.add(&pb, "attempt", string(&attempt.id.to_string()));
        ch.add(&pb, "payloadDigest", string(&digest));
        ch.add(&pb, "commit", integer(seq));
        ch.add(&pb, "inserted", integer(p.insert.len() as u64));
        ch.add(&pb, "deleted", integer(p.delete.len() as u64));
        if let Some(c) = &p.checkpoint {
            ch.add(&pb, "sourceCheckpoint", string(c));
        }
        ch.add(&pb, "publishedAtMs", ms(time_ms));
        ch.add(&pb, "graphProtection", string(&protection.to_string()));
        let r = run_iri(run.id);
        self.transition(&mut ch, &mut l, &run, State::Succeeded, None, None)?;
        ch.set(
            &mut l,
            &r,
            "publication",
            Some(string(&p.operation.to_string())),
        )?;
        if let Some(c) = &p.checkpoint {
            ch.set(&mut l, &r, "sourceCheckpoint", Some(string(c)))?;
        }
        ch.set(
            &mut l,
            &attempt_iri(attempt.id),
            "attemptState",
            Some(string(AttemptState::Published.as_str())),
        )?;
        drop(l);
        ch.apply(&mut t)?;
        ds.store().failpoint("jobs-before-publish-commit");
        let receipt = t.commit()?;
        ds.store().failpoint("jobs-published");
        self.runtime().revoke(run.id);
        if !run.inputs.is_empty() {
            self.maybe_release_hold(run.id);
        }
        Ok(Published {
            run: run.id,
            publication: p.operation,
            commit: receipt.commit.seq,
            version: run.version + 1,
            inserted: p.insert.len() as u64,
            deleted: p.delete.len() as u64,
            replayed: false,
        })
    }

    fn replay_publication(
        l: &mut Ledger<'_>,
        found: &ledger::PublicationRecord,
        digest: &str,
    ) -> Result<Published> {
        if found.digest != digest {
            return Err(managed(
                ManagedErrorKind::PublicationConflict,
                "the publication operation id was completed before with another result",
            ));
        }
        let version = l.run(found.run)?.map_or(0, |r| r.version);
        Ok(Published {
            run: found.run,
            publication: found.id,
            commit: found.commit,
            version,
            inserted: found.inserted,
            deleted: found.deleted,
            replayed: true,
        })
    }

    /// Record a typed failure of the attempt under its lease. A transient failure
    /// enters `retry-wait` with exponential backoff within the attempts and the
    /// horizon, and any other failure ends the run.
    pub fn fail(
        &self,
        run_id: Uuid,
        token: &str,
        f: &Failure,
        expected_version: Option<u64>,
        caller: &Caller,
    ) -> Result<RunView> {
        self.ensure_epoch()?;
        let Begun {
            mut t,
            settings,
            protection,
        } = self.begin(Uuid::new_v4(), None, Some("jobs: failure".into()))?;
        let base = t.base().clone();
        let prep = preparation()?;
        let mut l = Ledger::new(&prep, &base);
        let (run, attempt) = self.check_lease(&mut l, run_id, token, caller, protection)?;
        Self::expect_version(&run, expected_version)?;
        let now = now_ms();
        let def = settings.definitions.get(&run.definition);
        let (state, next, why) = after_failure(&run, def, now, f.transient, f.retry_after_ms);
        let diagnostic = ledger::redacted(&f.reason);
        let mut ch = Changes::default();
        let a = attempt_iri(attempt.id);
        ch.set(
            &mut l,
            &a,
            "attemptState",
            Some(string(AttemptState::Failed.as_str())),
        )?;
        ch.set(
            &mut l,
            &a,
            "diagnostic",
            (!diagnostic.is_empty()).then(|| string(&diagnostic)),
        )?;
        let reason = match (&why, diagnostic.is_empty()) {
            (Some(w), false) => Some(format!("{diagnostic} ({w})")),
            (Some(w), true) => Some(w.clone()),
            (None, false) => Some(diagnostic.clone()),
            (None, true) => Some(String::new()),
        };
        self.transition(&mut ch, &mut l, &run, state, next, reason)?;
        drop(l);
        ch.apply(&mut t)?;
        t.commit()?;
        self.runtime().revoke(run.id);
        if state.terminal() && !run.inputs.is_empty() {
            self.maybe_release_hold(run.id);
        }
        self.get(run.id, caller)?.ok_or_else(|| not_found(run.id))
    }

    /// Durably cancel `run`: fence its active attempt and end the run. Work already
    /// committed stays committed, and an external request already sent may still take
    /// effect. The submitter or an administrator may cancel.
    pub fn cancel(&self, run_id: Uuid, caller: &Caller) -> Result<RunView> {
        self.ensure_epoch()?;
        let Begun {
            mut t,
            settings: _,
            protection,
        } = self.begin(Uuid::new_v4(), None, Some("jobs: cancel".into()))?;
        let base = t.base().clone();
        let prep = preparation()?;
        let mut l = Ledger::new(&prep, &base);
        let Some(run) = l.run(run_id)? else {
            return Err(not_found(run_id));
        };
        self.authentic(&run, protection)?;
        let attempt = match run.current_attempt {
            Some(a) => l.attempt(a)?,
            None => None,
        };
        if !visible(&run, attempt.as_ref(), caller)
            || !(caller.admin || run.principal == caller.principal)
        {
            return Err(not_found(run_id));
        }
        if run.state.terminal() {
            return Err(Error::Conflict(format!(
                "run {run_id} is {} and cannot be cancelled",
                run.state.as_str()
            )));
        }
        let mut ch = Changes::default();
        if let Some(a) = &attempt
            && a.state == AttemptState::Active
        {
            ch.set(
                &mut l,
                &attempt_iri(a.id),
                "attemptState",
                Some(string(AttemptState::Cancelled.as_str())),
            )?;
        }
        let uncertain = run.state == State::Leased;
        self.transition(
            &mut ch,
            &mut l,
            &run,
            State::Cancelled,
            None,
            Some(if uncertain {
                format!(
                    "cancelled by {} while leased; an external request already sent may still take effect",
                    caller.principal.as_str()
                )
            } else {
                format!("cancelled by {}", caller.principal.as_str())
            }),
        )?;
        drop(l);
        ch.apply(&mut t)?;
        t.commit()?;
        self.runtime().revoke(run.id);
        if !run.inputs.is_empty() {
            self.maybe_release_hold(run.id);
        }
        self.get(run.id, caller)?.ok_or_else(|| not_found(run.id))
    }

    /// Create a new run from a failed or cancelled one, with its payload, checkpoint and
    /// inputs, under a new idempotency key. The old run stays as evidence.
    pub fn retry(&self, run_id: Uuid, idempotency_key: &str, caller: &Caller) -> Result<Submitted> {
        let Some(view) = self.get(run_id, caller)? else {
            return Err(not_found(run_id));
        };
        if !matches!(view.state, State::Failed | State::Cancelled) {
            return Err(Error::Conflict(format!(
                "run {run_id} is {}; only a failed or cancelled run is retried",
                view.state.as_str()
            )));
        }
        let payload = {
            let snap = self.main()?.store().snapshot();
            let prep = preparation()?;
            let mut l = Ledger::new(&prep, &snap);
            let run = l.run(run_id)?.ok_or_else(|| not_found(run_id))?;
            serde_json::from_str(&run.payload).map_err(|e| {
                Error::Corrupt(format!("the payload of run {run_id} is not JSON: {e}"))
            })?
        };
        self.submit(
            &Submission {
                definition: view.definition,
                payload,
                idempotency_key: idempotency_key.into(),
                checkpoint: view.checkpoint,
                inputs: view.inputs,
                message: Some(format!("jobs: retry of {run_id}")),
                retry_of: Some(run_id),
            },
            caller,
        )
    }

    /// The redacted state of `run`, `None` when it is unknown or not visible to the
    /// caller.
    pub fn get(&self, run_id: Uuid, caller: &Caller) -> Result<Option<RunView>> {
        let snap = self.main()?.store().snapshot();
        let prep = preparation()?;
        let mut l = Ledger::new(&prep, &snap);
        let Some(run) = l.run(run_id)? else {
            return Ok(None);
        };
        let attempt = match run.current_attempt {
            Some(a) => l.attempt(a)?,
            None => None,
        };
        if !visible(&run, attempt.as_ref(), caller) {
            return Ok(None);
        }
        let publication = match run.publication {
            Some(p) => l.publication(p)?,
            None => None,
        };
        Ok(Some(run.view(attempt.as_ref(), publication.as_ref())))
    }

    /// The visible runs matching `filter`, newest first, at most the filter's limit
    /// (default 100, at most 1000). The scan reads at most 4096 runs per state.
    pub fn list(&self, filter: &ListFilter, caller: &Caller) -> Result<Vec<RunView>> {
        let snap = self.main()?.store().snapshot();
        let prep = preparation()?;
        let mut l = Ledger::new(&prep, &snap);
        let states: Vec<State> = match filter.state {
            Some(s) => vec![s],
            None => State::ALL.to_vec(),
        };
        let limit = filter
            .limit
            .unwrap_or(DEFAULT_LIST_LIMIT)
            .clamp(1, MAX_LIST_LIMIT);
        let mut out = Vec::new();
        for st in states {
            for id in l.runs_in(st)? {
                let Some(run) = l.run(id)? else { continue };
                if let Some(d) = &filter.definition
                    && &run.definition != d
                {
                    continue;
                }
                let attempt = match run.current_attempt {
                    Some(a) => l.attempt(a)?,
                    None => None,
                };
                if !visible(&run, attempt.as_ref(), caller) {
                    continue;
                }
                let publication = match run.publication {
                    Some(p) => l.publication(p)?,
                    None => None,
                };
                out.push(run.view(attempt.as_ref(), publication.as_ref()));
            }
        }
        out.sort_by(|a, b| b.submitted_commit.cmp(&a.submitted_commit));
        out.truncate(limit);
        Ok(out)
    }

    /// Release the source holds of runs that no longer exist or are terminal and were
    /// last acquired at least `grace_ms` ago, such as a hold left by a crash between the
    /// hold and the submission commit. Returns the released hold ids.
    pub fn reconcile_holds(&self, grace_ms: i64) -> Result<Vec<Uuid>> {
        let ds = self.main()?;
        let snap = ds.store().snapshot();
        let prep = preparation()?;
        let mut l = Ledger::new(&prep, &snap);
        ds.store()
            .reconcile_holds(OwnerKind::Jobs, grace_ms, &mut |hold| {
                Ok(l.run(hold.id)?.is_some_and(|r| !r.state.terminal()))
            })
    }
}

/// Whether the caller may see `run`: administrators see every run, others the runs
/// they submitted or currently work on.
fn visible(run: &Run, attempt: Option<&Attempt>, caller: &Caller) -> bool {
    caller.admin
        || run.principal == caller.principal
        || attempt.is_some_and(|a| a.worker == caller.principal)
}
