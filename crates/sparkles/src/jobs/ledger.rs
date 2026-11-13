//! The ledger's records and keys in the dataset's control keyspace (spec C26 §2.2), read
//! from one control state and written as the control records of one transaction.
//!
//! Every record is a value under a key that starts with `jobs/` and names its kind and
//! identifier. A value is the record version byte [`RECORD_VERSION`] followed by the
//! record's JSON, whose members are fixed by the record kind. A reader refuses a record
//! of another version or with a member it does not know, as an integrity failure that
//! stops dispatch. A run's payload, inputs and submitted checkpoint are written once
//! under a key of their own, so a transition rewrites only the run's small record. Index
//! entries, such as `jobs/state/<state>/<run>`, are written in the same frame as the
//! records they index. Times are milliseconds since the epoch in UTC.

use crate::branch::CommitRef;
use crate::store::WriteTxn;
use crate::{Error, Result};
use oxrdf::NamedNode;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sparkles_core::control::ControlState;
use sparkles_core::jobs::{MAX_INPUTS, MAX_RUNS_PER_STATE};
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

/// The most runs one scan of a state reads. Settings validation bounds the waiting runs
/// and the leases a dataset can hold to this number, so a scan that is cut short
/// reports it instead of passing an undercount off as exact.
pub(super) const MAX_SCAN: usize = MAX_RUNS_PER_STATE as usize;

/// The most characters of a failure diagnostic that are kept.
pub(super) const MAX_DIAGNOSTIC_CHARS: usize = 512;

/// The version byte of every record this build writes and reads.
pub(super) const RECORD_VERSION: u8 = 1;

/// The value of an index entry: the version byte alone.
const INDEX_VALUE: [u8; 1] = [RECORD_VERSION];

pub(super) fn run_key(id: Uuid) -> Vec<u8> {
    format!("jobs/run/{id}").into_bytes()
}

pub(super) fn input_key(id: Uuid) -> Vec<u8> {
    format!("jobs/input/{id}").into_bytes()
}

pub(super) fn attempt_key(id: Uuid) -> Vec<u8> {
    format!("jobs/attempt/{id}").into_bytes()
}

fn run_attempts_prefix(run: Uuid) -> Vec<u8> {
    format!("jobs/run-attempt/{run}/").into_bytes()
}

fn run_attempt_key(run: Uuid, attempt: Uuid) -> Vec<u8> {
    format!("jobs/run-attempt/{run}/{attempt}").into_bytes()
}

pub(super) fn publication_key(id: Uuid) -> Vec<u8> {
    format!("jobs/publication/{id}").into_bytes()
}

pub(super) fn operation_key(id: Uuid) -> Vec<u8> {
    format!("jobs/operation/{id}").into_bytes()
}

fn state_prefix(state: State) -> Vec<u8> {
    format!("jobs/state/{}/", state.as_str()).into_bytes()
}

fn state_key(state: State, run: Uuid) -> Vec<u8> {
    format!("jobs/state/{}/{run}", state.as_str()).into_bytes()
}

pub(super) const EPOCH_KEY: &[u8] = b"jobs/epoch";

pub(super) const RECOVERY_KEY: &[u8] = b"jobs/recovery";

/// Milliseconds since the epoch in UTC.
pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// IRIs as their string in a record.
mod iri {
    use oxrdf::NamedNode;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(n: &NamedNode, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(n.as_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<NamedNode, D::Error> {
        let v = String::deserialize(d)?;
        NamedNode::new(v).map_err(serde::de::Error::custom)
    }
}

/// The state of a run (spec C26 §4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Queued,
    Leased,
    RetryWait,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
}

impl State {
    pub const ALL: [State; 7] = [
        State::Queued,
        State::Leased,
        State::RetryWait,
        State::Paused,
        State::Succeeded,
        State::Failed,
        State::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Leased => "leased",
            State::RetryWait => "retry-wait",
            State::Paused => "paused",
            State::Succeeded => "succeeded",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<State> {
        State::ALL.into_iter().find(|st| st.as_str() == s)
    }

    /// Whether the state is immutable.
    pub fn terminal(self) -> bool {
        matches!(self, State::Succeeded | State::Failed | State::Cancelled)
    }
}

/// The state of an attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttemptState {
    Active,
    Published,
    Failed,
    Interrupted,
    Expired,
    Cancelled,
}

impl AttemptState {
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptState::Active => "active",
            AttemptState::Published => "published",
            AttemptState::Failed => "failed",
            AttemptState::Interrupted => "interrupted",
            AttemptState::Expired => "expired",
            AttemptState::Cancelled => "cancelled",
        }
    }
}

/// A run as the ledger records it. The payload, the inputs and the checkpoint are in the
/// run's write-once input record ([`RunInput`]) and are filled in only by a complete
/// read ([`Ledger::run`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Run {
    pub id: Uuid,
    pub definition: String,
    pub revision: String,
    pub kind: String,
    pub kind_version: u32,
    #[serde(with = "iri")]
    pub principal: NamedNode,
    pub dataset_id: Uuid,
    pub branch_id: Uuid,
    pub payload_digest: String,
    pub submitted_ms: i64,
    /// The business commit the submission's frame followed.
    pub submitted_commit: u64,
    /// The control sequence number of the submission's frame, which orders submissions
    /// and decides a restored run's membership in a recovery (spec C26 §7.3).
    pub submitted_seq: u64,
    pub state: State,
    pub version: u64,
    pub attempts: u32,
    /// The attempts and the retry horizon the run was accepted with: its retry policy
    /// stays the one of its definition revision whatever the definition says now.
    pub attempts_allowed: u32,
    pub retry_horizon_ms: u64,
    pub generation: u64,
    pub current_attempt: Option<Uuid>,
    pub next_eligible_ms: Option<i64>,
    pub reason: Option<String>,
    pub publication: Option<Uuid>,
    pub retry_of: Option<Uuid>,
    pub operation: Uuid,
    /// The recovery that paused the run, while it is paused by one.
    pub recovery: Option<Uuid>,
    #[serde(skip)]
    pub payload: String,
    #[serde(skip)]
    pub checkpoint: Option<String>,
    #[serde(skip)]
    pub inputs: Vec<CommitRef>,
}

/// The write-once part of a run: what its submission asked for.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RunInput {
    pub payload: String,
    pub checkpoint: Option<String>,
    pub inputs: Vec<CommitRef>,
}

/// An attempt as the ledger records it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Attempt {
    pub id: Uuid,
    pub run: Uuid,
    #[serde(with = "iri")]
    pub worker: NamedNode,
    pub number: u32,
    pub generation: u64,
    pub epoch: Uuid,
    pub verifier: String,
    pub claimed_ms: i64,
    pub lease_expires_ms: i64,
    pub state: AttemptState,
    pub diagnostic: Option<String>,
}

/// A publication as the ledger records it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PublicationRecord {
    pub id: Uuid,
    pub run: Uuid,
    pub attempt: Uuid,
    pub digest: String,
    /// The commit the publication made, or the head it followed when its result
    /// changed no quad and it made none (spec C26 §4.3).
    pub commit: u64,
    pub committed: bool,
    pub inserted: u64,
    pub deleted: u64,
    pub checkpoint: Option<String>,
    pub published_ms: i64,
}

/// A submission operation: the run it created and the digest of its normalized request.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Operation {
    pub run: Uuid,
    pub request_digest: String,
    pub idempotency_key: String,
    pub operation_kind: String,
    #[serde(with = "iri")]
    pub principal: NamedNode,
    pub definition: String,
    pub revision: String,
}

/// The boot epoch the last process that opened the dataset recorded.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Epoch {
    pub epoch: Uuid,
    pub installed_ms: i64,
}

/// The phase of a recovery of restored work (spec C26 §7.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryPhase {
    /// The restored runs are being paused in bounded frames.
    Pausing,
    /// Every restored run is paused, and an operator decides what resumes.
    AwaitingDecision,
}

/// A `job:Recovery` record: the capture the dataset was restored from and where the
/// pause of its work stands.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Recovery {
    pub id: Uuid,
    /// The dataset the capture was taken of.
    pub source_dataset: Uuid,
    pub capture_commit: u64,
    pub capture_label: Option<String>,
    pub capture_ms: i64,
    /// The control sequence number of the restored checkpoint: the runs submitted at or
    /// below it are the restored work.
    pub checkpoint_seq: u64,
    pub phase: RecoveryPhase,
    pub started_ms: i64,
    /// The runs paused so far, and those an operator resumed or cancelled.
    pub paused: u64,
    pub resumed: u64,
    pub cancelled: u64,
}

/// The redacted state of a run for inspection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    pub id: Uuid,
    pub definition: String,
    pub revision: String,
    pub kind: String,
    pub kind_version: u32,
    pub principal: String,
    pub state: State,
    pub version: u64,
    pub attempts: u32,
    pub generation: u64,
    pub submitted_ms: i64,
    pub submitted_commit: u64,
    pub payload_digest: String,
    /// The submission operation, derived from the idempotency key's scope.
    pub operation: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
    pub inputs: Vec<CommitRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_eligible_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<AttemptView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication: Option<PublicationView>,
}

/// The redacted state of an attempt. It never carries the token or its verifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptView {
    pub id: Uuid,
    pub worker: String,
    pub number: u32,
    pub generation: u64,
    pub epoch: Uuid,
    pub state: AttemptState,
    pub claimed_ms: i64,
    pub lease_expires_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

/// A recorded publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationView {
    pub id: Uuid,
    pub attempt: Uuid,
    pub digest: String,
    /// The commit the publication made, or the head it followed when it made none.
    pub commit: u64,
    /// Whether the publication made a commit. One whose result changed no quad makes
    /// none.
    pub committed: bool,
    pub inserted: u64,
    pub deleted: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
    pub published_ms: i64,
}

impl Run {
    pub(super) fn view(
        &self,
        attempt: Option<&Attempt>,
        publication: Option<&PublicationRecord>,
    ) -> RunView {
        RunView {
            id: self.id,
            definition: self.definition.clone(),
            revision: self.revision.clone(),
            kind: self.kind.clone(),
            kind_version: self.kind_version,
            principal: self.principal.as_str().to_string(),
            state: self.state,
            version: self.version,
            attempts: self.attempts,
            generation: self.generation,
            submitted_ms: self.submitted_ms,
            submitted_commit: self.submitted_commit,
            payload_digest: self.payload_digest.clone(),
            operation: self.operation,
            checkpoint: self.checkpoint.clone(),
            inputs: self.inputs.clone(),
            next_eligible_ms: self.next_eligible_ms,
            reason: self.reason.clone(),
            retry_of: self.retry_of,
            attempt: attempt.map(|a| AttemptView {
                id: a.id,
                worker: a.worker.as_str().to_string(),
                number: a.number,
                generation: a.generation,
                epoch: a.epoch,
                state: a.state,
                claimed_ms: a.claimed_ms,
                lease_expires_ms: a.lease_expires_ms,
                diagnostic: a.diagnostic.clone(),
            }),
            publication: publication.map(|p| PublicationView {
                id: p.id,
                attempt: p.attempt,
                digest: p.digest.clone(),
                commit: p.commit,
                committed: p.committed,
                inserted: p.inserted,
                deleted: p.deleted,
                checkpoint: p.checkpoint.clone(),
                published_ms: p.published_ms,
            }),
        }
    }
}

fn corrupt(key: &[u8], what: &str) -> Error {
    Error::Corrupt(format!(
        "the job ledger record {} is damaged: {what}; dispatch is refused until it is repaired",
        String::from_utf8_lossy(key)
    ))
}

/// The value of a record.
pub(super) fn encode<T: Serialize>(record: &T) -> Arc<[u8]> {
    let mut out = vec![RECORD_VERSION];
    serde_json::to_writer(&mut out, record).expect("a ledger record serializes");
    out.into()
}

fn decode<T: DeserializeOwned>(key: &[u8], value: &[u8]) -> Result<T> {
    match value.split_first() {
        Some((&RECORD_VERSION, json)) => {
            serde_json::from_slice(json).map_err(|e| corrupt(key, &e.to_string()))
        }
        Some((v, _)) => Err(corrupt(key, &format!("record version {v} is unknown"))),
        None => Err(corrupt(key, "the value is empty")),
    }
}

/// The run id at the end of an index key.
fn indexed(key: &[u8], value: &[u8], prefix: &[u8]) -> Result<Uuid> {
    if value != INDEX_VALUE {
        return Err(corrupt(key, "the index entry's version is unknown"));
    }
    std::str::from_utf8(&key[prefix.len()..])
        .ok()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| corrupt(key, "the index key does not end in a UUID"))
}

/// The runs one scan of a state found.
pub(super) struct Scan {
    pub ids: Vec<Uuid>,
    /// Whether the ledger holds more runs in the state than [`MAX_SCAN`], so the ids
    /// are a prefix and their count is not the state's count.
    pub truncated: bool,
}

/// Reads of the ledger at one control state.
pub(super) struct Ledger<'a> {
    state: &'a ControlState,
}

impl<'a> Ledger<'a> {
    pub fn new(state: &'a ControlState) -> Ledger<'a> {
        Ledger { state }
    }

    fn get<T: DeserializeOwned>(&self, key: &[u8]) -> Result<Option<T>> {
        self.state.get(key).map(|v| decode(key, v)).transpose()
    }

    /// The ids of the runs in `state`, up to [`MAX_SCAN`], and whether the ledger holds
    /// more of them than the scan read.
    pub fn runs_in(&mut self, state: State) -> Result<Scan> {
        let prefix = state_prefix(state);
        let mut ids = Vec::new();
        let mut truncated = false;
        for (k, v) in self.state.prefix(&prefix) {
            if ids.len() == MAX_SCAN {
                truncated = true;
                break;
            }
            ids.push(indexed(k, v, &prefix)?);
        }
        Ok(Scan { ids, truncated })
    }

    /// The complete record of run `id`, with its payload, inputs and checkpoint: the
    /// checkpoint of its publication when it published one, and the submitted one
    /// otherwise.
    pub fn run(&mut self, id: Uuid) -> Result<Option<Run>> {
        let Some(mut run) = self.run_header(id)? else {
            return Ok(None);
        };
        let key = input_key(id);
        let input: RunInput = self
            .get(&key)?
            .ok_or_else(|| corrupt(&key, "the run's input record is missing"))?;
        if input.inputs.len() > MAX_INPUTS {
            return Err(corrupt(
                &key,
                &format!("more than {MAX_INPUTS} inputs are recorded"),
            ));
        }
        run.payload = input.payload;
        run.inputs = input.inputs;
        run.checkpoint = input.checkpoint;
        if let Some(p) = run.publication
            && let Some(p) = self.publication(p)?
            && p.checkpoint.is_some()
        {
            run.checkpoint = p.checkpoint;
        }
        Ok(Some(run))
    }

    /// The record of run `id` without its payload, checkpoint and inputs, which a
    /// scheduling decision over many runs does not need and must not retain. `payload`
    /// is empty, `checkpoint` is `None` and `inputs` holds nothing in the result.
    pub fn run_header(&mut self, id: Uuid) -> Result<Option<Run>> {
        let key = run_key(id);
        let run: Option<Run> = self.get(&key)?;
        if let Some(r) = &run
            && r.id != id
        {
            return Err(corrupt(&key, "the record names another run"));
        }
        Ok(run)
    }

    pub fn attempt(&mut self, id: Uuid) -> Result<Option<Attempt>> {
        let key = attempt_key(id);
        let a: Option<Attempt> = self.get(&key)?;
        if let Some(a) = &a
            && a.id != id
        {
            return Err(corrupt(&key, "the record names another attempt"));
        }
        Ok(a)
    }

    pub fn publication(&mut self, id: Uuid) -> Result<Option<PublicationRecord>> {
        let key = publication_key(id);
        let p: Option<PublicationRecord> = self.get(&key)?;
        if let Some(p) = &p
            && p.id != id
        {
            return Err(corrupt(&key, "the record names another publication"));
        }
        Ok(p)
    }

    /// The run and the normalized request digest a submission operation recorded.
    pub fn operation(&mut self, id: Uuid) -> Result<Option<(Uuid, String)>> {
        Ok(self
            .get::<Operation>(&operation_key(id))?
            .map(|o| (o.run, o.request_digest)))
    }

    /// Whether `principal` worked on any attempt of run `run`, current or earlier.
    pub fn worked_on(&mut self, run: Uuid, principal: &NamedNode) -> Result<bool> {
        let prefix = run_attempts_prefix(run);
        let ids: Vec<Uuid> = self
            .state
            .prefix(&prefix)
            .take(MAX_SCAN)
            .map(|(k, v)| indexed(k, v, &prefix))
            .collect::<Result<_>>()?;
        for id in ids {
            if self.attempt(id)?.is_some_and(|a| &a.worker == principal) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The boot epoch the ledger records, if any process recorded one.
    pub fn epoch(&mut self) -> Result<Option<Uuid>> {
        Ok(self.get::<Epoch>(EPOCH_KEY)?.map(|e| e.epoch))
    }

    /// The recovery of restored work, while one is open.
    pub fn recovery(&mut self) -> Result<Option<Recovery>> {
        self.get(RECOVERY_KEY)
    }
}

/// The records one transition writes, staged by kind and written as the control
/// records of its transaction by [`apply`](Changes::apply).
#[derive(Default)]
pub(super) struct Changes {
    /// changed runs, with the state their base record had (`None` for a new run)
    runs: BTreeMap<Uuid, (Option<State>, Run)>,
    attempts: BTreeMap<Uuid, (bool, Attempt)>,
    other: BTreeMap<Vec<u8>, Option<Arc<[u8]>>>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty() && self.attempts.is_empty() && self.other.is_empty()
    }

    /// The staged copy of `base`, created from it on first use.
    pub fn run(&mut self, base: &Run) -> &mut Run {
        &mut self
            .runs
            .entry(base.id)
            .or_insert_with(|| (Some(base.state), base.clone()))
            .1
    }

    /// A new run and its write-once input record.
    pub fn new_run(&mut self, run: Run, input: &RunInput) {
        self.other.insert(input_key(run.id), Some(encode(input)));
        self.runs.insert(run.id, (None, run));
    }

    /// The staged copy of `base`, created from it on first use.
    pub fn attempt(&mut self, base: &Attempt) -> &mut Attempt {
        &mut self
            .attempts
            .entry(base.id)
            .or_insert_with(|| (false, base.clone()))
            .1
    }

    pub fn new_attempt(&mut self, a: Attempt) {
        self.attempts.insert(a.id, (true, a));
    }

    pub fn publication(&mut self, p: &PublicationRecord) {
        self.other.insert(publication_key(p.id), Some(encode(p)));
    }

    pub fn operation(&mut self, id: Uuid, o: &Operation) {
        self.other.insert(operation_key(id), Some(encode(o)));
    }

    pub fn epoch(&mut self, e: &Epoch) {
        self.other.insert(EPOCH_KEY.to_vec(), Some(encode(e)));
    }

    pub fn recovery(&mut self, r: Option<&Recovery>) {
        self.other.insert(RECOVERY_KEY.to_vec(), r.map(encode));
    }

    /// Stage the records in `t`, with the index entries of every run whose state
    /// changed and of every new attempt.
    pub fn apply(self, t: &mut WriteTxn<'_>) -> Result<()> {
        let index: Arc<[u8]> = INDEX_VALUE.as_slice().into();
        for (id, (base, run)) in self.runs {
            t.control_put(&run_key(id), encode(&run))?;
            if base != Some(run.state) {
                if let Some(b) = base {
                    t.control_delete(&state_key(b, id))?;
                }
                t.control_put(&state_key(run.state, id), index.clone())?;
            }
        }
        for (id, (new, a)) in self.attempts {
            t.control_put(&attempt_key(id), encode(&a))?;
            if new {
                t.control_put(&run_attempt_key(a.run, id), index.clone())?;
            }
        }
        for (k, v) in self.other {
            match v {
                Some(v) => t.control_put(&k, v)?,
                None => t.control_delete(&k)?,
            }
        }
        Ok(())
    }
}

/// `text` without control characters, cut to [`MAX_DIAGNOSTIC_CHARS`].
pub(super) fn redacted(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_DIAGNOSTIC_CHARS)
        .collect()
}
