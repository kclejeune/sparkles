//! The ledger vocabulary and record shapes, read under a snapshot and written as the
//! quads of one transaction.
//!
//! Every record is a subject in [`LEDGER_GRAPH`] under the `job:` namespace. Mutable
//! members of a run and an attempt are single-valued triples that a transition replaces
//! in one commit. Times are `xsd:integer` milliseconds since the epoch in UTC. Each
//! record carries the ledger graph's protection nonce, so a reader can tell a record
//! written under the current protection from one planted while the graph was detached.

use crate::access::Preparation;
use crate::branch::CommitRef;
use crate::store::{Snapshot, WriteTxn};
use crate::{Error, Result};
use oxrdf::{GraphName, Literal, NamedNode, Quad, Term};
use sparkles_core::evidence::write::Reads;
use sparkles_core::jobs::{LEDGER_GRAPH, MAX_INPUTS, MAX_RUNS_PER_STATE, NAMESPACE};
use uuid::Uuid;

/// The memory a ledger read may use for its lookups.
pub(super) const READS_BYTES: u64 = 16 << 20;

/// The most runs one scan of a state reads. Settings validation bounds the waiting runs
/// and the leases a dataset can hold to this number, so a scan that is cut short
/// reports it instead of passing an undercount off as exact.
pub(super) const MAX_SCAN: usize = MAX_RUNS_PER_STATE as usize;

/// The most characters of a failure diagnostic that are kept.
pub(super) const MAX_DIAGNOSTIC_CHARS: usize = 512;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";

pub(super) fn job(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{NAMESPACE}{local}"))
}

pub(super) fn graph() -> NamedNode {
    NamedNode::new_unchecked(LEDGER_GRAPH)
}

pub(super) fn run_iri(id: Uuid) -> NamedNode {
    job(&format!("run:{id}"))
}

pub(super) fn attempt_iri(id: Uuid) -> NamedNode {
    job(&format!("attempt:{id}"))
}

pub(super) fn publication_iri(id: Uuid) -> NamedNode {
    job(&format!("publication:{id}"))
}

pub(super) fn operation_iri(id: Uuid) -> NamedNode {
    job(&format!("operation:{id}"))
}

pub(super) fn epoch_iri() -> NamedNode {
    job("epoch")
}

pub(super) fn string(v: &str) -> Term {
    Literal::new_simple_literal(v).into()
}

pub(super) fn integer(v: u64) -> Term {
    Literal::new_typed_literal(v.to_string(), NamedNode::new_unchecked(XSD_INTEGER)).into()
}

pub(super) fn ms(v: i64) -> Term {
    Literal::new_typed_literal(v.to_string(), NamedNode::new_unchecked(XSD_INTEGER)).into()
}

/// Milliseconds since the epoch in UTC.
pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn text(t: &Term) -> Option<String> {
    match t {
        Term::Literal(l) => Some(l.value().to_string()),
        Term::NamedNode(n) => Some(n.as_str().to_string()),
        _ => None,
    }
}

/// The state of a run (spec C26 §4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

    fn parse(s: &str) -> Option<AttemptState> {
        [
            AttemptState::Active,
            AttemptState::Published,
            AttemptState::Failed,
            AttemptState::Interrupted,
            AttemptState::Expired,
            AttemptState::Cancelled,
        ]
        .into_iter()
        .find(|st| st.as_str() == s)
    }
}

/// A run as the ledger records it.
#[derive(Clone, Debug)]
pub(super) struct Run {
    pub id: Uuid,
    pub definition: String,
    pub revision: String,
    pub kind: String,
    pub kind_version: u32,
    pub principal: NamedNode,
    pub payload: String,
    pub payload_digest: String,
    pub checkpoint: Option<String>,
    pub inputs: Vec<CommitRef>,
    pub submitted_ms: i64,
    pub submitted_commit: u64,
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
    pub protection: Option<Uuid>,
}

/// An attempt as the ledger records it.
#[derive(Clone, Debug)]
pub(super) struct Attempt {
    pub id: Uuid,
    pub run: Uuid,
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
#[derive(Clone, Debug)]
pub(super) struct PublicationRecord {
    pub id: Uuid,
    pub run: Uuid,
    pub attempt: Uuid,
    pub digest: String,
    pub commit: u64,
    pub inserted: u64,
    pub deleted: u64,
    pub checkpoint: Option<String>,
    pub published_ms: i64,
}

/// The redacted state of a run for inspection.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
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

/// A committed publication.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationView {
    pub id: Uuid,
    pub attempt: Uuid,
    pub digest: String,
    pub commit: u64,
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
                inserted: p.inserted,
                deleted: p.deleted,
                checkpoint: p.checkpoint.clone(),
                published_ms: p.published_ms,
            }),
        }
    }
}

fn corrupt(what: &str, s: &str) -> Error {
    Error::Corrupt(format!(
        "the job ledger record <{s}> is damaged: {what}; dispatch is refused until it is repaired"
    ))
}

/// The runs one scan of a state found.
pub(super) struct Scan {
    pub ids: Vec<Uuid>,
    /// Whether the ledger holds more runs in the state than [`MAX_SCAN`], so the ids
    /// are a prefix and their count is not the state's count.
    pub truncated: bool,
}

/// Reads of the ledger graph at one snapshot.
pub(super) struct Ledger<'a> {
    reads: Reads<'a>,
}

impl<'a> Ledger<'a> {
    pub fn new(preparation: &'a Preparation, snap: &'a Snapshot) -> Ledger<'a> {
        Ledger {
            reads: Reads::new(preparation, snap),
        }
    }

    fn objects(&mut self, s: &str, p: &str, limit: usize) -> Result<Vec<Term>> {
        Ok(self
            .reads
            .objects_up_to(s, &job(p).into_string(), LEDGER_GRAPH, limit)?
            .0)
    }

    fn single(&mut self, s: &str, p: &str) -> Result<Option<Term>> {
        let mut v = self.objects(s, p, 2)?;
        match v.len() {
            1 => Ok(v.pop()),
            0 => Ok(None),
            _ => Err(corrupt(&format!("{p} has several values"), s)),
        }
    }

    pub fn str(&mut self, s: &str, p: &str) -> Result<Option<String>> {
        Ok(self.single(s, p)?.as_ref().and_then(text))
    }

    pub fn int(&mut self, s: &str, p: &str) -> Result<Option<u64>> {
        match self.str(s, p)? {
            None => Ok(None),
            Some(v) => v
                .parse()
                .map(Some)
                .map_err(|_| corrupt(&format!("{p} is not an integer"), s)),
        }
    }

    pub fn ms(&mut self, s: &str, p: &str) -> Result<Option<i64>> {
        match self.str(s, p)? {
            None => Ok(None),
            Some(v) => v
                .parse()
                .map(Some)
                .map_err(|_| corrupt(&format!("{p} is not a time"), s)),
        }
    }

    pub fn uuid(&mut self, s: &str, p: &str) -> Result<Option<Uuid>> {
        match self.str(s, p)? {
            None => Ok(None),
            Some(v) => Uuid::parse_str(&v)
                .map(Some)
                .map_err(|_| corrupt(&format!("{p} is not a UUID"), s)),
        }
    }

    pub fn iri(&mut self, s: &str, p: &str) -> Result<Option<NamedNode>> {
        match self.single(s, p)? {
            Some(Term::NamedNode(n)) => Ok(Some(n)),
            None => Ok(None),
            Some(_) => Err(corrupt(&format!("{p} is not an IRI"), s)),
        }
    }

    fn required_str(&mut self, s: &str, p: &str) -> Result<String> {
        self.str(s, p)?
            .ok_or_else(|| corrupt(&format!("{p} is missing"), s))
    }

    fn required_int(&mut self, s: &str, p: &str) -> Result<u64> {
        self.int(s, p)?
            .ok_or_else(|| corrupt(&format!("{p} is missing"), s))
    }

    fn required_ms(&mut self, s: &str, p: &str) -> Result<i64> {
        self.ms(s, p)?
            .ok_or_else(|| corrupt(&format!("{p} is missing"), s))
    }

    fn is_a(&mut self, s: &str, class: &str) -> Result<bool> {
        self.reads
            .contains(s, RDF_TYPE, &job(class).into(), LEDGER_GRAPH)
    }

    /// The subjects whose `p` is `o`, up to `limit`, and whether more exist.
    pub fn subjects(&mut self, p: &str, o: &Term, limit: usize) -> Result<(Vec<NamedNode>, bool)> {
        self.reads
            .subjects_up_to(&job(p).into_string(), o, LEDGER_GRAPH, limit)
    }

    /// The ids of the runs in `state`, up to [`MAX_SCAN`], and whether the ledger holds
    /// more of them than the scan read.
    pub fn runs_in(&mut self, state: State) -> Result<Scan> {
        let (subjects, truncated) = self.subjects("state", &string(state.as_str()), MAX_SCAN)?;
        let prefix = format!("{NAMESPACE}run:");
        Ok(Scan {
            ids: subjects
                .iter()
                .filter_map(|s| s.as_str().strip_prefix(&prefix))
                .filter_map(|id| Uuid::parse_str(id).ok())
                .collect(),
            truncated,
        })
    }

    /// The complete record of run `id`, with its payload and inputs.
    pub fn run(&mut self, id: Uuid) -> Result<Option<Run>> {
        self.read_run(id, true)
    }

    /// The record of run `id` without its payload, checkpoint and inputs, which a
    /// scheduling decision over many runs does not need and must not retain. `payload`
    /// is empty, `checkpoint` is `None` and `inputs` holds nothing in the result.
    pub fn run_header(&mut self, id: Uuid) -> Result<Option<Run>> {
        self.read_run(id, false)
    }

    fn read_run(&mut self, id: Uuid, complete: bool) -> Result<Option<Run>> {
        let iri = run_iri(id);
        let s = iri.as_str();
        if !self.is_a(s, "Run")? {
            return Ok(None);
        }
        let state = self.required_str(s, "state")?;
        let state = State::parse(&state).ok_or_else(|| corrupt("state is unknown", s))?;
        let principal = self
            .iri(s, "principal")?
            .ok_or_else(|| corrupt("principal is missing", s))?;
        let (payload, inputs) = if complete {
            let inputs = self.objects(s, "input", MAX_INPUTS + 1)?;
            if inputs.len() > MAX_INPUTS {
                return Err(corrupt(
                    &format!("more than {MAX_INPUTS} inputs are recorded"),
                    s,
                ));
            }
            let inputs = inputs
                .iter()
                .filter_map(text)
                .map(|v| {
                    let (b, seq) = v
                        .split_once('@')
                        .ok_or_else(|| corrupt("input is not branch@seq", s))?;
                    Ok(CommitRef {
                        branch_id: Uuid::parse_str(b).map_err(|_| corrupt("input branch", s))?,
                        seq: seq.parse().map_err(|_| corrupt("input seq", s))?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            (self.required_str(s, "payload")?, inputs)
        } else {
            (String::new(), Vec::new())
        };
        let operation = self
            .uuid(s, "operation")?
            .ok_or_else(|| corrupt("operation is missing", s))?;
        Ok(Some(Run {
            id,
            definition: self.required_str(s, "definition")?,
            revision: self.required_str(s, "definitionRevision")?,
            kind: self.required_str(s, "kind")?,
            kind_version: self.required_int(s, "kindVersion")? as u32,
            principal,
            payload,
            payload_digest: self.required_str(s, "payloadDigest")?,
            checkpoint: if complete {
                self.str(s, "sourceCheckpoint")?
            } else {
                None
            },
            inputs,
            submitted_ms: self.required_ms(s, "submittedAtMs")?,
            submitted_commit: self.required_int(s, "submittedCommit")?,
            state,
            version: self.required_int(s, "version")?,
            attempts: self.required_int(s, "attempts")? as u32,
            attempts_allowed: self.required_int(s, "attemptsAllowed")? as u32,
            retry_horizon_ms: self.required_int(s, "retryHorizonMs")?,
            generation: self.required_int(s, "generation")?,
            current_attempt: self.uuid(s, "currentAttempt")?,
            next_eligible_ms: self.ms(s, "nextEligibleAtMs")?,
            reason: self.str(s, "reason")?,
            publication: self.uuid(s, "publication")?,
            retry_of: self.uuid(s, "retryOf")?,
            operation,
            protection: self.uuid(s, "graphProtection")?,
        }))
    }

    pub fn attempt(&mut self, id: Uuid) -> Result<Option<Attempt>> {
        let iri = attempt_iri(id);
        let s = iri.as_str();
        if !self.is_a(s, "Attempt")? {
            return Ok(None);
        }
        let state = self.required_str(s, "attemptState")?;
        let state = AttemptState::parse(&state).ok_or_else(|| corrupt("attemptState", s))?;
        Ok(Some(Attempt {
            id,
            run: self
                .uuid(s, "run")?
                .ok_or_else(|| corrupt("run is missing", s))?,
            worker: self
                .iri(s, "worker")?
                .ok_or_else(|| corrupt("worker is missing", s))?,
            number: self.required_int(s, "number")? as u32,
            generation: self.required_int(s, "generation")?,
            epoch: self
                .uuid(s, "epoch")?
                .ok_or_else(|| corrupt("epoch is missing", s))?,
            verifier: self.required_str(s, "tokenVerifier")?,
            claimed_ms: self.required_ms(s, "claimedAtMs")?,
            lease_expires_ms: self.required_ms(s, "leaseExpiresAtMs")?,
            state,
            diagnostic: self.str(s, "diagnostic")?,
        }))
    }

    pub fn publication(&mut self, id: Uuid) -> Result<Option<PublicationRecord>> {
        let iri = publication_iri(id);
        let s = iri.as_str();
        if !self.is_a(s, "Publication")? {
            return Ok(None);
        }
        Ok(Some(PublicationRecord {
            id,
            run: self
                .uuid(s, "run")?
                .ok_or_else(|| corrupt("run is missing", s))?,
            attempt: self
                .uuid(s, "attempt")?
                .ok_or_else(|| corrupt("attempt is missing", s))?,
            digest: self.required_str(s, "payloadDigest")?,
            commit: self.required_int(s, "commit")?,
            inserted: self.required_int(s, "inserted")?,
            deleted: self.required_int(s, "deleted")?,
            checkpoint: self.str(s, "sourceCheckpoint")?,
            published_ms: self.required_ms(s, "publishedAtMs")?,
        }))
    }

    /// The run and the normalized request digest a submission operation recorded.
    pub fn operation(&mut self, id: Uuid) -> Result<Option<(Uuid, String)>> {
        let iri = operation_iri(id);
        let s = iri.as_str();
        if !self.is_a(s, "Operation")? {
            return Ok(None);
        }
        let run = self
            .uuid(s, "run")?
            .ok_or_else(|| corrupt("run is missing", s))?;
        let digest = self.required_str(s, "requestDigest")?;
        Ok(Some((run, digest)))
    }

    /// Whether `principal` worked on any attempt of run `run`, current or earlier.
    pub fn worked_on(&mut self, run: Uuid, principal: &NamedNode) -> Result<bool> {
        let (subjects, _) = self.subjects("run", &string(&run.to_string()), MAX_SCAN)?;
        let prefix = format!("{NAMESPACE}attempt:");
        for s in &subjects {
            if !s.as_str().starts_with(&prefix) {
                continue;
            }
            if self.iri(s.as_str(), "worker")?.as_ref() == Some(principal) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The boot epoch the ledger records, if any process recorded one.
    pub fn epoch(&mut self) -> Result<Option<Uuid>> {
        let iri = epoch_iri();
        self.uuid(iri.as_str(), "epoch")
    }
}

/// The quads one transition adds to and removes from the ledger graph.
#[derive(Default)]
pub(super) struct Changes {
    insert: Vec<Quad>,
    delete: Vec<Quad>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.insert.is_empty() && self.delete.is_empty()
    }

    pub fn add(&mut self, s: &NamedNode, p: &str, o: Term) {
        self.insert.push(Quad::new(
            s.clone(),
            job(p),
            o,
            GraphName::NamedNode(graph()),
        ));
    }

    pub fn add_type(&mut self, s: &NamedNode, class: &str) {
        self.insert.push(Quad::new(
            s.clone(),
            NamedNode::new_unchecked(RDF_TYPE),
            job(class),
            GraphName::NamedNode(graph()),
        ));
    }

    /// Replace every value of `p` on `s` with `o`, or remove them all.
    pub fn set(
        &mut self,
        ledger: &mut Ledger<'_>,
        s: &NamedNode,
        p: &str,
        o: Option<Term>,
    ) -> Result<()> {
        for old in ledger.objects(s.as_str(), p, 64)? {
            self.delete.push(Quad::new(
                s.clone(),
                job(p),
                old,
                GraphName::NamedNode(graph()),
            ));
        }
        if let Some(o) = o {
            self.add(s, p, o);
        }
        Ok(())
    }

    pub fn apply(self, t: &mut WriteTxn<'_>) -> Result<()> {
        let mut labels = std::collections::HashMap::new();
        for q in &self.delete {
            let ids = t.encode_quad(q, &mut labels)?;
            t.delete(ids)?;
        }
        for q in &self.insert {
            let ids = t.encode_quad(q, &mut labels)?;
            t.insert(ids)?;
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
