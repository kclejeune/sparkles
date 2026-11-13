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

Jobs and their outbox entries are records in the dataset's control keyspace, a keyed
space of the main store that holds no RDF. Each record is a versioned value under a key
of the jobs owner. The keyspace belongs to the same store as the application quads and is
changed only under the same writer lock. A typed publisher stages its business quads and
its control records in one actual `WriteTxn`, and the store writes both into the same
record of the write-ahead log, sealed by one checksum and synchronized once. There is no
second queue database. A transition that changes no quad, such as a submission, a claim,
a renewal, a failure or a cancellation, is written as one batch of the store's control
log, which the writer orders with the write-ahead log as §7.1 describes. The two logs are
not synchronized by a protocol of their own. One writer lock and one sequence of control
batches order them.

Phase 1 manages jobs on `main`. A branch can be an input, identified by its stable UUID,
but publishing into it and updating a control record on `main` is refused. A branch store
has no control keyspace, and branch creation, a merge and a branch's base never carry
control records. Later branch jobs must keep their ledger and effects in that branch. A
merge never copies active leases or causes a delivery. Preview analysis is inert until an
explicit publication creates a job or result on the target branch.

A transition that changes no quad is not a commit. It takes no commit number and appears
in no history, change event, commit count or annotation, and it wakes no live query, kept
view or change subscriber. A publication with a net change to the quads is one commit
whose counts, changes and events are those of its business quads alone, and its commit
metadata names no run, attempt or operation. Progress reports are process-local and
coalesced, and a worker renews its lease infrequently rather than persisting every row. A
rebuildable ready-time index avoids repeatedly scanning the whole ledger. The cost of the
control log and its checkpoints must be measured before tuning checkpoint frequency or
introducing a different authoritative format.

The dispatcher reads only committed ledger states. It cannot see an outbox entry before
the transaction containing its business changes has durably committed. A transaction that
carries both publishes its snapshot before its control state. A reader outside the writer
that needs both loads the control state first and the snapshot second, and the snapshot
is then at least as new as the business head that the control state records, so a
publication record it sees names a commit the snapshot holds. The writer is never held
across a network call, a model request or execution of an external worker.

### 2.2 Records

| Record | Required content |
|---|---|
| `job:Definition` | A stable ID, registered kind and version, effective principal, target dataset and branch UUIDs, input selectors, retry policy and declared effects. |
| `job:Run` | A run UUID, definition revision, state, payload digest, source checkpoint, exact input references, attempt count and budgets. |
| `job:Attempt` | A run reference, worker identity, durable lease generation, boot epoch, lease expiry, attempt state and redacted diagnostic. |
| `job:Publication` | An operation UUID, run and attempt references, payload digest, resulting checkpoint and the committed result references. |
| `job:Delivery` | A stable event UUID, destination reference, payload digest, evidence visibility footprint, delivery state, attempt count and next eligible time. |
| `job:Recovery` | The source dataset identity, capture reference, paused state and recorded operator decision for resuming work or delivery. |

Definitions stay in the `jobs.json` settings file. Every other record is a control record
under a key that starts with `jobs/` and names its kind and identifier, such as
`jobs/run/<uuid>` or `jobs/publication/<uuid>`. The value is a record version byte
followed by the record's canonical JSON. A reader refuses a record whose version or
members it does not know, and such a record stops dispatch for the dataset as an
integrity failure. Index entries, such as `jobs/state/<state>/<run>` for the runs in one
state and `jobs/operation/<id>` for a submission's operation id, are written in the same
batch as the records they index. A scan by state therefore reads the index entries of one
committed control state. A run's payload, input references and source checkpoint are kept
under a key of their own that is written once at submission, so a transition rewrites
only the run's small mutable record, and a transition over many runs, such as an epoch
install or a restore pause, is written as several bounded frames. The `job:` names above
are record kinds, not RDF classes, and no record is an RDF term, so no record enters the
dataset's vocabulary, indexes or statistics.

Publication records are immutable. A run and its current lease are mutable state with
compare-and-set versions. Attempt and transition records form a bounded audit history.
Job state, payload fields and destination configuration do not contain secret values.
They hold references to the existing secret resolver and immutable external artifacts.

Every input reference includes dataset UUID, branch UUID, commit selector and artifact or
definition digest where applicable. A mutable name alone is insufficient. The consumer
pins required input snapshots before accepting a run and releases them only after no
retry can need them. If a referenced state cannot be retained, acceptance fails without
claiming that the work is queued.

### 2.3 Protected control records

The control keyspace lies outside the RDF dataset. SPARQL Update, Graph Store writes,
imports, RDF Patch, mapping output and merges address quads and cannot name a control
key, so no ordinary write can change a control record, whatever its grants. Query, graph
listing, history, diff, dump, export, statistics, the text, spatial and vector indexes,
reasoning, validation, forms, explanations, live queries and kept views read quads and
commits, so none of them can read a control record or count one. The exclusion is a
property of where the records live. No surface subtracts anything, and a new read surface
needs no exclusion of its own.

The core exposes the keyspace through two interfaces. An owner's control capability,
which the store grants to the jobs handle, reads the committed control state and stages
control records in a write transaction. The trusted capture of a backup and the offline
integrity check read the control state whole. The typed inspection API renders redacted
records from the control state. External workers never receive the capability or the raw
control state. The library's raw `Store` is a trusted interface, and a caller that holds
it can obtain the capability, as it can already write any quad. The server's job routes,
and the MCP inspection that item 901 adds, read records only through the jobs handle, so
neither needs a view of its own over the ledger.

A write-time guard validates the business quads of a commit and never sees its control
records. A transition that changes no quad runs no guard, since it changes no data the
guard validates, and a guard cannot read the ledger as a source or as data. Business
effects still pass the worker run's current effective data grants, write guard, quotas
and protected-branch checks. Internal access to job records cannot authorize a business
quad. The internal publisher capability is not a general write bypass.

The jobs owner keeps its entry in the managed-graph registry, which records its settings
file and raises the dataset reader, but it owns no graph. The `hidden` visibility of the
registry stays reserved and refused, as it was before jobs.

#### Alternatives to the control keyspace

Three other designs were compared. The first kept the ledger in a reserved RDF graph and
excluded it from each read surface. Every surface that read a snapshot, a cache or a
guard source without the subtraction showed the ledger, and counts that cover every graph
had to be redacted on their own, so the exclusion depended on each surface's code. The
second masked the snapshot itself by returning the ledger's quads as delta deletions, the
mechanism of triple-level access views. About 600 snapshot reads in the core would each
have had to be classified as raw or public, a writer that took a masked base would commit
the mask's deletions, every snapshot would cost work proportional to the ledger, and
commit counts, the change log and history would still need their own redaction. The third
paired a snapshot with the effective graph view at the library and server boundary.
`Dataset::store` stays public, so raw library reads and counting surfaces would still
depend on their callers. The control keyspace removes the ledger from every quad and
commit surface at once, at the cost of new log, checkpoint, backup and reader formats,
which §7 versions.

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
events. Each registered kind defines exactly which results and transitions are legal. It
cannot submit arbitrary protected graph mutations as part of its payload.

The library publisher takes the dataset writer and rechecks the lease, cancellation,
authorization and definition revision. Publication preconditions belong to the registered
operation kind. A maintained analysis uses F22's unchanged-input and freshness checks. A
C23 batch instead verifies its pinned iterator identity, fenced batch position, prior
checkpoint and current write preconditions. It intentionally applies each batch to
current data and must not reject a later batch merely because its own earlier effects
changed the iterator's original query inputs.

For an F22 publication, the check covers the declared read footprint, including
visibility dependencies, inferred-input freshness and expiry. Job transitions do not
advance the head, but other writers do, so the test cannot simply require the head to
equal the original input commit. If a bounded check cannot establish freshness under the
writer, an analysis publication refuses and requires a new run. A collector or batch uses
its own typed checkpoint and publication contract rather than inheriting the analysis
check accidentally. No estimate can substitute for any required precondition. It
validates outputs and budgets, then stages business quads, immutable publication record,
source checkpoint, completion state and outbox entries in one transaction. The actual
sequence is assigned under that writer and is the publication's commit. A publication
whose quads make no net change creates no commit. Its record names the business head it
follows instead of a commit, its receipt says that no business commit was created, and a
retry returns the same receipt. Validation refusal publishes none of these changes. A
synchronization error is not a success acknowledgement and can leave the transaction
outcome uncertain until recovery. Recovery retains either the complete transaction or
none of it, and the publication operation ID resolves retries without assuming an error
proved rollback.

The publication operation UUID and digest are checked before lease rejection on an
otherwise authorized retry. A completed publication with the same digest returns its
original receipt, even if the old lease subsequently expired. A different digest is `409
publication-conflict`. This lookup does not permit new effects with a stale token.

A retry interrupted after durable commit therefore cannot duplicate its database effects.
For a source checkpoint, the input page's facts and its next cursor become visible
together. A multi-page import remains several commits, and a different source is a
different checkpoint. No global feed transaction is implied.

Large results can first be uploaded as a bounded immutable artifact with a digest and
expiry. Uploading it is not publication. Publishing its RDF requires one permitted
transaction or an explicitly declared C23 sequence. Such a sequence records each batch as
its own idempotent publication and may leave completed batches after cancellation. It
cannot advertise atomic replacement of the whole result.

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

The control keyspace adds two files, a frame format that both of its logs share and a
third version of the write-ahead log. Each has a version that a reader checks first, and
a reader refuses a version it does not know through `FileKind::refuse` before it
interprets anything after the version. The central format registry in
`crates/sparkles-core/src/formats.rs` lists them, and X16 records them beside the
first-release cutoff.

| Kind | Where | Version | Content |
|---|---|---|---|
| `control-log` | `control.log` at the dataset root | 1 | A 16-byte header of magic and version, then one record per control-only batch. A record is the frame's length, a CRC-32 of the frame and the frame. |
| `control-checkpoint` | `control.bin` at the dataset root | 1 | The control state after one control sequence number, the business commit it follows and the entries in key order, ended by a SHA-256 of everything before it. It is replaced atomically. |
| control frame | inside both logs | 1 | A version byte, the frame's length, its control sequence number, the business commit it follows, a timestamp and its puts and deletes. |
| `wal` | `gen-*/wal.log` | 3 | Version 2 with control records. A control record carries 32 bytes of a frame, and a commit record whose transaction has any sets a control flag that its checksum covers. |
| control record value | inside a frame | 1 per record kind | A version byte and the record's canonical JSON, as §2.2 describes. |
| `dataset` reader | `dataset.json` | 9 | The control keyspace and the job ledger. |
| `jobs` settings | `jobs.json` | format 1, profile 1 | The definitions. Profile 1 names the record shapes and keys of §2.2. |
| backup feature | backup manifest | `control/1` | A backup that holds `control.bin` or `jobs.json` requires the feature, so a restore that does not know it refuses the backup. |

The write-ahead log keeps one record size. Within a transaction its control records
follow the quad records and precede the configuration record, and the frame they carry
ends at its declared length, with zeros after it to the end of the last record. A log of
version 3 is written by every store whose control keyspace is enabled, activated or not,
since it may carry both configuration and control records. A plain store writes version
1, an activated store without control writes version 2, and a store with control writes
version 3. Transaction boundaries, commit numbering and the durability rules of the log
are unchanged. Every parser that frames transactions, checks a seal or looks for a later
commit as proof of durability includes control records in the transaction and its
checksum, and in a log of version 3 it checks that they follow the quads and precede the
configuration record, that their frame's declared length fits them with zero padding
after it, and that the commit's control flag is set exactly when they are present. These
checks apply to sealed transactions below a checkpoint too. Every reader that decodes
quads decodes only insert and delete records and passes over configuration and control
records. A parser that meets a control record in a log of version 1 or 2 refuses it as
damage.

A key is at most 512 bytes and a value at most 4 MiB, and one frame holds at most 16 MiB.
The control state is kept in memory as a persistent ordered map, so a transaction copies
only the paths it changes and a reader holds one immutable state. An in-memory store
keeps only that state, with the same transactions and no files, and its backup capture
renders `control.bin` from it. The state's size is bounded by the jobs settings'
`maxControlBytes`, and a submission that would exceed it is refused with the overload
error of §4.1. The bound limits admission only. A transition of an admitted run is never
refused for the size of the control state, so admitted work is never stranded by it. The
memory of the control state is the price of leaving the ledger out of the indexes, and
terminal records leave it only through the retention of §3.

Enabling jobs raises `dataset.json`'s `minimumReader` to 9 through the existing
`commit::require_reader` gate. Under the writer it then writes an empty `control.bin` at
sequence zero, creates `control.log` with its header, and upgrades the current log's
header to version 3 in place. Finally it writes `jobs.json` and then `managed.json`. Each
step is durable before the next begins. A store whose `control.bin` exists treats its
keyspace as enabled, and a `control.log` without a `control.bin` is an integrity error,
so the loss of the checkpoint is never read as an empty ledger. An interrupted enablement
leaves a prefix of these steps, which is safe to resume, and a raised reader with no jobs
is safe to keep.

The header upgrade goes through the writer's own log handle. It drains the group, flushes
the buffered writer, drops the cached block of the direct writer so that no later direct
write reproduces the old header, writes and synchronizes the header record, and keeps the
writer's length and allocation as they were. A failure of the write or the
synchronization poisons the writer, since the header's state is then unknown. A store
with an enabled keyspace upgrades a current log that an interrupted enablement left at
version 1 or 2 the same way before the first transaction that carries a control record. A
generation switch installs a new log only when its version is the one the store writes at
the switch, so a compaction that created its log before enablement is abandoned at the
switch and runs again.

Supported older releases already check `minimumReader` before opening for mutation and
therefore refuse this dataset. A new field checked only by the jobs API would not provide
that protection. Releases predating that gate are outside the downgrade compatibility
boundary and must not open an enabled dataset. There is no automatic downgrade while job
records, required definitions or outbox entries remain. Supported readers also check the
registered manifest, its profile and each record's version before dispatch or typed
transitions. Unsupported versions are not ignored, truncated or interpreted as an empty
queue. A dump and reload copies the quads and not the control keyspace, so it starts a
dataset without a ledger.

The ready-time index and cached inspection summaries are derived, versioned artifacts.
They can be discarded and rebuilt from ledger records. Corrupt authoritative control
records prevent dispatch for that dataset and produce an integrity failure. A derived
index that misses a record cannot permanently lose work because periodic reconciliation
compares it with committed ledger state.

Startup first recovers the store, which replays the control keyspace as §7.2 describes,
and checks the formats. It then pauses work restored from a capture as §7.3 requires,
synchronizes a new boot epoch, expires old leases, rebuilds indexes and rechecks
definitions and principals. Only then can it admit claims. A failure before this sequence
completes leaves dispatch off. Server shutdown fences new claims and requests bounded
cancellation. It does not wait indefinitely for an external worker.

Control transitions are not commits, so application queries, analytical change consumers
and history never see them. Explicit consumers can subscribe to typed job events. C23
trigger definitions may submit C26 jobs, with C23 recursion and ordering limits. They do
not create another scheduler or imply atomic effects across trigger attempts.

### 7.1 Ordering against the write-ahead log and the commit tail

Every job transition is one write transaction of the dataset's main store under its
writer lock. Submission, claim, renewal, publication, failure, cancellation, retry and
recovery each open one `WriteTxn` with the jobs owner's control capability and stage
their control records in it. A publication also stages its business quads. The store
assigns the transaction's frame the next control sequence number under the lock, so the
sequence numbers order every frame the store ever wrote, and it writes the frame in one
of two places.

A transaction with a net change to the quads is an ordinary commit. Its control records
go into the same write-ahead log record as its quads, before its commit record, and the
commit's checksum covers them. A transaction with a control record never joins a group
commit and never takes the bulk rebuild path. It drains the group and commits through the
serial path, which writes the record, synchronizes the log and only then publishes the
snapshot and then the control state. A ledger record of a publication is therefore
durable exactly when its commit is durable, and a reader sees it only after that commit
is published.

A transaction with no net change to the quads is a control-only batch. It drains the
group, so every commit up to the head is durable, then appends its frame to `control.log`
with the head's commit number as the commit it follows, synchronizes the file and only
then publishes the control state. It runs no write guard and publishes no snapshot. A dry
run or preview that stages a control record is refused, since neither may write.

The following properties hold, and the recovery of §7.2 relies on them.

1. A frame is durable before the writer lock is released after it. The serial commit path
   and the control log both synchronize before the lock is released. In a store whose
   keyspace is enabled, a guard is armed before the first byte of any serial transaction
   reaches either log, whether or not it carries a frame, and disarmed only after the
   head and the log's length are recorded and the snapshot and the control state are
   published. The same guard covers a bulk commit and a compaction from the moment
   `CURRENT` may switch until the writer has adopted the new log and the snapshot of the
   new generation is published. The group commit keeps the fence it already has for an
   unwind of its coordinator. A failed write or synchronization, any other error in
   between, or an unwind that drops the armed guard poisons the writer, which refuses
   every later write until the store is opened again. An orderly close after the guard
   poisoned the writer truncates neither log, since the bytes past the published length
   may hold a durable frame or a commit that a frame follows. A control-only frame is
   written only when the writer's head and generation are those of the published
   snapshot. No frame is therefore ever written after one whose durability or publication
   is unknown, and no sequence number is used twice.
2. Durable frames therefore form a prefix of the sequence numbers. Frame n+1 is written
   only after frame n is durable, so a crash can lose only frames that were never
   acknowledged, and at most the last frame written can be partly on disk.
3. Every commit that a durable control-only frame follows is durable, because the group
   was drained before the frame was written. A ledger record never refers to a commit a
   crash can lose.
4. A commit's quads and the frame it carries are durable together or not at all, because
   one checksum covers both and the open keeps or drops the whole transaction.
5. A reader outside the writer reads one immutable control state, which records the
   business head it follows. The store publishes a transaction's snapshot before its
   control state, so a reader that loads the control state before the snapshot reads a
   snapshot that holds every commit the control state names.

Preconditions read the transaction's base control state, which is the published state
when the writer was taken. Transitions of one run therefore serialize on the writer. A
publication that reads `leased` at generation g and a cancellation that reads the same
base cannot both commit, because whichever takes the writer second reads the other's
frame in its base and refuses. This gives A6 without a separate lock.

The response is rendered after the transaction returns, so an acknowledged submission,
claim or publication is in a synchronized frame. After a crash before synchronization
succeeded, recovery keeps or drops the whole transaction, and nothing of it is visible in
part. A crash after synchronization keeps the whole transaction even when the response
was lost. This gives A2, A3 and A7, and the retry resolves the lost response from
committed state. A submission is recorded under an operation id derived from the dataset,
branch, principal, definition revision and idempotency key, and a publication under its
operation UUID. A retry looks the id up in the published control state before taking the
writer and again in the base of its own transaction, so no error is ever read as proof of
rollback.

The claim frame records the attempt's lease generation, the process's boot epoch and the
SHA-256 verifier of the random bearer token. A renewal, publication or failure verifies
the token digest, the generation of the run's current attempt, the epoch of the running
process and the lease expiry on the monotonic clock, all against committed state under
the writer. An old token cannot write after another worker's claim has committed, which
gives A4. The boot epoch is chosen at random when the dataset opens and is recorded by
the first transition of that process before that process admits any claim. An attempt
whose recorded epoch differs is interrupted, its lease counts as expired and its run
returns to its retry policy in a recovery frame. The previous process's tokens therefore
fail the epoch check before any new claim, whatever the wall clock says, which gives A5.
Lease expiry is never derived from a record's UTC time within one process, so a backward
clock change cannot lengthen a lease.

The committed run version is the authority for which monotonic deadline applies. Every
transition of a run writes the version after the one its base read, and transitions of
one run serialize on the writer, so a version names exactly one committed state. A claim
or renewal records its deadline in process memory under the version its frame will write,
before the control state publishes, so that no later writer can read the new attempt
without its deadline. A lease check under the writer admits only the deadline recorded
under the version it reads in its base. A deadline whose transaction was refused, unwound
or lost belongs to a version that was never committed, so no check honours it, and the
deadline of the committed version is never replaced or restored by another transaction's
rollback. A transaction that does not commit removes only its own entry, and a
transaction that publishes drops the entries of older versions. A transaction whose
outcome is uncertain also removes its entry, so its lease counts as expired, which fences
rather than extends it. Claim and renewal are the only transitions that keep a run leased
under a new version, and each records its own deadline, so another transition never
revives a recorded deadline of a version it wrote.

Input commits are pinned by a source hold under the run's id before the submission frame
is written, as the evidence profile pins its references. A crash between the hold and the
frame leaves an orphan that the hold reconciliation releases after its grace period, and
the hold is released when the run reaches a terminal state.

A checkpoint writes the whole control state to `control.bin` under the writer lock, so no
frame is written while it runs. It writes a temporary file, synchronizes it, renames it
over `control.bin` and synchronizes the directory, and only then replaces `control.log`
with an empty log of the same version through the same steps. A checkpoint runs before a
compaction or a bulk commit switches `CURRENT` to a new generation, since the old
generation's log may carry frames, and after the batch that takes `control.log` past both
64 MiB and the size of the last checkpoint, so a checkpoint never writes more than the
log it replaces. Frames at or below the checkpoint's sequence number that remain in
either log are skipped at recovery, so a crash between any two steps loses nothing. A
failed checkpoint before the switch leaves the old generation current.

### 7.2 Recovery at open

A store has no control keyspace while neither `control.bin` nor `control.log` exists and
no settings file or registry entry needs one. A `jobs.json`, a jobs entry in the registry
or a `control.log` without a `control.bin` is an integrity error, so a lost checkpoint is
never read as an empty ledger. A `control.bin` without a `control.log` is accepted in two
states only. One is an interrupted enablement, whose checkpoint is at sequence zero with
no `jobs.json` and no jobs entry in the registry. The other is a restored backup, whose
checkpoint carries a valid origin entry. A writable open then creates the empty log
durably before it admits a write. The origin is consumed by a frame that a checkpoint
follows at once, as §7.3 describes, so a checkpoint with an origin never has an
acknowledged frame after it, and once that checkpoint is written a missing log is
refused again. In every other state a missing `control.log` is an
integrity error, since checkpoints replace the log by a rename and never leave it
missing, and acknowledged control-only frames could have been lost with it. The open
reads `control.bin` and never replaces it with an empty one. It replays the write-ahead
log as before and collects the frames of the transactions it keeps. It reads
`control.log` and collects its frames up to the first record that is incomplete or fails
its checksum. It then merges the frames of both logs above the checkpoint's sequence
number in sequence order and applies them. All of this happens before the open truncates
either log.

The open refuses the store with an integrity error, and changes no file, when the
checkpoint follows a commit above the recovered head, when the merged frames do not
continue the checkpoint without a gap, when one sequence number appears twice, when a
control-only frame follows a commit above the recovered head, when a frame in the
write-ahead log does not name the commit that carries it, or when a damaged record of
`control.log` has a complete record after it. Each of these contradicts property 1 or 3
of §7.1. A gap explained by a commit that the replay would have dropped as a torn tail is
refused the same way, since a later durable frame proves that commit was durable.
Otherwise a damaged last record of `control.log` is a torn tail, which a writable open
truncates as it truncates the write-ahead log. A process that stopped without
synchronizing can leave bytes that only the page cache holds, so a writable open then
synchronizes the accepted prefix of `control.log` and its truncation, and the dataset
root's directory entries, which include a renamed `control.bin`, before it publishes the
recovered state or admits a write. If that fails, the store does not open for writing.

| Crash point | State after recovery |
|---|---|
| Before a frame's log write | The transaction is absent. A hold taken for a submission is an orphan that reconciliation releases. |
| During the write or before synchronization returns | The transaction is either wholly present or wholly absent, and it was not acknowledged. A retry resolves it by its operation id. |
| After synchronization, before the snapshot or control state publishes | The transaction is present. Its response was lost, and a retry returns the recorded receipt. |
| After a claim, before the worker receives its token | The attempt is recorded under the old epoch, which recovery interrupts into the retry policy. |
| After a publication, before its delivery is claimed | The publication and its pending deliveries are present together, and the dispatcher claims them after recovery. |
| After a delivery is sent, before it is marked | The delivery is still leased under the old epoch, so recovery returns it to pending and it is sent again with the same event UUID, as A8 requires. |
| During a checkpoint | Either the old or the new `control.bin` is present, and frames at or below its sequence number are skipped. |
| After an unwind between a log write and publication | The writer is poisoned, so no later frame reuses the sequence number, and the next open recovers the frame if it is durable. |
| During enablement | The steps already durable remain, and enablement or the first control transaction completes the rest. |
| During a compaction or bulk switch | The checkpoint precedes the switch, so either generation recovers the same control state. |

### 7.3 Backup, restore and clone

A backup capture holds the writer lock only to record its file lengths. Under that lock
it also takes the published control state, which holds every frame written before the
lock, and after the lock it renders that state as the capture's `control.bin` with an
origin entry that names the capture, the source dataset and the captured commit. The
capture copies the write-ahead log up to the captured commit as before, and every frame
in that prefix is at or below the rendered checkpoint's sequence number. It keeps the
bytes of the log's header record that it read under the lock and serves them in place of
the file's first record for the whole life of the capture, so a header upgrade by a later
enablement never reaches a capture taken before it, and an incremental backup that reads
the prefix twice reads the same bytes. It never copies `control.log`. The capture is
therefore one consistent cut of quads and ledger. Definition manifests and immutable
artifact references must also be captured or the backup explicitly reports them missing.

A restored dataset opens with that checkpoint and its origin entry. The origin entry
fences dispatch by itself. The first jobs recovery runs before it installs the process's
boot epoch or returns any interrupted attempt to its retry policy, and it writes one
small frame that removes the origin entry and records a `job:Recovery` record in the
`pausing` phase. Under the same writer lock, before the lock is released and before the
recovery goes on, the store writes a checkpoint that holds that frame, so `control.bin`
no longer carries the origin before any later frame can be written or any transition
acknowledged. The restored set is every run and delivery that the checkpoint holds.
The recovery pauses the nonterminal runs and the pending or leased deliveries of that set
in bounded frames, each of which records how far it got, and a last frame moves the
record to the `awaiting-decision` phase. A crash before the first frame leaves the origin
entry, and a crash during the pausing leaves the `pausing` record, so the next recovery
continues where it stopped. While an origin entry exists, every transition other than
that first recovery frame is refused under the writer, and a lookup of a recorded
receipt is answered. While a `pausing` record exists, a submission, a claim, a renewal, a
publication, a failure under a lease and every delivery claim or send are refused under
the writer, and a cancellation and a receipt lookup are answered. The epoch is installed only after the record reaches
`awaiting-decision`, so no admission check passes before then.

Once the record is `awaiting-decision`, runs and deliveries outside the restored set are
dispatched as usual, and the paused ones wait. This holds whether the restore kept the
dataset's UUID or minted a new one, including a retry of an older same-UUID backup. Old
lease tokens fail the epoch check of the new process. An operator must map principals and
secret references, confirm source continuity and choose whether previously uncertain
external effects may be retried before resuming selected paused work through the recovery
route of §6. That route refuses while the record is `pausing`, and it checks under the
writer that each run or delivery it resumes is paused and belongs to the restored set. A
paused run can be cancelled meanwhile.

Membership in the restored set is decided from the record itself. The recovery record
keeps the checkpoint's sequence number, and every run records the sequence number of the
frame that submitted it, so a run at or below that number belongs to the set. An explicit
retry of a member while its recovery awaits a decision, whether the member was paused and
cancelled or was already terminal, creates its new run paused and records the same
recovery on it, which makes the new run a member too. Only the recovery route then makes
it eligible. A submission that is not a retry of a member is admitted as usual.

The backup manifest lists `control.bin` and `jobs.json` as root files, and both are meta
files that a later backup replaces whole and never extends as an appended prefix. A
restore checks the control closure before it reidentifies or publishes the dataset,
whether or not an integrity check was asked for. The feature `control/1`, a `jobs.json`,
a jobs entry in the captured registry and a captured log of version 3 each require a
`control.bin`, and a `control.bin` requires the feature. A dataset reader of 9 alone does
not, since an interrupted enablement may leave it. The checkpoint must carry an origin
entry that names the backup's source dataset and captured commit, which must also be the
checkpoint's business head. A backup that holds `control.log` is refused, and so is one
that breaks any of these rules, such as one that keeps the feature and leaves out the
checkpoint. The persistent, in-memory and materialized captures all render the
checkpoint, and the extra files a backup creation may add or replace never include
`control.bin`. A restore that the current release refuses, such as a new-identity restore
of a dataset with configuration history, stays refused.

A clone is a new dataset whose copied `jobs.json` raises its reader to 9, and it starts
with an empty control keyspace, an empty `control.bin` and an empty `control.log`. A
clone of an activated store stays refused as it is now. A clone holds no run, lease or
delivery identity of its source, so nothing in it can resume old work or reuse a
delivery's deduplication identity. A later recovery feature may copy selected records
into a clone under new identities with `origin` references.

Restoring an old backup cannot recover evidence that a remote receiver accepted after
that capture. The recovery interface calls out that uncertainty. A provider with an
independent deduplication ledger can safely accept the event's retained UUID again, but
this feature does not assume every provider does so.

### 7.4 Migration

A dataset with a ledger in a hidden RDF graph was never released, so no migration reads
one. A dataset that is enabled for jobs by this build gets the control keyspace from its
first enablement.

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
| A10. A backup with a pending delivery is restored or cloned. | A restored delivery stays paused until explicit recovery, and a clone holds no ledger. Old lease tokens fail and no unsolicited send occurs. |
| A11. A queue profile is newer than the reader understands. | Mutation and dispatch refuse cleanly, leaving authoritative records unchanged. |
| A12. A worker outputs an oversized or invalid batch. | Validation refuses business effects, checkpoint and outbox together. The run records a bounded diagnostic. |
| A13. A scheduler misses many occurrences during downtime. | Coalescing creates one pending occurrence and records skipped slots within its bounds. |
| A14. A ready index is deleted or stale. | Rebuild and reconciliation find every eligible committed run without creating new run identities. |
| A15. Control-only and control-carrying transactions interleave with ordinary commits. | History, diffs, the change log, commit numbers and counts, statistics, listings, export, the indexes, a compaction, a branch's base and a restored backup's quads are the same as on a store given only the business writes. |
| A16. The control logs are inconsistent at open. | A gap, a repeated sequence number, a frame that follows a lost commit, a damaged record before a complete one or an unknown version is an integrity error, and the open changes no file. |
| A17. A crash interrupts enablement, a checkpoint or a generation switch. | The next open recovers the same control state, and an interrupted enablement can be resumed. Enablement with a live direct log shorter than one block leaves a header of version 3. |
| A18. A process crash is followed by a reopen that writes, then a power loss. | Frames recovered from the page cache were synchronized before the reopen wrote, so the two logs still form one prefix. |
| A19. A publication makes no net change to the quads. | It creates no commit, its record names the head it follows, and a retry returns the same receipt. |
| A20. A reader loads the control state and a snapshot while a publication commits. | The snapshot holds every commit the control state names. An ordinary commit that unwinds before its snapshot publishes poisons the writer, so no control-only frame follows it. |
| A21. A restore is interrupted, retried and recovered while workers and the operator act. | Nothing is claimed, sent or published before the restored set is paused, the recovery continues after a restart, and the recovery route refuses while pausing. |
| A22. A backup keeps the control feature or settings without its checkpoint. | The restore refuses it before reidentification, also without an integrity check. |
| A23. A bulk commit or a compaction unwinds after `CURRENT` switched. | The writer is poisoned, so no control-only frame follows a generation that is not published. |
| A24. `control.log` is lost after a control-only submission that follows a generation switch, or after a restored dataset's recovery has consumed its origin and acknowledged a cancellation or a submission. | The open refuses with an integrity error. |
| A25. A restored run is cancelled and retried, or a terminal restored run is retried, before a decision. | The new run is paused and belongs to the restored set until the recovery route resumes it. |
| A26. A backup is captured before enablement and written after it, also incrementally. | The backup holds the old header, has no control files and restores as a dataset without jobs. |

Fault injection covers before and after vocabulary append, WAL append, control log
append, synchronization, snapshot and control state publication, each step of a
checkpoint and response rendering. Each test reopens the store and checks
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
publication. The design instead keeps the ledger in the store's own control keyspace and
writes a publication's records in its commit's log record.

Control-only transitions could also be interleaved with the commits of the write-ahead
log as transactions of their own, so that the store keeps one log file. Every reader of
that log, which includes replay, the commit index, forward and backward history readers,
the torn-tail rule, the change log, diffs, branch bases, backups and the integrity check,
would then have to frame and skip transactions that are not commits, and the business
log would grow with lease renewals and trigger compactions of the index. The separate
control log keeps the business log's commit framing as it is, and §7.1 orders the two
logs by one writer lock and one sequence of frames.
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

Sections 2 and 7 describe the control keyspace that roadmap item 933 builds. Until it
ships, Phase 1 keeps the ledger in a hidden RDF graph as the rest of this section
describes.

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

The ledger graph is the first owner graph with the `hidden` visibility of the registry. The
registry lists the hidden graphs of its owners, and they leave every graph view a request
or a library query runs under: the server's authentication middleware subtracts them from
the principal's view for every route of the dataset, and the library subtracts them from
the view of every SPARQL query it runs, including a caller's explicit all-graphs view.
The server's shared options boundary subtracts them again for a dataset selected outside
the request path, such as a local `SERVICE` target or a resource header. The library's
schema, validation, forms, explanation, entailment analysis, ontology and GraphQL
drafting handles subtract them from the view they are given. Of the dataset's ordinary
surfaces, only the typed inspection of the jobs handle reads the ledger, and the raw
`Store` remains a trusted interface whose callers supply the exclusion. The surfaces that
take no view follow the exclusion on their own: the statistics
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

The per-surface exclusion was reviewed in several rounds, and each round found surfaces
that read a snapshot, a cache or a guard's source without the subtraction. Those findings
led to the comparison of structural alternatives in §2.3 and to item 933. Incremental
queries and kept views now resolve their access with the hidden graphs subtracted, and a
live subscription ends with a `forbidden` error when the hidden graphs of its dataset
change, including one admitted while they change. A write-time guard whose shapes or
schema are in a hidden graph is answered as no guard by validation, constraints and
GraphQL drafting. A write-time guard leaves the hidden graphs out of the data it
validates, enabling jobs installs the guard again with the ledger left out before the
ledger can hold a record, and the guard's kept state of the head is keyed by the hidden
graphs as well as its configuration, so a state kept before they changed is validated
again. A guard configuration that names a hidden graph as a source or a data graph is
refused as for a graph that does not exist. Merge reports count neither the
changes nor the conflicts, resolved or not, of a hidden graph, and a merge receipt, an MCP
merge result and a merge task's detail carry no commit counts on a dataset with hidden
graphs, also for a task that finished before the dataset hid them.

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
