# X09: Group commit by default

> **Status:** Phases 1 and 2 built, with exceptions listed in the Outcome. Phase 3 not started.
>
> **Phases:** Phase 1 is the measurements and fixes that the switch depends on: serial
> latency, reader tails, metrics and power-loss and sync-failure tests. Phase 2 widens
> admission to the writes that fall back today (commit messages, preconditions,
> validation, Graph Store and RDF Patch writes, full-text, spatial and embedding hooks,
> writes during compaction). Phase 3 makes group commit the default for persistent
> stores, with `--no-group-commit` to opt out. Phases 1 and 2 are built, and Phase 3 waits
> on the conditions of section 7.2.
>
> **User docs:** [FEATURES.md](../FEATURES.md) and [API.md](../API.md#metrics) for the
> metrics and the `groupCommit` and `commits` fields of `/$/stats`.
>
> Sections 1 to 16 are the design as written before implementation. The
> [Outcome](#outcome) section at the end records how it landed and where it differs.

This design was written from the Sparkles code (`crates/sparkles-core/src/store/group.rs`,
`store.rs`, `store/wal.rs`, `commit.rs`, `annotations.rs`, `crates/sparkles-server/src/main.rs`
and its router test `http/router_tests/group_commit.rs`), the write measurements in
[BENCHMARKS.md](../BENCHMARKS.md#writes-and-updates), the commit identity spec
[CI](CI-commit-identity.md) and its Outcome on the commit record's unfenced count, the
published work on group commit and on crash consistency listed in §16, the PostgreSQL
and MySQL documentation of their commit-delay settings, and the Linux documentation of
the device-mapper `log-writes` target. It builds on write-time validation
([C10](C10-write-time-validation.md)), automatic compaction
([C13](C13-automatic-compaction.md)), write previews ([C15](C15-write-previews.md)),
graph access control ([C12](C12-graph-access-control.md)), full-text search
([F03](F03-full-text-search.md)), embeddings on write ([F08](F08-embeddings-on-write.md)),
replication through RDF Patch ([F10](F10-replication.md)) and GeoSPARQL
([G01](G01-geosparql.md)).

## 1. Summary, goals, non-goals

Every Sparkles commit is durable when it is acknowledged: its WAL record and any new
vocabulary entries are synchronized first. By default each commit pays its own
synchronization under the single writer. An opt-in mode, `--experimental-group-commit`
(a hidden global flag, `crates/sparkles-server/src/main.rs:169`), lets concurrent
commits share one synchronization and acknowledges each only after the prefix that holds
it is durable. In BENCHMARKS.md it gives 1.6× to 2.45× the acknowledged transactions of
the default writer with four writers, at 1.05M and 10.5M, with every receipt contiguous
and the final state correct.

The mode is off by default and admits only some writes. This spec lists what has to be
true before it becomes the default, makes the remaining writes eligible where that is
safe, and defines the tests and measurements that gate the switch.

**Goals**

1. Group commit is the default for persistent stores in the server, the CLI and the
   embedding API, and `--no-group-commit` turns it off.
2. Acknowledged commits are durable, as today, under process crashes, power loss and
   failed synchronizations.
3. A serial writer is no slower than with the default writer today, within 5%.
4. Readers under write load are no slower than with the default writer, within 10% at
   p99.
5. The writes that fall back to the ordinary path today join groups when they can do so
   without weakening their guarantees (§6).
6. Operators can see whether grouping happens, how large groups are and why writes fall
   back (§8).

**Non-goals**

- Acknowledging writes before they are durable. No asynchronous commit mode is added.
- Group commit for in-memory stores, which have no WAL.
- Grouping bulk commits, which switch `CURRENT` to a rebuilt generation and need no WAL
  records for their quads.
- Parallel writers. Transactions still run one at a time under the writer lock. Only the
  synchronization is shared.
- Changing the WAL layout. The commit record's unfenced count (CI Outcome, byte 28)
  already supports groups. Phase 2 adds one flag bit to byte 27 and no field.

## 2. The current design

`store/group.rs` implements caller-led durable prefix publication:

1. A transaction that is admitted (§2.1) writes its WAL records into the log with a
   buffered write, under the writer lock, and enqueues a `Pending` entry with its
   snapshot, its changes and a `Ticket`. It records in its commit record how many
   commits before it may not be durable yet (`seal_wal_commit_unfenced`, at most
   `MAX_UNFENCED` = 255). It then releases the writer lock and waits on its ticket.
2. The first waiter that finds no fence running becomes the leader. It seals the whole
   queue as one prefix and calls `sync_commit_reused`, which runs `fdatasync` on the WAL
   in parallel with the delta vocabulary's sync. There is no collection delay: the
   leader seals immediately, and commits that arrive during the fence form the next
   prefix.
3. If the sync succeeds, the leader publishes every commit of the prefix in order
   (`publish_group_prefix`: WAL end, WAL index, change log, catalog, compaction clock,
   current snapshot, commit watch) and sets their tickets. If it fails, the coordinator
   fails, every queued and in-flight ticket fails, and the writer is poisoned
   (`Error::Poisoned`: later writes are refused until the store is reopened; reads go
   on).
4. A queue is bounded by `MAX_COMMITS` = 64 commits and `MAX_BYTES` = 4 MiB. A
   transaction that would exceed it drains the queue first.
5. A SPARQL update may start from the *speculative* head, the state written but not yet
   synchronized (`Store::speculative_base`). Its caller sees nothing of that state, and
   its commit fails with its predecessor's. An explicit transaction waits for the
   pending sync before it starts, since it exposes its view to the caller.

### 2.1 Admission today

`Store::group_admission` and `WriteTxn::commit_inner` admit a commit only when all of
these hold. Anything else drains the queue and takes the ordinary path:

| Condition | Source |
|---|---|
| Group commit enabled, writer not poisoned, persistent store with a WAL | `group.is_some()`, `!w.poisoned`, `w.wal.is_some()` |
| No compaction running | `w.tap.is_none()` |
| Kind is `Transaction` or `Update` | Graph Store writes, uploads and RDF Patch are not admitted |
| No `bypass_validation`, no dry run, no commit message, no precondition, no graph restriction | `WriteOptions` fields |
| No change digests, no required or installed guard | `commit_digests()`, `guard_required()`, `guard` |
| No full-text index, no spatial index, no pending embedding work | `text_enabled()`, `geo`, `embed.works` |
| Not bulk, not forced, not a merge, at most 4 MiB of WAL | `commit_inner` |
| A net change | A no-op drains, so its receipt names a durable head |

### 2.2 What the tests cover

`group.rs` has 24 tests: ordering and receipts under concurrency, reopen,
singleton commits, publication order, dependent no-ops, a later write failure during a
running fence, a failed fence, a panicking publisher, cancellation of a waiting
capture, previews and preconditions observing only a durable head, a bounded queue, and
a child-process test that exits at four failpoints (`group-appended`, `group-sealed`,
`group-synced`, `group-published`) and checks that every acknowledged commit survives.
Two tests check that damage inside an unfenced suffix is treated as a torn tail and
damage before a later durable record as corruption. The server has one router test
with 32 concurrent HTTP updates.

Process exit does not simulate power loss: written but unsynchronized bytes survive a
process exit in the page cache. The tests do not cover lost writes, reordered writes or
a failed `fdatasync`.

### 2.3 Measurements and open findings

From [BENCHMARKS.md](../BENCHMARKS.md#concurrent-durable-commits):

| | Default writer | Group commit |
|---|---|---|
| Four writers, 1.05M, transactions/s | 221.5–231.8 | 529.0–543.3 |
| Four writers, 10.5M, transactions/s | 201.1–204.0 | 402.3–406.9 |
| Four writers and two readers, 1.05M, transactions/s | 1,375–1,386 | 3,344–3,365 |
| Four writers and two readers, 10.5M, transactions/s | 202–231 | 370–386 |
| Four writers and two readers, 10.5M, reader p99 (ms) | 1.410–1.549 | 1.960–2.143 |

Two findings gate the switch:

1. **Reader tail.** At 10.5M, readers' p99 is about 40% higher with group commit. The
   cause is not known. Candidates are the leader's publication work, the extra
   snapshots published per second, and contention on the coordinator's mutex.
2. **Serial latency.** No single-writer run with group commit is published. An admitted
   singleton writes with a buffered write and `fdatasync`, while the default writer
   uses `O_DIRECT | O_DSYNC` writes into preallocated space (`commit_direct`), which a
   device with FUA can persist without a cache flush. A serial writer could be slower
   with grouping on.

## 3. Guarantees

These hold with group commit on, and the tests of §10 check each of them.

1. **Durable acknowledgement.** A receipt is returned only after the WAL records and
   vocabulary entries of its commit, and of every commit before it, are synchronized.
2. **Gap-free order.** Commit sequence numbers are allocated under the writer lock and
   published in that order. A failed commit never leaves a published gap: the commits
   after it in its prefix and queue fail too.
3. **Readers see durable state only.** A snapshot is published after its prefix is
   durable. Only a SPARQL update's own transaction may read speculative state, and its
   commit cannot outlive its predecessor's failure.
4. **Recovery yields a prefix.** After a crash or power loss, the recovered head is a
   commit at or after the last acknowledged one, and every commit up to it is intact.
   Damage inside an unfenced suffix is a torn tail.
5. **Bounded wait.** A commit waits for at most the fence in flight when it enqueues and
   the fence that holds it. A leader leads at most one fence for others before its own
   ticket is set, because the prefix it seals includes its own commit.
6. **Failure is explicit.** A failed synchronization fails every commit that was not
   acknowledged, poisons the writer and reports `Error::Poisoned` (HTTP 503), including
   the commit that led the fence. The ordinary path reports the raw I/O error for the
   commit whose own synchronization failed and `Error::Poisoned` for later writes.

## 4. Phase 1: measurements and fixes before the switch

### 4.1 Serial latency

Run the durable commit latency harness of BENCHMARKS.md (serial single-triple commits,
6 rounds, fresh stores, literal and IRI inserts and deletes, 1.05M and 10.5M) with
grouping on and off.

If the grouped singleton is more than 5% slower at the median or p90, an idle fast path
is added: when the coordinator has no fence running and an empty queue, and no other
transaction is waiting for the writer lock, the commit takes the direct path
(`commit_direct`) under the writer lock, as the ordinary writer does. A writer waiting
for the lock is visible through a counter incremented before `lock()` and decremented
after. The fast path keeps the commit record's unfenced count at 0. A commit that
arrives while a fast-path commit holds the lock queues as usual and groups with the
commits after it.

### 4.2 Reader tails

Profile the 10.5M four-writer, two-reader run with grouping on and off, with
`perf record` on the server and per-request timers. Then fix what the profile shows.
The candidate fixes, each measured separately:

- Publish one snapshot per prefix instead of one per commit. The intermediate snapshots
  of a prefix are never visible to readers in practice, since the leader publishes them
  back to back. The catalog, the change log and the commit watch still get every commit.
  The current snapshot is stored once with the last commit's state.
- Move the change log and WAL index updates of `publish_group_prefix` out of the
  coordinator's mutex, keeping their order.
- Pin the leader's work to the writing thread's runtime worker, so readers' workers are
  not taken by publication.

The gate is §9's reader p99.

### 4.3 Synchronization failures

An `fdatasync` that fails can drop dirty pages from the page cache, so retrying it can
report success for data that was lost (Rebello et al., §16). The ordinary path and the
group path already poison the writer on any failed sync. Phase 1 adds tests that make
the sync fail (§10.3) and checks:

- every unacknowledged commit returns an error;
- no snapshot after the last durable commit is published;
- the store reopens to a prefix that includes every acknowledged commit;
- the vocabulary sync's failure is handled the same way as the WAL's, including when
  only one of the two parallel syncs fails.

### 4.4 Metrics

The metrics of §8 land in Phase 1, so the effect of the switch can be seen in
deployments that try the flag before the default changes.

## 5. Cancellation, deadlines and fairness

- **Before the WAL write.** A write's cancellation and deadline (`WriteOptions::check`)
  are checked before anything is written, as today. A cancelled or expired write
  publishes nothing.
- **After the WAL write.** Once its bytes are in the log, a commit cannot be withdrawn:
  a later commit may already name it as a predecessor. A client that disconnects while
  waiting loses its receipt, and the commit stands. This is the same as the ordinary path
  after its write starts. The commit's tracing span already records its sequence number
  (`seq`). The access log gains the same field for writes, so an operator can find the
  commit of a disconnected request.
- **`no_wait`.** A write with `no_wait` never waits for a fence. It fails with
  `Error::WriterBusy` when it finds a fence running or commits queued once it holds the
  writer lock, and when its own commit would be grouped, before its guard runs and before
  anything reaches the WAL. After a failed fence it fails with `Error::Poisoned`, since a
  retry cannot succeed until the store is reopened.
  The server's in-place updates set `no_wait` and retry a `WriterBusy` on the blocking
  pool, so a runtime worker never parks in a fence.
- **Fairness.** The queue is first in, first out, and prefixes are published in order.
  A transaction that waits for the writer lock is not overtaken by grouped commits
  except through the lock's own order. Phase 1 measures, with 16 writers, the spread of
  per-writer throughput. If one writer's share differs from the mean by more than 25%,
  the writer lock becomes a fair lock (`parking_lot::Mutex` unlocks fairly on request
  with `unlock_fair`), used after a commit enqueues.

### 5.1 Batch wait

The leader seals immediately. A collection delay, as in PostgreSQL's `commit_delay` or
MySQL's `binlog_group_commit_sync_delay`, would trade latency for larger groups. With a
single writer lock, commits arrive at most as fast as transactions execute, and the
fence in flight already collects them. Phase 1 measures group sizes with 4 and 16
writers. A delay option is added only if a delay of at most 200 µs raises throughput by
more than 10% at 16 writers without raising p50 by more than its length. Its default is
0 in any case.

## 6. Phase 2: wider admission

Each item below becomes eligible separately, with its own test, and the rest keep
falling back until they land.

| Fallback today | Design for admission |
|---|---|
| Commit message, change digests | The annotation record is appended without its own sync. The fence synchronizes the annotation file in parallel with the WAL and vocabulary. `annotations.rs` already drops records above the recovered head at open, so a record whose commit was not durable is removed. The opposite case needs more: the kernel may write a commit's WAL records back before its annotation, so a commit that was never acknowledged could be recovered without the message or digest it asked for. The commit record of such a commit carries `WAL_FLAG_ANNOTATED`, and replay treats a flagged commit whose annotation is missing as a torn tail, unless a later record shows the commit was durable (see the Outcome). |
| Precondition (`If-Match`, RDF Patch `H prev`) | The precondition is checked against the speculative head. If it holds, the commit joins the group, and fails with its predecessor. If it fails, the transaction drains the queue and checks again against the durable head before it reports 412, so a refusal is never based on state that a failed sync then lost. |
| Required or installed guard (C10) | Validation runs under the writer lock against the transaction's base, as today. An update may validate against the speculative head. An accepted write joins the group. A rejection drains the queue first and reports only if the head it was judged against is durable. Otherwise it is re-validated. A validation summary in a receipt is returned with the receipt, after the fence. |
| `bypass_validation` | The grouped record carries `WAL_FLAG_UNVALIDATED` like the ordinary record: `seal_wal_commit_unfenced` takes the flags that today are passed as 0. |
| Graph restriction (`WriteOptions::graphs`, C12) | The restriction is enforced as each quad is added, before commit, so it does not depend on the commit path. The condition is removed from admission once a test shows a refused quad never reaches the WAL in either path. |
| Graph Store PUT, POST, DELETE and uploads that are not bulk | Admitted as `Update` is. Explicit transactions keep waiting for durable state before they start (§2, item 5). |
| RDF Patch (`CommitKind::Patch`) | Admitted with its `H prev` handled as a precondition. A replica applying patches (F10) keeps the ordinary path, since it must not acknowledge a pull before it is durable and has no concurrent writers to group. |
| Full-text index (F03) | `publish_group_prefix` calls `Store::maintain_text` for each commit in commit order, as `publish_log` does on the ordinary path. Grouped snapshots then carry the text state instead of `text: None`. The text index already replays the WAL after a crash, so it never depends on a commit it has not seen being durable. |
| Spatial index (G01) | `Store::maintain_geo` runs at publication, in order, for each commit of a prefix, as in `publish_log`. |
| Pending embedding work (F08) | `Store::embed_noted` runs at publication for each commit, in order, as in `publish_log`. |
| Compaction running (`tap`) | The tap records each commit's changes when the commit enqueues, under the writer lock. A failed fence poisons the writer, and the compaction checks the poison flag before its switch and aborts, so a generation never contains a commit that was not durable. |

Merges, forced commits, bulk commits and commits above 4 MiB keep the ordinary path.
No-ops keep draining.

## 7. Phase 3: the default

### 7.1 Options

| Surface | Today | After |
|---|---|---|
| `StoreOptions` | `experimental_group_commit: bool`, default `false` | `group_commit: bool`, default `true`; ignored for in-memory stores |
| CLI and server | Hidden `--experimental-group-commit` | `--no-group-commit` (global), and `SPARKLES_GROUP_COMMIT=off` |
| Python, Node.js, JVM | Not exposed | A `group_commit` option where the binding exposes store options, default on |
| `/$/stats` | Not reported | `groupCommit: true/false` per dataset |

There is no release yet, so `--experimental-group-commit` is removed rather than kept as
an alias. The docs that name it (FEATURES.md and the concurrent sections of
BENCHMARKS.md) change in the same commit. The server test
`durable_grouping_requires_explicit_cli_opt_in` in `main.rs` is replaced by one that
checks the default is on and that `--no-group-commit` turns it off.

### 7.2 Conditions for the switch

1. §9's gates are met on `forge` at 1.05M and 10.5M.
2. Every test of §10 passes on ext4 and XFS, and the power-loss test of §10.2 passes
   for 1,000 crash points per file system.
3. The fallbacks of §6 that ordinary deployments hit (commit messages, preconditions,
   guards, full-text, compaction) are admitted, or the metrics of §8 show they are rare
   in the measured workloads.
4. BENCHMARKS.md is refreshed under the protocol of [X06](X06-benchmark-coverage.md),
   with the write tables measured with the new default and a row for
   `--no-group-commit`.

## 8. Observability

| Metric | Type | Meaning |
|---|---|---|
| `sparkles_commit_sync_seconds` | Histogram, labels `dataset`, `path` (`direct`, `buffered`, `group`) | Time of each synchronization |
| `sparkles_group_commit_size` | Histogram, label `dataset` | Commits per fence |
| `sparkles_group_commit_wait_seconds` | Histogram, label `dataset` | From enqueue to acknowledgement |
| `sparkles_group_commit_fallbacks_total` | Counter, labels `dataset`, `reason` | Commits that took the ordinary path, by the first failing condition of §2.1 |
| `sparkles_group_commit_failures_total` | Counter, label `dataset` | Fences that failed |

The reasons are a fixed set named after the conditions (`message`, `precondition`,
`guard`, `text`, `geo`, `embed`, `compaction`, `kind`, `bulk`, `size`, `noop`,
`digests`, `bypass`, `graphs`, `poisoned`). `/$/stats` reports the same counts per
dataset. The request log's write records gain the commit's group size.

## 9. Targets

Measured on `forge` with the harnesses of BENCHMARKS.md, ranges over fresh processes.

| Measurement | Gate |
|---|---|
| Serial commits, literal and IRI inserts and deletes, 1.05M and 10.5M | Median and p90 within 5% of the default writer of today |
| Four writers, 50,000 changes | At least the published group-commit ranges (529–543 and 402–407 transactions/s) |
| Four writers and two readers, 10.5M | Reader p99 within 10% of the default writer (1.410–1.549 ms) |
| 16 writers | Per-writer throughput within 25% of the mean |
| Mixed mode of `scripts/bench.sh` | Reads/s within 5% of today, writes/s at least today's |
| The `updates` churn (5,000 serial commits) | Commits/s within 5% of today |

## 10. Tests

### 10.1 Unit and integration

- The existing tests of `group.rs` run with the new default, and with grouping off.
- A model-based stress test: N threads issue random small transactions, updates,
  no-ops, previews, preconditions and explicit transactions, with random failpoints
  (`group-appended`, `group-sealed`, `group-synced`, `group-published`) set to panic or
  to fail. It checks the guarantees of §3 against a sequential model after every run,
  and the reopened store against the acknowledged log.
- One test per admission change of §6, including the failure of a fence with each kind
  of hook or annotation pending.
- A router test per HTTP write kind (update, Graph Store, upload, RDF Patch) with 32
  concurrent requests, as `router_tests/group_commit.rs` does for updates.

### 10.2 Power loss

Process exit leaves written pages in the page cache, so it cannot show lost writes. The
power-loss test runs the store on a block device behind Linux's device-mapper
`log-writes` target, which records every write, flush and FUA in order. A driver runs
16 writers against the server and records each acknowledged receipt to a log on another
device. Afterwards a replay tool reconstructs the device as it was at each flush or FUA
mark, and at random points between them with unflushed writes dropped. For each image,
the store is opened and checked:

- the head is at or after the last receipt acknowledged before that point;
- every acknowledged commit is present with its changes, annotation and vocabulary
  terms;
- `sparkles check` passes;
- a reopened store accepts writes and continues the sequence without a gap.

This needs root and a kernel with the target, so it runs in a dedicated CI job on a
self-hosted runner or a VM, nightly, on ext4 and XFS. It covers the ordinary path too.

### 10.3 Failed synchronization

An `LD_PRELOAD` shim (a test-only C file in `crates/sparkles-core/tests/`) makes
`fdatasync`, `fsync` or an `O_DSYNC` `pwrite` on the WAL or `delta.vocab` fail with
`EIO` at a chosen call. The device-mapper `error` or `flakey` target is used where the
shim cannot reach the call. The test checks §4.3's conditions for each call position:
the first sync, a sync during a running fence, the vocabulary sync alone, and the WAL
sync alone.

### 10.4 Server crashes under load

`kill -9` of the server during an HTTP write load with 16 clients, at random times, 200
times per file system, with receipts logged by the clients. After each restart, every
logged receipt's commit is present.

## 11. Architecture

| Area | Files and types | Change |
|---|---|---|
| Option | `store.rs` (`StoreOptions`), `sparkles-server/src/main.rs` (`Cli`, `store_opts`), the bindings' option types | Rename, default on, opt-out flag and variable |
| Idle fast path | `store.rs` (`commit_inner`, `commit_direct`), `store/group.rs` (`Coordinator`) | Waiter counter, direct path when idle |
| Publication | `store/group.rs` (`publish_group_prefix`, `drive`) | One snapshot per prefix, work outside the mutex, hooks for text, spatial and embeddings |
| Admission | `store.rs` (`group_admission`, `speculative_base`), `store/group.rs` (`publish_grouped`) | §6 conditions, flags, preconditions, guard rejections |
| Annotations | `annotations.rs`, `store.rs` (`annotate`) | Append without sync, sync in the fence |
| Compaction | `store/compaction.rs` (`Tap`), `store.rs` | Tap at enqueue, poison check before the switch |
| Metrics | `sparkles-server/src/obs.rs`, `/$/stats` | §8 |
| Tests | `store/group.rs`, `crates/sparkles-core/tests/`, `sparkles-server/src/http/router_tests/group_commit.rs`, a new power-loss harness under `scripts/` | §10 |

## 12. Phases

| Phase | Content | Size | Gate |
|---|---|---|---|
| 1 | §4: serial and reader measurements, the idle fast path if needed, reader-tail fixes, sync-failure and power-loss tests, metrics | S–M | A1–A5 |
| 2 | §6: admission of messages, digests, preconditions, guards, Graph Store, uploads, RDF Patch, full-text, spatial, embeddings and compaction | M | A6–A9 |
| 3 | §7: default on, opt-out flag, docs and BENCHMARKS.md refresh | S | A10 |

## 13. Acceptance examples

- **A1 — Serial parity.** 5,000 serial single-triple commits at 10.5M with the default
  on have a median and p90 within 5% of `--no-group-commit`.
- **A2 — Concurrent gain kept.** Four writers at 1.05M acknowledge at least 529
  transactions/s with contiguous receipts.
- **A3 — Reader tail.** With four writers and two readers at 10.5M, reader p99 is
  within 10% of the run with grouping off.
- **A4 — Failed sync.** A WAL `fdatasync` that fails during a fence of 10 commits fails
  all 10 requests with 503, publishes none of them, and the reopened store's head is at
  or after the last commit acknowledged before the fence, with no gap.
- **A5 — Power loss.** On ext4 and XFS, 1,000 replayed crash points each recover a head
  at or after the last acknowledged receipt, with `sparkles check` passing.
- **A6 — Messages.** 32 concurrent updates with commit messages are grouped (the size
  histogram shows fences larger than one), and every receipt's annotation is present
  after a crash at `group-sealed`.
- **A7 — Precondition refusal.** An `If-Match` write whose tag matches the durable head
  but not a speculative one that later fails is accepted after the drain, not refused.
- **A8 — Guard rejection.** A write rejected by a SHACL guard against a speculative
  state is reported only after that state is durable, and is re-validated if the fence
  fails.
- **A9 — Compaction.** Writes during an automatic compaction are grouped, and a fence
  failure during the build aborts the compaction without switching generations.
- **A10 — Default and opt-out.** `sparkles serve` reports `groupCommit: true` in
  `/$/stats`, and `sparkles --no-group-commit serve` reports `false`. An in-memory
  dataset reports `false`.

## 14. Rejected alternatives

- **An asynchronous commit mode** that acknowledges before synchronization, as
  Oxigraph's default effectively does. It would win the write tables that Oxigraph wins
  today, but it changes what a receipt means. X06 §5.1 reports durability classes
  instead.
- **A dedicated log-writer thread.** A thread that owns the log and syncs it adds a
  hand-off per commit. The caller-led leader of `group.rs` already syncs without one,
  and a waiter has nothing else to do.
- **Retrying a failed `fdatasync`.** After a failure the kernel may have dropped the
  dirty pages, so a successful retry proves nothing. The writer is poisoned instead.
- **A nonzero default batch delay.** It adds latency to every serial commit. §5.1 adds
  one only on evidence, with a default of 0.
- **Grouping bulk commits.** A bulk commit's durability point is the `CURRENT` switch,
  not the WAL. There is nothing to share.
- **Keeping the flag opt-in.** The concurrent gain applies to every persistent server
  with more than one writer, and the guarantees are the same as the ordinary path's
  once §10 passes.

## 15. Open questions

1. Whether the idle fast path (§4.1) is needed at all. It depends on the serial
   measurement.
2. Whether one snapshot per prefix (§4.2) changes anything a client can observe. A
   reader started between two publications of one prefix could see the earlier commit
   today. After the change it sees the later one, which is also durable.
3. Whether the power-loss harness can run on hosted CI, or only on a self-hosted
   runner.
4. Whether dataset settings (C19) should be able to turn grouping off per dataset, or a
   process-wide option is enough.

## 16. Sources

- D. J. DeWitt, R. H. Katz, F. Olken, L. D. Shapiro, M. R. Stonebraker and D. A. Wood,
  "Implementation Techniques for Main Memory Database Systems", SIGMOD 1984, where group
  commit was described.
- P. Helland, H. Sammer, J. Lyon, R. Carr, P. Garrett and A. Reuter, "Group Commit
  Timers and High Volume Transaction Systems", HPTS 1987.
- T. S. Pillai et al., "All File Systems Are Not Created Equal: On the Complexity of
  Crafting Crash-Consistent Applications", OSDI 2014.
- A. Rebello, Y. Patel, R. Alagappan, A. C. Arpaci-Dusseau and R. H. Arpaci-Dusseau,
  "Can Applications Recover from fsync Failures?", USENIX ATC 2020.
- The PostgreSQL documentation of `commit_delay` and `commit_siblings`, and the MySQL
  documentation of `binlog_group_commit_sync_delay`.
- The Linux kernel documentation of the device-mapper `log-writes`, `flakey` and `error`
  targets.
- [BENCHMARKS.md](../BENCHMARKS.md), "Writes and updates".
- The Sparkles code named in §2 and §11, and the specs CI, C10, C12, C13, C15, C19, F03,
  F08, F10, G01 and X06.

## Outcome

Phases 1 and 2 are built behind `--experimental-group-commit`, with the exceptions below.
Phase 3 is not started, because conditions 1, 2 and 4 of section 7.2 need a quiet `forge`
run and a host with device-mapper `log-writes`, and condition 3 is not yet met for
full-text indexes. See [ROADMAP](../ROADMAP.md) item 3.

### Phase 1

- **Metrics.** `Store::commit_metrics` counts synchronizations by path, commits per fence,
  the wait from enqueue to acknowledgement, failed fences and fallbacks by their first
  failing condition. `/$/metrics` exports them as `sparkles_commit_sync_seconds`,
  `sparkles_group_commit_size`, `sparkles_group_commit_wait_seconds`,
  `sparkles_group_commit_fallbacks_total` and `sparkles_group_commit_failures_total`.
  `/$/stats` reports `groupCommit` and `commits` per dataset, where `fences` counts the
  fences that synchronized successfully and `failures` the ones that did not. The fallback
  reasons are `poisoned`, `kind`, `text`, `geo`, `embed`, `bulk`, `size` and `noop`. The
  other seven reasons of section 8 (`message`, `precondition`, `guard`, `compaction`,
  `digests`, `bypass` and `graphs`) name conditions that Phase 2 admits, so they are no
  longer reasons and have no series.
- **Serial latency (A1).** A grouped singleton was 4% to 9% slower than the default writer at
  1.05M, so the idle fast path of section 4.1 was added
  (`StoreOptions::group_commit_idle_direct`, on by default, hidden flag
  `--no-group-commit-idle-direct`). With it, serial literal and IRI inserts at 10.5M on a
  quiet Namespace instance were within 2.3% of the default writer at p90 and 0.5% at p50
  (5,000 commits, 8 rounds). A noisier instance showed up to 19% at p90 inside a 3.7× spread
  in the default writer's own runs. The idle path needs a recheck on `forge`.
  The Namespace serial throughput figures for literal, IRI and message commits are
  withdrawn because the client included warmups in their elapsed denominator. The timed
  latency samples excluded warmups and remain the serial latency evidence above. Delete
  seeding was already outside the timer. New serial throughput figures require a new run.

- **Concurrent gain (A2).** With 4 ms added to every synchronization, which reproduces the
  default writer's 230 transactions/s of `forge`, four writers reach 466 transactions/s
  (2.0×) and sixteen writers 1,783 (7.8×). The absolute `forge` ranges of section 9 are not
  measured.
- **Reader tail (A3).** The 40% regression at 10.5M did not reproduce on Namespace. Reader
  p99 was between 0.73× and 1.19× of the default writer's across runs, with overlapping
  ranges. No fix was made, so the candidate fixes of section 4.2 remain untried and the
  measurement has to be repeated on `forge`.
- **Failed synchronization (A4).** A router test fails ten requests of one fence with 503
  and publishes none of them. After reopening, the commit acknowledged before the fence and
  its insert are present and the head has no gap. Complete records of the failed fence may
  still recover, because the kernel can write them back before the failed sync, so A4 now
  says the head is at or after the last acknowledged commit rather than equal to it. A test under an `LD_PRELOAD` shim
  (`crates/sparkles-core/tests/power_loss.rs`, `tests/support/iofault.c`) fails the WAL, the
  vocabulary or both at chosen calls and checks the reopened store.
- **Power loss (A5), partly.** The same test records the store's I/O and replays crash images
  in user space, with each unsynchronized 4 KiB page kept or dropped at random. Every
  acknowledged commit and its message survives, `sparkles check` passes and the next commit
  continues the sequence. It ran up to 500 crash points per configuration, on ext4 only. It
  cannot reproduce file system journal and metadata ordering, so it does not replace the
  1,000-point `log-writes` runs on ext4 and XFS, which need root and the device-mapper
  target.
- **Fairness and batch wait.** Sixteen writers kept every writer's share within 0.063 of the
  total. Group sizes were large with a slow device, so no collection delay was added.
- **Wording fix.** Section 3, item 6 said the failing commit reports `Error::Poisoned` as the
  ordinary path does. The ordinary path returns the raw I/O error for that commit, and the
  leader of a failed fence now returns `Error::Poisoned`, so every request of the group gets
  503.

### Phase 2

| Item | Result |
|---|---|
| Update, Graph Store, upload, RDF Patch | Admitted as `Update` is, and may start from the speculative head. A router test sends 32 concurrent requests of each kind. |
| Commit message, change digest | Admitted (A6). The annotation is written without a sync before the WAL records, and the fence syncs `annotations.bin` in parallel with the log. |
| Precondition | Checked against the speculative head, and a refusal drains first (A7). |
| Bypass, graph restriction | Admitted. A refused quad of a restricted write never reaches the log. |
| Guard | Admitted. The SHACL guard keeps the state of each commit in flight and checks against the speculative head (A8). |
| Compaction | Admitted. The tap records each commit at enqueue, a drain precedes every catch-up round and the switch, and a failed fence aborts the compaction (A9). |
| Full-text, spatial, embeddings | Not admitted. See below. |
| ShEx guard | Takes the group path but not the speculative head, so each fence holds one commit. |

Four points differ from the design.

1. **A flag bit in the commit record.** Section 1 ruled out a format change. A commit's
   WAL records can reach the disk before its unsynchronized annotation, so after a crash a
   commit that was never acknowledged could be recovered without its message or change digest,
   leaving a hole in the digest chain. Such a commit carries `WAL_FLAG_ANNOTATED` (bit 1 of
   byte 27, see [CI](CI-commit-identity.md)), and replay treats a flagged commit whose
   annotation is missing as a torn tail, unless a later commit record shows it was durable. In
   that case its fence synced the annotation too, the file lost it afterwards, and the commit
   stays with a warning. The check is skipped when `annotations.bin` is missing or belongs to
   another dataset. `sparkles check` applies the same rule. Readers that predate the bit
   ignore it and lose only this check. This is an additive change under
   [X12 section 4.3](X12-release-engineering.md#43-how-a-format-changes), as recorded in
   `crates/sparkles-core/src/formats.rs`, so neither the WAL version nor the minimum reader
   changes for this bit.
2. **Errors that rest on speculative state.** Preconditions, every guard error, triple
   protections at both the per-row and commit checks, and an RDF Patch's late `H prev`
   mismatch can be judged against a speculative head. `Store::after_refusal` waits for the
   commits in flight to be durable and reports `Error::Poisoned` if their fence failed.
   SPARQL class and pattern protections use this routing too. Another drain after a
   protection check already drained is a no-op. RDF Patch syntax, term, graph, prefix and
   folding errors are reported immediately because they do not depend on the speculative
   head. Cancellation and deadline errors retain their direct propagation. A guard
   rejection increments its rejection counter even when the client receives `Poisoned`
   because the predecessor's fence failed.
3. **A7 as worded cannot happen.** A failed fence poisons the writer, so a write whose tag
   matched only the durable head is refused as poisoned. If the fence succeeds, the durable
   head is the speculative head and the refusal stands. The test checks both outcomes, and
   that nothing is reported before the fence is done. With `no_wait`, a data-dependent
   refusal that needs to drain instead returns `WriterBusy`, rather than the usual
   precondition or permission refusal, since it cannot wait. A `no_wait` write is also
   refused with `WriterBusy` when it finds a fence running or commits queued, and when its
   own commit would be grouped. After a failed fence it is refused with `Poisoned`. This is a core write option,
   with no HTTP header. The HTTP error conversion maps an escaping `WriterBusy` to 503.
   The server's internal inline attempt instead retries it through the blocking path.
4. **Full-text, spatial and embedding datasets stay on the ordinary path.** The design had
   `publish_group_prefix` call `maintain_text`, `maintain_geo` and `embed_noted`. A review
   found that the coordinator publishes without the writer lock, while the spatial build,
   text recovery and text rebuild install a view under that lock, so a grouped commit could
   publish an older view over a finished build. The fix is to drain the group in each of those
   installers, which is small, but such a dataset also could not start a transaction from the
   speculative head, since the snapshot carries no index state. Each fence would then hold one
   commit and the dataset would gain nothing. The fallback reasons `text`, `geo` and `embed`
   still count these commits. The ShEx guard is in the same position, because its typing state
   chains by commit number and has no pending list yet.

An independent RDF Patch abort returns the published head and its matching annotation
without waiting for a pending fence. A successful head check against the speculative
prefix makes the abort wait and propagate any fence failure. Ignored headers and
discarded data or prefix rows do not add a drain. Dry runs retain their admission drain
and distinguish a skipped leading check from an executed late check.

After a bypass, an unknown baseline also invalidates cached graph-backed shapes. The
next relevant check reloads the authoritative speculative state, then stages refreshed
shapes with its successful sequence for publication. Dry runs and rejected checks do not
publish them. Grandfather comparisons use the actual pre-schema. Repair of an unparseable
bypassed pre-schema treats post-state blocking results as new, while read failures
propagate and cannot be interpreted as an empty schema or a parse-only repair.

A validated version 1 WAL header establishes modern framing. Missing commit metadata
in such a log is damaged transaction state, not a reconstructed legacy commit. Later
checksum-valid records and their unfenced counts decide whether the suffix is torn or
the damage is corruption.

Headerless logs preserve the mixed legacy and v2 interpretation of
[CI section 5.6](CI-commit-identity.md#56-dataset-file-creation-migration), including legacy
records after v2 records. Torn modern metadata can be byte-identical to a genuine legacy
record even after earlier v2 commits. Existing rules may retain that legacy interpretation
or conservatively refuse the image. This inherited ambiguity affects ordinary and grouped
writes and remains in Phase 3.

### Tests

`group.rs` has 74 tests, including failed-fence cases for preconditions, guard errors,
class protections and late RDF Patch preconditions. They also cover annotation recovery,
an idle commit after grouped ones, a stateful guard that must be told of each commit before
the next check, and writes during compaction. Corrupting the first transaction preserves
its separate header, with absolute record positions checked in new and headerless logs.
Deterministic metadata tests cover earlier holes, direct metadata loss, later durability
proofs, invalid CRCs and conservative legacy refusal. Mixed headerless histories retain
their legacy payloads, reconstructed identities and readable history after current writes.

The SHACL tests cover a limit across concurrent writes and a shapes change in flight.
`a_bypassed_commit_discards_validation_of_an_unpublished_write` and its grouped counterpart
check that a quota-refused write followed by a bypassed commit with the same number discards
the unpublished state and makes the next validation full. A paused-fence test checks that
a later bypass preserves earlier pending state. Graph-backed bypass tests cover ordinary
and grouped validation, a refreshed pending schema before publication, quota refusal,
dry-run and rejection invariance, malformed repairs, severity thresholds and grandfather
comparison. Actual undecodable graph rows verify that reader failures propagate without
promoting cache state. The server has five router tests covering
independent receipts, failed fences, each HTTP write kind and published identities for
plain patch aborts. Core and HTTP Patch tests also preserve ordinary dry-run previews
with matching and mismatching leading head preconditions.

The six power-loss tests cover acknowledged data and messages, including failure of the
annotation file's sync inside a fence and during the first message of a database.
`a_failed_first_annotation_sync_keeps_later_messages` checks that a failed first record
preserves the new header for a later acknowledged message. The single-writer and concurrent
annotation-fence cases explicitly disable idle direct commits. The crash harness selects
pending pages independently for each write as a conservative overapproximation, which
need not be a coherent physical page or sector snapshot. It is not physical crash proof.
The model-based stress test
of section 10.1, the `kill -9` runs of section 10.4, and a prune followed by reopening a
flagged commit at the WAL tail remain unwritten.

### Left for Phase 3

1. The gates of section 9 on `forge`, in particular reader p99 and serial parity at 1.05M
   and 10.5M.
2. The 1,000-point `log-writes` runs on ext4 and XFS, and the `kill -9` runs.
3. Full-text, spatial and embedding datasets and the ShEx guard grouping, or measurements
   that show them rare.
4. The BENCHMARKS.md refresh under X06, with a `--no-group-commit` row.
5. The default, the opt-out flag and the removal of `--experimental-group-commit`.
6. Resolve the inherited ambiguity of torn metadata in headerless WALs while preserving
   mixed-history compatibility and strict legacy corruption checks.
