# C13: Automatic compaction

> **Status:** designed, not built
>
> **User docs:** [API: Automatic compaction](../API.md#automatic-compaction) ·
> [Usage: Automatic compaction](../USAGE.md#automatic-compaction) ·
> [Features](../FEATURES.md#storage-tdb2-equivalent)
>
> This is the design as written before implementation. The [Outcome](#outcome) section
> at the end records how it landed.

This spec was written from the Sparkles code, from the public documentation of other
engines and from general knowledge of their published behaviour. The sources are LSM
compaction in LevelDB and RocksDB, QLever's background index rebuild, PostgreSQL's
autovacuum thresholds and TDB2's compaction. Fluree was not consulted. It builds on
durable commit identity ([CI](CI-commit-identity.md)), retained generations and named
snapshots ([F06](F06-snapshots-and-point-in-time.md)), backup leases
([F05](F05-snapshot-repositories.md)), and the storage quotas and budgets of
[C01](C01-observability-and-budgets.md).

## 1. Summary, goals, non-goals

A Sparkles store is an immutable generation (a sorted vocabulary and seven sorted,
compressed permutations) plus a delta of inserted and deleted quads. The delta lives in
memory as persistent ordered sets, and a persistent store logs it to the generation's
write-ahead log. Compaction merges the base and the delta into a new generation and
switches `CURRENT` to it. Today compaction runs only when someone asks for it, through
`sparkles compact --loc DB` or `POST /$/compact/{ds}`.

A server that takes writes for weeks never compacts on its own, and that costs it in
four ways:

* **Fast paths lose their exactness.** Counts from the statistics, the characteristic
  sets, sampled estimates and block skipping describe the base index as the last build
  wrote it. With a delta, the planner corrects the counts with one index probe per
  changed value, or falls back to a scan when that costs more. A large delta turns
  `GROUP BY ?p` with `COUNT` from a metadata lookup into thousands of probes.
* **Scans slow down.** A scan merges the delta keys in its range into every base block
  it reads. Blocks that the delta touches lose their fast paths, and delta keys are
  compared one by one.
* **Memory grows.** Each delta quad costs about 450 bytes in seven ordered sets.
  A million single-quad updates hold about 450 MB that a compaction would turn into
  about 30 MB of compressed blocks.
* **Restarts slow down.** Opening the store replays the whole write-ahead log of the
  current generation, which grows by 33 bytes per changed quad and 33 per commit.

There is a fifth cost in the current code. `Store::compact` holds the writer lock for the
whole rebuild, so every write waits for it. At 1M quads that is about a second, and at
10M quads about ten. An automatic compaction that stalls writes for ten seconds at an
unpredictable moment is worse than none.

**Goals.**

* Compact each dataset in the background when its delta is large in absolute terms or
  relative to the base, when its write-ahead log or delta memory passes a size, when its
  oldest uncompacted change gets old, or when it has been idle for a while with a delta
  worth folding.
* Build the new generation without the writer lock. Writes continue during the build,
  and the commits made meanwhile are carried into the new generation. The writer lock is
  held only for the final switch.
* Never break a guarantee that manual compaction keeps. Readers keep their snapshots,
  named snapshots and the retention window keep their commits, backups keep their
  leased generation, quotas never refuse a compaction, and a crash at any point recovers
  to a consistent store.
* Bound the cost. At most one compaction runs per dataset, automatic compactions take a
  slot of `--max-tasks`, and a server-wide cap limits how many run at once. The build
  uses a limited number of threads at a lower OS priority, and can be held to an average
  write rate.
* Make it visible and adjustable. Server flags give the defaults, a dataset can override
  them in `compaction.json` through the API or the CLI, and an off switch exists at both
  levels. `/$/stats`, a status endpoint, metrics and the UI's dataset page report what
  the policy sees and why it did or did not compact.

**Non-goals.**

* Incremental or partial compaction, such as LSM levels or rewriting only the blocks a
  delta touches. A Sparkles generation is one sorted run per permutation, and the bulk
  builder rewrites all of it at 4.7 s per 10.5M quads. That is fast enough that a full
  rewrite is the right unit for now.
* Locating delta quads per block, as QLever's `LocatedTriples` does. That would change
  the read path and is a separate decision ([COMPARISON](../COMPARISON.md)).
* Compaction in embedded use. The library gets the policy evaluation and the background
  build, and a host program can call them. The scheduler lives in the server.

## 2. User-visible behaviour

### 2.1 The policy

A dataset's compaction policy has these settings. Every one of them can be set on the
server and overridden per dataset.

| Setting | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Automatic compaction for the dataset. `--no-auto-compact` turns it off for the whole server. |
| `minDeltaQuads` | 10,000 | The floor. No quad-count, idle or age trigger fires with a smaller delta. |
| `deltaRatio` | 0.05 | The relative trigger. It fires when the delta reaches `minDeltaQuads + deltaRatio × base quads`. |
| `maxDeltaQuads` | 1,000,000 | The absolute trigger. It fires at this delta size whatever the base. |
| `maxDeltaMb` | 512 | It fires when the estimated memory of the delta and its new terms passes this many MiB. |
| `maxWalMb` | 1024 | It fires when the write-ahead log of the current generation passes this many MiB. |
| `idleSeconds` | 300 | It fires when the delta is at least `minDeltaQuads` and no commit has been made for this long. `0` turns it off. |
| `maxAgeSeconds` | 86,400 | It fires when the oldest commit not yet compacted is older than this, with any delta. `0` turns it off. |
| `minIntervalSeconds` | 60 | No automatic compaction starts sooner than this after the previous one ended. |

The delta size is inserted plus deleted quads, as `deltaQuads` in `/$/ready` reports it.
The relative trigger follows PostgreSQL's autovacuum threshold, `base + scale factor ×
rows`, so a small dataset compacts after a fixed number of changes and a large one after
a share of its size. QLever's automatic rebuild uses the same three numbers in the form
`automatic:min:max:fraction`. The default ratio is half of QLever's example of 0.1,
because Sparkles' exact statistics degrade with every changed value and its builder is
fast.

The age trigger guarantees that every change is compacted within a day even when the
delta stays small. The idle trigger folds a moderate delta while nobody is writing,
which is when the work costs least.

### 2.2 When an automatic compaction waits

A due compaction does not start, and the status says why, while any of these holds:

| Reason | Condition |
|---|---|
| `min-interval` | The previous compaction ended less than `minIntervalSeconds` ago. |
| `backoff` | The previous automatic compaction failed. The wait starts at one minute and doubles up to an hour. |
| `bulk-load` | A bulk load or bulk commit is rebuilding the dataset, or a `load` task for it is running. |
| `reasoning` | A reasoning run is in progress. It holds the writer lock and may commit a rebuild. |
| `backup` | A backup is reading the current generation under a lease. Compacting now would keep two full indexes on disk until the upload ends. |
| `restore` | The dataset is being restored in place. |
| `history` | The compaction would make the retention window drop a generation it covers, because the window already holds `--history-max-generations` generations or `maxBytes` of them. |
| `disk` | The file system lacks room for the new generation plus `--min-free-disk-mb`. |
| `running-limit` | `--auto-compact-max-running` automatic compactions are running on the server. |
| `slots` | No `--max-tasks` slot is free. Automatic compactions never queue behind user tasks. |

A manual compaction ignores all of these except a running compaction of the same
dataset, as it does today.

### 2.3 Configuration

Server flags give the server-wide policy and the resources:

| Flag | Default |
|---|---|
| `--no-auto-compact` | Automatic compaction is on. |
| `--auto-compact-min-quads N` | 10000 |
| `--auto-compact-ratio R` | 0.05 |
| `--auto-compact-max-quads N` | 1000000 |
| `--auto-compact-max-delta-mb N` | 512 |
| `--auto-compact-max-wal-mb N` | 1024 |
| `--auto-compact-idle SECS` | 300 |
| `--auto-compact-max-age SECS` | 86400 |
| `--auto-compact-min-interval SECS` | 60 |
| `--auto-compact-threads N` | A quarter of the cores, at least 1 |
| `--auto-compact-io-mb N` | 0, which means no limit. Otherwise the average MiB per second that a build may write. |
| `--auto-compact-max-running N` | 1 |

A dataset's own settings live in `compaction.json` in its directory, like `quota.json`
and `reasoning.json`. The file holds only the settings the dataset overrides:

```json
{ "format": 1, "enabled": true, "deltaRatio": 0.02, "idleSeconds": 60 }
```

`PUT /$/compaction/{ds}` replaces the file with the settings in the body, and `DELETE`
removes it so that the server's settings apply again. These
need `admin` on the dataset. A setting the dataset sets wins over the server flag. A
read-only server answers `403` to the writes. In-memory datasets keep their settings in
memory for the life of the dataset.

`--no-auto-compact` turns automatic compaction off for every dataset, whatever its own
file says. A dataset's `"enabled": false` turns it off for that dataset alone.

### 2.4 Status

`GET /$/compaction/{ds}` returns:

```ts
type CompactionStatus = {
  dataset: string;
  enabled: boolean;                 // effective: the server switch and the dataset's own
  serverEnabled: boolean;           // false under --no-auto-compact
  policy: CompactionPolicy;         // the effective settings
  own: Partial<CompactionPolicy>;   // the dataset's compaction.json
  state: "off" | "idle" | "due" | "deferred" | "running";
  trigger?: string;                 // what makes it due, e.g. "delta 63,012 quads >= 62,500 (10,000 + 5% of 1,050,240)"
  deferred?: string;                // a reason of §2.2
  task?: string;                    // the running compaction task
  measures: {
    baseQuads: number; deltaQuads: number; deltaBytes: number; walBytes: number;
    idleSeconds: number | null;     // since the last commit
    oldestChangeSeconds: number | null;  // age of the oldest commit not yet compacted
    threshold: number;              // the delta size the relative trigger fires at
  };
  last?: {                          // the last compaction since the server started
    automatic: boolean; trigger?: string;
    startedAt: string; finishedAt: string; seconds: number;
    lockMs: number;                 // how long the final switch held the writer lock
    caughtUpCommits: number;        // commits made during the build and carried over
    generation: string; outcome: "done" | "failed" | "abandoned" | "cancelled";
    error?: string;
  };
  automaticRuns: number; failures: number;
};
```

`/$/stats/{ds}` gains the same object as `compaction`. The metrics gain:

| Metric | Type | Labels |
|---|---|---|
| `sparkles_compactions_total` | counter | `dataset`, `mode="auto\|manual"`, `outcome` |
| `sparkles_compaction_seconds` | histogram | `dataset` |
| `sparkles_compaction_lock_seconds` | histogram | `dataset` |
| `sparkles_compaction_running` | gauge | `dataset` |
| `sparkles_compaction_due` | gauge | `dataset` |

The UI's dataset page shows, in its Storage panel, the state, the trigger or deferral,
the threshold the delta is measured against, and the last compaction with its duration
and lock time. An admin can turn automatic compaction off or on for the dataset there.

### 2.5 CLI

```
sparkles compaction --loc DB                        # the policy, measures and verdict
sparkles compaction --loc DB --set deltaRatio=0.02 --set idleSeconds=60
sparkles compaction --loc DB --default              # remove compaction.json
sparkles compaction --server URL --dataset ds [--set …|--default]
sparkles compact --loc DB --if-due                  # compact only when the policy says so
```

`compact --if-due` lets a cron job keep a database that no server holds compacted.

### 2.6 Rust API

```rust
pub struct CompactionPolicy { /* the settings of §2.1, all resolved */ }
pub struct CompactionSettings { /* the same, each an Option: a dataset's overrides */ }
pub struct CompactionMeasures { /* base_quads, delta_quads, delta_bytes, wal_bytes, ... */ }
pub enum Verdict { NotDue, Due(Trigger) }

impl CompactionPolicy {
    pub fn with(&self, own: &CompactionSettings) -> CompactionPolicy;
    pub fn verdict(&self, m: &CompactionMeasures) -> Verdict;
}

pub struct CompactOptions {
    pub threads: Option<usize>, pub low_priority: bool, pub io_bytes_per_sec: Option<u64>,
    pub cancel: Option<Arc<AtomicBool>>, pub progress: Option<ProgressFn>,
}
pub struct CompactReport { pub generation: String, pub caught_up_commits: u64,
                           pub lock: Duration, pub build: Duration, pub total: Duration }

impl Store {
    pub fn compaction_measures(&self) -> CompactionMeasures;
    pub fn compaction_settings(&self) -> CompactionSettings;
    pub fn set_compaction_settings(&self, s: Option<CompactionSettings>) -> Result<()>;
    pub fn compact_with(&self, o: &CompactOptions) -> Result<CompactReport>;
    pub fn compaction_blocker(&self, need_disk: bool) -> Option<&'static str>;
}
```

`Store::compact` keeps its signature and calls `compact_with` with default options.

## 3. Prior art

* **LSM trees.** LevelDB and RocksDB flush a memtable when it reaches a size and merge
  sorted runs when a level holds too many files or too many bytes. Compaction runs on
  background threads with a limited thread count, and RocksDB offers a rate limiter on
  compaction writes. Writes are throttled only when compaction falls behind. Sparkles'
  delta plays the memtable's part, and its generation is a single sorted run, so the
  size triggers and the background threads carry over. The levels do not.
* **QLever.** QLever keeps updates as delta triples located per block. An index rebuild
  writes a new index from a snapshot of the current state in the background, then
  carries the updates made since the snapshot into the new index's delta, remapping
  their ids, and swaps the new index in. The server option `--rebuild-index-strategy
  automatic:min:max:fraction` starts a rebuild after an update once the delta reaches
  `fraction` of the index size, never below `min` and always at `max`. This spec takes
  both ideas. The catch-up of §4.4 is Sparkles' version of the remapping, done per
  commit so that history survives.
* **PostgreSQL autovacuum.** A table is vacuumed once its dead tuples pass
  `autovacuum_vacuum_threshold + autovacuum_vacuum_scale_factor × reltuples` (50 and
  0.2 by default). A launcher wakes every `autovacuum_naptime` and starts at most
  `autovacuum_max_workers` workers. The workers pace their I/O with a cost limit and a
  delay. The settings can be overridden per table. The trigger formula, the server-wide
  cap on workers, the pacing and the per-table overrides all carry over.
* **TDB2.** Jena's TDB2 compacts only on request (`tdb2.tdbcompact`, Fuseki's
  `/$/compact`), into a new `Data-NNNN` directory, and blocks writers while it runs.
  Sparkles' manual compaction followed it. Automatic compaction is a departure that is
  allowed because the data and the API do not change.

## 4. Semantics

### 4.1 What a compaction does

A compaction never changes the data or the head commit. The new generation's base holds
the state of a commit `c0`. Its write-ahead log holds the commits after `c0` that were
made during the build, so the new generation covers the commits from `c0` to the head.
The old generation covers its own base up to the head at the switch. The two ranges
overlap, and the newer generation owns the commits in the overlap
([F06 §4.1](F06-snapshots-and-point-in-time.md)). When no write happens during the
build, `c0` is the head and the new log is empty, which is what manual compaction
produces today.

### 4.2 Readers

Readers hold `Arc<Snapshot>`s. A snapshot of the old generation keeps its memory maps
and file handles after the switch, as it does after a manual compaction. A query that
started before the switch finishes on the old generation. The result cache is keyed by
generation and commit, so entries of the old generation are never served for the new
one. Their bytes age out of the cache.

### 4.3 Writers during the build

The build reads a snapshot of commit `c0` without any lock. Writes commit as usual during
the build, to the old generation's delta and log. The compaction asks the writer path to
keep a copy of each commit's changes in memory, in a list that the compaction drains.
A commit pays for one vector push. When the compaction ends, or is abandoned, the list
is dropped.

A bulk commit during the build makes the compaction moot. The bulk commit rebuilds the
dataset itself, so the compaction gives up, removes its directory and reports
`abandoned`. So does a manual compaction that switches first.

### 4.4 The catch-up and the switch

When the build finishes, the compaction opens the new generation and carries the
commits made since `c0` into it. For each commit, every quad's ids are translated:
inline values and blank nodes keep their ids, and a vocabulary id becomes its key, which
is looked up in the new generation's vocabulary or added to its delta vocabulary. The
translated changes are applied to a new delta, and the commit is written to the new
generation's log with its original number, timestamp, kind and flags. The log stays a
faithful record, because each change took effect against the same logical state.

The catch-up runs in rounds. The first rounds drain the list without the writer lock and
translate what they drained. When a round finds fewer than 64 commits, the compaction
takes the writer lock, drains the rest, translates it, and publishes:

1. sync the new log and delta vocabulary, write `commit.json` with the base commit
   `c0`, and sync the generation's directory and the root;
2. switch `CURRENT`;
3. hand the new log to the writer, publish the new snapshot, and record the generations
   for history;
4. release the writer lock.

Removing an old generation that nothing needs happens after the lock is released.

A write waits for the writer lock only during this final step. Its cost is the last
round of catch-up plus about four `fsync` calls, and, for a dataset with a spatial index,
building the spatial index's base for the new generation, which already happens under
the lock today.

### 4.5 History, named snapshots and retention

Compaction keeps the F06 rules. The outgoing generation is kept when a pin or the
retention window needs a commit that only it holds, and is collected otherwise. With
the catch-up, the outgoing generation holds the commits up to `c0` alone. The ones after
`c0` are also in the new generation.

The retention window is best effort. It keeps at most `--history-max-generations`
generations, less those that pins hold, and at most its `maxBytes`. A busy dataset that
compacts every few minutes could push the window's oldest generations out and cut the
window short. An automatic compaction therefore checks first whether the window would
drop a generation it covers. If it would, the compaction waits with `history`. A manual
compaction goes ahead, as before. Named snapshots never block a compaction, because a
pin reserves its generation when it is made.

### 4.6 Backups and restores

A backup capture leases the current generation and reads its files while the upload
runs ([F05 §2.2](F05-snapshot-repositories.md)). A compaction during the upload is safe,
but it leaves two full indexes on disk until the upload ends, and the next incremental
backup uploads the whole new generation. An automatic compaction waits with `backup`
while a lease is held on the current generation.

An in-place restore waits up to 30 seconds for requests to release the dataset. A
running compaction holds the dataset, so the restore cancels it first. The cancelled
compaction removes its directory and ends `cancelled`. The scheduler skips a dataset
while it is being restored.

The frequency of compaction is a trade-off for incremental backups. Between two
compactions a backup uploads only the log's new segment. After a compaction it uploads
the whole generation. The defaults compact a slowly changing dataset at most once a day,
through the age trigger.

### 4.7 Quotas and free space

A compaction temporarily needs room for the new generation and the builder's temporary
files next to the old one. The rules are:

* **Quotas never refuse a compaction** ([API: Storage quotas](../API.md#storage-quotas)).
  The new generation's directory is left out of the dataset's measured size while it is
  built. A user's write is then never refused because a background compaction happens to
  be running. After the switch the old generation is collected, or kept for history, and
  the directory is measured again.
* **The free-space reserve still applies.** The build stops and removes its directory
  when the file system goes below `--min-free-disk-mb`, as a manual compaction does.
  Before it starts, an automatic compaction estimates its peak need as the current
  generation's size scaled by the share of quads it will hold, plus 32 bytes per quad
  for the builder's temporary files. It waits with `disk` if the free space does not
  cover that and the reserve.

### 4.8 Other indexes and caches

* **Full-text search.** The index is keyed by terms, not ids, and survives compaction.
  It is checkpointed under the lock before the switch, as today.
* **Spatial index.** A new generation needs a new base for its spatial index. The base
  is built under the writer lock during the switch, reusing the literals the previous
  base parsed. This is the largest part of the lock time for datasets with a spatial
  index.
* **Vector indexes.** A new generation's HNSW graphs are built again in the background.
  Searches are exact until they are ready. Frequent compaction of a dataset with large
  vector indexes costs CPU, and the per-dataset settings can slow it down.
* **Reasoning.** An incremental reasoning run needs the commits since the last run. If a
  compaction collected the generation that held them, the next run materializes in full.
* **Write-time ShEx validation.** The guard's typing cache does not survive a
  generation switch, so the first write after a compaction validates more nodes.

### 4.9 Crash safety

| Point of the crash | State after reopening |
|---|---|
| During the build | `CURRENT` names the old generation. The new directory has a number greater than the current one, and the open removes it as an interrupted rebuild. |
| During the catch-up | The same. The commits are in the old generation's log, which is still current. |
| After `commit.json`, before `CURRENT` | The same. |
| After `CURRENT` | The new generation is current. Its base holds `c0` and its log holds every commit after `c0` up to the switch. The open replays the log. Commits made after the switch are in the new log too. |
| After the switch, before the old generation is removed | The open collects the old generation if nothing needs it. |

The new log is synced before `CURRENT` switches, so every acknowledged commit is in the
log of whichever generation `CURRENT` names.

## 5. Design

### 5.1 Engine

A new module `store/compaction.rs` holds the policy types, `compaction.json`, the
measures, the blockers that the engine can see (a bulk rebuild, a lease on the current
generation, the history check, the disk check) and `compact_with`.

* `WriterState` gains `tap: Option<Vec<TapCommit>>`, where `TapCommit` holds a commit's
  info, its next blank-node counter and its changes. `publish_log` pushes to it when it
  is set. `rebuild_locked` clears it, which makes a running compaction give up.
* Generation numbers are reserved. A background compaction claims the next number and
  builds in `gen-<n>`. A bulk commit that runs meanwhile takes a number above any claimed
  one, so the two never share a directory.
* The build runs in its own rayon pool with the configured thread count. On Linux the
  pool's threads lower their priority with `setpriority` (nice 10). The builder's
  interrupt hook checks the cancel flag and the free-space reserve, and paces the
  build: when the bytes written to the new directory run ahead of the configured rate,
  it sleeps until they no longer do.
* `Quota` gains an excluded directory, the one being built.
* The catch-up and switch follow §4.4. In-memory stores take the same path without a
  log.
* `rebuild_locked` keeps its job for bulk commits and the manual path for stores that
  cannot use the background path.

### 5.2 Server

A new module `compaction.rs` holds the scheduler, the status, the HTTP handlers and the
metrics. One thread wakes every second, like the automatic reasoning loop. For each
dataset it reads the measures, which are O(1), resolves the policy, and decides. It
starts at most one compaction per tick per dataset as a cancellable `compact` task with
the message prefix `auto:`. The task takes a `--max-tasks` slot only when one is free.
A manual `POST /$/compact/{ds}` runs the same background build with the server's thread
count and no I/O limit.

### 5.3 Files

| File | Written by | Contents |
|---|---|---|
| `<db>/compaction.json` | `set_compaction_settings`, with `write_atomic` | `format` and the overridden settings |
| `<db>/gen-<n>/` | the build | a generation as today, plus the catch-up's log and delta vocabulary |

Backups and clones leave `compaction.json` out, like `quota.json`, because it is
operational configuration rather than data.

## 6. Phasing

**Phase 1.** Everything in this spec: the background build with catch-up, the policy,
the scheduler, the configuration, the status, the metrics, the CLI and the UI.

**Later.** Partial compaction of the permutations a delta touches, delta quads located
per block, and adaptive thresholds that learn from the measured cost of statistics
corrections.

## 7. Acceptance examples

* **A1. The relative trigger.** A dataset of 100,000 quads with the defaults becomes due
  at 15,000 delta quads (10,000 + 5,000) and not at 14,999.
* **A2. The floor.** A dataset with 9,999 delta quads that has been idle for an hour is
  not due. With 10,000 it is due with the idle trigger.
* **A3. The age trigger.** A one-quad delta whose commit is older than `maxAgeSeconds`
  is due.
* **A4. Off.** Under `--no-auto-compact`, or with `"enabled": false`, the state is `off`
  and nothing compacts.
* **A5. Writes during the build.** A background compaction of a 200,000-quad store runs
  while another thread commits single-quad inserts and deletes. Every commit succeeds.
  After the switch the dump equals the dump of an uncompacted copy that took the same
  commits, every commit made during the build reads the same at `?at=commit:N` as
  before, and the store reopens to the same state.
* **A6. Readers.** A query result taken before the compaction equals the same query on
  the new generation. A snapshot held across the switch still answers.
* **A7. Crash.** A failpoint stops the compaction after the build, after the catch-up and
  after `commit.json`. Each time, reopening gives the same head and dump, and no `gen-*`
  directory beyond the current one remains.
* **A8. A bulk commit during the build.** The compaction ends `abandoned` and its
  directory is gone. The bulk commit's generation is current.
* **A9. Retention.** With `keepCommits` covering more commits than eight compactions
  keep, the ninth automatic compaction waits with `history`. A manual one goes ahead.
* **A10. Pins.** A pin in the middle of the current generation keeps the outgoing
  generation readable after an automatic compaction.
* **A11. Quota.** A dataset just under its quota keeps accepting small commits while a
  compaction builds next to it.
* **A12. Free space.** With a reserve larger than the free space, an automatic compaction
  waits with `disk`.
* **A13. Configuration.** `PUT /$/compaction/{ds}` with `{"deltaRatio": 0.02}` survives a
  restart, `GET` shows `own.deltaRatio` and the effective policy, and `DELETE` restores
  the server's value.
* **A14. Write latency.** On a 1.05M-quad store taking single-triple commits, the writer
  lock is held for under 50 ms by the switch, excluding a spatial index.

## 8. Rejected alternatives

* **Holding the writer lock for the whole automatic build.** It is simple and it is
  today's manual path, but at 10M quads it stalls every write for about ten seconds.
* **One thread per dataset.** A server with hundreds of datasets would keep hundreds of
  idle threads. One scheduler thread and the task system bound the work instead.
* **Carrying the net delta instead of each commit.** It is less code, but the commits
  made during the build would lose their own states in the new generation, and a
  point-in-time read of one of them would need the old generation.
* **Refusing writes while a compaction builds.** Nothing requires it, since the catch-up
  is exact.
* **A size-only trigger.** A delta of 10,000 quads matters little for a 100M-quad base
  and a lot for a 50,000-quad one, so the relative trigger is needed. A relative-only
  trigger would let a huge base accumulate an unbounded delta, so the absolute cap is
  needed too.
* **Compacting on every idle period.** Every compaction makes the next incremental
  backup a full upload, so a small delta waits for the floor or the age trigger.

## 9. Open questions

1. Should the defaults depend on the dataset's size, for example a lower ratio for small
   datasets? The formula already scales, so the answer is no until measurements say
   otherwise.
2. Should the age trigger skip datasets with configured backup schedules? A daily full
   upload may be costly over a slow link. The per-dataset setting can turn it off.

## 10. Sources

* LevelDB's implementation notes on compactions, and the RocksDB wiki pages on leveled
  compaction, background threads and the rate limiter, from general knowledge.
* QLever's server option `--rebuild-index-strategy` and the comments in its
  `DeltaTriples.h`, `IndexRebuilder.h` and `IndexSwap.h` headers, read in the checkout
  of 2026-09-30 (Apache-2.0).
* The PostgreSQL documentation of routine vacuuming and the autovacuum settings, from
  general knowledge.
* Apache Jena's TDB2 documentation of `tdb2.tdbcompact` and Fuseki's `/$/compact`.
* The Sparkles code, the F05, F06 and C01 specs, and the storage quota rules in
  [API.md](../API.md#storage-quotas).

## Outcome

Not built yet.
