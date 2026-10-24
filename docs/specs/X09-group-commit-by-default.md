# X09: Group commit by default

> **Status:** Phases 1, 2 and 3 built, with exceptions listed in the Outcome. Group
> commit is on by default for persistent stores. Phase 4 is built and passes its crash
> runs, so it also groups the commits of datasets with a full-text, spatial or embedding
> index, but its performance gates of section 17.9 have not run on `forge`.
>
> **Phases:** Phase 1 is the measurements and fixes that the switch depends on: serial
> latency, reader tails, metrics and power-loss and sync-failure tests. Phase 2 widens
> admission to the writes that fall back today (commit messages, preconditions,
> validation, Graph Store and RDF Patch writes, full-text, spatial and embedding hooks,
> writes during compaction). Phase 3 makes group commit the default for persistent
> stores, with `--no-group-commit` to opt out. Phase 4 admits datasets with a full-text,
> spatial or embedding index. Phases 1 to 3 are done, and Phase 4 is built with its
> gates pending. Phase 3 meets the conditions
> of section 7.2, the third as amended on 2026-10-17.
>
> **User docs:** [FEATURES.md](../FEATURES.md), [USAGE.md](../USAGE.md) for
> `--no-group-commit`, and [API.md](../API.md#metrics) for the metrics, the `groupCommit`
> and `commits` fields of the JSON metrics and the `groupCommit` field of `/$/stats/{ds}`.
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
(a hidden global flag at the time, `crates/sparkles-server/src/main.rs:169`), lets concurrent
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

The Today column describes the state before the switch. There is no release yet, so
`--experimental-group-commit` is removed rather than kept as
an alias. The docs that name it (FEATURES.md and the concurrent sections of
BENCHMARKS.md) change in the same commit. The server test
`durable_grouping_requires_explicit_cli_opt_in` in `main.rs` is replaced by one that
checks the default is on and that `--no-group-commit` turns it off.

### 7.2 Conditions for the switch

1. §9's gates are met on `forge` at 1.05M and 10.5M.
2. Every test of §10 passes on ext4 and XFS, and the power-loss test of §10.2 passes
   for 1,000 crash points per file system.
3. The fallbacks of §6 that ordinary deployments hit (commit messages, preconditions,
   guards, compaction) are admitted, or the metrics of §8 show they are rare in the
   measured workloads. Amended on 2026-10-17. Datasets with a full-text, spatial or
   embedding index keep the ordinary path under the default, because such a commit takes
   exactly the path it takes today and only misses the gain. Section 17 designs their
   admission as Phase 4.
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
| Four writers and two readers, 10.5M, both settings at the same write rate | Reader p99 within 10% of the default writer |
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
| 4 | §17: grouping on datasets with a full-text, spatial or embedding index | M–L | §17.9 |

## 13. Acceptance examples

- **A1 — Serial parity.** 5,000 serial single-triple commits at 10.5M with the default
  on have a median and p90 within 5% of `--no-group-commit`.
- **A2 — Concurrent gain kept.** Four writers at 1.05M acknowledge at least 529
  transactions/s with contiguous receipts.
- **A3 — Reader tail.** With four writers and two readers at 10.5M, both runs paced to the
  same write rate, reader p99 is within 10% of the run with grouping off. Pacing keeps the
  comparison about the cost of grouping, because an unpaced run with grouping on commits
  more writes and its readers compete with them.
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

## 17. Phase 4: group commit for indexed datasets

Phase 3 leaves datasets with a full-text, spatial or embedding index on the ordinary path.
A commit on such a dataset takes exactly the path it takes with grouping off, so the
default cannot slow it down, but it gains nothing either. This section designs their
admission. It was written from the code at the end of Phase 3, and every function it names
is at that revision. The [Phase 4 Outcome](#phase-4) records how it was built, where the
build differs from this section and how the open questions of section 17.10 were decided.

### 17.1 Why these datasets fall back

`Store::group_check` in `store.rs` decides admission for both `speculative_base`, when a
transaction starts, and `group_admitted`, when it commits. It returns `Fallback::Text` when
`text_enabled()` holds, `Fallback::Geo` when `self.geo` is set and `Fallback::Embed` when an
embedding index has work registered. No other index forces a fallback. HNSW vector indexes
catch up in the background from published snapshots, as `store/vector.rs` builds them, and
the ShEx guard takes the group path with one commit per fence.

Three properties of the ordinary path stand in the way.

1. **Maintenance reads the published head.** `WriteTxn::publish_log` calls
   `maintain_text`, `maintain_geo` and `embed_noted` after the commit's synchronization and
   before `current.store`. `maintain_geo` passes `self.current.load_full()` to
   `index_commit` as the state the commit started from, which is true only when nothing is
   in flight. `TextIndex::apply_commit` in `text.rs` changes the live index writer that
   every view shares.
2. **One installer does not drain.** The coordinator publishes without the writer lock.
   Text recovery in `store/text_recovery.rs`, `enable_text` and the spatial
   reconfiguration drain the group through `guarded_writer` before they install a view, so
   no grouped commit can publish over it. The spatial build thread that `spawn_geo_build`
   in `store/geo.rs` starts takes `writer.lock()` without draining. With grouped commits on
   a spatial dataset, a fence could then publish a snapshot carrying an older view over the
   one the build installed.
3. **A speculative snapshot has no index view.** A transaction that starts from the
   speculative head would see the index state of the published head, or none. Phase 2's
   review found that admitting these datasets at publication alone would make every
   transaction drain first, so each fence would hold one commit.

### 17.2 Where index state goes

There are three ways to place index state relative to the fence.

- **Apply after the fence.** `publish_group_prefix` runs each index's maintenance for each
  commit of the prefix, in commit order, after the synchronization. The live index then
  only ever holds durable commits. The cost is that the index work of a whole prefix runs
  in the leader after the synchronization, which delays every waiter. A transaction that
  starts from a speculative base also needs the index state of its base, which this does
  not give it.
- **Carry speculative views.** `publish_grouped` runs maintenance under the writer lock,
  against the transaction's base, as `publish_log` does, and puts the resulting view in the
  staged snapshot. Readers of the published head never see it. A transaction that starts
  from the speculative head sees it, as section 3 allows. This needs the index's view to be
  a value that can be dropped when a fence fails, and it needs the index's own files never
  to hold a commit above the durable head.
- **Stage per group.** Speculative changes go to a small overlay that the speculative
  snapshot carries. The leader applies the overlay to the live index at publication. Queries
  on a speculative snapshot merge the published view with the overlay.

The recommendation differs per index, because their state differs.

| Index | State today | Recommended placement |
|---|---|---|
| Spatial, G01 | `GeoView` values in memory, cloned per commit by `maintain_geo`. The base is rebuilt in the background at open. | Carry speculative views. The view is already an immutable value, so a failed fence drops it with the snapshot. |
| Full-text, F03 | One live index writer shared by all views, with disk commits by `commit_locked` and checkpoints that name the last applied commit. | Stage per group. The live writer only takes durable commits, so its disk state never runs ahead of the log. |
| Embeddings, F08 | A work queue of pairs keyed by commit number. The embedder writes vectors back in its own `CommitKind::Embed` commits. | Apply after the fence. `embed_noted` only schedules background work, so it costs little in the leader, and it never needs speculative state. |

For full-text, carrying speculative views was considered and rejected. `seal_for` serves
read-your-writes by sealing a batch, and `settle_locked` commits it to disk when it has
staged documents. A search on a speculative view would therefore write documents of a
commit that a failed fence or a crash could still lose. After such a crash the index on
disk is ahead of the log, and the only way back is a full rebuild. Staging keeps the
existing invariant that `apply_commit` sees only durable commits.

### 17.3 Changes by file

The build differs from this list in the overlay search, the spatial installer and the
order of embedding work. The [Phase 4 Outcome](#phase-4) describes what was built.

- `store.rs`, `group_check`. Remove `Text`, `Geo` and `Embed` as unconditional reasons.
  Keep a fallback while text recovery has a pending job, while the
  spatial view is building or failed, and while a text rebuild runs. Name these
  `text-recovering`, `geo-building` and `text-rebuilding` in `Fallback` and in the metrics of
  section 8.
- `store/group.rs`, `publish_grouped`. After the commit's records are written and before it
  is enqueued, compute the speculative spatial view with `maintain_geo` and the text overlay
  with a new `TextIndex::stage`, both against `self.base`, and store them in the staged
  snapshot. Record the pairs for embeddings in the `Pending` entry.
- `store/group.rs`, `publish_group_prefix`. For each commit in order, call
  `TextIndex::apply_staged` with that commit's overlay, then `embed_noted` with
  `p.snapshot`, then publish. The spatial view needs no work there.
- `store/geo.rs`, `maintain_geo` and `index_commit`. Take the transaction's base as the
  state before the commit, instead of `self.current`. In the build thread of
  `spawn_geo_build`, replace `writer.lock()` with `guarded_writer()`, so that a build
  installs its view only when no grouped commit is in flight. The thread holds weak
  handles to the writer state and to `current`, not the `Store`, and `guarded_writer` and
  `drain_group` are `Store` methods that need the coordinator. The thread therefore gets a
  weak handle to the coordinator, or to the store, as well.
- `text.rs`. Add `Overlay`, the documents a commit adds and the keys it removes, with
  `stage(snap, log) -> Overlay` and `apply_staged(&Overlay)`. `apply_commit` becomes
  `stage` followed by `apply_staged`, so the ordinary path keeps one code path. Text
  queries on a snapshot with overlays search the view and then the overlays in commit
  order, where a later overlay removes or replaces the documents of earlier ones.
- `store/text_recovery.rs`, `Recovery::record`. A recovery job records commits at
  publication, from the overlay, not at speculation.
- `store/compaction.rs`. The checkpoint before a switch already runs under
  `guarded_writer`, which drains, so it sees only durable commits. Add a debug assertion
  that the text index's applied commit is at or below the published head there and in
  `checkpoint_locked`.

### 17.4 What a fence makes durable, and in what order

A fence's synchronization does not change. It syncs the WAL, `delta.vocab` and
`annotations.bin`, as section 6 and the Phase 2 Outcome describe. No index file joins the
fence, because every index is derived from the log. The order of each commit's work is the
following.

1. Under the writer lock, the transaction writes its WAL records and terms, computes its
   spatial view and text overlay from its base, and enqueues. Nothing of an index reaches
   disk.
2. The leader syncs the prefix.
3. For each commit of the prefix, in order, the leader applies its text overlay to the live
   writer, schedules its embedding pairs and publishes its snapshot.
4. Later, the text checkpoint makes the live writer durable with a payload naming the last
   applied commit, which step 3 bounds by the published head.

The invariant is that no index's durable state names a commit above the durable head of the
log. The text checkpoint is the only durable index state that names a commit, and step 3
applies only fenced commits to it.

Step 3 runs inside the coordinator's `drive`, which today holds the state mutex while it
publishes, and `enqueue` takes the same mutex. A text `apply_staged` or an `embed_noted`
there would block every writer's `enqueue`, and those writers hold the writer lock, so all
new transactions would wait for the index work. Step 3 therefore publishes outside the
state mutex. The leader takes the prefix out of the queue under the mutex, releases it,
applies and publishes in order, and takes the mutex again only to update the queue's
counts. Only the leader publishes, so the order of commits is unchanged. Today `drive`
holds the mutex across `publish_group_prefix`, and `fail_locked` runs under the same mutex,
so the change needs a
second critical section that updates `commits`, `bytes` and the count of commits in flight
after the prefix is published. A failure in step 3 comes after the first release, and the
leader must then take the mutex again to fail the queue, so that no commit enqueued in
between is acknowledged. Question 5 of section 17.10 asks how. The gates of section 17.9
measure the cost.

### 17.5 Recovery after a crash at each step

| Crash point | State on disk | What open does |
|---|---|---|
| During step 1 | Part of the commit's records, with no index state | The replay truncates the torn tail. No index has seen the commit. |
| After step 2, before step 3 | The prefix is durable and the text checkpoint is older | `open_text` catches up from the WAL after the checkpoint's commit, as today. The spatial view is rebuilt in the background, and the embedder's queue starts again from its pass. |
| During step 3 | Some overlays applied in memory, none checkpointed | The same as above, since the live writer's memory is lost. |
| During step 4 | A checkpoint at or below the published head, or the previous one | The catch-up of `open_text` covers the difference. |
| A fence fails | The writer is poisoned and the overlays are dropped with the queue | No overlay was applied, so the live writer holds only durable commits and needs no recovery. |
| The leader panics, or an apply fails, during step 3 | The prefix is durable. The live text index holds some of its overlays, and the writer is poisoned, so the prefix's later commits report `Poisoned` although they are durable | Nothing is marked. The live index holds only durable commits, and the catch-up of `open_text` at the next open brings the checkpoint to the head as after a crash between steps 2 and 3. The index is at or below the durable head, so the invariant holds. |
| The text index's own tick commits the live writer during step 3 | A checkpoint naming a commit of the prefix, which is durable | Nothing beyond the usual catch-up, because the checkpoint is at or below the durable head. |

A power loss inside a fence's unfenced suffix can lose commits that a speculative
transaction saw, but no index outlives them, because overlays and speculative spatial
views live only in memory and are dropped.

### 17.6 Background maintenance

The spatial build was built with a wait on the coordinator rather than
`guarded_writer`, as the [Phase 4 Outcome](#phase-4) describes.

- **Spatial build.** The build thread installs its view under `guarded_writer` after this
  change, so it never meets a grouped commit in flight. Commits made during the build are
  admitted only once the view is ready. Until then they fall back with `geo-building`, and
  the build brings its view up to them as it does for ordinary commits today.
- **Text recovery and rebuild.** A dataset whose text index recovers or rebuilds falls back
  until the job finishes. The job then installs under `guarded_writer`.
- **HNSW catch-up.** Vector indexes read published snapshots only, so grouping does not
  change them. `vectors_switched` runs under the writer lock with the group drained, at a
  compaction or bulk commit.
- **Compaction carry-over.** The compaction tap records grouped commits as they enqueue, as
  Phase 2 built it. The text checkpoint and the HNSW carry-over before the switch run
  under the drained writer lock.
- **Embedding writes.** `CommitKind::Embed` commits keep the ordinary path, because they
  come from one background worker and gain nothing from grouping.

### 17.7 Memory

A speculative snapshot holds a spatial view and a text overlay for each commit in flight.
The queue limits of section 6 bound them, 64 commits or 4 MiB of WAL records per dataset.
The bound is on records, so on documents and not on bytes. A staged document holds its
whole literal, and a queue of long literals can hold far more than 4 MiB of them.
A spatial view is a clone of the previous one, which shares its base and copies the rows
of the commits since the last build. A text overlay holds the documents of the commit's
touched quads. Both are freed when their commit publishes or its fence fails. The extra
memory is at most one fence's worth of index changes per dataset, which buys the
throughput of grouping for indexed datasets. The Outcome of this phase will state the
measured amount.

### 17.8 Tests

- **Model-based stress test.** `store/group_model.rs` gains runs on datasets with each
  index. The model checks text queries against a scan of the model's literals, spatial
  queries against a brute-force distance over the model's points, and that embedding work
  is never scheduled for a commit above the published head. Faults include a failure of
  `index_commit`, which `Store::fail_next_geo_commit` already injects through the
  `GeoIndex::fail_next` field, a failed text commit through `Store::fail_next_text_commit`,
  and a failed text apply.
- **Admission tests.** One test per new fallback reason, and one per installer showing that
  a build, recovery or rebuild that finishes during a fence installs only after the fence.
- **Power loss.** `tests/power_loss.rs` runs its crash images on a dataset with full-text
  search, and each image checks that text results match the recovered data. The shim in
  `tests/support/iofault.c` records the text index's writes already, but it must learn
  `rename` and directory synchronization first, because the index replaces its metadata
  file atomically.
- **`log-writes` and `kill -9`.** `scripts/power-loss.sh` gains a `--index text|geo|embed`
  option that configures the index before the load. After each crash, `crash-harness
  verify` runs a text or spatial query for a sample of receipts and checks their presence.
  Each kind runs the 1,000-point replays on ext4 and XFS and 200 kills on each.

### 17.9 Performance gates

Measured on `forge` under the protocol of [X06](X06-benchmark-coverage.md), with each
index configured.

| Measurement | Gate |
|---|---|
| Serial commits, 1.05M and 10.5M | Median and p90 of the grouped path no more than 5% slower than the ordinary path on the same dataset, judged on the median ratio between neighbouring processes |
| Four writers, 50,000 changes | Median ratio between neighbouring processes of at least 1 + 0.5 × min(1, m / 1.2 ms) in transactions/s, where m is the median mean commit synchronization time of the ordinary processes |
| Text query p50 and p99 during a four-writer load | Within 10% of the same load with grouping off |
| Spatial query p50 and p99 during a four-writer load | Within 10% of the same load with grouping off |
| Memory | Peak RSS during the four-writer load within 64 MiB of grouping off |

The serial and throughput gates were amended on 2026-10-23 at the user's decision, after
the first forge campaign and with its data in view. As first written the serial gate asked
for a median and a p90 within 5% on either side of the ordinary path, and the throughput
gate asked for at least 1.5 times the ordinary path's transactions per second. The
campaign showed that the mean time of a commit synchronization on `forge` moves between
processes from about 0.23 ms to 1.7 ms and often changes inside a block, and that the
ordinary path ran slower in the last position of a block than in the first. A median over
ten processes of one setting then followed which setting drew more slow processes.

A neighbour of a grouped process is the ordinary process next to it in the same block, that
is position 0 with 1 and position 3 with 2 of the order ordinary, grouped, grouped,
ordinary. The drive's state changes slowly, so a neighbour pair mostly shares it. The serial
gate is one-sided because a grouped path that is faster than the ordinary one is not a
regression, and five of the first eight misses were of that kind.

The throughput gate is stated against the synchronization time because grouping saves
synchronization time and nothing else. A gain of 1.5 can be asked for only when the
synchronization is a large part of an ordinary commit, which it is at 1.2 ms and above,
the low edge of the slow state that the campaign measured. Below that the requirement
falls in proportion to the synchronization time, to 1.0 for one that takes no time. The
breakpoint was chosen after the campaign and the linear shape is an assumption. Two
sensitivities bound that choice. A breakpoint of 1.0 ms still passes all six cells. A flat
1.5 judged on neighbour pairs passes five of six, with full-text at 1.05M at 1.496. Judging
each pair against the requirement for the synchronization time of its own ordinary
neighbour gives a positive median margin in all six cells. Two other forms were rejected.
Dividing the throughput by the synchronization time punishes grouping for the longer
synchronization of a fence, which is 1.4 to 1.5 ms against about 1.0 ms for one ordinary
commit. A throughput with the synchronization time subtracted leaves about 7% of an
ordinary commit, so noise dominates it.

`scripts/bench-indexed-report.py` judges these two gates. The harness's `gate-summary.json`
keeps the verdicts as first written. The next forge campaign is the first to judge the
amended gates before it has any data, and it confirms them.

### 17.10 Open questions

1. Whether a text overlay can be searched with the same scoring as the view. A document
   in an overlay has no collection statistics of its own, so BM25 scores of overlay hits
   may differ slightly from those after publication. Searching overlays only for matching,
   with scores taken from the view's statistics, is the likely answer.
2. Whether the ShEx guard should gain a pending list in the same phase, so that its fences
   hold more than one commit. It does not fall back today, so it is not required here.
3. Whether a spatial build should admit commits during the build by carrying its journal
   in speculative snapshots, rather than falling back until the view is ready.
4. Whether the leader's text apply in step 3 needs a separate thread when a prefix holds
   many large overlays, which the four-writer gate will show.
5. How the leader updates the queue's counts and fails the queue after it released the
   state mutex in step 3, as section 17.4 describes. Either `fail` takes the mutex itself
   and marks the queue failed before any later fence can lead, or the leader holds a failure
   flag that `enqueue` reads before it admits a commit.

## Outcome

Phases 1 and 2 were built behind `--experimental-group-commit`, with the exceptions below.
Phase 3 meets the conditions of section 7.2, the third as amended on 2026-10-17. It passed
the `kill -9` runs and the 1,000-point `log-writes` runs on ext4 and XFS, resolved the
ambiguity of headerless logs, and made group commit the default. See
[ROADMAP](../ROADMAP.md) item 3. Phase 4 admits datasets with a full-text, spatial or
embedding index, as [ROADMAP](../ROADMAP.md) item 86.

### Phase 1

- **Metrics.** `Store::commit_metrics` counts synchronizations by path, commits per fence,
  the wait from enqueue to acknowledgement, failed fences and fallbacks by their first
  failing condition. `/$/metrics` exports them as `sparkles_commit_sync_seconds`,
  `sparkles_group_commit_size`, `sparkles_group_commit_wait_seconds`,
  `sparkles_group_commit_fallbacks_total` and `sparkles_group_commit_failures_total`.
  `/$/stats` reports `groupCommit` and `commits` per dataset, where `fences` counts the
  fences that synchronized successfully and `failures` the ones that did not. The fallback
  reasons are `poisoned`, `kind`, `text`, `geo`, `embed`, `bulk`, `size` and `noop`.
  Section 8 also lists `message`, `precondition`, `guard`, `compaction`, `digests`, `bypass`
  and `graphs`. Phase 2 admits the commits those names describe, so they are no longer
  reasons and have no series.
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

  The `forge` run on 2026-10-16 used the NVMe device without a delay shim and six rounds.
  With both settings paced to 3,452 transactions/s, reader p99 was 3.980 ms off and
  3.986 ms on, a ratio of 1.002, so A3 is met. Unpaced, grouping on committed 1.74 times
  as many transactions per second as off, and reader p99 rose from 4.639 to 5.039 ms, a
  ratio of 1.086. That rise is the cost of serving more writes, and it stays inside the
  10% bound.
- **Failed synchronization (A4).** A router test fails ten requests of one fence with 503
  and publishes none of them. After reopening, the commit acknowledged before the fence and
  its insert are present and the head has no gap. The kernel can write complete records of
  the failed fence back before the sync fails, and those records may recover. A4 therefore
  says the head is at or after the last acknowledged commit, not equal to it.

  A second test runs under an `LD_PRELOAD` shim (`crates/sparkles-core/tests/power_loss.rs`
  and `tests/support/iofault.c`). It fails the WAL, the vocabulary or both at chosen calls
  and checks the reopened store.
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
Phase 3 added the model-based stress test of section 10.1, the `kill -9` runs of section
10.4, and a prune followed by reopening flagged commits at the WAL tail.

### Left for Phase 3

Phase 2 left six items. The Phase 3 section below records what became of each.

1. The gates of section 9 on `forge`, in particular serial parity at 1.05M
   and 10.5M. Met.
2. The 1,000-point `log-writes` runs on ext4 and XFS, and the `kill -9` runs. Both passed,
   the `log-writes` runs on `forge` on 2026-10-17 with the switched build.
3. Full-text, spatial and embedding datasets and the ShEx guard grouping, or measurements
   that show them rare. Condition 3 was amended instead, and section 17 designs their
   admission as Phase 4.
4. The BENCHMARKS.md refresh under X06, with a `--no-group-commit` row. Refreshed. The
   tables compare group commit with the `--no-group-commit` writer, measured on one binary
   before the switch made grouping the default.
5. The default, the opt-out flag and the removal of `--experimental-group-commit`. Landed.
6. Resolve the inherited ambiguity of torn metadata in headerless WALs while preserving
   mixed-history compatibility and strict legacy corruption checks. Resolved.

### Phase 3

**Headerless logs.** A torn version-2 commit record whose bytes after a sector boundary
read as zero is byte-identical to a legacy record. In a headerless log the old rules could
recover such a record as a reconstructed legacy commit, with data records no checksum
covers. They could also refuse the store after a power loss inside a group's unfenced
suffix. The first write open of a headerless log now records the log's logical end as
`walStrictFrom` in the generation's `commit.json`, once. It does so after replay, the
truncation of any torn tail and a sync, and before anything is appended. Records after the
offset follow the rules of a log with a header record. Damage before it is corruption,
because those bytes were durable when the offset was recorded, and a log that ends before
it is refused. Records before it keep the mixed interpretation of
[CI section 5.6](CI-commit-identity.md#56-dataset-file-creation-migration), so existing
mixed histories read as before. `sparkles check` applies the same rule, and it reads the
offset after the log, so a live writer cannot append past an offset it has not seen. The
member is additive under [X12 section 4.3](X12-release-engineering.md#43-how-a-format-changes).
A release that predates it ignores it and reads the log with the old rules. `commit.json`
now refuses a `format` other than 1. A truncation to an empty log is now synced before
the first append. The design came from a consult on the ambiguity, which recommended
setting the offset once rather than advancing it, so that damage below it stays
detectable.

The offset says that the bytes before it are durable together with the vocabulary terms
and annotation records they name. The first write open therefore records it after
`delta.vocab` is settled and `annotations.bin` is synced, not only after the log's own sync.
A review found the earlier order. A power loss between a durable offset and those syncs,
after an older build had left unsynced tails, made the store refuse to open, or brought back
an unacknowledged commit without its message. A test checks the order of the durable steps
of open. A log that the replay cuts to nothing, one that began with a torn header record
included, records an offset of 0. A recorded offset with no `wal.log` is corruption in open
as well as in `sparkles check`, because the member is recorded only for a log that exists.
Before the offset, the `at < from` rule decides one case alone, a flagged commit whose
annotation is missing, which is kept with a warning as a proven commit is, and a test pins
it.

The member adds no reader version. A build that predates it ignores it and reads the log
by the rules it always used, and every build with the reader requirement of the dataset
file writes metadata on every commit, so its appends after the offset are well framed. A
version bump would only refuse builds that already read the log safely. `commit.json`
refuses a `format` other than 1 in open, in `sparkles check` and in the restore path of
`commit::reidentify`, and rewriting it keeps members that a later release added. Two cases remain. A headerless log that this release has never opened for
writing keeps the mixed rules for the bytes it holds, so a torn record that an older
build left there is still ambiguous. Legacy records that a downgraded build appends after
the offset are treated as damage, and downgrading stays unsupported.

**Indexed and ShEx-guarded datasets.** Datasets with a full-text, spatial or embedding
index stay on the ordinary path. These indexes are opt-in per dataset. A commit on such a
dataset takes exactly the path that the default writer takes, so grouping cannot slow it
down, and it only misses the gain. Admitting it needs index state in speculative
snapshots, because without it every fence would hold one commit. The user amended
condition 3 of section 7.2 on 2026-10-17 to accept this, and section 17 designs the
admission as Phase 4, [roadmap](../ROADMAP.md) item 86, which was built on 2026-10-18.
In every measured workload below no commit fell back from the group for any reason, and `sparkles_group_commit_fallbacks_total`
had no series. None of those workloads had an index, so they say nothing about indexed
datasets. ShEx-guarded datasets take the group path with one commit per fence, as Phase 2
left them.

**Section 9 gates.** These runs used `forge` on 2026-10-17 with one binary at revision
758a6a25, comparing the default writer with `--experimental-group-commit`, and nothing
else running on the host. [BENCHMARKS.md](../BENCHMARKS.md#concurrent-durable-commits)
has the tables.

| Gate | 1.05M | 10.5M | Target |
|---|---|---|---|
| Serial literal, IRI and delete, p50 and p90 against the default writer | 0.996 to 1.012 | 0.981 to 1.009 | Within 5% |
| Four writers, transactions/s | 6,174 | 6,160 | At least 529 and 402 |
| Sixteen writers, largest share deviation | 1.0% | 1.2% | Within 25% |
| bench.sh mixed reads/s and writes/s against the default writer | 1.015 and 1.023 | 1.034 and 1.018 | Reads within 5%, writes at least 1 |
| bench.sh updates churn, commits/s against the default writer | 1.002 | 1.018 | Within 5% |

The ratios are computed from the rounded medians in BENCHMARKS.md. The reader tail at an
equal write rate was met before Phase 3.

**Crash runs.** `scripts/power-loss.sh` and `scripts/crash-harness.rs` implement
sections 10.2 and 10.4. The `kill9` mode starts sixteen writers that log every receipt,
kills the server with SIGKILL at a random moment between 0.2 and 2.2 seconds, restarts it
and checks every receipt logged in the store so far. It starts a new store every 20 kills.
With grouping on, 200 kills on a new XFS file system and 200 on a new ext4 file system
of an 8-vCPU Namespace instance had no failure, over 2,812,041 and 2,715,882 receipts. A
further 200 kills on the laptop's ext4 had none over 1,625,905 receipts. Every check found
the head at or after the last receipt, every receipt's triple and message, and
`sparkles check` passing before and after the restart.
A load that took no receipt, or had a failed write, now fails the run. Before this a server
that refused every write passed with nothing checked, which happened once in the
`logwrites` mode when the server's 1 GiB free-space reserve exceeded its 512 MiB file
system. The `selftest` mode checks this without root.
The `logwrites` mode puts the store's file system behind the device-mapper `log-writes`
target, marks the log after every batch of receipts, and replays crash images that hold
the writes before the last flush, every FUA write and a random part of the rest per 4 KiB
block. It checks each image as the restart above does, with the next commit continuing the
sequence. A run whose marks fail, or that makes none, fails too, since its images would be
checked against no receipts.

The `log-writes` runs ran on `forge` on 2026-10-17 with a build of the switch branch at or
after merge 62f2ada8, which brought in e9f4acb5 and 42c160a9. A run cannot pass without
both, and the runs did not record the exact revision. Each run replayed 1,000 crash
images of a load of sixteen writers for 20 s with a mark every 50 ms, and each load
crossed one automatic compaction. The `--no-group-commit` loads commit far fewer
transactions through `log-writes`, so they ran with `--auto-compact-min-quads 1000` to cross
one as well. Every image passed.

| Run | Receipts | Marks | Log entries | Images | Failed |
|---|---:|---:|---:|---:|---:|
| ext4, group commit | 47,383 | 369 | 66,463 | 1,000 | 0 |
| XFS, group commit | 29,135 | 358 | 24,868 | 1,000 | 0 |
| ext4, `--no-group-commit` | 9,394 | 367 | 36,824 | 1,000 | 0 |
| XFS, `--no-group-commit` | 4,859 | 361 | 14,248 | 1,000 | 0 |

Two earlier runs found two disagreements between `sparkles check` and open, and no lost
commit. An image of a power loss during a compaction can keep `CURRENT` on the old
generation and hold the new one's directory, which check reports as a leftover warning
until the next rebuild replaces it. With `kill -9` the switch of `CURRENT` survives in the
page cache, so only `log-writes` showed it. It appeared with and without group commit, 7
images in 1,000 with grouping and 26 in 1,000 without, and the harness now accepts a
leftover warning after open, while any other warning or error still fails. One image held a
log record torn at a 4 KiB block boundary after the last commit, with zeros where its ids
were. Open truncated it with the incomplete transaction, but check called the ids errors.
Check now reports them as warnings. Since 2026-10-18 open removes a leftover generation
that is an unfinished build and sets any other aside, as the C13 follow-up describes, and
after open the harness accepts only an old generation or one set aside.

**Tests.** `store/group_model.rs` is the model-based stress test of section 10.1. Four to
eight threads issue random inserts, deletes, multi-quad transactions, no-ops, dry runs and
writes whose precondition holds or fails, with and without messages. A run may arm a panic
at one of the group's four failpoints or a failed synchronization of the log or of the
vocabulary. It checks unique commit numbers, a poisoned writer after a failed
synchronization, that every snapshot a reader saw is the model's state at its commit, and
that the reopened store holds every acknowledged commit and message in an order the model
allows. After a failed synchronization it also checks that no reader saw a commit past the
last acknowledged one and that no commit after an uncertain one was acknowledged. Every
recovered commit must carry its message. It runs 24 seeds by default in under two seconds,
and twelve invocations of 300 seeds each passed. Publishing the commits of a failed fence
before failing them makes it fail in every invocation, and so does removing the poisoning of
a failed fence. `tests/annotations.rs` prunes the catalog while messaged commits sit at the
WAL tail and reopens with and without grouping.

**Annotations after a failed synchronization.** The ordinary path used to take a
commit's annotation and merge record back when the synchronization of its log records
failed. The records could still reach the disk, so the commit could come back after a
restart without its message or change digest, and with digests on the next commit then
hashed against a zero parent. A failed synchronization poisons the writer, and open drops
the records of commits above the recovered head, so the path now keeps both records. A
recovered commit has its message, digest and merge parent, and a commit that did not
survive leaves nothing behind. The model test above now requires every recovered commit to
carry its message, and `a_commit_kept_after_a_failed_sync_keeps_its_message_and_digest`
checks the digest chain across such a commit. A bulk commit whose `CURRENT` switch reports
an error after the rename took effect now poisons the writer and keeps its annotation, as a
compaction already did. Nothing on disk changed.

**The default.** Group commit is on by default. `StoreOptions::group_commit` replaces
`experimental_group_commit` with a default of true and is ignored for in-memory stores.
The CLI and server take the global `--no-group-commit`, and `SPARKLES_GROUP_COMMIT=off`,
`false`, `no` or `0` does the same. Any non-empty value besides those and `on`, `true`,
`yes` or `1` is ignored with a warning, so a typo is not silent.
`--experimental-group-commit` is removed, as section 7.1 says. The Node binding takes a
`groupCommit` open option and the Python binding a `group_commit` keyword on `Dataset`,
`Dataset.open` and `Catalog`. The JVM binding exposes no store options, so it takes the
default and a JVM user cannot turn grouping off. `/$/stats/{ds}` and the JSON metrics
report `groupCommit` per dataset. A persistent dataset with grouping on holds up to 64
queued commits or 4 MiB of log records, plus the speculative snapshots of the commits in
flight, which is the memory that buys the throughput in BENCHMARKS.md. The size of those
snapshots was not measured. The 1,000-point `log-writes` runs on ext4 and XFS passed with
it on 2026-10-17.

### Phase 4

Phase 4 was built on 2026-10-18 as [roadmap](../ROADMAP.md) item 86. Datasets with a
full-text, spatial or embedding index now group their commits. The placement of section
17.2 holds for each index, and the differences from sections 17.3 to 17.6 are listed below.
Nothing on disk changed, so no format version moved. Every index stays derived from the
log, and no index file joins the fence. The work this phase leaves open is listed at the
end.

**Admission.** `group_check` no longer refuses a dataset for having an index. A commit falls
back with `text-recovering` while a full-text recovery job is pending, with
`text-rebuilding` while an online rebuild of the full-text index runs, and with
`geo-unavailable` while the spatial view builds or after its build failed. The reasons
`text`, `geo` and `embed` are gone from `sparkles_group_commit_fallbacks_total`.

**Spatial views.** `maintain_geo` takes the transaction's base as the state before the
commit, and `publish_grouped` puts the resulting view in the speculative snapshot, as
section 17.2 recommends. The build thread of `spawn_geo_build` does not call
`guarded_writer`, because that needs the `Store` and the thread holds only weak handles.
It holds a weak handle to the coordinator instead. It takes the writer lock, which keeps
new commits out, and waits for the coordinator to settle, while the threads of the queued
commits lead their own fences. It installs its view only under the writer lock with the
queue empty, so no grouped commit can publish an older view over it. The wait gives up
only when 2 s pass with no commit leaving the group, which a commit whose thread panicked
after queueing it causes, and then the thread releases the lock so that a writer can
lead that commit, and tries again. A wait with a fixed limit would let new commits in
each time it gave up, and with fences longer than the limit the view would never install
while writers kept committing. A test installs a build under sixteen writers whose fences
take 80 ms each. While the installer holds the writer lock, new transactions wait for it.
That stall lasts about two fences, the one running and the one that publishes the queued
commits, and 2 s when a commit was left queued by a thread that panicked. Commits made
during a build, or after a build failed, fall back with `geo-unavailable`, which answers
question 3 of section 17.10 for this phase. Section 17.3 named this reason
`geo-building`, which misnamed the failed case.

**Full-text changes.** Section 17.3 described an overlay that queries on a speculative
snapshot would search after the published view. The built design keeps the staging and
drops the overlay search. `TextIndex::stage` computes the commit's documents under the
writer lock from the speculative snapshot, and `Staged::apply` applies them to the live
writer when the leader publishes the commit, in commit order and after the fence. The
speculative snapshot carries a pending view. A search on it waits until that commit is
published and then searches the published view of exactly that commit, so its results and
BM25 scores are those of the commit after publication. This answers question 1 of section
17.10 without a second scoring rule. A search on a speculative snapshot can therefore wait
for one fence, which only a transaction that started from the speculative head can meet.
If the fence fails, the search reports that the writer is poisoned. The wait is bounded at
30 s and then reports a timeout, since a commit whose thread panicked after queueing it
waits for the next writer, and that writer may be the searching transaction. If the
commit's documents fail to apply, the index is marked stale, the commit stays
acknowledged, and the search reports the index as unavailable, as on the ordinary path.
Such a commit keeps its base's view resolved to the published view behind it, never the
base's pending view, which holds the base's outcome. Otherwise on a stale index each
commit's view would hold the previous one, and dropping the chain after tens of thousands
of commits would overflow the stack. A test drops 20,000 such views on a small stack. A
staged change computed before a change of the
index's configuration is computed again at publication. A panic while a commit's changes
are staged poisons the writer, because the commit's record is already in the log and no
fence would cover it.

**Embedding work.** `publish_group_prefix` calls `embed_noted` for each commit after its
snapshot is published, not before as section 17.3 orders it. The worker then reads the
commit from published state, and it never sees a commit above the published head. The
work queue is in memory and restarts from the embedder's pass at open, so the order adds no
durable state.

**Publication outside the state mutex.** `drive` takes the prefix out of the queue under
the coordinator's mutex, publishes each commit without the mutex, and takes the mutex again
to mark each commit's ticket and, once the prefix is done, to update the queue's counts.
Text staging and embedding scheduling therefore never block a writer's `enqueue`. Question
5 of section 17.10 is answered by the first of its two options. A leader that panics or
fails after it released the mutex fails the queue from its drop guard, which takes the
mutex and marks the coordinator failed before any later fence can lead. Commits that it
had not yet published report `Poisoned`, although they are durable, as section 17.5 says.

**Text recovery.** `Recovery::record` was left unchanged. A pending recovery job keeps
every commit out of the group with `text-recovering`, so the job never sees a grouped
commit and needs no record at publication. A debug assertion before each compaction's
text checkpoint checks that the index holds no commit above the published head, unless
the group failed, since a leader that fails between the apply and the publication leaves
one durable commit applied but unpublished.

**Tests.** `store/group_index_tests.rs` has one test for each new fallback reason, tests
that a staged text change reaches the live index only after its fence and that a search on
a speculative head waits for publication, and tests that a spatial build installs only
after the fence and that embedding work is scheduled only at publication. A failed text
apply at publication keeps the commit and marks the index stale. Sixteen writers commit
concurrently to a dataset with all three indexes, and every index agrees with the data
afterwards. A crash test runs a child process per index kind that stops at each of the
group's four failpoints, checkpoints the text index there and checks that its payload never
names a commit above the durable head, then reopens and compares each index with the
recovered data.

`store/group_model.rs` runs the model-based stress test on a dataset with all three
indexes as well, with a failed spatial update and a failed text update among the faults.
It checks text queries against the model's literals, spatial queries against the model's
points, and that embedding work is never scheduled above the published head. It runs 12
seeds by default, and 240 further seeds passed. Applying the text changes at speculation,
skipping them at publication and removing the spatial build's wait for the coordinator
each make these tests fail.

The I/O shim of `tests/power_loss.rs` now records `rename`, `unlink` and file creation and
replays them into each crash image, with a directory sync making them durable. The
full-text index's atomic metadata writes now rename through `std::fs::rename`, so the shim
sees them. `crash_images_of_a_dataset_with_full_text_search` checks each image's text
checkpoint against the recovered head and text search results against a scan, with
grouping on and off. Its writer checkpoints the index at every fence before the
synchronization and from a side thread, and the test requires an image whose checkpoint
names a commit of the run and a recorded rename of `meta.json`. Applying text changes
before their fence makes it fail. The checkpoint at each fence needs the `failpoints`
feature, which a workspace build turns on, and without it the test is ignored. The module
comment
lists the limits of the shim's model of directory operations. `scripts/power-loss.sh` and
`scripts/crash-harness.rs` take `--index text|geo|embed`, and after each crash
`crash-harness verify` checks a sample of receipts through a text or spatial query, or the
embedding index's applied commit. Before anything opens the store, which would rebuild an
index found ahead of the log, `verify --index text` reads the commit that the checkpoint
names and fails when it is above the head or when `sparkles check` reports the index ahead
of the data. A `meta.json` that is missing, torn or not valid JSON names no commit. Open
rebuilds such an index, as `sparkles check` reports, and the search check then compares the
rebuilt index with the data.

**Crash runs.** On 2026-10-18 the `kill9` mode of `scripts/power-loss.sh` ran 200 kills
with each index on a new ext4 and a new XFS file system of an 8-vCPU Namespace instance,
with a build of cf833158 and the text check that bounds the checkpoint by the head before
the first open. Every check compared the index with the recovered data as well as every
receipt. No kill failed. An earlier run, with a build of 80f19430 and a text check that ran
after open, had no failure either.

| Index | ext4 receipts | XFS receipts | Failed |
|---|---:|---:|---:|
| Full-text | 1,698,295 | 1,702,653 | 0 |
| Spatial | 2,057,776 | 2,177,125 | 0 |
| Embedding | 2,118,683 | 2,371,029 | 0 |

The `logwrites` mode ran on `forge` on 2026-10-18 with a build of 80f19430, under the
host's lock for heavy work. Each run replayed 1,000 crash images of a 20 s load of sixteen
writers with a mark every 50 ms, and each image's check also compared the index with the
recovered data. With the spatial index each request also inserted a point. Every image
passed. These runs predate cf833158 and the text check that reads the checkpoint before
the first open, so they would not have noticed an index ahead of the log that open
rebuilt. A rerun with cf833158 or later is queued on `forge` with the gates.

| Run | Receipts | Marks | Log entries | Images | Failed |
|---|---:|---:|---:|---:|---:|
| ext4, full-text | 41,277 | 367 | 58,984 | 1,000 | 0 |
| XFS, full-text | 47,652 | 366 | 42,955 | 1,000 | 0 |
| ext4, spatial | 26,874 | 361 | 28,724 | 1,000 | 0 |
| XFS, spatial | 27,968 | 359 | 23,212 | 1,000 | 0 |
| ext4, embedding | 36,649 | 364 | 46,025 | 1,000 | 0 |
| XFS, embedding | 26,315 | 357 | 22,619 | 1,000 | 0 |

**Measurements.** [BENCHMARKS.md](../BENCHMARKS.md#group-commit-on-indexed-datasets) has
Namespace screens of every index against origin/main and against `--no-group-commit`.
With 4 ms added to every synchronization on the 10.5M-triple dataset, four writers ran
2.0 to 2.2 times and sixteen writers 7.7 to 8.5 times the transactions per second of
origin/main, where every commit fell back, and none fell back after the change. Serial p50
and p90 stayed within 3.2% of grouping off. On the instances' own synchronization of about
0.1 ms, four writers with the full-text index ran at 1.005 times grouping off, below the
gate of 1.5, and sixteen writers at 1.6 times. The gates of section 17.9 are defined on
`forge`, which item 1's benchmarks held while this phase was built, so they have not run,
and Namespace effects under about 5% do not settle a gate. The text and spatial query
latency during a four-writer load was not measured, because `write-bench` reads with a
count query. The spatial screens had no geometries, and the embedding screens ran with
`--no-embedding`, so neither measured the cost of the index's own work.

**Memory.** With four writers, the servers' peak resident memory with grouping on was
within 5 MiB of grouping off for every index. With sixteen writers it was 15 MiB higher
with the full-text index, 18 MiB higher with the embedding index and 21 MiB higher with the
spatial index. That is the memory the speculative views, staged documents and queued
commits cost in exchange for the throughput above. The four-writer figure is the one the
64 MiB gate of section 17.9 names, and it was measured on Namespace, not `forge`. The
screens used short literals, and the staged documents are bounded in number, not in bytes,
as section 17.7 now says.

**Read latency on Intel.** The Namespace screen of the query suite measured `two-hop-count`
5.4% to 6.8% slower and `star-lookup` 10.2% slower on the server side for the change. The
rerun on atlas, an Intel Core i5-13500, on 2026-10-18 compared `23075691` with the tip of
origin/main after item 86, `cb94caf6`, whose tree equals `1f377923`. It used the 27 queries
of `scripts/bench.sh` on the 1.05M-triple dataset, with `scripts/nsc-bench-remote.sh` run
on the host as `scripts/nsc-bench.sh` runs it on an instance. Each server process was pinned
to one performance core with the client on another, with six rounds of A, B, B, A and 20
timed requests per query and process. Both binaries returned the same answers to every
query. `two-hop-count` took 2.64 ms on `23075691` and 2.56 ms after, 2.8% faster, and a
second run of ten rounds with 100 requests per process measured 2.60 and 2.54 ms, 2.5%
faster, in nine of ten rounds. `star-lookup` took 0.058 and 0.059 ms, 1.8% more, and
0.057 ms for both builds in the second run, 1.5% more with a spread of 61% between the
processes of one build. `employee-docs` took 0.033 ms for both builds and 0.032 ms for both
in the second run. The geometric mean of the server times over the 27 queries was 1.001. The
slowest of the others on the server side were `lang-filter` at plus 3.3% and `star-join`
and `expr-order-key` at plus 2.5%, and a rerun of ten rounds with 50 requests reproduced
`lang-filter` at plus 3.2% and `star-join` at plus 2.7% in every round, though `expr-order-key`
came out at plus 0.5%. These are 0.04 to 0.05 ms on queries of 1.5 to 1.7 ms. They are under
the 5% that settles an effect, the item changed the commit path and not the query executor, and the client's
wall time for both queries did not change, so they look like a difference of code layout.
The wall time of `optional-chain` was 22.05 ms and 23.01 ms, 4.3% more in every round, with
a server time of 2.02 ms against 2.03 ms. The wall time of `star-join` was the same for both
builds in the rerun except in three of ten rounds, where one server process of the new build
took 1 ms more. Both wall differences are steps of 1 ms that the server time does not show,
and they are open. So the screen's `two-hop-count` and `star-lookup` slowdowns did not
reproduce on Intel, and no query changed by more than the 5% that the host resolves.

The background full-text checkpoint was revised on 2026-10-23. The periodic tick
captures its directory and commit generation under the live writer lock, then
synchronizes files and directories after releasing that lock. A lease keeps the
directory paths stable. Completion updates scheduling only for the same directory
and generation. Concurrent document changes and metadata publication retain the
recovery marker, and a failed parent-directory synchronization remains pending for
retry. Explicit close and rebuild keep their durability ordering. The disk format
and WAL ordering are unchanged.

Index replacement drains the old merge workers. Successful reconfiguration and
disable also wait for old checkpoint leases, including leases held by stale or
already-closed writers, before their paths can be reused. Streaming file I/O keeps
the number of open handles bounded. Replacement may wait for merge workers and a
checkpoint, and atomic metadata publication takes a completion-state lock. These
costs buy stable paths and prevent a retired owner from synchronizing or removing
a replacement's files.

The publication-progress regression failed before the change and passed after it.
Real-directory and Store tests cover concurrent checkpoints, metadata publication,
marker failures, late writes, retained old owners, reconfiguration and reopen
answers. The background crash-image fixture drives the actual periodic tick and
requires a new transaction to start and acknowledge within the same successful
file synchronization. A bounded handshake forces that interleaving. A delayed
receipt from an earlier transaction cannot satisfy it. The explicit and background
syscall histories replay against receipts, annotations, the durable head and
text/RDF equality. Ten histories and 2,496 replayed crash images passed. This
fixture is a durability test, not a native latency measurement. The older kill and
log-writes tables above continue to describe their recorded sources.

The full gate on 2026-10-23 failed one image of this fixture with a panic inside
Tantivy. The image kept the recovery marker, and `meta.json` named a segment whose
`.pos` file had been torn to 6 bytes. Open verified the index, as the marker requires,
and the order of synchronization was correct. Tantivy's footer parser rejects a file
shorter than 4 bytes but subtracts its 8-byte trailer from the length of a file of 4 to
7 bytes, so its checksum check panicked and the open failed instead of rebuilding the
index. The `sparkles check` probe had the same panic. Verification and the probe now
report a committed file shorter than the trailer as damaged before Tantivy reads it,
and treat any panic inside the checksum check as damage. Text open also takes a panic
while it inspects or opens an existing index as damage and rebuilds the index from the
RDF data. The fault predates this revision, since verification has worked this way
since F03, and the background fixture made such images more common. A failing image
needs a committed file torn to that range, and verification visits the files in hash
set order, so the failure was intermittent. On atlas the background fixture panicked
in 4 of 36 finished runs on `6699f27d` and in 4 of 35 on `0fd5f81d`, always in this
verification. With the fix all 104 runs passed, 64 of them on `6d1d810f` and 40 on
`7126fd1e`, which only adds the reset of the raced count described below. A test
tears every segment file of a marked index to each length from 0 to 9 bytes and
requires `sparkles check` to report damage and open to rebuild. It panicked before
the fix.

A background checkpoint that a write raced keeps the marker. Under commits more
frequent than one synchronization, every attempt could race, the marker would never
clear, and every crash would pay verification at open. After three raced background
checkpoints in a row, the tick now holds the live writer lock through the next
synchronization, as an explicit checkpoint does, so no text publication can race it.
That attempt blocks text publication for one synchronization, at most once in four
checkpoint ticks under steady writes, and in exchange the time in which a crash pays
verification stays bounded. Any clean checkpoint resets the count, and so does the
locked attempt itself, because a Tantivy merge writes without the lock and can still
race it. A test publishes inside every unlocked synchronization and requires the
fourth tick to clear the marker, and after a merge's write races the locked attempt it
requires the next three ticks to release the lock again. It fails when the fallback is
disabled or when the count is kept after a raced locked attempt. The forge acceptance of
2026-10-23 below saw write stalls that match the period of the locked attempt, but it did
not trace the ticks, and roadmap item 110 confirms the cause.

**Bounded locked checkpoint.** Roadmap item 110 traced the ticks on `forge` on 2026-10-24
and confirmed the cause. Each background checkpoint now logs at debug level, under the
target `sparkles::text::checkpoint`, how long it held the live writer lock before its file
synchronization, how long the synchronization took, how long it kept text publication out
in all, and whether it cleared the marker. In a mixed full-text run at 1.05M with a build
of e411a772 and this trace, every unlocked checkpoint was raced, because each reader's
search seals the open batch and commits it to the index's files, and every fourth tick
then held the live writer lock for a median of 74 ms and at most 166 ms, about every
5.3 s. Each of these locked ticks synchronized every file of the index again, a median of
76 ms in the unlocked ticks as well. All 16 write stalls of the ordinary process lay inside
a locked tick, with a median of 127 ms and a maximum of 177 ms as the writers saw them, and
none lay elsewhere.

The checkpoint now synchronizes only the files finished or replaced since a pass last
synchronized them, and the locked attempt follows an unlocked pass instead of replacing it.
The directory records the name of each file as Tantivy finishes it and of each metadata
file when its atomic replacement has been renamed into place. A pass takes the recorded
names, synchronizes those files and then the index directory, and puts the names back if
any synchronization fails or the pass panics. A directory opened with its marker present may hold files that
an earlier process wrote and never synchronized, so the first pass after such an open
synchronizes every file in it, as every pass did before. After three raced ticks, the next
tick runs its unlocked pass first, and when a write races that pass too, it repeats the
unlocked pass for the files written during the one before, up to four times, until none
are left. Each repetition has only the files of a shorter interval to synchronize, so it
takes a fraction of the time of the one before, and a repetition that no write races
clears the marker without the tick ever holding the lock for its I/O. Otherwise the tick
then takes the live writer lock and runs a last pass, which finds only the segments and
metadata of the commits that the readers sealed during the last repetition, so it holds the
lock for a few file synchronizations rather than for all of them. The count of raced
ticks starts again after such a tick whatever its outcome, as before.

A pass synchronizes its files from up to eight threads. On `forge` each synchronization of a
newly written file took about 1.5 ms on its own, because each forced its own journal commit,
and the segments that the readers' searches commit are merged away within seconds, so nearly
every file in the directory was new at each tick and synchronizing only the new files did
not shorten the unlocked pass. Synchronizations issued together share journal commits. With
one thread the locked pass still held the lock for a median of 24 ms, and with eight threads
the unlocked pass fell from 77 ms to 28 or 32 ms, the repetitions caught up, and no tick of
the same run needed the lock for its I/O. The first error any thread meets fails the pass,
which puts all of its names back, and a helper thread that cannot be started leaves its
share to the others. Synchronization order among a pass's files does not
matter for the invariant, because the marker goes only after every one of them and the
directory have been synchronized.

[BENCHMARKS.md](../BENCHMARKS.md#bounded-full-text-checkpoint-on-forge) has the traced runs
of each step. Over two processes of each setting at a fixed 500 transactions/s, the shipped
build had no write stall over 25 ms where the trace of e411a772 had 73, its longest write
took 19.5 ms against 171.4 ms, and its maximum schedule lag was 31.0 and 37.9 ms ordinary
and 10.1 and 10.7 ms grouped, within the 50 ms rule, against 159.6 to 165.6 ms. The reader p99
was 3.8% higher on average, within the spread of these runs, and item 170 follows it up.

The crash argument is the invariant of `text/lazydir.rs`, that `meta.json` never refers to
unsynchronized data unless the marker exists, and the change keeps it. The marker is
removed only when no write was counted and no atomic replacement was in flight between a
pass's snapshot of the write count and its check, as before. Every write the snapshot
counts recorded its name in the same critical section that counted it, so the name was
either taken by this pass or by an earlier pass that synchronized it successfully. Tantivy
never changes a finished file, so a file synchronized by an earlier pass is still durable.
A metadata replacement records its name only after its rename, so a pass that synchronized
the old file under that name, while the replacement was in flight, saw the in-flight count
and kept the marker, and the next pass synchronizes the new file. A file deleted since it
was recorded is skipped, because no metadata that a later checkpoint can declare clean
refers to it. The index directory is synchronized whenever a file was created, renamed or
removed since the last time, so the names of the synchronized files are durable before the
marker goes. Nothing changes in the WAL ordering. A checkpoint writes no index content and
changes no payload, the payload in `meta.json` still comes from a commit made under the
live writer lock from fenced commits only, and the marker removal and its parent directory
synchronization are unchanged. No format changed, so no version moved.

The tests cover each step. `a_raced_pass_keeps_its_progress` requires the next pass after a
raced one to synchronize only the file written during it, and
`replaced_metadata_is_synchronized_again` requires a metadata replacement that finishes
during a pass, or is in flight across it, to be synchronized by the next pass. That test
fails when the name is recorded when the replacement starts. `a_failed_pass_puts_its_files_back`,
`a_pass_failing_after_another_threads_sync_puts_both_files_back`,
`a_panicking_pass_puts_its_files_back`, `a_pass_without_helper_threads_synchronizes_every_file`
and `a_directory_opened_with_its_marker_synchronizes_every_file` cover a failed pass, a
failure after another thread's synchronization, a panic, a helper that cannot start and a
reopen after a crash. `a_locked_pass_first_catches_up_without_live` requires the
repetition to run without the lock and to clear the marker when nothing races it.
`steady_publication_cannot_starve_the_background_checkpoint` races every unlocked pass and
every repetition with a publication, and requires the fourth tick to end in one pass that
holds live and clears the marker, and the count to start again when a merge's write races
that pass. The background crash fixture
of `tests/power_loss.rs` runs a reader that searches throughout, so its commits race the
unlocked passes. It stops the index's segment merges, which write without the lock and
could race every locked pass, and its writers go on past their commits until a locked pass
has cleared the marker, so every recorded run contains such a removal. It then replays the
crash images against the receipts and the recovered text.

This change removes the background file-synchronization wait from the live writer
lock. Tantivy publication can still occur inside a group leader's fence, and WAL
and vocabulary synchronization can still delay writes. It does not establish the
50 ms load-quality rule or the serial, throughput, reader and memory gates of
section 17.9.

**Forge acceptance.** The gates of section 17.9 ran on `forge` on 2026-10-23 with
`scripts/bench-indexed-commit.py` and a release binary of 5b40d0ec built on `forge` before
timing. Each of six cells, one index on a dataset of 1.05M or 10.5M triples, ran five blocks
of an ordinary, a grouped, a grouped and an ordinary process on native storage, with the
server pinned to the performance cores. Every process checked its acknowledgements and index
answers live and after a cold reopen. [BENCHMARKS.md](../BENCHMARKS.md#group-commit-on-indexed-datasets-on-forge)
has the tables. The harness judged the gates as first written and found 28 of 36 serial
checks and 3 of 6 throughput gates met. The user then amended those two gates, as section
17.9 describes, and `scripts/bench-indexed-report.py` judged the campaign again under the
amended gates without a rerun. The results are the following.

| Gate | Result |
|---|---|
| Serial commits, p50 and p90 no more than 5% slower | 35 of 36 checks met. The miss is the p90 of spatial literal inserts at 10.5M, with a paired ratio of 1.077. |
| Four writers, at least the required gain | Met in all six cells. The paired ratios and the requirements are 1.50 and 1.41 for full-text at 1.05M, 1.68 and 1.48 at 10.5M, 1.52 and 1.45 for spatial at 1.05M, 1.59 and 1.41 at 10.5M, and 2.38 and 1.43 and 2.06 and 1.46 for embedding. |
| Text query p50 and p99 during four writers | Ratios of 0.987 and 1.000 at 1.05M and 0.997 and 0.982 at 10.5M. Not established, because the schedule lag requirement failed for both settings. |
| Spatial query p50 and p99 during four writers | Ratios of 0.964 and 0.925 at 1.05M and 1.002 and 1.044 at 10.5M. Not established, because the schedule lag requirement failed for the ordinary writer at both sizes and for one grouped process at 10.5M. |
| Peak resident memory within 64 MiB | Met in all ten cells. The largest certified difference was 19.6 MiB, for embedding at 1.05M. |

Every gate is therefore met except the p90 of spatial serial inserts at 10.5M and the two
reader gates, which are not established. The text mixed cells also need a rerun once the
stall below is bounded.

The serial and throughput misses of the first judgement follow the drive more than the
engine. The server's own counters show the mean time of a commit synchronization at about
0.23 ms in some processes and between 1.2 and 1.7 ms in others of the same cell and setting,
and it changed inside blocks. The slow state is independent of the writer. Of the 360 serial
processes, 32 of 180 ordinary and 27 of 180 grouped ones had a mean synchronization above 1 ms,
and a Fisher test gives a p value of 0.57. The full-text serial IRI inserts at 1.05M had pooled
ratios of 2.27 and 1.56 and paired ratios of 1.001 and 0.973, with an engine overhead that
differed by 2.9%, so they were drive state and not a regression. The engine overhead is the
mean request latency less the mean synchronization time over the same commits. It can be
computed for 10 of the 18 serial cells and differs by at most 3.6% between the settings in
8 of them. It is 6.8% lower with grouping for full-text literals at 10.5M and 12.7% higher for
spatial literals at 10.5M, at 0.213 against 0.240 ms. Of the 36 paired serial medians, 19
are above 1.0, with a median of 1.001 and a sign test p value of 0.43, so the single-writer
path shows no general cost. With four writers a fence carried a median of 1.9 to 2.0 commits
in the text and spatial cells. The ordinary writer ran 22% to 31% slower in the last position
of a block than in the first, so the pooled gain depends on the neighbour. Among 40 pairs of
text and spatial processes the gain was a median of 1.27 where the ordinary synchronization
took 0.8 to 1.0 ms and 1.78 where it took 1.2 to 1.6 ms. The first bin is under its own
requirement, so the cells pass because each one mixes drive states.

Every mixed process in the text cells stopped all four writers together for a median of 70
to 150 ms about every 5.4 s, with grouping and without. That is the period of the locked
fallback of the background checkpoint, and the run recorded no ticks, so the cause is probable
and not confirmed. The longest stall of most processes was 150 to 190 ms, and the extremes
were 575 ms for a grouped process and 584 ms for an ordinary one. It fails the 50 ms schedule
lag of the load-quality rule in both settings, so no text reader comparison can be
certified. Grouping did not cause it, and the ordinary path shows the same cadence and larger
extremes. The spatial processes with grouping had no such stall, and only one of them exceeded the
limit, by 8 µs at 10.5M. The ordinary spatial processes exceeded it at both sizes. At 1.05M
they fell behind their schedule by 78 to 209 ms without any request slower than 22 ms, which
shows that the rate of 80% of the slowest unpaced process left too little room. The write p99 of the mixed
runs was lower with grouping.

Two Opus agents were consulted read-only. The first, before the results were recorded, found
the diagnosis of the stall plausible and suggested a short run under `strace` on another host
to confirm it. It advised that the lane keep the harness verdicts, change no engine code and
not rerun a cell to chase a pass, and it found the carry-over between positions of a block
and the calibration headroom described above. The lane followed that advice. The second,
after the amendments, found the throughput form defensible but partly fitted to the
campaign, which is why section 17.9 names the breakpoint as chosen afterwards and gives the
sensitivities. It asked for the Fisher and sign tests above, and for a follow-up item for
each measured slowdown that the amended gates let through.

**Open under item 86.** Item 86 stays partly done. The p90 of spatial serial inserts at 10.5M
missed its amended gate, the reader gates are not established, and the text mixed cells need
a rerun. The follow-ups are [ROADMAP](../ROADMAP.md) items 110 to 112, which the standing
rule that every accepted regression gets its own item produced. Item 110 bounds the write
stall of the locked checkpoint, confirms it by tracing the ticks and ends by rerunning the
reader gates of the text and spatial cells with more headroom in the mixed rate. Item 111
investigates the spatial serial slowdowns at 10.5M, which are the missed p90, an accepted p50
at 1.025 and an accepted IRI p90 at 1.032, an engine overhead of 0.240 against 0.213 ms and a
reader p99 ratio of 1.044. Item 112 investigates the cost of a fence for indexed datasets,
whose synchronization takes 1.4 to 1.5 ms against about 1.0 ms for one ordinary commit and
which carries only 1.9 to 2.0 commits with four writers, so that the gain of 1.50 to 1.68 for
full-text and spatial datasets stays near the original bar of 1.5. A later campaign should
quiet the drive and probe its flush time before each process, randomize the order and leave
more headroom in the mixed rate. Three smaller items wait for that work. Staged documents
could count their bytes in the queue's limit, since a queue of long literals now holds more
than 4 MiB of them. The leader applies a prefix's text changes, a Tantivy commit included
when the batch is full, while its fence is still active, which delays the next fence and is
question 4 of section 17.10. The forge runs showed no text stall that this causes beyond the
locked checkpoint's, but they do not rule it out, since the mixed cells cannot be certified.
The model test has no write transaction that searches text, so the wait of a search on a
speculative head has only unit tests.
