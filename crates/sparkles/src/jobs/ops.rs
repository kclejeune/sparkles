//! The typed transitions of the ledger: submission, claim, renewal, publication,
//! failure, cancellation, retry, inspection and the boot-epoch recovery.
//!
//! Each transition opens one transaction under the jobs owner's capability, reads its
//! preconditions from the transaction's base snapshot, stages its changes and commits.
//! See C26 §7.1 for why this ordering gives the acceptance cases.

use super::ledger::{
    self, Attempt, AttemptState, Changes, Epoch, Ledger, Operation, PublicationRecord, Recovery,
    RecoveryPhase, Run, RunInput, RunView, State, now_ms,
};
use super::{Caller, Jobs, managed};
use crate::branch::CommitRef;
use crate::managed::{ManagedErrorKind, OwnerKind};
use crate::store::{ControlOwner, HoldPublication, WriteTxn};
use crate::{Error, Result};
use oxrdf::{GraphName, NamedNode, Quad, Term};
use sparkles_core::control::{ControlState, ORIGIN_KEY};
use sparkles_core::jobs::{
    Definition, MAX_IDEMPOTENCY_KEY_BYTES, MAX_INPUTS, OPERATION_RETRY, OPERATION_SUBMIT, Settings,
    backoff_ms, canonical_json, digest_hex, new_token, operation_id, token_matches,
};
use std::sync::Arc;
use uuid::Uuid;

/// The most runs one recovery frame pauses and one recovery request resumes.
const PAUSE_BATCH: usize = 256;

/// The bytes of run and attempt records that one recovery frame stages at most, half
/// of the largest frame, so that long principal IRIs never make a recovery frame too
/// large to write.
const FRAME_BUDGET: usize = sparkles_core::control::MAX_FRAME / 2;

/// The bytes run `run`'s record and attempt `attempt`'s record take in a frame.
fn staged_bytes(ch: &mut Changes, run: &Run, attempt: Option<&Attempt>) -> usize {
    let mut bytes = ledger::encode(ch.run(run)).len() + 64;
    if let Some(a) = attempt {
        bytes += ledger::encode(ch.attempt(a)).len() + 64;
    }
    bytes
}

/// The recovery of restored work, as [`Jobs::resume`] reports it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryView {
    pub id: Uuid,
    /// The dataset the restored capture was taken of.
    pub source_dataset: Uuid,
    pub capture_commit: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_label: Option<String>,
    pub capture_ms: i64,
    pub phase: RecoveryPhase,
    pub started_ms: i64,
    /// The restored runs that are still paused.
    pub paused: u64,
    pub resumed: u64,
    pub cancelled: u64,
    /// Whether no paused run of the restored set is left, which ends the recovery.
    pub ended: bool,
}

/// What a recovery request did.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryStatus {
    /// The dataset's recovery of restored work, `None` when it has none.
    pub recovery: Option<RecoveryView>,
    /// The runs this request resumed.
    pub resumed: Vec<Uuid>,
}

impl RecoveryStatus {
    fn of(rec: &Recovery, left: u64, resumed: Vec<Uuid>) -> RecoveryStatus {
        RecoveryStatus {
            recovery: Some(RecoveryView {
                id: rec.id,
                source_dataset: rec.source_dataset,
                capture_commit: rec.capture_commit,
                capture_label: rec.capture_label.clone(),
                capture_ms: rec.capture_ms,
                phase: rec.phase,
                started_ms: rec.started_ms,
                paused: left,
                resumed: rec.resumed,
                cancelled: rec.cancelled,
                ended: left == 0,
            }),
            resumed,
        }
    }
}

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

    /// The operation kind the idempotency key is scoped to (spec C26 §3).
    fn operation_kind(&self) -> &'static str {
        if self.retry_of.is_some() {
            OPERATION_RETRY
        } else {
            OPERATION_SUBMIT
        }
    }

    /// The input references, sorted and without repeats: the order a caller lists them
    /// in is not part of the request.
    fn normalized_inputs(&self) -> Vec<CommitRef> {
        let mut inputs = self.inputs.clone();
        inputs.sort_unstable();
        inputs.dedup();
        inputs
    }

    /// `sha256:` and the digest of the normalized request: the canonical payload, the
    /// starting checkpoint, the sorted input references and the retried run, each
    /// length-framed and an absent member marked as absent, so that a changed
    /// checkpoint, input set or predecessor under a known key is a conflict and not a
    /// replay (spec C26 §3). JSON whitespace and member order do not change it.
    pub fn digest(&self) -> String {
        let mut bytes = b"sparkles-jobs-submission-v1\0".to_vec();
        frame(&mut bytes, &canonical_json(&self.payload));
        optional(
            &mut bytes,
            self.checkpoint.as_ref().map(|c| c.as_bytes().to_vec()),
        );
        let inputs = self.normalized_inputs();
        bytes.extend_from_slice(&(inputs.len() as u64).to_be_bytes());
        for i in &inputs {
            frame(&mut bytes, format!("{}@{}", i.branch_id, i.seq).as_bytes());
        }
        optional(&mut bytes, self.retry_of.map(|r| r.as_bytes().to_vec()));
        digest_hex(&bytes)
    }
}

/// Append `field` with its length first, so two fields cannot be confused for one.
fn frame(bytes: &mut Vec<u8>, field: &[u8]) {
    bytes.extend_from_slice(&(field.len() as u64).to_be_bytes());
    bytes.extend_from_slice(field);
}

/// Append an optional field: a discriminant and, when present, the framed field, so that
/// an absent member and an empty one differ.
fn optional(bytes: &mut Vec<u8>, field: Option<Vec<u8>>) {
    match field {
        None => bytes.push(0),
        Some(f) => {
            bytes.push(1);
            frame(bytes, &f);
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
    /// delete quads and its checkpoint, an absent checkpoint being another content than
    /// an empty one. Quad order does not change it.
    pub fn digest(&self) -> String {
        let mut lines: Vec<String> = self
            .insert
            .iter()
            .map(|q| format!("+{q}"))
            .chain(self.delete.iter().map(|q| format!("-{q}")))
            .collect();
        lines.sort();
        let mut bytes = b"sparkles-jobs-publication-v1\0".to_vec();
        frame(&mut bytes, lines.join("\n").as_bytes());
        optional(
            &mut bytes,
            self.checkpoint.as_ref().map(|c| c.as_bytes().to_vec()),
        );
        digest_hex(&bytes)
    }
}

/// The receipt of a publication.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Published {
    pub run: Uuid,
    pub publication: Uuid,
    /// The commit the publication made, or the head it followed when its result changed
    /// no quad and it made none.
    pub commit: u64,
    /// Whether the publication made a commit (spec C26 §4.3).
    pub committed: bool,
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

/// Whether `e` is a refusal that wrote nothing, as opposed to an error after which the
/// commit's outcome is uncertain. Every variant here is documented as raised before the
/// log record is written. I/O, corruption and poisoned outcomes stay uncertain.
fn refused(e: &Error) -> bool {
    matches!(
        e,
        Error::Conflict(_)
            | Error::StorageFull(_)
            | Error::Managed(_)
            | Error::Branch(_)
            | Error::PreconditionFailed(_)
            | Error::NotPermitted(_)
            | Error::Invalid(_)
            | Error::Rejected(_)
            | Error::GuardMissing(_)
            | Error::BudgetExceeded(_)
            | Error::NotFound(_)
            | Error::Unsupported(_)
            | Error::DryRun(_)
            | Error::Cancelled
            | Error::Timeout
            | Error::WriterBusy
    )
}

/// The time the retries of `run` end under the policy it was accepted with (spec C26
/// §4.4).
fn retry_horizon(run: &Run) -> i64 {
    run.submitted_ms
        .saturating_add(i64::try_from(run.retry_horizon_ms).unwrap_or(i64::MAX))
}

/// The retry decision after a failed, expired or interrupted attempt (spec C26 §4.4),
/// under the attempts and horizon the run was accepted with, so that an edited or removed
/// definition does not reinterpret accepted work: the run ends when the failure is
/// permanent, its attempts are used up, its horizon passed or the requested delay does
/// not fit in what is left of the horizon, and otherwise waits for the later of the
/// exponential backoff and the requested delay.
fn after_failure(
    run: &Run,
    now: i64,
    transient: bool,
    retry_after_ms: Option<u64>,
) -> (State, Option<i64>, Option<String>) {
    let failed = |why: String| (State::Failed, None, Some(why));
    if !transient {
        return (State::Failed, None, None);
    }
    if run.attempts >= run.attempts_allowed {
        return failed(format!("{} attempts were exhausted", run.attempts_allowed));
    }
    let horizon = retry_horizon(run);
    let remaining = horizon.saturating_sub(now);
    if remaining < 0 {
        return failed("the retry horizon passed".into());
    }
    let requested = match retry_after_ms {
        None => 0,
        Some(ms) => match i64::try_from(ms) {
            Ok(v) if v <= remaining => v,
            _ => {
                return failed(format!(
                    "the requested delay of {ms} ms passes the retry horizon"
                ));
            }
        },
    };
    let backoff = i64::try_from(backoff_ms(run.attempts, MAX_BACKOFF_MS)).unwrap_or(i64::MAX);
    let next = now.saturating_add(backoff.max(requested));
    if next > horizon {
        return failed("the retry horizon passed".into());
    }
    (State::RetryWait, Some(next), None)
}

/// A transaction under the jobs owner's control capability, with the recorded settings
/// and the control state it starts from.
struct Begun<'s> {
    t: WriteTxn<'s>,
    settings: Settings,
    base: Arc<ControlState>,
}

/// What a submission checked before it took the writer.
struct Staging<'a> {
    s: &'a Submission,
    caller: &'a Caller,
    def: &'a Definition,
    payload: &'a str,
    inputs: &'a [CommitRef],
    digest: &'a str,
    revision: &'a str,
    op: Uuid,
    run: Uuid,
}

/// A submission under the writer: its staged transaction, or the receipt of the run
/// another submission of the key recorded.
enum Staged<'s> {
    Ready(Box<WriteTxn<'s>>, State),
    Replayed(Submitted),
}

thread_local! {
    /// The source holds the submissions this thread is committing have taken for
    /// publication. The guard is not `Send`, and the publish hook runs on the committing
    /// thread, so the slot lives there.
    static HELD: std::cell::RefCell<Vec<HoldPublication>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Releases the publication guards a submission pushed into this thread's slot, also
/// when the commit unwinds, so that no guard outlives its commit.
struct HeldForPublication(usize);

impl HeldForPublication {
    fn mark() -> HeldForPublication {
        HeldForPublication(HELD.with(|h| h.borrow().len()))
    }
}

impl Drop for HeldForPublication {
    fn drop(&mut self) {
        let released = HELD.try_with(|h| {
            h.try_borrow_mut()
                .map(|mut h| {
                    let at = self.0.min(h.len());
                    h.split_off(at)
                })
                .unwrap_or_default()
        });
        drop(released);
    }
}

impl Jobs {
    fn begin<'s>(
        &'s self,
        operation: Uuid,
        write: Option<Arc<crate::access::GraphAccess>>,
        message: Option<String>,
    ) -> Result<Begun<'s>> {
        self.begin_with(operation, write, message, None)
    }

    /// [`begin`](Self::begin) with a hook that runs inside the commit, right before it
    /// publishes.
    fn begin_with<'s>(
        &'s self,
        operation: Uuid,
        write: Option<Arc<crate::access::GraphAccess>>,
        message: Option<String>,
        before_publish: Option<crate::guard::PublishHook>,
    ) -> Result<Begun<'s>> {
        let begun = self.begin_inner(operation, write, message, before_publish)?;
        // A restored checkpoint's origin entry fences every transition but the first
        // recovery frame, which removes it (spec C26 §7.3).
        if begun.base.get(ORIGIN_KEY).is_some() {
            return Err(managed(
                ManagedErrorKind::JobsRecovering,
                "the dataset was restored and its jobs recovery has not run yet; retry",
            ));
        }
        Ok(begun)
    }

    fn begin_inner<'s>(
        &'s self,
        operation: Uuid,
        write: Option<Arc<crate::access::GraphAccess>>,
        message: Option<String>,
        before_publish: Option<crate::guard::PublishHook>,
    ) -> Result<Begun<'s>> {
        let ds = self.main()?;
        let (settings, bytes) = self.enabled_settings()?;
        let store = ds.store();
        let capability = store.control_writer(ControlOwner::Jobs)?;
        // The business quads of a publication are checked against the caller's view one
        // by one in `publish`, and a view with triple rules is refused there.
        let _ = write;
        let opts = crate::guard::WriteOptions {
            message: message.map(Arc::from),
            graphs: None,
            before_publish,
            ..Default::default()
        };
        let mut t = store.try_write_with(crate::commit::CommitKind::Transaction, opts)?;
        // the settings this transaction was admitted under are the recorded ones
        if store.managed_settings(OwnerKind::Jobs)?.as_deref() != Some(&bytes[..]) {
            return Err(Error::Conflict(
                "the jobs settings changed while the operation was prepared; retry".into(),
            ));
        }
        t.set_control(capability)?;
        let _ = operation;
        let base = t.control_base()?;
        Ok(Begun { t, settings, base })
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
        // Restored work is paused before the epoch is installed, so no admission check
        // passes before then (spec C26 §7.3).
        self.pause_restored()?;
        let now = now_ms();
        let mut interrupted = Vec::new();
        let mut ended = Vec::new();
        // The interrupted leases go into frames of at most FRAME_BUDGET bytes, and the
        // epoch into the last one. A crash between them leaves the epoch of an earlier
        // process, which this process's recovery replaces.
        loop {
            let Begun { mut t, base, .. } =
                self.begin(Uuid::new_v4(), None, Some("jobs: boot epoch".into()))?;
            let mut l = Ledger::new(&base);
            if l.epoch()? == Some(rt.epoch()) {
                rt.mark_installed();
                return Ok(Recovered {
                    epoch: rt.epoch(),
                    installed: false,
                    interrupted,
                });
            }
            let mut ch = Changes::default();
            let mut bytes = 0;
            let mut full = false;
            // A leased run beyond a cut scan keeps its old epoch, which every leased
            // transition and the next claim refuse, so recovery stays possible.
            for id in l.runs_in(State::Leased)?.ids {
                if bytes >= FRAME_BUDGET {
                    full = true;
                    break;
                }
                let Some(run) = l.run_header(id)? else {
                    continue;
                };
                let Some(attempt) = run.current_attempt else {
                    continue;
                };
                let Some(attempt) = l.attempt(attempt)? else {
                    continue;
                };
                if attempt.epoch == rt.epoch() {
                    continue;
                }
                let a = ch.attempt(&attempt);
                a.state = AttemptState::Interrupted;
                a.diagnostic = Some("interrupted by a restart of the dataset's process".into());
                let (state, next, reason) = after_failure(&run, now, true, None);
                self.transition(&mut ch, &run, state, next, reason);
                bytes += staged_bytes(&mut ch, &run, Some(&attempt));
                rt.revoke_through(run.id, run.version + 1);
                if state.terminal() {
                    ended.push(run.id);
                }
                interrupted.push(run.id);
            }
            if !full {
                ch.epoch(&Epoch {
                    epoch: rt.epoch(),
                    installed_ms: now,
                });
            }
            drop(l);
            ch.apply(&mut t)?;
            t.commit()?;
            if !full {
                break;
            }
        }
        rt.mark_installed();
        // A run that waits to retry still needs its pinned inputs.
        self.release_holds_of(&ended);
        Ok(Recovered {
            epoch: rt.epoch(),
            installed: true,
            interrupted,
        })
    }

    /// The published control state, read outside the writer. A dataset whose keyspace
    /// is not enabled reads as an empty ledger.
    fn published(&self) -> Result<Arc<ControlState>> {
        Ok(self.main()?.store().control_state().unwrap_or_default())
    }

    /// Recover the work of a restored capture: consume the checkpoint's origin entry into
    /// a `pausing` recovery record, then pause the restored runs in bounded frames, and
    /// move the record to `awaiting-decision` once none is left. A crash between the
    /// frames leaves the origin or the `pausing` record, so the next recovery continues
    /// where this one stopped.
    fn pause_restored(&self) -> Result<()> {
        loop {
            let Begun { mut t, base, .. } =
                self.begin_inner(Uuid::new_v4(), None, Some("jobs: recovery".into()), None)?;
            let mut l = Ledger::new(&base);
            let now = now_ms();
            let mut ch = Changes::default();
            if let Some(origin) = base.origin()? {
                t.control_consume_origin()?;
                ch.recovery(Some(&Recovery {
                    id: Uuid::new_v4(),
                    source_dataset: origin.dataset,
                    capture_commit: origin.commit,
                    capture_label: Some(origin.label).filter(|l| !l.is_empty()),
                    capture_ms: origin.time_ms,
                    checkpoint_seq: base.seq,
                    phase: RecoveryPhase::Pausing,
                    started_ms: now,
                    paused: 0,
                    resumed: 0,
                    cancelled: 0,
                }));
                drop(l);
                ch.apply(&mut t)?;
                t.commit()?;
                self.ds.store().failpoint("jobs-recovery-started");
                continue;
            }
            let Some(mut rec) = l.recovery()? else {
                return Ok(());
            };
            if rec.phase != RecoveryPhase::Pausing {
                return Ok(());
            }
            let mut paused = 0;
            let mut bytes = 0;
            let mut full = false;
            'states: for st in [
                State::Queued,
                State::RetryWait,
                State::Leased,
                State::Paused,
            ] {
                for id in l.runs_in(st)?.ids {
                    if paused == PAUSE_BATCH || bytes >= FRAME_BUDGET {
                        full = true;
                        break 'states;
                    }
                    let Some(run) = l.run_header(id)? else {
                        continue;
                    };
                    // A run that an earlier recovery paused, captured while it awaited a
                    // decision, joins this recovery's set.
                    if st == State::Paused {
                        if run.recovery == Some(rec.id) {
                            continue;
                        }
                        ch.run(&run).recovery = Some(rec.id);
                        bytes += staged_bytes(&mut ch, &run, None);
                        paused += 1;
                        continue;
                    }
                    let mut attempt = None;
                    if let Some(a) = run.current_attempt
                        && let Some(a) = l.attempt(a)?
                        && a.state == AttemptState::Active
                    {
                        let staged = ch.attempt(&a);
                        staged.state = AttemptState::Interrupted;
                        staged.diagnostic = Some("interrupted by a restore of the dataset".into());
                        attempt = Some(a);
                    }
                    self.transition(
                        &mut ch,
                        &run,
                        State::Paused,
                        None,
                        Some("paused by the recovery of a restored backup".into()),
                    );
                    ch.run(&run).recovery = Some(rec.id);
                    bytes += staged_bytes(&mut ch, &run, attempt.as_ref());
                    paused += 1;
                }
            }
            rec.paused += paused as u64;
            if !full {
                rec.phase = RecoveryPhase::AwaitingDecision;
            }
            ch.recovery(Some(&rec));
            drop(l);
            ch.apply(&mut t)?;
            t.commit()?;
            self.ds.store().failpoint("jobs-recovery-paused");
        }
    }

    /// Refuse `what` while a recovery is pausing restored work (spec C26 §7.3).
    fn refuse_while_recovering(&self, l: &mut Ledger<'_>, what: &str) -> Result<()> {
        if l.recovery()?
            .is_some_and(|r| r.phase == RecoveryPhase::Pausing)
        {
            return Err(managed(
                ManagedErrorKind::JobsRecovering,
                format!("{what} is refused while the jobs recovery pauses restored work; retry"),
            ));
        }
        Ok(())
    }

    /// Resume the paused runs `runs` of the dataset's recovery of restored work, which
    /// makes them eligible again (spec C26 §6 and §7.3). Only an administrator may, and
    /// only once the recovery awaits a decision. Each run must be paused and belong to the
    /// restored set. With no runs, it reports the recovery. Once no paused run of the
    /// set is left, the recovery ends.
    pub fn resume(&self, runs: &[Uuid], caller: &Caller) -> Result<RecoveryStatus> {
        if !caller.admin {
            return Err(Error::NotPermitted(
                "resuming restored work needs administration of the dataset".into(),
            ));
        }
        if runs.len() > PAUSE_BATCH {
            return Err(Error::invalid(format!(
                "a recovery request resumes at most {PAUSE_BATCH} runs"
            )));
        }
        // A request that finds a restore's origin or a pausing recovery is refused, so
        // the operator decides only once every restored run is paused. It drives the
        // pausing to its end first, so that the next request can be admitted.
        let published = self.published()?;
        let pausing = published.origin()?.is_some()
            || Ledger::new(&published)
                .recovery()?
                .is_some_and(|r| r.phase == RecoveryPhase::Pausing);
        if pausing {
            self.ensure_epoch()?;
            return Err(managed(
                ManagedErrorKind::JobsRecovering,
                "the jobs recovery was pausing restored work when the request arrived; read the recovery and retry",
            ));
        }
        self.ensure_epoch()?;
        let Begun { mut t, base, .. } =
            self.begin(Uuid::new_v4(), None, Some("jobs: resume".into()))?;
        let mut l = Ledger::new(&base);
        let Some(mut rec) = l.recovery()? else {
            if runs.is_empty() {
                return Ok(RecoveryStatus::default());
            }
            return Err(Error::Conflict(
                "the dataset has no recovery of restored work".into(),
            ));
        };
        self.refuse_while_recovering(&mut l, "resuming restored work")?;
        let mut ch = Changes::default();
        let mut resumed = Vec::new();
        for &id in runs {
            if resumed.contains(&id) {
                continue;
            }
            let run = l.run_header(id)?.ok_or_else(|| not_found(id))?;
            if run.state != State::Paused || run.recovery != Some(rec.id) {
                return Err(Error::Conflict(format!(
                    "run {id} is not paused by the dataset's recovery"
                )));
            }
            self.transition(
                &mut ch,
                &run,
                State::Queued,
                None,
                Some(format!("resumed by {}", caller.principal.as_str())),
            );
            resumed.push(id);
        }
        rec.resumed += resumed.len() as u64;
        let left = Self::paused_members(&mut l, &rec, &resumed)?;
        let status = RecoveryStatus::of(&rec, left, resumed);
        // the recovery stays once no run is paused, so that a retry of a restored run
        // that ended still waits for a decision
        ch.recovery(Some(&rec));
        drop(l);
        if !ch.is_empty() {
            ch.apply(&mut t)?;
            t.commit()?;
        }
        Ok(status)
    }

    /// The paused runs of `rec`'s restored set, other than `leaving`.
    fn paused_members(l: &mut Ledger<'_>, rec: &Recovery, leaving: &[Uuid]) -> Result<u64> {
        let scan = l.runs_in(State::Paused)?;
        let mut left = 0;
        for id in &scan.ids {
            if leaving.contains(id) {
                continue;
            }
            if l.run_header(*id)?
                .is_some_and(|r| r.recovery == Some(rec.id))
            {
                left += 1;
            }
        }
        if scan.truncated {
            left += 1;
        }
        Ok(left)
    }

    fn ensure_epoch(&self) -> Result<()> {
        self.recover().map(|_| ())
    }

    /// Stage a run's state change: state, version, next eligible time and reason.
    fn transition(
        &self,
        ch: &mut Changes,
        run: &Run,
        state: State,
        next_eligible_ms: Option<i64>,
        reason: Option<String>,
    ) {
        let r = ch.run(run);
        r.state = state;
        r.version = run.version + 1;
        r.next_eligible_ms = next_eligible_ms;
        if let Some(reason) = reason {
            r.reason = Some(ledger::redacted(&reason));
        }
    }

    fn maybe_release_hold(&self, run: Uuid) {
        if let Err(e) = self.ds.store().release_holds(OwnerKind::Jobs, &[run]) {
            tracing::warn!(target: "sparkles::jobs", "the source hold of run {run} was not released: {e}");
        }
    }

    /// Release the source holds of runs that ended. A hold that was never acquired is
    /// ignored by the store.
    fn release_holds_of(&self, ended: &[Uuid]) {
        if ended.is_empty() {
            return;
        }
        if let Err(e) = self.ds.store().release_holds(OwnerKind::Jobs, ended) {
            tracing::warn!(target: "sparkles::jobs", "the source holds of ended runs were not released: {e}");
        }
    }

    /// Whether any run is queued, leased or waiting to retry.
    pub(super) fn has_active_runs(&self) -> Result<bool> {
        let state = self.published()?;
        let mut l = Ledger::new(&state);
        for st in [State::Queued, State::Leased, State::RetryWait] {
            if !l.runs_in(st)?.ids.is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The number of runs in `state`, or `jobs-overloaded` when the ledger holds more
    /// than one scan reads, since an undercount must not admit work past the bound.
    fn exact_count(l: &mut Ledger<'_>, state: State) -> Result<usize> {
        let scan = l.runs_in(state)?;
        if scan.truncated {
            return Err(managed(
                ManagedErrorKind::JobsOverloaded,
                format!(
                    "more than {} runs are {}; no run was stored",
                    ledger::MAX_SCAN,
                    state.as_str()
                ),
            ));
        }
        Ok(scan.ids.len())
    }

    /// Submit a run. The same key with the same normalized request returns the existing
    /// run, a different request is `idempotency-conflict`, and admission past the
    /// dataset's bound on waiting runs is `jobs-overloaded` without storing a run.
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
        // A submitter whose own grants do not reach a definition graph cannot have
        // work published there on its behalf (C26 §4.2).
        if let Some(access) = &caller.write {
            for g in &def.graphs {
                let term = Term::NamedNode(NamedNode::new_unchecked(g));
                if !access.writable(Some(&term)) {
                    return Err(crate::access::GraphAccess::refused(Some(&term)));
                }
            }
        }
        let payload = canonical_json(&s.payload);
        if payload.len() as u64 > def.max_input_bytes {
            return Err(Error::invalid(format!(
                "the payload is larger than {} bytes",
                def.max_input_bytes
            )));
        }
        let payload = String::from_utf8(payload).expect("JSON is UTF-8");
        let inputs = s.normalized_inputs();
        if inputs.len() > MAX_INPUTS {
            return Err(Error::invalid(format!(
                "a submission pins at most {MAX_INPUTS} input commits"
            )));
        }
        let digest = s.digest();
        let revision = def.revision();
        let store = ds.store();
        let op = operation_id(
            ds.dataset_id(),
            store.branch_id(),
            s.operation_kind(),
            caller.principal.as_str(),
            &s.definition,
            &revision,
            key,
        );
        // A repeated key answers before anything is held or locked.
        {
            let state = self.published()?;
            let mut l = Ledger::new(&state);
            if let Some(found) = l.operation(op)? {
                return self.replay(&mut l, found, &digest, state.follows);
            }
        }
        let run = Uuid::new_v4();
        let held = !inputs.is_empty();
        if held {
            store
                .acquire_hold(OwnerKind::Jobs, run, &inputs)
                .map_err(|e| match e {
                    Error::HistoryGone(_) => Error::Conflict(
                        "an input commit is no longer retained, so the run cannot pin it".into(),
                    ),
                    e => e,
                })?;
            store.failpoint("jobs-held");
        }
        // The hold is taken for publication inside the commit, right before it
        // publishes, and kept in this thread's slot until the commit returns, so a
        // reconciliation that chose the hold as an orphan before this point cannot
        // release it under a run that then commits, and one that released it already
        // refuses the commit. The hook runs on this thread, inside `commit`.
        let release = HeldForPublication::mark();
        let hook = held.then(|| {
            let held_by = ds.clone();
            let inputs = inputs.clone();
            crate::guard::PublishHook::new(move || {
                let guard = held_by
                    .store()
                    .hold_publication(OwnerKind::Jobs, run, &inputs)?;
                HELD.with(|h| h.borrow_mut().push(guard));
                Ok(())
            })
        });
        let staged = self.stage_submission(
            Staging {
                s,
                caller,
                def,
                payload: &payload,
                inputs: &inputs,
                digest: &digest,
                revision: &revision,
                op,
                run,
            },
            hook,
        );
        let (t, state) = match staged {
            Ok(Staged::Ready(t, state)) => (*t, state),
            Ok(Staged::Replayed(r)) => {
                // the run this submission would have made is not recorded
                if held {
                    self.maybe_release_hold(run);
                }
                return Ok(r);
            }
            Err(e) => {
                // nothing was committed, so the hold under the new run is an orphan
                if held {
                    self.maybe_release_hold(run);
                }
                return Err(e);
            }
        };
        store.failpoint("jobs-before-commit");
        let committed = t.commit();
        // the publication guard goes first, since a release waits for every publication
        drop(release);
        let receipt = match committed {
            Ok(r) => r,
            Err(e) => {
                // A refusal wrote nothing, so the hold under the new run is an orphan. An
                // error whose outcome is uncertain keeps it, since the run may be
                // durable, and reconciliation decides later.
                if held && refused(&e) {
                    self.maybe_release_hold(run);
                }
                return Err(e);
            }
        };
        store.failpoint("jobs-committed");
        Ok(Submitted {
            run,
            version: 1,
            commit: receipt.commit.seq,
            replayed: false,
            state,
        })
    }

    /// Take the writer and stage the run of a submission, or answer a key that another
    /// submission recorded in the meantime. Nothing is committed.
    fn stage_submission<'s>(
        &'s self,
        st: Staging<'_>,
        hook: Option<crate::guard::PublishHook>,
    ) -> Result<Staged<'s>> {
        let ds = self.main()?;
        let store = ds.store();
        let Begun {
            mut t,
            settings,
            base,
        } = self.begin_with(st.op, None, st.s.message.clone(), hook)?;
        // The definition the request was checked against must be the one the admitted
        // settings hold, or a change between the two reads could revoke the submitter,
        // disable the definition or shrink its bounds under a run that still commits.
        if settings.definitions.get(&st.s.definition) != Some(st.def) {
            return Err(Error::Conflict(format!(
                "definition {:?} changed while the submission was prepared; retry",
                st.s.definition
            )));
        }
        let mut l = Ledger::new(&base);
        if let Some(found) = l.operation(st.op)? {
            return self
                .replay(&mut l, found, st.digest, base.follows)
                .map(Staged::Replayed);
        }
        self.refuse_while_recovering(&mut l, "a submission")?;
        let waiting = Self::exact_count(&mut l, State::Queued)?
            + Self::exact_count(&mut l, State::RetryWait)?;
        if waiting >= settings.max_queued as usize {
            return Err(managed(
                ManagedErrorKind::JobsOverloaded,
                format!(
                    "{waiting} runs are waiting, the dataset's bound of {}; no run was stored",
                    settings.max_queued
                ),
            ));
        }
        let input = RunInput {
            payload: st.payload.to_string(),
            checkpoint: st.s.checkpoint.clone(),
            inputs: st.inputs.to_vec(),
        };
        // An explicit retry of a member of a restored set that awaits a decision joins
        // the set paused, so only the recovery route makes it eligible (spec C26 §7.3).
        let mut ch = Changes::default();
        let mut joins = None;
        if let Some(of) = st.s.retry_of
            && let Some(mut rec) = l.recovery()?
            && let Some(old) = l.run_header(of)?
            && (old.submitted_seq <= rec.checkpoint_seq || old.recovery == Some(rec.id))
        {
            rec.paused += 1;
            ch.recovery(Some(&rec));
            joins = Some(rec.id);
        }
        let state = if joins.is_some() {
            State::Paused
        } else {
            State::Queued
        };
        let stamp = t.control_stamp()?;
        ch.operation(
            st.op,
            &Operation {
                run: st.run,
                request_digest: st.digest.to_string(),
                idempotency_key: st.s.idempotency_key.clone(),
                operation_kind: st.s.operation_kind().to_string(),
                principal: st.caller.principal.clone(),
                definition: st.s.definition.clone(),
                revision: st.revision.to_string(),
            },
        );
        ch.new_run(
            Run {
                id: st.run,
                definition: st.s.definition.clone(),
                revision: st.revision.to_string(),
                kind: st.def.kind.clone(),
                kind_version: st.def.version,
                principal: st.caller.principal.clone(),
                dataset_id: ds.dataset_id(),
                branch_id: store.branch_id(),
                payload_digest: digest_hex(st.payload.as_bytes()),
                submitted_ms: stamp.time_ms,
                submitted_commit: stamp.head,
                submitted_seq: stamp.seq,
                state,
                version: 1,
                attempts: 0,
                attempts_allowed: st.def.attempts,
                retry_horizon_ms: st.def.retry_horizon_ms,
                generation: 0,
                current_attempt: None,
                next_eligible_ms: None,
                reason: None,
                publication: None,
                retry_of: st.s.retry_of,
                operation: st.op,
                recovery: joins,
                payload: String::new(),
                checkpoint: None,
                inputs: Vec::new(),
            },
            &input,
        );
        drop(l);
        ch.apply(&mut t)?;
        // Admission is bounded by the size of the control state with every record the
        // submission stages, so that the memory it takes stays within the settings'
        // bound. A transition of an admitted run is never refused for it (spec C26 §7).
        if t.control_bytes()? > settings.max_control_bytes {
            return Err(managed(
                ManagedErrorKind::JobsOverloaded,
                format!(
                    "the job ledger would grow past the dataset's bound of {} bytes; no run was stored",
                    settings.max_control_bytes
                ),
            ));
        }
        Ok(Staged::Ready(Box::new(t), state))
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
                "the idempotency key was used before with another payload, checkpoint, input set or retried run",
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
    /// submitted first. A run whose lease expired is first scheduled under its retry
    /// policy, like an interrupted attempt (spec C26 §4.2), and is claimed with a later
    /// generation once it is due. A waiting run whose horizon passed, and one whose
    /// attempts ran out, fails instead. Every such change commits with the claim.
    pub fn claim(&self, definitions: &[String], caller: &Caller) -> Result<Option<Claimed>> {
        let ds = self.main()?;
        self.ensure_epoch()?;
        let rt = self.runtime();
        let Begun {
            mut t,
            settings,
            base,
        } = self.begin(Uuid::new_v4(), None, Some("jobs: claim".into()))?;
        let mut l = Ledger::new(&base);
        self.refuse_while_recovering(&mut l, "a claim")?;
        let now = now_ms();
        let permitted = |def: &Definition, name: &str| {
            def.enabled
                && (definitions.is_empty() || definitions.iter().any(|d| d == name))
                && (def.workers.is_empty()
                    || def.workers.iter().any(|w| w == caller.principal.as_str()))
        };
        // The scan reads each run's scheduling members only; the chosen run's payload
        // and inputs are read once below, so a claim retains no payload it does not
        // return.
        let mut ch = Changes::default();
        let mut candidates: Vec<Run> = Vec::new();
        let mut ended: Vec<Uuid> = Vec::new();
        let mut leased_by_definition: std::collections::HashMap<String, u32> = Default::default();
        for st in [State::Queued, State::RetryWait, State::Leased] {
            let scan = l.runs_in(st)?;
            if scan.truncated {
                return Err(managed(
                    ManagedErrorKind::JobsOverloaded,
                    format!(
                        "more than {} runs are {}; dispatch is refused until runs are cancelled",
                        ledger::MAX_SCAN,
                        st.as_str()
                    ),
                ));
            }
            for id in scan.ids {
                let Some(run) = l.run_header(id)? else {
                    continue;
                };
                let def = settings.definitions.get(&run.definition);
                match run.state {
                    State::Queued => {}
                    State::RetryWait => {
                        if run.next_eligible_ms.is_some_and(|n| n > now) {
                            continue;
                        }
                        // a retry that comes due after its horizon is over
                        if now > retry_horizon(&run) {
                            self.transition(
                                &mut ch,
                                &run,
                                State::Failed,
                                None,
                                Some("the retry horizon passed".into()),
                            );
                            ended.push(run.id);
                            continue;
                        }
                    }
                    State::Leased => {
                        if !rt.lease_expired(run.id, run.version) {
                            *leased_by_definition
                                .entry(run.definition.clone())
                                .or_default() += 1;
                            continue;
                        }
                        // An expired attempt may have had an external effect, so the
                        // run follows the retry policy instead of being taken at once.
                        if let Some(a) = run.current_attempt
                            && let Some(a) = l.attempt(a)?
                            && a.state == AttemptState::Active
                        {
                            ch.attempt(&a).state = AttemptState::Expired;
                        }
                        let (state, next, reason) = after_failure(&run, now, true, None);
                        let reason = Some(match reason {
                            Some(why) => format!("the lease expired ({why})"),
                            None => "the lease expired".into(),
                        });
                        self.transition(&mut ch, &run, state, next, reason);
                        rt.revoke_through(run.id, run.version + 1);
                        if state.terminal() {
                            ended.push(run.id);
                        }
                        continue;
                    }
                    _ => continue,
                }
                let Some(def) = def else {
                    continue;
                };
                if def.revision() != run.revision || !permitted(def, &run.definition) {
                    continue;
                }
                candidates.push(run);
            }
        }
        candidates.sort_by_key(|r| r.submitted_seq);
        let mut chosen = None;
        for run in candidates {
            let def = &settings.definitions[&run.definition];
            if run.attempts >= run.attempts_allowed {
                // a waiting run that was recorded with its attempts used up ends here
                self.transition(
                    &mut ch,
                    &run,
                    State::Failed,
                    None,
                    Some(format!("{} attempts were exhausted", run.attempts_allowed)),
                );
                ended.push(run.id);
                continue;
            }
            let leased = leased_by_definition
                .get(&run.definition)
                .copied()
                .unwrap_or(0);
            if leased >= def.concurrency {
                continue;
            }
            chosen = Some((run.id, def.clone()));
            break;
        }
        let Some((chosen, def)) = chosen else {
            drop(l);
            if !ch.is_empty() {
                ch.apply(&mut t)?;
                t.commit()?;
                self.release_holds_of(&ended);
            }
            return Ok(None);
        };
        let run = l
            .run(chosen)?
            .ok_or_else(|| Error::Corrupt(format!("run {chosen} vanished under the writer")))?;
        let attempt = Uuid::new_v4();
        let (token, verifier) = new_token();
        let generation = run.generation + 1;
        let number = run.attempts + 1;
        let expires = now.saturating_add(def.lease_ms as i64);
        let r = ch.run(&run);
        r.state = State::Leased;
        r.version = run.version + 1;
        r.generation = generation;
        r.attempts = number;
        r.current_attempt = Some(attempt);
        r.next_eligible_ms = None;
        ch.new_attempt(Attempt {
            id: attempt,
            run: run.id,
            worker: caller.principal.clone(),
            number,
            generation,
            epoch: rt.epoch(),
            verifier,
            claimed_ms: now,
            lease_expires_ms: expires,
            state: AttemptState::Active,
            diagnostic: None,
        });
        drop(l);
        ch.apply(&mut t)?;
        ds.store().failpoint("jobs-before-claim-commit");
        // The monotonic deadline exists before the commit publishes, while this
        // transaction still holds the writer, so no claim can read the new attempt
        // without its deadline and take it over as expired.
        let grant = rt.grant(run.id, run.version + 1, def.lease_ms);
        t.commit()?;
        grant.keep();
        ds.store().failpoint("jobs-claim-committed");
        self.release_holds_of(&ended);
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

    /// The run, its active attempt and its definition when `token` is the live lease of
    /// `run` held by the caller and the definition still admits the run and the worker,
    /// or `lease-lost`. A definition that was removed, disabled or edited since the
    /// claim, or that no longer lists the worker, fences the lease like an expiry (spec
    /// C26 §4.2 and §4.3).
    fn check_lease(
        &self,
        l: &mut Ledger<'_>,
        settings: &Settings,
        run: Uuid,
        token: &str,
        caller: &Caller,
    ) -> Result<(Run, Attempt, Definition)> {
        let rt = self.runtime();
        let Some(run) = l.run(run)? else {
            return Err(not_found(run));
        };
        // A caller that is not an administrator, the submitter or a worker of one of the
        // run's attempts learns nothing: an invisible run answers as an unknown one. The
        // worker of a superseded attempt still learns that its lease is lost.
        if !Self::may_act_on(l, &run, caller)? {
            return Err(not_found(run.id));
        }
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
        if rt.lease_expired(run.id, run.version) {
            return Err(lease_lost("the lease expired"));
        }
        let Some(def) = settings.definitions.get(&run.definition) else {
            return Err(lease_lost("the definition is no longer registered"));
        };
        if !def.enabled {
            return Err(lease_lost("the definition is disabled"));
        }
        if def.revision() != run.revision {
            return Err(lease_lost(
                "the definition changed since the run was accepted",
            ));
        }
        if !def.workers.is_empty() && !def.workers.iter().any(|w| w == caller.principal.as_str()) {
            return Err(lease_lost("the definition no longer admits the worker"));
        }
        Ok((run, attempt, def.clone()))
    }

    /// Whether `caller` may learn the state of `run` through a lease operation.
    fn may_act_on(l: &mut Ledger<'_>, run: &Run, caller: &Caller) -> Result<bool> {
        Ok(caller.admin
            || run.principal == caller.principal
            || l.worked_on(run.id, &caller.principal)?)
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
            base,
        } = self.begin(Uuid::new_v4(), None, Some("jobs: renew".into()))?;
        let mut l = Ledger::new(&base);
        self.refuse_while_recovering(&mut l, "a renewal")?;
        let (run, attempt, def) = self.check_lease(&mut l, &settings, run, token, caller)?;
        Self::expect_version(&run, expected_version)?;
        let extend = extend_ms
            .unwrap_or(def.lease_ms)
            .min(def.max_lease_extension_ms)
            .max(1);
        let now = now_ms();
        let expires = now.saturating_add(extend as i64);
        let mut ch = Changes::default();
        ch.attempt(&attempt).lease_expires_ms = expires;
        ch.run(&run).version = run.version + 1;
        drop(l);
        ch.apply(&mut t)?;
        // The new deadline counts only once this commit's version is the committed one,
        // and one that does not commit leaves the committed deadline as it was.
        let grant = self.runtime().grant(run.id, run.version + 1, extend);
        let committed = t.commit();
        #[cfg(test)]
        self.runtime().after_commit();
        committed?;
        grant.keep();
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
            let state = self.published()?;
            let mut l = Ledger::new(&state);
            if let Some(found) = l.publication(p.operation)? {
                return Self::replay_publication(&mut l, &found, run_id, token, caller, &digest);
            }
        }
        let Begun {
            mut t,
            settings,
            base,
        } = self.begin(p.operation, caller.write.clone(), p.message.clone())?;
        let mut l = Ledger::new(&base);
        if let Some(found) = l.publication(p.operation)? {
            return Self::replay_publication(&mut l, &found, run_id, token, caller, &digest);
        }
        self.refuse_while_recovering(&mut l, "a publication")?;
        let (run, attempt, def) = self.check_lease(&mut l, &settings, run_id, token, caller)?;
        Self::expect_version(&run, p.expected_version)?;
        let count = (p.insert.len() + p.delete.len()) as u64;
        if count > def.max_output_quads {
            return Err(Error::invalid(format!(
                "the result holds {count} quads, more than the definition's {}",
                def.max_output_quads
            )));
        }
        if caller.write.as_ref().is_some_and(|a| a.triples.is_some()) {
            return Err(Error::NotPermitted(
                "a publication under a view that protects triples is not supported; the worker needs graph-level grants".into(),
            ));
        }
        {
            let registry = ds.store().managed_snapshot()?;
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
                if registry
                    .as_ref()
                    .is_some_and(|r| r.graphs.owner_of(g.as_str()).is_some())
                {
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
        // A result that changes no quad makes no commit: its record names the head it
        // follows, and its receipt says no commit was made (spec C26 §4.3).
        let stamp = t.control_stamp()?;
        let mut ch = Changes::default();
        ch.publication(&PublicationRecord {
            id: p.operation,
            run: run.id,
            attempt: attempt.id,
            digest: digest.clone(),
            commit: stamp.commit.unwrap_or(stamp.head),
            committed: stamp.commit.is_some(),
            inserted: p.insert.len() as u64,
            deleted: p.delete.len() as u64,
            checkpoint: p.checkpoint.clone(),
            published_ms: stamp.time_ms,
        });
        self.transition(&mut ch, &run, State::Succeeded, None, None);
        ch.run(&run).publication = Some(p.operation);
        ch.attempt(&attempt).state = AttemptState::Published;
        drop(l);
        ch.apply(&mut t)?;
        ds.store().failpoint("jobs-before-publish-commit");
        let receipt = t.commit()?;
        ds.store().failpoint("jobs-published");
        self.runtime().revoke_through(run.id, run.version + 1);
        if !run.inputs.is_empty() {
            self.maybe_release_hold(run.id);
        }
        Ok(Published {
            run: run.id,
            publication: p.operation,
            commit: receipt.commit.seq,
            committed: receipt.committed,
            version: run.version + 1,
            inserted: p.insert.len() as u64,
            deleted: p.delete.len() as u64,
            replayed: false,
        })
    }

    /// The original receipt of a completed publication, for the worker that made it:
    /// the operation must belong to `run_id` and `token` must be the token of the
    /// attempt that published, even when its lease has since ended. Anyone else gets
    /// `lease-lost`, so an operation id alone discloses nothing.
    fn replay_publication(
        l: &mut Ledger<'_>,
        found: &ledger::PublicationRecord,
        run_id: Uuid,
        token: &str,
        caller: &Caller,
        digest: &str,
    ) -> Result<Published> {
        let attempt = l.attempt(found.attempt)?;
        let owner = attempt.as_ref().is_some_and(|a| {
            found.run == run_id
                && a.run == run_id
                && a.worker == caller.principal
                && token_matches(token, &a.verifier)
        });
        if !owner {
            // the run's own workers and submitter learn that the token is wrong, anyone
            // else that nothing is known
            let visible = match l.run_header(run_id)? {
                Some(run) => Self::may_act_on(l, &run, caller)?,
                None => false,
            };
            return Err(if visible {
                lease_lost("the token does not match the attempt that published")
            } else {
                not_found(run_id)
            });
        }
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
            committed: found.committed,
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
            base,
        } = self.begin(Uuid::new_v4(), None, Some("jobs: failure".into()))?;
        let mut l = Ledger::new(&base);
        self.refuse_while_recovering(&mut l, "a failure")?;
        let (run, attempt, _def) = self.check_lease(&mut l, &settings, run_id, token, caller)?;
        Self::expect_version(&run, expected_version)?;
        let now = now_ms();
        let (state, next, why) = after_failure(&run, now, f.transient, f.retry_after_ms);
        let diagnostic = ledger::redacted(&f.reason);
        let mut ch = Changes::default();
        let a = ch.attempt(&attempt);
        a.state = AttemptState::Failed;
        a.diagnostic = (!diagnostic.is_empty()).then(|| diagnostic.clone());
        let reason = match (&why, diagnostic.is_empty()) {
            (Some(w), false) => Some(format!("{diagnostic} ({w})")),
            (Some(w), true) => Some(w.clone()),
            (None, false) => Some(diagnostic.clone()),
            (None, true) => Some(String::new()),
        };
        self.transition(&mut ch, &run, state, next, reason);
        drop(l);
        ch.apply(&mut t)?;
        t.commit()?;
        #[cfg(test)]
        self.runtime().after_commit();
        self.runtime().revoke_through(run.id, run.version + 1);
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
            base,
        } = self.begin(Uuid::new_v4(), None, Some("jobs: cancel".into()))?;
        let mut l = Ledger::new(&base);
        let Some(run) = l.run(run_id)? else {
            return Err(not_found(run_id));
        };
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
            ch.attempt(a).state = AttemptState::Cancelled;
        }
        let uncertain = run.state == State::Leased;
        self.transition(
            &mut ch,
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
        );
        // A cancelled member of a restored set leaves the paused runs, and the recovery
        // stays, so that a retry of any member still waits for a decision (spec C26
        // §7.3).
        if run.state == State::Paused
            && let Some(mut rec) = l.recovery()?
            && run.recovery == Some(rec.id)
        {
            rec.cancelled += 1;
            ch.recovery(Some(&rec));
        }
        drop(l);
        ch.apply(&mut t)?;
        t.commit()?;
        self.runtime().revoke_through(run.id, run.version + 1);
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
            let state = self.published()?;
            let mut l = Ledger::new(&state);
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
        let state = self.published()?;
        let mut l = Ledger::new(&state);
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
        let state = self.published()?;
        let mut l = Ledger::new(&state);
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
            for id in l.runs_in(st)?.ids {
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
        out.sort_by_key(|a| std::cmp::Reverse((a.submitted_commit, a.submitted_ms)));
        out.truncate(limit);
        Ok(out)
    }

    /// Release the source holds of runs that no longer exist or are terminal and were
    /// last acquired at least `grace_ms` ago, such as a hold left by a crash between the
    /// hold and the submission commit. Returns the released hold ids.
    pub fn reconcile_holds(&self, grace_ms: i64) -> Result<Vec<Uuid>> {
        let ds = self.main()?;
        // Each question reads the committed head of its moment: the store asks again
        // under the lock every publication takes, and that answer must see a run whose
        // commit published after the first question.
        ds.store()
            .reconcile_holds(OwnerKind::Jobs, grace_ms, &mut |hold| {
                let state = ds.store().control_state().unwrap_or_default();
                let mut l = Ledger::new(&state);
                Ok(l.run_header(hold.id)?.is_some_and(|r| !r.state.terminal()))
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
