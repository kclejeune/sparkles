# C26: Durable jobs and delivery

> **Status:** Phase 1 shipped for the `quads` kind.
>
> **Phases:** Phase 1 adds durable jobs, fenced worker leases, atomic publication and
> checkpoints through the library and HTTP. Phase 2 adds the delivery outbox, recurring
> collection and recovery controls. Phase 3 integrates registered maintenance jobs,
> bindings and the UI.
>
> **User docs:** [AI and agents guide](../ai/README.md#durable-jobs) and the
> [HTTP API reference](../API.md#durable-jobs).
>
> The [Outcome](#outcome) section records implementation when it ships.

This design extends [CI](CI-commit-identity.md), [P06](P06-library-admin-api.md),
[C21](C21-outbound-notifications.md) and [C23](C23-batched-updates-and-triggers.md).
[C25](C25-evidence-and-assessments.md) supplies evidence records,
[F22](F22-analysis-lifecycle.md) supplies analysis dependencies and publication rules,
and [C27](C27-valid-time-profiles.md) supplies validity semantics. Sources are public
HTTP and webhook specifications and the Sparkles transaction, task and backup code.

## 1. Goals and boundary

Programs collecting changing sources or maintaining derived information need to remember
what work was accepted, which inputs it used and whether its effects were committed.
A process-local task record cannot provide this after a restart. A notification sent
after an ordinary update can also be lost between that update and the send.

The feature adds a durable job ledger and an outbox under each dataset. It supports
registered built-in work and external workers using bounded, typed requests. It does
not execute submitted shell commands or install executable code. Domain collectors,
parsers and predictive models remain external programs.

The feature guarantees atomic database publication and at-least-once attempts. It does
not promise exactly-once execution or exactly-once effects on an external service.
It does not coordinate transactions across datasets, branches or remote services.
An external worker must describe whether its external operations are safe to retry.

Existing `/$/tasks` records in `state.rs` are kept in process memory and have IDs from
a process-local counter. They retain their current contract. A durable job has a UUID
and can link its present attempt to a P06 task, whose cancellation and progress controls
are reused. Restart does not make an old task ID a durable job identity.

## 2. Ownership and stored state

### 2.1 One transaction boundary

Jobs and their outbox entries are ordinary RDF records in reserved graphs using the
`job:` namespace `urn:x-sparkles:jobs:`. They are stored in the same dataset and branch
as the application changes they publish. A typed publisher stages both kinds of changes
in one actual `WriteTxn`, then uses the existing durable commit path. There is no second
queue database or independent queue journal to synchronize with the store's WAL.

Phase 1 manages jobs on `main`. A branch can be an input, identified by its stable UUID,
but publishing into it and updating a control record on `main` is refused. Later branch
jobs must keep their ledger and effects in that branch. A merge never copies active
leases or causes a delivery. Preview analysis is inert until an explicit publication
creates a job or result on the target branch.

Lease, retry and checkpoint mutations therefore create normal commits. This increases
commit and history volume. Progress reports are process-local and coalesced, and a
worker renews its lease infrequently rather than persisting every row. A rebuildable
ready-time index avoids repeatedly querying the entire ledger. The cost must be measured
before tuning checkpoint frequency or introducing a different authoritative format.

The dispatcher reads only committed ledger states. It cannot see an outbox entry before
the transaction containing its business changes has durably committed. The writer is
never held across a network call, a model request or execution of an external worker.

### 2.2 Records

| Record | Required content |
|---|---|
| `job:Definition` | A stable ID, registered kind and version, effective principal, target dataset and branch UUIDs, input selectors, retry policy and declared effects. |
| `job:Run` | A run UUID, definition revision, state, payload digest, source checkpoint, exact input references, attempt count and budgets. |
| `job:Attempt` | A run reference, worker identity, durable lease generation, boot epoch, lease expiry, attempt state and redacted diagnostic. |
| `job:Publication` | An operation UUID, run and attempt references, payload digest, resulting checkpoint and the committed result references. |
| `job:Delivery` | A stable event UUID, destination reference, payload digest, evidence visibility footprint, delivery state, attempt count and next eligible time. |
| `job:Recovery` | The source dataset identity, capture reference, paused state and recorded operator decision for resuming work or delivery. |

Publication records are immutable. A run and its current lease are mutable state with
compare-and-set versions. Attempt and transition records form a bounded audit history.
Job state, payload fields and destination configuration do not contain secret values.
They hold references to the existing secret resolver and immutable external artifacts.

Every input reference includes dataset UUID, branch UUID, commit selector and artifact
or definition digest where applicable. A mutable name alone is insufficient. The
consumer pins required input snapshots before accepting a run and releases them only
after no retry can need them. If a referenced state cannot be retained, acceptance fails
without claiming that the work is queued.

### 2.3 Protected control graphs

Only the typed jobs API can mutate reserved control graphs. Ordinary SPARQL Update,
Graph Store writes, imports, RDF Patch, mapping output and merges reject such mutations,
even when an endpoint principal has wildcard data-write grants. The core internal
publisher capability permits only validated job transitions for the owning dataset.

The public query, graph listing, history, diff, dump, export and reasoning surfaces
exclude these graphs unless the typed inspection API explicitly renders a redacted
record. Aggregate counts and change events follow that exclusion too. The backup and
offline integrity paths use a distinct trusted capture capability. External workers
never receive that capability or the raw control snapshot.

#### Where the exclusion is enforced

Phase 1 enforces the exclusion per surface. The registry's owner entry names the hidden
graphs, the store answers them through `Store::hidden_graphs`, and every read surface
subtracts them from the caller's graph view, through `Dataset::hide_owner_graphs`, the
query defaults or the principal's per-dataset hidden graphs. Surfaces that report counts
redact them under `Principal::partial_view`. Three review rounds each found surfaces that
read a snapshot without this subtraction, so three structural alternatives were compared.

The first alternative masks the snapshot itself. `Store::snapshot` and the historical
and branch snapshot accessors would return the snapshot with the ledger's quads taken out
as delta deletions, the mechanism that triple-level access views already use
(`access::triples::apply`). A trusted accessor would return the raw snapshot to the
writer, compaction, merge, backup, integrity and the jobs owner. Every quad-reading
surface, including statistics, listings, export and library callers of `Dataset::store`,
would then exclude the ledger without its own code. This alternative fits the current
model, but it is not adopted. About 600 snapshot reads inside the core would each have to
be classified as raw or public, and a writer that took a masked base would commit the
mask's deletions as real ones. The masked delta holds every ledger quad, so a snapshot
costs work proportional to the ledger at each commit and every scan of a predicate the
ledger shares, such as `rdf:type`, merges a large deletion set. A masked snapshot also
turns off predicate-pattern planning and some result caches, which would slow every query
on a dataset with jobs. Finally, `GraphAccess::masked` leaves an already masked snapshot
unchanged, so triple-level protections would need a composed mask first, or they would
silently stop applying. Commit counts, the change log and history are not snapshot reads
and would still need their per-surface redaction.

The second alternative keeps the ledger out of the RDF index. Ledger records would be
stored in a separate keyed control space of the same store, written in the same log
record and checkpoint as the business quads and read only through the typed jobs API and
the trusted capture path. Query, statistics, listings, export, indexes, history and
commit counts would never see them, and no read pays for them. It is the structural
answer, but it changes this specification's model. Section 2.1 stores jobs as RDF records
in reserved graphs, and the log record, checkpoint, backup and reader 9 formats would all
change. It also has to be coordinated with the storage format cutoff of X16.

The third alternative is a read-view type at the library and server boundary that pairs a
snapshot with the effective graph view and is the only way facades obtain a snapshot.
`Dataset::store` stays public, so raw library reads would keep depending on the caller,
and counting surfaces that read a snapshot's sizes would still need their own redaction.
It narrows the per-surface work without closing it.

The second alternative is the decided direction. Roadmap item 905 builds it after item
479 sets the format cutoff, and it amends section 2.1 and the log record, checkpoint,
backup and reader formats. Until then the per-surface exclusion stays, every surface a
review finds gets a regression test, and the library's raw `Store` remains a trusted
interface whose callers supply the exclusion, as roadmap item 902 records.

Business effects still pass the worker run's current effective data grants, write guard,
quotas and protected-branch checks. Internal access to job records cannot authorize a
business quad. The proposed internal publisher is not a general write bypass.

## 3. Submission and idempotency

`POST /$/jobs/{ds}` accepts a registered kind, typed payload, input selectors, optional
source checkpoint and an idempotency key. Definition creation and destination changes
require dataset administration. Submitting a known definition requires the capability
that definition declares, plus the relevant Query or Update permissions.

An idempotency key is scoped to dataset UUID, branch UUID, effective principal, definition
revision and operation kind. Its UTF-8 representation is limited to 256 bytes. The
server stores a digest over a normalized typed payload, input references and effect
scope. JSON whitespace and member order do not change this digest.

The same key and digest return the existing run without queueing work again. Reuse with
a different digest returns `409 idempotency-conflict`. Callers cannot probe another
principal's keys. Keys remain reserved while work or delivery is active and for at least
30 days after terminal state. Garbage collection reports its key-retention horizon.
A caller cannot assume deduplication after that horizon and should keep a permanent
application operation ID when a longer lifetime is required.

For example, a collector can submit one advisory revision and its content digest. Its
retry after a lost HTTP response receives the same run. A source replacing the contents
under that revision ID is a conflict to investigate, not a second indistinguishable run.

After the submission transaction is synchronized, the server returns `202`, the job
UUID, committed version and `Location`. A timeout or connection loss before receipt
leaves the outcome unknown to the caller, who resolves it by retrying the same key.
This is the same acknowledgement discipline as other durable writes.

## 4. Worker protocol and state transitions

### 4.1 States and limits

A run is `queued`, `leased`, `retry-wait`, `paused`, `succeeded`, `failed` or `cancelled`.
Terminal states are immutable except for a separate explicit retry that creates a new
run with its own idempotency key. A retry preserves the failed run as evidence.

Defaults are eight attempts, a 24-hour retry horizon, a 60-second lease and a maximum
five-minute lease extension. Scheduling and lease times are server timestamps. Every
job has limits for input bytes, output quads, retained artifact bytes, wall time and
memory where the execution is local. External worker memory cannot be enforced by the
server, which still bounds accepted results and lease duration.

The server bounds queued work and retained state per dataset and principal. Admission
past a quota returns a defined overload error and does not store a run. Queue fairness
is FIFO among eligible jobs within a kind, with bounded concurrency per kind and dataset.
No feature promises completion time merely because a run was admitted.

### 4.2 Claim and renewal

A worker authenticates separately from the submitting principal. Its worker grant
restricts dataset, registered kinds and output scope. Claiming a run cannot broaden the
intersection of the worker grant and the definition's current principal grants.

Claiming atomically checks eligibility, increments a monotonic lease generation and
persists the attempt with a random boot epoch. The response returns an opaque bearer
lease token bound to the run, attempt, generation, worker and epoch. A token's verifier
is durable. Raw tokens are never written into RDF, logs, URLs or inspection responses.

Renewal checks the same tuple, current authorization, attempt version and expiry while
holding the writer. A stale generation, expired lease, cancelled run or epoch mismatch
returns `409 lease-lost`. An expired worker cannot revive its attempt by renewing after
another worker became eligible.

The dispatcher uses a monotonic clock for expiry during a process lifetime. It also
stores UTC timestamps for reporting. A backward wall-clock adjustment cannot lengthen
the active lease. Restart installs and synchronizes a fresh epoch before dispatch starts,
invalidating every old token. Expired interrupted attempts are marked and scheduled
according to their retry policy rather than assumed to have produced no external effect.

### 4.3 Publication

`POST /$/jobs/{ds}/{id}/publish` accepts the token, expected job version, publication
operation UUID, staged typed result, optional source checkpoint and optional outbox
events. Each registered kind defines exactly which results and transitions are legal.
It cannot submit arbitrary protected graph mutations as part of its payload.

The library publisher takes the dataset writer and rechecks the lease, cancellation,
authorization and definition revision. Publication preconditions belong to the
registered operation kind. A maintained analysis uses F22's unchanged-input and
freshness checks. A C23 batch instead verifies its pinned iterator identity, fenced
batch position, prior checkpoint and current write preconditions. It intentionally
applies each batch to current data and must not reject a later batch merely because
its own earlier effects changed the iterator's original query inputs.

For an F22 publication, the check covers the declared read footprint, including
visibility dependencies, inferred-input freshness and expiry. A lease or checkpoint
commit advances the head, so the test cannot simply require the head to equal the
original input commit. Only control graphs proved outside the full input and protection
footprint may be ignored. If a bounded check cannot establish freshness under the
writer, an analysis publication refuses and requires a new run. A collector or batch
uses its own typed checkpoint and publication contract rather than inheriting the
analysis check accidentally. No estimate can substitute for any required precondition.
It validates outputs and budgets, then stages business quads, immutable publication
record, source checkpoint, completion state and outbox entries in one transaction.
The actual sequence is assigned under that writer and is the publication's commit.
Validation refusal publishes none of these changes. A synchronization error is not a
success acknowledgement and can leave the transaction outcome uncertain until recovery.
Recovery retains either the complete transaction or none of it, and the publication
operation ID resolves retries without assuming an error proved rollback.

The publication operation UUID and digest are checked before lease rejection on an
otherwise authorized retry. A completed publication with the same digest returns its
original receipt, even if the old lease subsequently expired. A different digest is
`409 publication-conflict`. This lookup does not permit new effects with a stale token.

A retry interrupted after durable commit therefore cannot duplicate its database effects.
For a source checkpoint, the input page's facts and its next cursor become visible
together. A multi-page import remains several commits, and a different source is a
different checkpoint. No global feed transaction is implied.

Large results can first be uploaded as a bounded immutable artifact with a digest and
expiry. Uploading it is not publication. Publishing its RDF requires one permitted
transaction or an explicitly declared C23 sequence. Such a sequence records each batch
as its own idempotent publication and may leave completed batches after cancellation.
It cannot advertise atomic replacement of the whole result.

### 4.4 Cancellation and failure

Cancellation is durable. It fences the active attempt and prevents further publication,
then signals its current P06 `Control`. Work already committed remains committed.
An external request already sent may still take effect, and the state reports that
uncertainty rather than claiming rollback.

Transient failures enter `retry-wait` with bounded exponential backoff and jitter.
Registered HTTP handlers may honor a valid `Retry-After` within the remaining horizon.
Authentication failures, forbidden destinations, invalid schemas and unsupported kind
versions pause or fail without repeated network attempts. Exhausted retries enter
`failed` with a redacted reason and may be inspected or explicitly resubmitted.

Recurring definitions persist their next scheduled occurrence. An occurrence key derives
from definition revision and intended schedule slot, so a restart cannot enqueue it
twice. The default missed-slot policy coalesces to one pending run and records skipped
slots. A bounded catch-up count must be explicitly configured. Source state remains in
its committed checkpoint, never in a worker's remembered pagination token alone.

## 5. Durable delivery

The outbox is for dataset application events whose creation must be atomic with changes
to data. Existing C21 operator notifications retain their current in-memory queue and
are not silently converted to durable application deliveries.

An event's UUID, type, schema version and payload digest stay fixed across retries.
Envelope identifiers follow the existing C21 convention. A compatible adapter can
render a CloudEvents envelope, but the two wire formats have distinct version labels.
Destinations are administrator-defined references with secret references, allowlisted
methods and bounded payloads. A worker cannot use a payload field as an arbitrary URL.

A dispatcher claims a delivery with the same fencing rules as a job. It checks current
sender grants, destination configuration and the recipient's permitted evidence footprint
immediately before rendering and sending. Grants or source visibility that have changed
put the delivery in `paused` or replace it with a newly authorized redacted event under
a new event UUID. A signature authenticates the transmitted event, not its factual truth.

The network send occurs without the dataset writer. After an accepted response, a
separate transaction marks it delivered. A crash after the receiver accepts but before
that transaction can cause a duplicate send. Consumers receive the stable event UUID
as their deduplication key. Ordered delivery requires an explicit stream key and sequence,
allows only one active delivery per stream and blocks later events on an unresolved
earlier one. There is no total ordering across streams.

There is a bounded race between the final authorization check and a network send.
Revocation cannot recall bytes already transmitted. The contract guarantees rechecks
on each attempt and blocks all later sends, not instantaneous cancellation of a request
already in flight. Restricted payloads can use authenticated fetch links instead of
embedding evidence, so retrieval performs its own current authorization check.

HMAC signing, HTTPS verification, outbound rules, redirect policy and secret handling
reuse C21. A success is a configured 2xx response. A timeout leaves receipt uncertain.
Permanent failures and exhaustion keep a terminal delivery record for inspection.
Manual redelivery creates an audited new attempt of the same event and may duplicate
external effects. Suppression and cooldown are application policies, not an excuse to
discard durable state without a recorded transition.

## 6. API, library and configuration

| Method and route | Meaning |
|---|---|
| `POST /$/jobs/{ds}` | Submit a typed run with an idempotency key and return a durable receipt. |
| `GET /$/jobs/{ds}` | Page through visible runs with bounded state and kind filters. |
| `GET /$/jobs/{ds}/{id}` | Read redacted state, input references, attempts and result references. |
| `POST /$/jobs/{ds}/claim` | Claim one eligible run of the caller's permitted kinds. |
| `POST /$/jobs/{ds}/{id}/renew` | Renew a live lease using its token and expected version. |
| `POST /$/jobs/{ds}/{id}/publish` | Atomically commit typed outputs, checkpoint and outbox entries. |
| `POST /$/jobs/{ds}/{id}/fail` | Record a typed failure under the current lease. |
| `POST /$/jobs/{ds}/{id}/cancel` | Durably fence the run and request cancellation. |
| `POST /$/jobs/{ds}/{id}/retry` | Create an explicitly authorized new run from a terminal one. |
| `GET /$/jobs/{ds}/deliveries` | Inspect deliveries visible to the caller with payloads redacted. |
| `POST /$/jobs/{ds}/recover` | Validate restored identities and explicitly resume selected paused work. |

Static route segments are reserved and cannot be interpreted as job UUIDs. Requests
use bounded JSON with a schema version. The `Authorization` header authenticates the
caller, and `Sparkles-Job-Lease` carries the separate bearer lease token. Both headers
are redacted by logging and tracing. Responses never echo tokens. Unknown run UUIDs and
invisible runs share the same not-found response.

`Dataset::jobs()` exposes a library handle following P06. Trusted callers supply
the same authorization context and typed transition checks the server uses. The
transactional publication method is owned by this handle and wraps one `WriteTxn`.
It does not accept an arbitrary callback that can perform network effects under the
writer. Bindings use the same typed requests and receipts, and parity tests cover them.

Dataset settings contain registered definitions, concurrency and retention bounds,
allowed worker kinds and destination references through C19. Definitions have immutable
revisions. Editing one cannot change the meaning of an existing accepted run. Disabling
a definition stops claims and pauses unsent delivery derived from it. Re-enabling it
does not bypass a recovery or authorization hold.

## 7. Persistence, recovery and compatibility

The format registry gains a `jobs` profile version 1 for ledger graphs and a versioned
`jobs.json` settings manifest. Enabling jobs reserves a new dataset-reader capability
at implementation and raises `dataset.json`'s `minimumReader` through the existing
`commit::require_reader` gate before writing the manifest or the first ledger record.
The capability number comes from the central format registry, not this specification.
The marker is synchronized before new semantics can commit. An interrupted enablement
may leave the reader requirement raised with no jobs, which is safe to resume.

Supported older releases already check `minimumReader` before opening for mutation and
therefore refuse this dataset. A new field checked only by the jobs API would not provide
that protection. Releases predating that gate are outside the downgrade compatibility
boundary and must not open an enabled dataset. There is no automatic downgrade while
job records, required definitions or outbox entries remain. Supported readers also check
the registered manifest and RDF profile version before dispatch or typed transitions.
Unsupported versions are not ignored, truncated or interpreted as an empty queue.

The ready-time index and cached inspection summaries are derived, versioned artifacts.
They can be discarded and rebuilt from ledger records. Corrupt authoritative control
records prevent dispatch for that dataset and produce an integrity failure. A derived
index that misses a record cannot permanently lose work because periodic reconciliation
compares it with committed ledger state.

Startup first recovers the store and checks the format. It then synchronizes a new
boot epoch, expires old leases, rebuilds indexes and rechecks definitions and principals.
Only then can it admit claims. A failure before this sequence completes leaves dispatch
off. Server shutdown fences new claims and requests bounded cancellation. It does not
wait indefinitely for an external worker.

Backups capture committed ledger state with the corresponding application quads.
Definition manifests and immutable artifact references must also be captured or the
backup explicitly reports them missing. Restore and clone start all nonterminal work
and pending deliveries paused, including a retry of an older same-UUID backup. An
operator must map principals and secret references, confirm source continuity and
choose whether previously uncertain external effects may be retried. A clone gets
new job identities with `origin` references and cannot reuse old leases or delivery
deduplication identities as live work.

Restoring an old backup cannot recover evidence that a remote receiver accepted after
that capture. The recovery interface calls out that uncertainty. A provider with an
independent deduplication ledger can safely accept the event's retained UUID again,
but this feature does not assume every provider does so.

Application queries and analytical change consumers exclude control-only commits from
their semantic input footprint. Explicit consumers can subscribe to typed job events.
C23 trigger definitions may submit C26 jobs, with C23 recursion and ordering limits.
They do not create another scheduler or imply atomic effects across trigger attempts.

### 7.1 Ordering against the write-ahead log and the commit tail

Every job transition is one ordinary commit of the dataset's main store. Submission,
claim, renewal, publication, failure, cancellation, retry and recovery each open one
`WriteTxn` under the dataset writer with the jobs owner's managed-write capability. The
transaction stages the ledger quads and, for a publication, the business quads, the
publication record and the source checkpoint together. The existing commit path appends
them as one log record, synchronizes the log and only then publishes the snapshot and
returns the receipt. There is no second journal. A ledger record is durable exactly when
its commit is durable, and a reader sees it exactly when that commit is published.

Preconditions read the transaction's base snapshot, which is the head when the writer was
taken. Transitions of one run therefore serialize on the writer. A publication that reads
`leased` at generation g and a cancellation that reads the same base cannot both commit,
because whichever takes the writer second reads the other's commit in its base and
refuses. This gives A6 without a separate lock.

The response is rendered from the receipt after `commit` returns, so an acknowledged
submission, claim or publication is in a synchronized log record. A crash before
synchronization loses the whole record and nothing of it is visible after recovery. A
crash after synchronization keeps the whole record even when the response was lost. This
gives A2, A3 and A7, and the retry resolves the lost response from committed state. A
submission is recorded under an operation id derived from the dataset, branch, principal,
definition revision and idempotency key, and a publication under its operation UUID. A
retry looks the id up in the current head before taking the writer and again in the base
of its own transaction, so no error is ever read as proof of rollback.

The claim commit records the attempt's lease generation, the process's boot epoch and
the SHA-256 verifier of the random bearer token. A renewal, publication or failure
verifies the token digest, the generation of the run's current attempt, the epoch of the
running process and the lease expiry on the monotonic clock, all against committed state
under the writer. An old token cannot write after another worker's claim has committed,
which gives A4. The boot epoch is chosen at random when the dataset opens and is recorded
by the first transition of that process before that process admits any claim. An attempt
whose recorded epoch differs is interrupted, its lease counts as expired and its run
returns to its retry policy in a recovery commit. The previous process's tokens therefore
fail the epoch check before any new claim, whatever the wall clock says, which gives A5.
Lease expiry is never derived from a record's UTC time within one process, so a backward
clock change cannot lengthen a lease.

Input commits are pinned by a source hold under the run's id before the submission
commit, as the evidence profile pins its references. A crash between the hold and the
commit leaves an orphan that the hold reconciliation releases after its grace period, and
the hold is released when the run reaches a terminal state. Enabling jobs raises the
dataset's `minimumReader` durably before `jobs.json` and then `managed.json` are written,
and the registry's owner entry is the write fence of the ledger graph, so an older
release refuses to open the dataset rather than treat the ledger as an ordinary graph.

## 8. Acceptance and verification

The following examples are requirements for executable tests rather than claims about
the current implementation.

| Example | Required result |
|---|---|
| A1. The submission response is lost after synchronization. | Retrying the same key returns the same run and receipt. A changed payload conflicts. |
| A2. The process stops before a submission commits. | Recovery finds either the complete committed run or no run, never a dispatchable partial record. |
| A3. An advisory page import stops around publication. | Its facts, checkpoint and publication record are all present or all absent. A retry does not skip or duplicate a page. |
| A4. Worker A stalls and worker B obtains a later generation. | A cannot renew, publish or mark failure with its old token. Only B can change the active attempt. |
| A5. A process restarts while the old worker still runs. | The new boot epoch fences the old token before any new claim or publication. |
| A6. A cancellation races publication. | Writer ordering produces either the committed publication or cancellation before publication. No cancelled predecessor can publish later effects. |
| A7. Synchronization fails while publishing results and an event. | The response does not acknowledge success. Recovery sees one complete transaction outcome, and retries resolve by publication ID. |
| A8. A receiver accepts an event and the sender stops before acknowledgement. | Recovery may resend the same event UUID. The test receiver demonstrates deduplication and the system reports possible duplicate effects. |
| A9. A source or output grant is revoked. | The next claim, publication and delivery refuse that authority. Inspection and derived counts do not disclose hidden records. |
| A10. A backup with a pending delivery is restored or cloned. | Delivery stays paused until explicit recovery. Old lease tokens fail and no unsolicited send occurs. |
| A11. A queue profile is newer than the reader understands. | Mutation and dispatch refuse cleanly, leaving authoritative records unchanged. |
| A12. A worker outputs an oversized or invalid batch. | Validation refuses business effects, checkpoint and outbox together. The run records a bounded diagnostic. |
| A13. A scheduler misses many occurrences during downtime. | Coalescing creates one pending occurrence and records skipped slots within its bounds. |
| A14. A ready index is deleted or stale. | Rebuild and reconciliation find every eligible committed run without creating new run identities. |

Fault injection covers before and after vocabulary append, WAL append, synchronization,
snapshot publication and response rendering. Each test reopens the store and checks
business quads and ledger invariants together. Power-loss tests use the existing durable
write harness and preserve the acknowledgement boundary. In-memory tests alone do not
establish crash safety.

Authorization tests cover all public read and write surfaces, including backups,
history, diffs, aggregates, merges and delivery rendering. Property tests generate
claim, renew, publish, cancel, restart and retry interleavings against a small sequential
state-machine oracle. A fake clock exercises backward UTC jumps and lease exhaustion.

Measure submission, renewal and publication overhead with no jobs, idle jobs and active
workers, including durable single-write latency, retained history, ledger growth and
recovery time. Record the memory tradeoff of the ready index and the throughput impact
of checkpoint frequency. No benchmark threshold is weakened without a roadmap follow-up.

### Batch publication acceptance

A C23 iterator selects items without a completion timestamp at its pinned initial
commit. Batch one adds timestamps and advances its checkpoint atomically. Batch two
must apply the next pinned solutions to the current dataset successfully, although
batch one's writes changed the iterator query's source. A stale worker replaying batch
one must still be fenced or return the original publication receipt. This distinguishes
batch semantics from F22's changed-input refusal without weakening either contract.

## 9. Delivery phases

Phase 1 supplies the versioned profile, protected ledger, typed library and HTTP
submission, worker leases, atomic publication, idempotency, restart fencing and crash
tests. It can run external collection and analysis workers on `main`. It depends on
no F13 graph view or general commit trigger.

Phase 2 adds the outbox, recurring definitions, C21 transport adapters, restore and
clone recovery workflows and destination authorization tests. Durable delivery is not
advertised until A7 through A10 pass with real restart and receiver fixtures.

Phase 3 integrates selected registered maintenance types, C23 trigger submission,
Python, Node and JVM handles, MCP inspection and the UI's job and delivery pages.
Existing task routes remain compatible and link to durable jobs where one exists.

## 10. Alternatives and sources

A separate queue database would require a second transactional protocol for application
publication. The initial design instead pays for ledger commits in the existing store.
An in-memory queue with periodic snapshots cannot preserve accepted work across a crash.
Exactly-once remote delivery cannot be guaranteed without receiver cooperation. A general
server command runner would create an unrelated execution and credential boundary.

The design was written from the Sparkles transaction and task code, the existing specs
linked above, [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html),
[Standard Webhooks](https://www.standardwebhooks.com/) and
[CloudEvents 1.0.2](https://github.com/cloudevents/spec/blob/v1.0.2/cloudevents/spec.md).
Those external references inform HTTP retry handling, event identity and transport
authentication. They do not supply the dataset transaction or lease implementation.
No new runtime dependency is selected by this specification.

## Provenance

This design shares the [evidence and analysis provenance record](shared/PROVENANCE.md#evidence-durable-work-and-maintained-analysis)
with C25, C26, C27, F22 and C22b. The record preserves their common standards
and each profile's adopted and rejected decisions. It does not imply shared
implementation status. This spec's status and Outcome describe its delivery.

## Outcome

Phase 1 shipped for the `quads` kind. The jobs owner `OwnerKind::Jobs` of the
managed-graph registry protects one hidden control graph, `urn:x-sparkles:jobs:ledger`, and
keeps its definitions in `jobs.json` at format 1 and profile 1. Enabling jobs raises the
dataset reader to 9 through `require_reader` before `jobs.json` and `managed.json` are
written. `Dataset::jobs()` submits with an idempotency key scoped to the dataset, the branch,
the principal, the definition's name and revision and the operation kind, over a digest of
the canonical payload, the checkpoint, the sorted input references and the retried run, as
§3 says. It claims with a lease generation, a boot epoch and a bearer token whose SHA-256
verifier is the only durable trace, renews, publishes the business quads with the
publication record, the checkpoint and the completion state in one transaction, fails with
bounded exponential backoff and jitter within the remaining horizon, cancels, retries and
inspects. Every leased transition rechecks under the writer that the definition exists, is
enabled, keeps the run's revision and admits the worker, and an expired lease enters the
retry policy before another worker takes the run over, as §4.2 requires. A run records
the attempts and retry horizon it was accepted with and keeps that policy when its
definition is edited or removed, so an expiry or a restart never ends accepted work under
another revision. A lease operation by a caller with no part in the run answers as for an
unknown run. A submission
revalidates its definition against the settings admitted under the writer, pins at most
4096 inputs, and takes its source hold for publication inside the commit so a
reconciliation cannot release it under a run that commits. `maxQueued` and the sum of the
definitions' concurrency are bounded to the 4096 runs one scan reads exactly, and a scan
that is cut short refuses admission and dispatch instead of undercounting. A disabled
configuration keeps the ledger graph protected and hidden under its nonce. The HTTP routes of §6 except
`deliveries` and `recover` are served, and the OpenAPI description, the auth route table
and the API reference describe them. A run's definition revision is the digest of the
definition's canonical JSON, and a run whose definition changed is not claimed. Input
commits are pinned by a source hold under the run's id and released at the terminal state,
also when a restart or a claim ends the run, and kept while the run waits to retry.
A restart records a new epoch in the ledger before the process admits its first claim,
renewal or publication, and interrupts attempts of earlier epochs into the retry policy.

Tests cover A1, A4, A6, A9 and A12 in one process. The crash tests for A2, A3, A5 and A7
unwind the operation at failpoints before and after the submission, claim and publication
commits, drop the dataset handle, reopen the dataset and check the business quads and the
ledger together. They are reopen tests around the commit boundary in one process and rest
on the store's write-ahead log for what happens at a synchronization error or a power
loss, which they do not inject. The ordering
argument is §7.1.

The ledger graph is the first owner graph with the `hidden` visibility of §2.3. The
registry lists the hidden graphs of its owners, and they leave every graph view a request
or a library query runs under: the server's authentication middleware subtracts them from
the principal's view for every route of the dataset, and the library subtracts them from
the view of every SPARQL query it runs, including a caller's explicit all-graphs view.
Of the surfaces that take a view, only the typed inspection of the jobs handle reads the
ledger. The surfaces that take no view follow the exclusion on their own: the statistics
count only the visible graphs, the dataset listings count only visible quads, SHACL and
ShEx validation exclude the hidden graphs and answer a request that names one as for a
graph that does not exist, materialization and reasoning diagnostics never read them
whatever inputs a request names, and an MCP call hides each dataset's control graphs from
its principal before any tool runs, since its dataset is an argument and not a path
segment. A hidden graph does not restrict a grant, so whole-dataset operations such as
branches and source holds stay allowed, but it makes the principal's view partial, and
the counts that cover every graph leave the history responses as they do for a limited
grant. A registry this build cannot enforce still hides the graphs its file names as
hidden, and one whose file does not read hides the graphs that every configured owner
always hides.

Several parts of the design wait for later items. The library's Graph Store, history,
diff and dump handles apply a caller's view when one is given but do not subtract hidden
graphs on their own, which item 902 covers with the dispatch follow-ups. The text, geo
and vector indexes index whatever graphs their scope names and answer hits before the
query's view applies, which item 904 covers. The dispatcher
scans the committed ledger by state within the bound of 4096 runs per state instead of
keeping a ready-time index, inspection has no cursor, quotas are per dataset, a server
shutdown does not fence claims, and the authority fence between a publication's
preparation and its commit is the next request's recheck, all under item 902. A
submission is refused when the submitter's own view cannot write a definition graph, but
the submitter's effective grants are not recorded and intersected with the worker's at
claim and publication, which item 903 adds. Current task records and the C21 delivery
queue remain process-local. The outbox, recurring definitions, the restore and clone
recovery and the `deliveries` and `recover` routes are Phase 2 of item 339, the analysis
and batch kinds are items 899 and 900, and the bindings, MCP and UI are item 901.

The shared managed-write boundary of §2.3 and §7 is available from C25 Phase 2. Dataset
reader 6 marks `managed.json`, a registry of owners whose graphs ordinary writes cannot
change, and a `ManagedWrite` capability admits an owner's typed transaction. Jobs
register as a second owner kind with its own settings file in the same registry, and
enabling them raises the dataset reader to 9 as §7 requires, since a build that knows
only the registry would still render the control graphs on its read surfaces. They use
the `hidden` visibility that the earlier build reserved and refused, and keep the `refuse`
merge policy. No commit kind code is allocated by that boundary.

### Roadmap follow-ups

#### Operations and streaming workstream

These phases belong to [ROADMAP](../ROADMAP.md) item 467, the operations and streaming workstream. Each phase gets its own numbered item when it is ready to start.

**Scheduled stored queries and updates.** Run stored queries, updates and notebook runs on a schedule and deliver their results. Recurring reports and maintenance updates need an external scheduler today.

**Kafka and MQTT ingestion connectors.** Ingest streams with offsets and exactly-once commits. No streaming source exists. It depends on item 339.
