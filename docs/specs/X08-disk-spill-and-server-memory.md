# X08: Disk spill and server-wide memory

> **Status:** Phase 1 shipped, Phase 2 implemented with acceptance open, Phase 3 specified
>
> **Phases:** Phase 1 is shipped. It is server-wide cache sizing, a smaller memory
> profile and the server-wide query memory pool with admission control. Phase 2 implements
> batch-oriented hash join and grouping kernels in the cursor engine, then disk spill for
> hash joins, sorts, grouping and DISTINCT. Spill remains opt-in until every Phase 2b
> acceptance gate passes under roadmap item 93. Phase 3 is not built. It reduces the
> idle server footprint, the Python process footprint and the size of the full-text index.
>
> **User docs:** [USAGE](../USAGE.md#memory-and-caches) and
> [API](../API.md#query-memory-pool) cover memory management.
> [Usage](../USAGE.md#incremental-query-execution) covers execution modes, and
> [Memory and caches](../USAGE.md#memory-and-caches) covers opt-in spill.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how Phases 1 and 2 landed and where they differ from the text above.

This design was written from the Sparkles code at `fc123d04` (the store's caches, the
query context and its charges, the cursor operators, the server's limits and flags), the
specs [C01](C01-observability-and-budgets.md), [X05](X05-streaming-query-execution.md),
[F11](F11-encryption-at-rest.md), [C13](C13-automatic-compaction.md),
[P01](P01-python-bindings.md) and [F03](F03-full-text-search.md), the measurements in
[BENCHMARKS](../BENCHMARKS.md#memory-and-the-speed-it-buys), and published work on
external hashing and sorting: M. Kitsuregawa, H. Tanaka and T. Moto-oka, "Application of
Hash to Data Base Machine and Its Architecture", New Generation Computing 1(1), 1983 (the
Grace hash join); D. DeWitt et al., "Implementation Techniques for Main Memory Database
Systems", SIGMOD 1984 (the hybrid hash join); G. Graefe, "Query Evaluation Techniques for
Large Databases", ACM Computing Surveys 25(2), 1993 (external sorting, partitioning and
aggregation); D. Knuth, *The Art of Computer Programming*, Vol. 3, for replacement
selection and multiway merging; and L. Kuiper, P. Boncz and H. Mühleisen, "Robust
External Hash Aggregation in the Solid State Age", ICDE 2024, for aggregation that spills
partitions of partial states. The Linux `open(2)` manual page (`O_TMPFILE`) and the
cgroup v2 documentation (`memory.max`) inform the temporary files and the defaults.

## 1. Summary, goals, non-goals

Sparkles bounds the estimated memory of each query and fails a query that exceeds it. It
does not bound the memory of all queries together, its caches grow with the number of
datasets, and no operator can move state to disk. Three facts at `fc123d04` set the
problem:

* `StoreOptions` (`crates/sparkles-core/src/store.rs`) gives each store its own
  `BlockCache` of `cache_bytes` (1 GiB by default), its own `ResultCache` of
  `result_cache_bytes` (512 MiB) with a SERVICE cache of `service_cache_bytes` (64 MiB),
  and its own history cache of `history_cache_bytes` (1 GiB). The server passes the same
  options to every dataset, so ten datasets may cache 10 GiB of blocks. Branches of one
  dataset already share the dataset's block cache through `branching::OpenCtx`. C01 §4.5
  records the per-dataset sizing.
* Each query's `Ctx` (`sparql/ctx.rs`) counts its live bytes in `mem_live` against
  `mem_limit`, which the server sets from `--query-memory-mb` (8 GiB by default). Sixteen
  concurrent queries may each reach 8 GiB. C01 Phase 3 planned a pool
  (`--total-query-memory-mb`) and did not build it.
* The cursor operators of X05 charge their growing state and fail with
  `Error::BudgetExceeded` when it does not fit. X05 §4.3 requires an implementation design
  for spill (reserved disk bytes per query and server, ownership of temporary files,
  cleanup, encryption) before any spill lands. This spec is that design.

**Goals**

1. Size the block, result, SERVICE and history caches once per server (or per embedded
   `Catalog`), so that the memory a server uses does not grow with its datasets.
2. Make the default memory profile smaller where measurements show no loss, and offer a
   `small` profile for constrained hosts.
3. Bound the estimated memory of all queries together, admit queries against that bound,
   and refuse with `503` and `Retry-After` instead of letting the process run out of
   memory.
4. Close the measured gaps between streaming and eager execution on joins and grouping
   with kernels that work on whole batches.
5. Let hash joins, sorts, grouping and DISTINCT finish when their state exceeds memory, by
   spilling to temporary files under a disk reservation, with crash-safe cleanup and no
   plaintext from encrypted datasets.
6. Reduce idle server RSS, the Python binding's RSS and the full-text index's size, with
   each saving measured.

**Non-goals**

* An RSS limit. Budgets stay estimates of query-owned bytes (C01 §4.3), and the pool
  bounds the sum of those estimates. Mapped index pages, allocator slack and caches are
  bounded separately.
* Spill for property paths, EXISTS memos, SERVICE results, full-text, vector or spatial
  candidate sets in this spec. Their state stays budgeted and fails explicitly.
* Spill inside the eager executor (`sparql/exec.rs`). Spill lives in the cursor engine,
  and eager queries reach it through the retry of §6.7.
* Distributed or out-of-process execution, or persistent cursors.
* Changing the meaning of `--query-memory-mb` for a single query.

## 2. User-visible behaviour

### 2.1 Server flags

| Flag | Default | Meaning |
|---|---|---|
| `--cache-mb` | 1024 | Block cache for the whole server. Before X08 it was per dataset. |
| `--result-cache-mb` | 256 | Result cache for the whole server (was 512 per dataset). 0 disables it. |
| `--service-cache-mb` | 64 | Cache of remote SERVICE results for the whole server (was per dataset). |
| `--history-cache-mb` | 512 | Materialized past states for the whole server (was 1024 per dataset). |
| `--memory-profile` | `default` | `small` or `default`. A profile sets the defaults of the flags in this table, and explicit flags override it (§3.3). |
| `--total-query-memory-mb` | half the memory limit (§4.2) | The pool for the estimated memory of all queries and updates. 0 turns the pool off. |
| `--query-admission-wait` | 2 s | How long a request waits for admission before `503`. |
| `--spill` | `on` | `on`, `off` or `encrypted` (§6.6). |
| `--spill-dir` | `<data-dir>/.spill` | Directory of temporary files. Must be on a local file system. |
| `--spill-max-mb` | 25% of the free space of `--spill-dir` at start, at most 64 GiB | Disk reservation for all queries together. |
| `--query-spill-mb` | 16384 | Disk reservation for one query. Requests may lower it with `spill-mb=`. |

The `--query-memory-mb` flag keeps its meaning and its default of 8 GiB, and
`memory-mb=` still lowers it per request. A query's effective limit is the lower of its
own budget and what the pool can grant it (§4).

`sparkles serve --memory-profile small` sets `--cache-mb 256`, `--result-cache-mb 64`,
`--service-cache-mb 16`, `--history-cache-mb 128`, `--query-memory-mb 1024`,
`--total-query-memory-mb 2048`. The values come from §3.3.

Concurrency is not a new flag. The rate limiter already caps concurrent requests per
class (`--rate-limit query=…,concurrency=N` and `client-concurrency=N`, answering `503`
with `Retry-After`; C01 Outcome). The pool adds what those caps cannot express: one byte
budget shared by all queries, and admission on bytes.

### 2.2 HTTP

* A request that cannot be admitted within `--query-admission-wait` answers `503` with
  `Retry-After: 1` and the C01 error body, with budget kind `server-memory`.
* A query whose state grows past what the pool can grant, with spill unavailable for the
  operator, fails with budget kind `server-memory`. It answers `503` with `Retry-After`,
  since a retry can succeed when the server is less busy. A query that exceeds its own
  `memory-mb` keeps answering `507` with kind `memory`.
* A query that exceeds its disk reservation fails with kind `spill` and status `507`.
* The plan (`explain`, `meta.plan` in the Sparkles JSON format) marks operators that
  spilled, with bytes written, partitions, runs and merge passes. `meta.memory` gains
  `grantedBytes` and `spilledBytes`.
* `GET /$/server/memory` (permission `metrics`) returns the pool's capacity, granted
  bytes, the admission queue length, the caches' capacities and bytes per dataset, and the
  spill reservation in use.

### 2.3 Metrics

New series, following C01 §4.5:

| Name | Type | Labels |
|---|---|---|
| `sparkles_query_pool_capacity_bytes`, `…_granted_bytes` | gauge | – |
| `sparkles_query_admission_waiting` | gauge | – |
| `sparkles_query_admission_wait_seconds` | histogram | – |
| `sparkles_query_admission_rejected_total` | counter | – |
| `sparkles_spill_bytes_total` | counter | `dataset,operator` |
| `sparkles_spill_reserved_bytes` | gauge | – |
| `sparkles_spilled_queries_total` | counter | `dataset` |

`sparkles_block_cache_capacity_bytes` and `sparkles_result_cache_capacity_bytes` lose
their `dataset` label, because the capacity is server-wide. `…_bytes`, `…_hits_total`
and `…_misses_total` keep it, counted per dataset (§3.2). This is a change of series
shape, recorded in the API docs and the release notes.

### 2.4 Rust API

`CatalogOptions` (`crates/sparkles/src/catalog/mod.rs`) gains `memory: MemoryOptions`
with the cache sizes, the pool and the spill settings. A `Catalog` builds one
`SharedCaches` and one `QueryPool` and hands them to every dataset it opens. A `Dataset`
opened on its own keeps its own caches, sized by `StoreOptions` as today, and has no pool
unless `DatasetOptions::pool` names one. `QueryOptions` gains
`max_spill_bytes: Option<u64>`.

The Python, JVM and Node bindings open stores through `Catalog` or `Dataset` and inherit
this. Their default profile becomes `small` for an in-memory dataset (§8.2).

## 3. Server-wide caches (Phase 1)

### 3.1 Sharing the caches

The block cache is keyed by `(PermIndex::uid, block, column)`, and `uid` comes from
`index::next_uid()`, a process-wide counter. The result cache's key starts with the
generation's `uid` and the commit (`sparql/cache.rs`, `raw_key`). Both keys are therefore
already unique across stores in one process, so one cache can serve every dataset without
a change of key semantics.

`SharedCaches` holds `Arc<BlockCache>`, `Arc<ResultCache>` (with its `ServiceCache`) and
a `HistoryCache` budget. `StoreOptions` gains `shared: Option<SharedCaches>`. When it is
set, `Store::open` and `Store::in_memory_from` use its caches instead of building their
own at `store.rs` lines 1386–1390 and 1560–1584. `BlockCache::read_through` already gives
one-off scans a view that does not fill the cache, and it keeps working on the shared
cache.

The history cache (`Store::snapshot_at`, bounded by `history_cache_bytes`) is a list of
materialized past states per store. It becomes a charge against a shared `ByteBudget` (an `AtomicU64` with a
capacity). A store that cannot charge an entry evicts its own oldest entries first, and
then materializes without caching, as it does today when an entry exceeds the budget.

### 3.2 Per-dataset accounting and clearing

A shared cache must still say how much of it each dataset holds, and
`POST /$/cache/clear/{ds}` must clear one dataset only.

* Each store gets a 32-bit namespace from the catalog, and the cache keys gain it:
  `(ns, uid, block, column)` for blocks, and a `ns` prefix for result keys. A
  `quick_cache` lifecycle hook on insertion and eviction adds and subtracts the entry's
  weight on a per-namespace counter, and the hit and miss counters move to the namespace.
* Clearing a dataset's result cache bumps the namespace's epoch, which is part of the
  key, so its old entries stop matching and age out. The response reports the bytes the
  namespace held. The SERVICE cache already has scopes per caller and gets the same
  epoch.
* `BlockCache` no longer claims to be process-wide in its doc comment; the comment at
  `index.rs` line 780 says so today while each store makes its own.

No dataset has a reserved share. A dataset that is queried keeps its blocks and one that
is idle loses them, which is the behaviour of an LRU over all datasets. A per-dataset cap
is left to open question 2.

### 3.3 The default and `small` profiles

The measurements on the 10.5M-triple suite (BENCHMARKS, "Memory and the speed it buys")
found that the workload touches about 378 MiB of blocks, that a 256 MiB cache ran at the
same speed as 1 GiB (237 against 234 queries per second, 1,056 against 1,058 ms for the
28 query means) and saved 138 MiB of RSS after the run, and that no cache cost 46% of
throughput. The result cache was off in those runs.

This spec keeps the block cache at 1 GiB for the default profile, because it is now one
cache for all datasets and DBpedia-scale data touches more than 378 MiB. It lowers the
result cache to 256 MiB and the history cache to 512 MiB, since both are now shared. The
`small` profile takes the 256 MiB block cache that the measurement supports and smaller
values for the rest. Before either profile ships, the suite runs at 1.05M, 10.5M and on
DBpedia with both profiles and the current defaults, warm and cold, and the profile's
values change if a query regresses by more than 5% at the tie threshold of BENCHMARKS.

A profile is a set of defaults. `sparkles serve` resolves each flag from the command
line, then the profile, then the built-in default, and logs the resolved values once at
start. The C19 settings file may name a profile as well.

## 4. The query memory pool (Phase 1)

### 4.1 Grants

The pool is a `QueryPool` in `sparkles-core` (`crates/sparkles-core/src/sparql/pool.rs`),
shared through `Arc`. It has a capacity, a granted total and a FIFO queue of waiters.

Each `Ctx` gets an optional `PoolGrant`. A grant reserves pool bytes in granules of
4 MiB. Its accounted total adds every eager context's admitted high-water estimate,
live cursor charges and charges made directly against the grant. Contexts sharing one
grant own separate contributions.

`Charge::add` first checks the local `mem_live` against `mem_limit`. An eager context
then admits the higher of its previous contribution and its new live estimate.
`check_output` admits the live estimate plus the output table's estimated bytes. Only
the increase of that context's contribution is charged to the grant, including when the
grant already holds enough capacity. A rejected estimate leaves its contribution
unchanged and rolls back any failed live increment. The pool never waits during growth.

An eager context keeps that contribution until its last owner drops, including a
`QueryResult` or a separately retained `Arc<Ctx>`. This conservatively retains transient
build state and graph deduplication along with the final output, so the reservation can
exceed the final table's bytes. The pool bounds admitted estimates and does not bound RSS.

Cursor contexts charge their live state and release it promptly. Conversion after eager
preparation transfers only the live contribution to the cursor and releases the excess
eager estimate. A `QueryBatch` retained after cursor close keeps its charge until it
drops. Growth and trim share a grant-local lock, while charges fitting the current grant
use an atomic fast path. A slow charge publishes its usage before new capacity becomes
available, and a failed growth removes only that charge. The grant trims after enough
memory is released and keeps its admission floor until its last owner drops.

An update's WHERE contexts share one grant, as they share `rows_produced`. Charges that
C01 lists outside queries (SHACL-SPARQL, `/$/reason`, ShEx validation) take grants from
the same pool.

### 4.2 Admission

A request is admitted before it parses, after the rate limiter's per-class concurrency
cap has let it through. Admission takes an initial grant of 16 MiB. If the pool cannot
give it, the request waits in the queue in arrival order, cancellable by disconnect,
until `--query-admission-wait` passes. Then it answers `503` with `Retry-After: 1`. A
request whose own `memory-mb` is below the initial grant takes only that much.

The queue is FIFO, so a large waiter blocks smaller ones behind it. This is deliberate:
skipping it would starve queries that need more than the minimum. A waiter at the head
that waits for memory lets a later request pass when the later request needs only the
minimum, at most eight times in a row.

The default capacity is half of the memory available to the process: the cgroup v2
`memory.max` of the server's cgroup when it is set, and physical memory otherwise, less
the configured caches. A host where that is below 256 MiB gets 256 MiB and a warning.
`--total-query-memory-mb 0` turns the pool off, which is today's behaviour.

The rate limiter's per-class and per-client concurrency caps (`ratelimit.rs`) stay as
they are and run first. They count requests. The pool counts estimated bytes.

### 4.3 Deadlock and fairness

A query holds one grant and asks for more only from its own thread or its cursor's
pulls. It never waits for memory while holding a grant: a failed growth is an error or a
spill, never a wait. Admission is the only wait, and a request in the queue holds nothing.
The pool therefore cannot deadlock.

A single query may take the whole pool if it is alone. Two rules keep one query from
starving the rest: a query's grant may not exceed its own `mem_limit`, and while the
admission queue is not empty, a query may not grow its grant past half the capacity. A
query that hits that rule spills or fails with `server-memory`.

### 4.4 Embedded use

`Catalog::open` builds a pool from `MemoryOptions` and passes it to every dataset's
queries. Python's `Catalog`, the JVM's catalog and Node's catalog expose the capacity as
an option and default to no pool, since an embedding program usually runs few queries.
A program that opens many datasets gets the shared caches by opening them through one
catalog.

## 5. Batch kernels (Phase 2a)

X05's Outcome measured streaming at 10.5M as 35% slower than eager on `optional-chain`,
28% on `group-avg`, and 22% on `minus` and `expr-bind-group`, and found that profiles
single out no one cost. The cursor's hash join (`sparql/cursor/binary.rs`) walks one
probe row at a time through a state machine (`Candidates`) over chained build rows. The
eager join (`exec.rs`, `join_pairs` and `materialize`) builds all its pairs and then
gathers each output column in one pass.

### 5.1 Hash join

* **Build layout.** The build input is grouped by key into contiguous ranges: one pass
  counts the rows per key into an `FxHashMap<Id, u32>` of slot numbers, a prefix sum gives
  each key its start, and a second pass scatters row numbers into one `Vec<u32>`. A key's
  rows are then one slice, in build order, instead of a chain through `next`. Rows with an
  unbound key stay in their own slice. The charge covers the map, the offsets and the row
  slice, as `HashIndex::new` charges the chains today.
* **Probe.** A probe batch is joined in two passes. The first looks up every probe key
  and appends `(probe_row, build_row)` pairs to a pair buffer until the buffer holds the
  output cap. The second gathers each output column from the pairs, column by column. A
  probe row whose matches cross the cap resumes at its offset in the key's slice, so the
  output stays bounded.
* **OPTIONAL, semi, anti and MINUS** use the same lookup and record, per probe row,
  whether any pair passed the join's expression. The expression runs on the gathered
  pair table, once per batch, as the eager `left_filter` does.
* The existing fast path that joins whole probe rows when every build row binds the key
  becomes the general path.

### 5.2 Grouping

The cursor group (`sparql/cursor/group.rs`) updates aggregate state per row. The batch
kernel computes, for a whole input batch, a vector of group slots (one hash lookup per
row into a table of group keys), then updates each aggregate column by column over that
vector. COUNT, SUM, MIN, MAX and AVG over inline numbers update typed arrays. Other
aggregates keep their per-row path. Groups with one key column look up raw ids. Composite
keys hash the row's key ids once.

### 5.3 Gates

The kernels land behind the existing `Optimizations` switch, one per kernel, and replace
the old paths when the Namespace suite (`mise run bench:nsc`, A/B/B/A, as in X05's
Outcome) shows streaming within 10% of eager on `optional-chain`, `group-avg`, `minus`,
`expr-bind-group` and `distinct-join` at 1.05M and 10.5M, with no other query slower by
more than the tie threshold. X05's other follow-ups (LIMIT into the remaining fallbacks,
FOLD running state, parallel decoding, earlier first bytes) are tracked in X05's
addendum.

## 6. Disk spill (Phase 2b)

### 6.1 Where spill applies

| Operator | Spill algorithm | State that stays in memory |
|---|---|---|
| Hash join and its OPTIONAL, semi, anti and MINUS forms (`cursor/binary.rs`) | Hybrid hash join with recursive partitioning (§6.3) | Partition 0's table, write buffers, the unbound-key build rows |
| Blocking ORDER BY without LIMIT (cursor sort) | External merge sort (§6.4) | One run's buffer, merge cursors |
| GROUP BY with aggregates (`cursor/group.rs`) | Partitioned hash aggregation of partial states (§6.5) | One partition's groups |
| DISTINCT (`cursor/distinct.rs`) | Partitioned deduplication, order kept by sequence numbers (§6.5) | One partition's key set |
| Merge-join runs of equal keys | Spooled run (§6.3) | The current pair window |

Top-k keeps its bounded heap and never spills. Registered aggregates, fold aggregates,
EXISTS memos, transitive paths and the search operators keep failing at their budget.

### 6.2 Switching to spill

An operator starts in memory. When a charge for its state fails with `NeedsSpill`
(§4.1) or with its own `memory` budget and spill is enabled, it switches to its spill
algorithm once, carrying the state it holds. A query whose budget is too small for the
minimum working memory of the algorithm (the write buffers of its partitions, §6.3) fails
as before. The switch is recorded in the plan. The operator never switches back.

Spill uses the query's `Ctx::check` for cancellation and the deadline at every frame read
or written, and its I/O runs on the thread that pulls the cursor. No background thread
writes spill files.

### 6.3 Hash join

When the build side does not fit, the join partitions both sides by a hash of the key
column with a seed per recursion level:

1. **Fan-out.** The number of partitions `P` is the estimated build bytes divided by the
   memory left for the join, rounded up to a power of two between 8 and 64. Each partition
   writer holds a 256 KiB buffer, charged. If `P` buffers do not fit, `P` halves until they
   do, and below 8 the join fails with its budget error.
2. **Hybrid.** Partition 0 keeps its build rows in memory and builds the slice table of
   §5.1. Partitions 1 to P−1 write their build rows to spill files.
3. **Probe.** Probe rows of partition 0 join at once and produce output. Others are
   written to their partition's probe file.
4. **Pairs.** Each spilled partition pair is joined in turn: the build file is read into
   a table, and the probe file streams through it. A build file that still does not fit
   is partitioned again with the next seed, up to three levels.
5. **Skew.** A partition that does not shrink when partitioned again holds one heavy key.
   It is joined as a block nested loop: the build file is read in chunks that fit, and the
   probe file is read once per chunk. OPTIONAL and anti joins keep a bitmap of probe rows
   that matched, charged, and emit unmatched rows after the last chunk.

SPARQL compatibility needs care with unbound values. The join keys on the column with
the fewest unbound build rows, as now. Build rows with an unbound key are compatible with
every probe row, so they stay in memory and are joined against every partition's probe
rows. If they do not fit, the join fails with its budget error. Probe rows with an
unbound key must meet every build row. They go to a separate file, which is joined
against each build partition in turn after the partition's own probe file, with a matched
bitmap for OPTIONAL and anti joins. Every probe row therefore sees all its candidate
matches in one partition pass or in the unbound pass, and the OPTIONAL extension is
emitted only after both.

Output order changes: rows of partition 0 come first. The cursor's sort claims on the
join's output are cleared once it spills. A merge join over a run of equal keys larger
than its budget spools the run to a file and replays it for each row of the other side's
run.

### 6.4 External sort

The sort reads input batches into a charged buffer. When the buffer reaches its memory,
it sorts the buffer with the existing key and comparator (`sortkey.rs`, `exec.rs`'s
comparator) and writes it as a run. Each row in a run carries its sequence number in the
input, so equal keys keep input order, which the comparator's tie rule requires.

Rows are written as ids. For each ORDER key, the run also stores the key's sort class and,
for numbers, strings and booleans, the value that decides within the class, as the
`SortKey` of `sortkey.rs` does for top-k (X05 Outcome). Merging compares those stored
keys and calls the comparator, which decodes terms from the vocabulary, only for pairs
that the stored keys do not decide. Query-local terms (`Tag::Local`) stay valid because the query's local
vocabulary stays in memory for the query's life and is charged.

The merge reads at most 64 runs at once, with a 256 KiB charged read buffer per run.
More runs merge in passes. Output is emitted from the last pass as batches.

### 6.5 Grouping and DISTINCT

**Grouping.** When the group table does not fit, the operator writes its current groups
as partial states to `P` partition files by a hash of the group key, then sends later
input rows to their partition. Mergeable aggregates (COUNT, SUM, AVG as sum and count,
MIN, MAX, SAMPLE, and COUNT over DISTINCT through a per-group key set) write their state.
GROUP_CONCAT writes its partial string with the group's first sequence number, so
partials concatenate in input order. Errors in an aggregate are part of the state, so a
group whose aggregate failed still projects unbound. Each partition is then aggregated
alone. A partition that does not fit is partitioned again with the next seed.

The empty-group rule (a query without GROUP BY over no rows produces one row) is decided
before spilling and does not change.

**DISTINCT.** Partitioned deduplication by a hash of the whole row. When the operator's
input carries an order that the query requires, as in `SELECT DISTINCT … ORDER BY`, each
row keeps its sequence number, each partition keeps the first occurrence of each row,
and the partitions are merged by sequence number, which restores the input order.

### 6.6 Files, ownership and cleanup

**Directory.** `--spill-dir` holds one directory per server process,
`<boot-id>-<pid>-<random>`, created at start with mode 0700 and a marker file
`SPARKLES-SPILL` that holds the format name and the process's start time. The process
holds an exclusive `flock` on the marker for its life.

**Files.** A spill file is opened with `O_TMPFILE` in the process directory where the
file system supports it, so it has no name and the kernel frees it when it is closed or
when the process dies. Elsewhere it is created as `q<request-seq>-<n>.spl` and unlinked
at once, which has the same effect on Unix. A file's handle belongs to the operator that
wrote it. Dropping the operator, on completion, error, cancellation or cursor close,
closes the handle and frees the space.

**Crash cleanup.** At start, the server walks `--spill-dir` and removes a directory only
when its name matches the pattern, it holds the marker with the right format name, its
`flock` can be taken (so no live process holds it), and it holds no files other than the
marker and `*.spl` files. Anything else is left in place with a warning. Since files are
unlinked on creation, a crash normally leaves only empty directories.

**Reservation.** Before writing, an operator reserves disk in 64 MiB extents from the
query's reservation (`--query-spill-mb`, lowered by `spill-mb=`) and the server's
(`--spill-max-mb`). The server reservation also keeps `--min-free-disk-mb` free on the
spill file system, checked with `statvfs` when an extent is reserved. A failed reservation
fails the query with kind `spill`. An `ENOSPC` or other I/O error is the same failure,
with the error's text in the log only.

**Format.** A file starts with a 32-byte header: the magic `SPKSPL01`, a format version,
the operator kind, the column count, the codec and an encryption flag. Frames follow. A
frame has a 16-byte header (payload length, row count, CRC-32 of the header and payload
computed as the delta vocabulary's chunks are, with `flate2::Crc`) and a payload of
columns. Each column is encoded with `index::encode_column` (zig-zag delta varints, then
LZ4), which the index blocks use. Sort runs add their key columns. Frames hold at most
4,096 rows. A reader checks the CRC and the row count and fails with `Error::Corrupt` on a
mismatch. The format is private to one process and is never read by another version.

### 6.7 Encryption

A spilled frame holds ids, and inline ids carry literal values (integers, decimals,
dates). [F11](F11-encryption-at-rest.md) Phase 3, encryption of the database directory,
is not built. This spec still fixes the rule now:

* `--spill encrypted` encrypts every frame with AES-256-GCM through `aws-lc-rs`, which
  `sparkles-backup` already uses for encrypted repositories. Each spill file has a random
  256-bit key generated at creation and held only in memory, and the nonce is the file's
  number and the frame's number. The key is never written. After a crash the files are
  unreadable and the cleanup above removes them.
* When F11 Phase 3 lands, a dataset with encryption at rest spills only encrypted, and
  `--spill on` means encrypted for that dataset. With `--spill off` or without the
  `backup-encryption` feature in the build, its queries fail within their memory budget,
  as X05 §4.3 requires.
* Swap is outside this design. The operator documentation states that an encrypted
  deployment should also encrypt swap or run without it.

### 6.8 Eager queries

The eager executor does not spill. Two paths bring eager queries to it:

* With `execution=auto`, a SELECT whose plan estimate exceeds half its memory budget and
  whose root operators all have cursor implementations runs as a cursor. This extends
  X05's measured admission rules and goes through the same measurement gate.
* An eager query that fails its memory budget before producing any output, and that
  called no SERVICE, registered callback or `LOAD`, is run again once as a cursor with
  spill allowed. The access log and the plan record the retry. A query with side
  effects is not retried.

## 7. Architecture

| Part | Location | Change |
|---|---|---|
| `SharedCaches`, `ByteBudget` | `crates/sparkles-core/src/store.rs`, new `store/caches.rs` | Built by the catalog, taken by `Store::open` and `Store::in_memory_from` through `StoreOptions::shared` |
| `BlockCache` | `crates/sparkles-core/src/index.rs` | Namespace in the key, per-namespace counters |
| `ResultCache`, `ServiceCache` | `crates/sparkles-core/src/sparql/cache.rs`, `svccache.rs` | Namespace and epoch in the key, per-namespace counters, `clear_namespace` |
| `QueryPool`, `PoolGrant` | new `crates/sparkles-core/src/sparql/pool.rs` | Admission queue, grants, metrics |
| `Ctx`, `Charge` | `crates/sparkles-core/src/sparql/ctx.rs` | Optional grant, `NeedsSpill`, `spill: Option<Arc<QuerySpill>>` |
| Spill files | new `crates/sparkles-core/src/spill/` (`dir.rs`, `file.rs`, `frame.rs`, `crypt.rs`) | Directory ownership, reservation, frames, encryption |
| Cursor operators | `sparql/cursor/binary.rs`, `group.rs`, `distinct.rs`, `merge.rs`, the `Kind::OrderBy` operator in `cursor.rs` | Batch kernels, spill modes |
| `CatalogOptions`, `MemoryOptions` | `crates/sparkles/src/catalog/mod.rs` | Shared caches and pool for every dataset |
| Server | `crates/sparkles-server/src/main.rs`, `state.rs` (`Limits`), `http.rs` (`query_options`), `obs.rs` | Flags, profile resolution, admission middleware, metrics, `/$/server/memory` |
| Bindings | `sparkles-py`, `sparkles-ffi`, `sparkles-node` | Profile defaults, catalog options |

`Error` gains `NeedsSpill` (internal, never returned to callers) and the budget kinds
`server-memory` and `spill`. The server maps them as in §2.2.

## 8. Footprint (Phase 3)

Each item starts with a breakdown and ends with a measured change or a documented
reason to keep the cost.

### 8.1 Idle server RSS

The suite measured RSS two seconds after start at 52 MiB at 1.05M and 53 MiB at 10.5M,
against 22 and 28 MiB for Oxigraph (BENCHMARKS, "Server memory and index size"). The
breakdown comes from `/proc/self/smaps_rollup` and mimalloc's statistics on an idle
server, split into anonymous memory, file-backed pages and thread stacks. The candidates,
each to be confirmed by the breakdown before it changes:

* thread pools started at once (Tokio workers, the Rayon pool, the idle-release thread)
  and the heap each thread's first allocation reserves in mimalloc;
* cache tables sized at creation, such as the result cache's table for 10,000 entries;
* the vocabulary's sparse index (`vocab.idx`), the statistics with their characteristic
  sets (up to 10,000 per generation), and the numeric column, which are read at open;
* file-backed pages of the binary and its embedded UI assets touched during start.

Changes that do not cost query speed (lazy pools, lazy tables) are made. Data that the
first query needs anyway (statistics, the sparse index) stays loaded, and the docs say
so. The target is RSS after start within 1.5 times Oxigraph's at both scales.

### 8.2 Python process RSS

The Python read process peaked at 135 MiB against 97 MiB for pyoxigraph, and mimalloc
raised the benchmark process's peak by about 20% (P01 Outcome). An in-memory dataset
opened from Python today gets the server's default caches (1 GiB of blocks, 512 MiB of
results, 1 GiB of history). Three changes are measured:

* the `small` profile as the default for datasets opened through the bindings, with an
  option to choose `default`;
* `mi_collect(true)` after a bulk load and after a transaction that wrote more than a
  threshold, so memory used by the load returns to the OS;
* mimalloc's purge delay, set to return freed segments sooner in the bindings than in the
  server.

The target is a read-process peak within 1.1 times pyoxigraph's on the bindings
benchmark, with no case slower than the tie threshold.

### 8.3 Full-text index size

The full-text index is 13.1 MB at 1.05M and 122.5 MB at 10.5M, against 11.1 and 99.9 MB
for QLever, whose text index indexes different predicates and features. Tantivy's
`Searcher::space_usage` gives a size per field and per structure. The schema
(`text.rs`, `schema`) stores `key` as an indexed bytes field, `s` and `o` as fast bytes
columns that hold whole terms, `p` and `g` as fast strings, and the text with frequencies
and positions in one field per language. Options to measure:

* positions only for configurations that need phrase or proximity queries, as a
  `TextConfig` option, with the default kept when Lucene syntax with phrases is in use;
* the object column holding a hash of the literal instead of its bytes, with the literal
  read from the vocabulary for hits, where the cost to hit decoding stays under 5%;
* the `key` field replaced by the hash it already derives from.

Any change to the index layout goes through the existing config hash, which rebuilds the
index.

## 9. Phases

| Phase | Deliverable | Gate |
|---|---|---|
| 1a | Shared caches with per-dataset accounting, profiles, the new defaults | A1–A3. Suite at 1.05M, 10.5M and DBpedia within the tie threshold. |
| 1b | The pool, admission, `/$/server/memory`, metrics, embedded catalog pools | A4–A7. The pool adds at most 2% or 0.5 µs to single-client latency, as the Outcome's latency gate measures it. |
| 2a | Batch hash join and grouping kernels | §5.3 |
| 2b | Spill for hash join, sort, grouping, DISTINCT; the eager retry | A8–A14. Spill off by default until every gate passes, then on. |
| 3 | Footprint items of §8 | A15–A17 |

## 10. Acceptance examples

* **A1 — One cache.** A server with ten datasets of 1.05M triples each, `--cache-mb 256`,
  runs the suite against each in turn. The block cache never holds more than 256 MiB in
  total, `sparkles_block_cache_bytes` summed over datasets equals the cache's weight, and
  `RssAnon` after the run is at most `RssAnon` of a server with one dataset, plus 1.25
  times the difference between the two caches' bytes, plus nine times the anonymous
  memory of an idle opened dataset, within 10%. The block cache's bytes never exceed
  `--cache-mb`. The first version of this gate bounded total RSS by 1.1 times that of a
  server with one dataset, and the second counted each cached byte once. The Outcome
  records why it changed twice.
* **A2 — Clearing one dataset.** After queries on `a` and `b`, `POST /$/cache/clear/a`
  reports `a`'s entries and bytes. A repeated query on `b` hits the cache, and one on `a`
  misses.
* **A3 — Profiles.** `serve --memory-profile small --cache-mb 512` logs a block cache of
  512 MiB and the `small` values for the rest.
* **A4 — Admission.** With `--total-query-memory-mb 64` and `--query-memory-mb 64`, a
  query that holds 60 MiB of state runs while a second request waits. With
  `--query-admission-wait 0.2`, the second answers `503` with `Retry-After: 1` and kind
  `server-memory`. After the first ends, the second succeeds.
* **A5 — Queueing.** With a pool of 64 MiB and three queries that each hold 30 MiB, the
  third waits, and `sparkles_query_admission_waiting` reads 1 while it does. A rate-limit
  concurrency cap below the pool's capacity still answers `503` first, as today.
* **A6 — No deadlock.** 64 clients run joins that each want 40% of the pool, for a
  minute. Every request ends with a result, `503` or a `server-memory` failure, and the
  granted total returns to zero.
* **A7 — Grant release.** A cursor that returns batches and is dropped by the caller
  returns its grant. A batch retained after close keeps its share until it drops.
* **A8 — Hash join spill.** A join whose build side is 4 times `memory-mb` returns the
  same bag as with an unlimited budget, at batch sizes 1, 7 and 4,096, and the plan shows
  the partitions, recursion levels and bytes spilled.
* **A9 — Unbound keys and OPTIONAL.** Spilled OPTIONAL and MINUS over inputs where 5% of
  build rows and 5% of probe rows leave the key unbound, with a filter in the OPTIONAL,
  match eager results. Each probe row's unbound extension appears exactly when it has no
  passing match.
* **A10 — Skew.** A join where one key holds 90% of both inputs completes through the
  block nested loop within its memory budget.
* **A11 — External sort.** ORDER BY over 20 million rows with mixed numbers, strings,
  dates with and without time zones and unbound values, under a 64 MiB budget, returns the
  same sequence as an unlimited sort, including ties in input order.
* **A12 — Grouping and DISTINCT.** GROUP BY with COUNT, SUM, AVG, MIN, MAX, SAMPLE,
  GROUP_CONCAT and COUNT(DISTINCT) over 10 million groups under a small budget matches
  eager results. `SELECT DISTINCT … ORDER BY` keeps the order after spilling.
* **A13 — Failures and cleanup.** Reservation denial, an injected `ENOSPC`, an injected
  CRC mismatch, cancellation during a write and a deadline during a merge each fail the
  query with the right kind, and `--spill-dir` holds no files afterwards. A process killed
  with SIGKILL during a spill leaves no `*.spl` file, and the next start removes its
  directory. A foreign directory in `--spill-dir` is left in place.
* **A14 — Encryption.** With `--spill encrypted`, no spill frame contains the bytes of a
  known inline value or of a 16-byte pattern from the input, checked by reading the
  files through `/proc/<pid>/fd` during the query.
* **A15 — Idle RSS.** RSS two seconds after start at 1.05M and 10.5M is within 1.5 times
  Oxigraph's, or the spec's Outcome lists what remains and why.
* **A16 — Python RSS.** The bindings benchmark's read process peaks within 1.1 times
  pyoxigraph's.
* **A17 — Text index.** Each layout change reports the index size and the full-text
  queries' times before and after.

## 11. Testing and measurement

* Unit tests for grants (granule rounding, release on drop, the half-capacity rule) and
  for the frame codec (round trips, CRC failures, truncated files).
* A differential test, as X05's, compares cursor and eager answers over generated joins,
  OPTIONAL, MINUS, grouping, DISTINCT and sorts at budgets that force zero, one and three
  levels of partitioning, over a compacted base, pending changes and an access-restricted
  view.
* A fault-injection layer under the spill file API fails the n-th write, read, fsync or
  reservation.
* The W3C SPARQL 1.1 query evaluation tests run with a 1 MiB budget and spill on, through
  cursors at batch sizes 1 and 4,096.
* Performance: the Namespace suite for the kernels; a spill suite that runs the joins,
  sorts and groups of `bench:nsc` at budgets of 100%, 50%, 25% and 10% of their in-memory
  peak and reports time, bytes spilled and passes; the 10.5M memory probe of BENCHMARKS
  for the profiles; DBpedia warm and cold for the cache defaults.

## 12. Rejected alternatives

* **Divide the configured cache size by the number of datasets.** It starves a busy
  dataset when many idle ones exist, and it changes each time a dataset is added.
* **A memory-mapped scratch area instead of files.** Dirty mapped pages count toward RSS
  and the cgroup's memory until written back, which defeats the purpose.
* **Spill in the eager executor.** Its operators build whole tables, so spill there would
  duplicate the cursor operators. The retry of §6.8 covers eager queries.
* **Sort-merge join as the only spilled join.** Sorting both inputs costs more than
  partitioning for the usual case where the build side exceeds memory by a small factor,
  and it loses the in-memory partition of the hybrid join.
* **Waiting for memory during execution.** A query that waits while holding a grant can
  deadlock with others. Execution fails or spills, and only admission waits.
* **Writing spill files under the dataset directory.** Backups, quotas and `sparkles
  check` would see them, and a dataset may sit on slower or replicated storage.

## 13. Open questions

1. Should the pool be on by default, as designed, or off until a release lets operators
   opt in? Turning it on changes behaviour for servers that today run many large queries
   at once.
2. Should a dataset have a minimum or maximum share of the shared block cache, settable
   per dataset under C19?
3. Should spill be allowed for updates' WHERE clauses, which hold the writer lock while
   they run?
4. Is 16 MiB the right initial grant for admission? Small queries need far less, and a
   smaller grant admits more of them at once.

## Outcome

Algorithm SERVICE calls now share the query ledger and pool admission with ordinary
query work. Native allocation and output leases use one admission transaction, and a
refused increment changes neither live ownership nor the successful native peak.
Eager owners retain their admitted high-water contribution. Cursor owners retain live
storage through fallback transfer, sorting, returned batches and early close. Each
public context operation still reads its current memory limit and grant. Existing
contributions remain attached to their original grants when those fields change.

Projection and result caches keep plain data rather than query owners. A warm caller
admits its own storage before use or copying. Closing an algorithm call transfers its
finished output to query-only ownership without releasing and reacquiring its grant.
Algorithm accumulation reuses admitted columns and grows them geometrically, keeping
the incoming part and complete replacement charged while the old backing is live.
Spare capacity stays charged with the output, trading unused capacity for fewer
allocations and prefix copies.
These capacity and ownership checks retain the existing estimates for other executor
kernels and decoded terms. They exclude allocator metadata, size classes and process
RSS, and do not establish a performance result.

Roadmap item 2 finished Phase 1 on 2026-10-18, when its A1 and latency gates were met.
The Phase 1a suite runs at 10.5M and on DBpedia remain, and roadmap item 1 takes them.
Phase 2 is built for sorts, groupings and DISTINCT under item 7, and item 85 added
hash-join spill, the merge-join spool, DISTINCT below an ORDER BY and encryption on
2026-10-18. Spill remains opt-in. Item 93 records the full-scale acceptance runs,
the review of enabling spill by default and the remaining execution work. Phase 3
is not built and belongs to item 40.

### Phase 1a: shared caches

The block, result, SERVICE and history caches are sized once per server and per embedded
`Catalog`. `serve` builds one `SharedCaches` through its catalog, and every dataset, with
its branches, reads through a `CacheScope` of it that `StoreOptions::shared` carries. The
spec named the field's type `SharedCaches`. The type is `Option<CacheScope>`, which is a
`SharedCaches` plus the dataset's namespace, because a dataset and its branches share one
namespace and so one set of counters.

* **Block cache.** The key stays 16 bytes. Its third field holds the column in the low 8
  bits and the 24-bit namespace above it, so the hot path pays no extra hashing. Each
  store has a `BlockCache` view with its own hit and miss counters. A view's `bytes` and
  `entries` count its namespace by walking the cache, which `/$/server/memory` and the
  metrics do once per scrape for all datasets. This replaces the lifecycle hook of §3.2,
  because quick_cache does not call a hook for replaced entries, and a walk is exact. A
  walk costs microseconds for a cache of a few thousand columns.
* **Result and SERVICE caches.** Keys are `(namespace, string)` and a lookup borrows the
  string. `POST /$/cache/clear/{ds}` removes the namespace's entries with `retain` instead
  of bumping an epoch. It is exact, so the counters report what was cleared, and it costs
  a pass over the cache. Both caches keep an entry up to an eighth of their budget, and
  quick_cache refuses an entry heavier than 97% of one shard's share, so each has at
  most four shards. With quick_cache's default of four shards per CPU, a 64 MiB result
  cache on a host with 16 CPUs had shards of 1 MiB and dropped the 8 MiB entries that it
  accepted. The block cache keeps the default, because quick_cache keeps at least 32
  expected columns in each of its shards, so a full column always fits.
* **History cache.** Each store's materialized states stay a list, charged to a shared
  `ByteBudget` that every mutation settles. A store that finds the budget over evicts its
  own oldest states, and when it still holds only the state it just built, that state is
  returned without being cached.
* **Defaults and profiles.** The defaults are the block cache at 1 GiB, the result cache at
  256 MiB, the SERVICE cache at 64 MiB and the history cache at 512 MiB, all for the
  server. `StoreOptions::default` takes the same result and history sizes. `--memory-profile
  small` sets 256, 64, 16 and 128 MiB, a query budget of 1 GiB and a pool of 2 GiB. A flag
  overrides the profile, and `serve` logs the resolved values once at start. The C19
  settings file cannot name a profile, and the bindings default to the `default`
  profile. §8.2 of this spec covers that change.
* **Capacity series.** `sparkles_block_cache_capacity_bytes` and
  `sparkles_result_cache_capacity_bytes` carry no `dataset` label. The JSON snapshot keeps
  `capacityBytes` in each dataset's block and result cache, with the shared capacity.
* **Catalogs.** `CatalogOptions::memory.caches` is `FromStore` (the default, sized by the
  store options), `Sizes` or `PerDataset`. A `Dataset` opened on its own keeps its caches.
* **Entries of removed datasets.** The entries of a deleted or reopened dataset stay in the
  shared caches until they are evicted. This is harmless, because the keys include the
  store's identity.

### Phase 1b: the query memory pool

`QueryPool` and `PoolGrant` are in `sparkles-core` (`sparql/pool.rs`) as §4.1 describes.
A plain eager query charges a lock-free fast account of atomic counters, and
`Charge::add` admits its growth there with a compare-and-swap. A query that calls a graph
algorithm, keeps an output lease or moves to a cursor is promoted to the query ledger, and
from then on `Charge::add` admits local and pool growth under the ledger's lock before
publishing live ownership. Cursor state returns through the same ledger, and the pool
never makes a running query wait. The grant adds eager contexts' retained estimates to live cursor charges,
and growth and trimming of one grant run under a lock of its own. The differences from
the text:

* **Admission** is an axum layer inside the rate limiter, as §4.2 says, and passes the
  grant to handlers through a task-local that `query_options` reads. A request is a query
  or an update by the rate limiter's classes. Other requests are not admitted. A query
  they run, MCP tool calls included, starts with an empty grant from its dataset's pool,
  does not queue and fails with `server-memory` when the pool is short.
* **The initial grant** is 16 MiB, the server's `--query-memory-mb` if that is lower, and
  never more than the pool. It does not follow a request's own `memory-mb`, because the
  parameters are read after admission. The effect is that a request that lowers its budget
  still reserves 16 MiB until it runs.
* **Fairness.** Waiters are served in arrival order. A waiter behind the head may pass
  only waiters that are larger than it and do not fit in the pool now, at most eight times
  in a row. While requests wait, a query may not grow its grant past half of the pool,
  and it always keeps the initial grant. A grant gives back memory only when it holds more
  than 20 MiB beyond its use, and keeps its first 16 MiB.
* **Default size.** Half of the memory the process may use (the cgroup v2 limit of the
  server's cgroup and its ancestors, or `MemTotal`) less the configured caches, and at
  least 256 MiB with a warning. The pool is on by default, which is the answer to open
  question 1, and it stays on after the review fixes below. On a host where the memory
  cannot be read the pool is off until `--total-query-memory-mb` sets it.
* **Behaviour change on small hosts.** Before the pool, concurrent queries on a small host
  all ran. Now each request reserves 16 MiB at admission, so on a host whose pool sits at
  the 256 MiB floor the seventeenth concurrent query or update waits for
  `--query-admission-wait` and then answers `503` with `Retry-After`. Larger hosts have
  room for hundreds of concurrent requests. `--total-query-memory-mb 0` turns the pool off
  and restores the earlier behaviour. This was decided on 2026-10-13 and belongs in the
  release notes.
* **`NeedsSpill`**, `spilledBytes` and the spill reservation in `/$/server/memory` belong
  to Phase 2 and are not built. `meta.memory.grantedBytes` is the most the query held.
* **Not covered.** SHACL-SPARQL, `/$/reason` and ShEx do not take grants, because they run
  through the engine's contexts without a request grant. A SERVICE call from a query to
  its own server is admitted like any request, so it can wait for memory that its caller
  holds, and it ends with `503` at the admission deadline. With `SILENT` the failed call
  yields no rows for that pattern instead of an error. Routes that count as queries but
  do not run through the request's options, such as GraphQL, SHACL, ShEx, diff,
  `memory/brief` and the `/$/stats`, `/$/schema` and `/$/validate` reads, hold the
  admitted grant idle while a query of their own takes a second grant from the dataset's
  pool. That reserves more than they use but bypasses nothing. Uploads and Graph Store
  writes hold an admission grant for the whole load, and the memory of the load itself is
  not charged to the pool.
* **Errors.** `BudgetKind::ServerMemory` (`server-memory`) answers `503` with
  `Retry-After: 1`, counts as a budget in `sparkles_budget_exceeded_total`, and is
  `budget-server-memory` in the MCP tools. The optional operator caches that fall back on
  a memory failure treat it like `memory`.
* **Embedded.** `MemoryOptions::pool_bytes` gives a catalog a pool. Its datasets' queries
  start with an empty grant and fail with `server-memory` without queueing, and a caller
  that wants admission uses `QueryPool::admit_blocking`. Python
  (`total_query_memory_bytes`) and Node (`totalQueryMemoryBytes`) pass the option. The JVM
  bindings do not.

A review by a second model changed the code before it shipped. A pool smaller than 16 MiB
refused every request, so admission now clamps the initial grant to the pool. A newcomer
could overtake a queued request, so the check and the reservation of `try_grant` happen
under the queue's lock. The bypass rule was looser than §4.2, and the history eviction
left one state per store over a full budget. The counters that decide wake-ups use
sequentially consistent ordering.

That review did not find every defect, and a later review of the merged lane found
more, which are now fixed.

* Grant growth and trimming could leave accepted charges unbacked or fail simultaneous
  growth although the pool had room. The grant now keeps one accounted total and
  serializes growth and trimming. A slow charger publishes its contribution before new
  capacity becomes available, and a failed growth removes only its contribution. Checked
  additions reject counter overflow. Trimming rereads usage before returning granules.
* Eager output estimates and retained results could lose coverage after their build
  guards dropped. Each eager context now owns its highest successfully admitted local
  estimate, from both `Charge::add` and `check_output`. The grant adds those independent
  contributions, even when an estimate fits its existing capacity. A contribution lasts
  until the context's last owner drops. It can exceed the final result's bytes because
  transient tables and deduplication remain included. This is an estimate policy, not an
  RSS bound. Cursor contexts retain live charges and still trim after releasing state.
* Eager CONSTRUCT retains the admitted estimate of decoded triples, named graph quads
  and deduplication copies. Eager DESCRIBE now checks traversal state and lexical output
  before allocation through a scoped charge. Cursor DESCRIBE keeps its weak charge owner.
  Both graph forms retain coverage while their result or context remains alive.

* **Paths that skipped the pool.** `POST /{ds}/update` ran without the admitted grant and
  without the dataset's pool. The Python query defaults dropped the pool. Memory clones
  adopted by a catalog had no pool, and `Transaction::update_with` skipped the dataset's
  pool, which Node, Python and the JVM bindings reach. All of them now take the pool.
* **Shared namespaces.** A memory clone kept its source's cache namespace, so clearing the
  clone cleared the source's cached results and the per-dataset figures counted both
  datasets under both names. Every memory clone now gets a namespace of its own, which
  also covers the standalone copies of the Node, Python and JVM bindings.

A second independent review and validation found the slow-growth publication race,
missing charge-based CONSTRUCT retention, uncharged eager DESCRIBE and the missing sum of
retained contexts on one grant. Regression tests cover oversubscribed concurrent growth,
fitting output estimates, separate context lifetimes, graph lexical payload, budget
failure cleanup and conversion from eager preparation to cursor accounting. The profile
runs at 10.5M and DBpedia remain, and roadmap item 1 takes them.

A plain query charges the lock-free fast account and never takes the ledger lock. Once a
query is promoted to the ledger, the ledger serializes admission and successful
ownership publication for its charges and adapted algorithm buffers. Pool releases and
wakeups run after that lock is released. This changes the retained-estimate path from the earlier fast
read and separate growth helper. The earlier isolated Intel comparison covered only
`types-grouped` and does not measure this repair. The Gates section records the A1 and
latency gates, and roadmap item 1 takes the 10.5M and DBpedia profile runs.

### Gates

* **A1.** With ten datasets of 1.05M triples each and `--cache-mb 256`, the block cache
  held 251 MiB in 1,063 columns after each dataset ran two queries, and the per-dataset
  bytes added up to that figure. RSS was 770 MiB, against 467 MiB for one dataset and
  1,074 MiB for the build before this change, whose ten caches held about 450 MiB of
  blocks. The bound of 1.1 times one dataset's RSS did not hold, and the mechanism was
  not short of anything. One dataset at 1.05M triples needs about 40 MiB of blocks, so the
  one-dataset server never fills its cache, while the ten-dataset server fills it to 251
  MiB as intended. That accounts for about 210 MiB of the 303 MiB difference. The rest,
  about 10 MiB per extra dataset, is each dataset's vocabulary, statistics and mapped
  index pages. Mapped pages that queries touched count in RSS as file-backed pages, which
  the kernel can reclaim and which Sparkles does not own, and §1 already says that mapped
  pages are bounded separately.

  On 2026-10-13 the gate was restated in terms of `RssAnon`, as §10 now says. The design
  of this spec is unchanged, and only the gate that measures it moved.

  The restated bound was measured on forge on 2026-10-18 with `scripts/x08-gates/a1.sh`,
  three repeats of each case. It reads `VmRSS`, `RssAnon`, `RssFile` and `RssShmem` from
  `/proc/<pid>/status` and the cache bytes from `/$/server/memory` for ten datasets and
  for one, at `--cache-mb 256` and at `--cache-mb 32`, after the suite ran against every
  dataset. It also reads servers that opened their datasets and ran no query, and an idle
  opened dataset costs about 228 KiB of anonymous memory. At 5b2085bc the bound failed.

  Two causes were found and one was fixed. mimalloc 3 served every request of the server
  through its aligned entry point, which puts a power-of-two size of 64 KiB or more into
  the next size class, so each full decoded column of 256 KiB took 320 KiB. The allocator
  of the server, the bindings and the memory examples, `crates/sparkles-alloc`, now calls
  the plain entry points for alignments up to 8 bytes, and a decoded column is a boxed
  slice behind its `Arc`, whose header no longer pushes it past 256 KiB. The other cause
  is allocator overhead that grows with the bytes the caches hold. Ten datasets at 32 MiB
  hold 115 MiB more in the result cache than one. They use 154 MiB more anonymous memory,
  of which the same run with the result cache off accounts for 12 MiB, so the result cache
  takes about 1.2 bytes of heap for each byte it counts. Pages that evicted entries leave
  in the heaps of the tokio and Rayon threads add to it, and limiting both pools to one
  thread brought a run with the result cache off to 1.08 times the restated bound on the
  development laptop. No cost was found that grows with the number of datasets beyond the
  idle one.

  | Build | `--cache-mb` | `RssAnon`, one dataset | `RssAnon`, ten datasets | Cache bytes, one and ten | Against the restated bound | Against the amended bound |
  |---|--:|--:|--:|--:|--:|--:|
  | 5b2085bc | 256 | 132.5 MiB | 572.5 MiB | 60.6 and 378.7 MiB | 1.265 | 1.076 |
  | 5b2085bc | 32 | 120.7 MiB | 274.8 MiB | 44.1 and 160.1 MiB | 1.152 | 1.027 |
  | With the allocator fix | 256 | 120.7 MiB | 492.9 MiB | 60.6 and 379.3 MiB | 1.117 | 0.946 |
  | With the allocator fix | 32 | 111.0 MiB | 264.5 MiB | 44.3 and 159.9 MiB | 1.157 | 1.027 |

  The allocator fix lowered VmRSS with ten datasets at 256 MiB from 702.8 to 612.8 MiB.
  On 2026-10-18 the user decided that the bound counts 1.25 bytes of anonymous memory for
  each cached byte, as §10 now says, and the gate passes. Roadmap item 88 covers the
  remaining overhead. The 12 MiB and the 1.2 bytes per cached byte above come from the
  same script run with `A1_SERVE_ARGS=--result-cache-mb 0`, which turns the result cache
  off.
* **A2.** Covered by `shared_caches.rs`. Clearing one namespace leaves the other's entries,
  and only its own queries miss afterwards. The HTTP route reports the namespace's counts.
* **A3.** `serve --memory-profile small --cache-mb 512` logs `block cache 512 MiB, result
  cache 64 MiB, SERVICE cache 16 MiB, history cache 128 MiB, query budget 1024 MiB, query
  pool 2048 MiB`.
* **A4 and A5.** `http/pool_tests.rs` holds 56 of 64 MiB, and a request answers `503` after
  the wait with `Retry-After: 1` and the budget `server-memory`. Another request waits
  while `sparkles_query_admission_waiting` reads 1 and then succeeds when the memory
  returns. The rate limiter's caps answer first, as before.
* **A6.** `pool.rs` runs 64 threads for five rounds each that ask for 40% of the pool
  under a 300 ms wait. Every one ends with a grant or `server-memory`, and the pool
  returns to zero.
* **A7.** `query_pool.rs` drops a cursor with a batch in hand and then the batch, and the
  pool returns to zero.
* **Latency.** `mise run bench:nsc 899fb251 23807e08` at 1.05M triples, 27 queries, four
  rounds on an AMD EPYC instance with the pool on in the candidate. The geometric mean of
  the server times was 0.951 and of the client times 0.963, and no query over 0.3 ms was
  slower by more than 3.0%. The smallest queries showed more. `values-star` (0.018 ms) read +29.9%
  and +24.4%, and `types-grouped` +7.7% on the client. A rerun of the six smallest
  queries with 300 requests and six rounds read +12.3% for `values-star`, +8.2% for
  `star-lookup` and +5.5% for `types-grouped` on the server, and `star-lookup` had read
  -6.2% in the first run. The spread between server processes of one binary is 60 to 150%
  for these queries, larger than the effect, so the screen cannot settle the 2% gate for
  queries under 0.3 ms. That needs a quiet Intel host.

  The gate ran on forge, an Intel Core i5-13500, on 2026-10-18. The comparison that the
  earlier plan named did not isolate the pool. The commit before this phase, 899fb251,
  against the fixed build 4d174d02 on one shared store read 1.074 on `employee-docs`,
  1.066 on `star-lookup` and 1.056 on `values-star`, while 4d174d02 with the pool off
  against the same build with the pool on stayed within noise. Profiles put the extra 3,300
  instructions of a `values-star` request in the scans, the index join, reallocation and
  the clock, outside the pool, and roadmap item 88 covers them. A comparison of 899fb251
  with the current build also includes the later work of F14 and X07. The harness had two
  effects of its own larger than 2%. Separate stores for A and B moved queries of a few
  microseconds by 5 to 14%, and the second position of each A, B, B, A round ran 1 to 5%
  faster.

  On 2026-10-18 the user decided that the gate compares the shipped build with the pool on
  against the same build with `--total-query-memory-mb 0`. Both read one shared store, the comparison
  runs in both orders at 20 rounds of 300 requests with one Rayon thread, and
  `scripts/x08-gates/combine.py` combines the two orders to cancel the position bias. A
  query passes when the upper end of its 95% interval is at most 1.02 or its median added
  time is at most 0.5 µs. Repeated runs of one comparison differed by up to 4% on queries
  under 15 µs, more than a bound of 1.02 can resolve, so the absolute bound covers them.

  The fast account charged the grant at every growth of its eager estimate, three or four
  times for a small query. It now takes from the grant in steps of 64 KiB, so a plain query
  can hold up to 64 KiB of the pool beyond its estimate. In a loop inside one process this
  lowered the grant's share of the execution time from 3.8% to 1.3% on `count-all`, from
  6.6% to 1.4% on `distinct-obj` and from 1.2% to 0.6% on `values-star`. With it, every
  query passes. `scripts/x08-gates/latency.sh` reproduces the run.

  | Query | Pool off | Pool on against off | 95% interval | Added time |
  |---|--:|--:|---|--:|
  | `distinct-obj` | 3.2 µs | 1.019 | 1.006 to 1.029 | 0.06 µs |
  | `count-all` | 3.5 µs | 1.015 | 1.002 to 1.026 | 0.05 µs |
  | `predicate-counts` | 3.7 µs | 1.032 | 1.020 to 1.039 | 0.12 µs |
  | `types-grouped` | 6.4 µs | 1.053 | 1.041 to 1.068 | 0.34 µs |
  | `path-plus` | 8.6 µs | 0.969 | 0.956 to 0.978 | −0.27 µs |
  | `values-star` | 13.5 µs | 1.034 | 1.027 to 1.045 | 0.45 µs |
  | `employee-docs` | 31.0 µs | 1.013 | 1.007 to 1.019 | 0.42 µs |
  | `star-lookup` | 57.4 µs | 1.008 | 1.001 to 1.013 | 0.46 µs |
  | `subquery-agg` | 190.3 µs | 1.002 | 0.999 to 1.004 | 0.32 µs |
  | `minus` | 280.8 µs | 1.008 | 1.005 to 1.010 | 2.20 µs |

  `path-plus` reads faster with the pool on, which shows the size of the differences
  between runs that remain after the bias is cancelled. The Namespace screen of the step
  and the allocator fix against 5b2085bc gave a geometric mean of 0.978 over 27 queries
  with the same answers, and no query read above 1.062.

### What remains

* Roadmap item 88 takes the allocator overhead of cached bytes, which the amended A1
  bound allows for. Freed cache pages stay in the heaps of the tokio and Rayon threads, and the
  heap behind the result cache is about 1.2 times the bytes it counts. The idle release
  could collect on the tokio workers as well, or the caches could charge the sizes that
  mimalloc reports. The item also covers the 3% of extra instructions on queries of a few
  microseconds from 899fb251 to 4d174d02, which profiles did not trace to one cause.
* **X08-P1-grant-routing.** Admit MCP tool calls and the other requests that never queue,
  and route the admitted grant into every route that counts as a query.
* **X08-P1-nested-service.** Recognise a SERVICE call to the same server, without a header
  that a client could forge, so that it does not wait behind its caller's grant. Today it
  answers `503` after the admission wait, and `SILENT` turns that into empty results.
* **X08-P1-admit-after-body.** Admit a POST query or update after its body is read under
  the existing body limit, give uploads and Graph Store writes no 16 MiB initial grant,
  and wake only the waiters that fit when memory returns. Today admission happens before
  the body is read, and the server sets no body read timeout, so a slow client holds its
  grant while it sends. Every release wakes every waiter.
* **X08-P1-retained-results.** Done on 2026-10-23 for SELECT and ASK, as the section
  below describes. A CONSTRUCT or DESCRIBE result still keeps its query's peak estimate,
  because its decoded triples are not measured when the query finishes.
* A lock-free `try_grant` when no request waits, measured with the latency gate above.
  Admission takes the queue's lock on every request.
* The 10.5M and DBpedia runs of the profile gate in §3.3, warm and cold, which the Phase
  1a gate requires. Only the 1.05M screen ran, and roadmap item 1 runs them on forge with
  its benchmark refresh. The first batch of that refresh, on 2026-10-18 and 2026-10-19
  with commit 81478fbe, ran the 10.5M suite with the default profile only, so it does not
  decide the gate. In the same batch, SP²Bench at 10M triples met the default profile's
  query budget of 8,192 MiB and pool of 5,344 MiB. Sparkles answered `q4` with 503 and
  `q5a`, `q6` and `q12a` with 507 within 0.3 s, and its access log gives the outcome
  `budget` for all four.
* A settings-file profile (C19) and the `small` default for the bindings (§8.2).
* Admitting SHACL-SPARQL, reasoning and ShEx through the pool, an initial grant that
  follows `memory-mb`, and the JVM option.

### Retained eager results (2026-10-23)

An eager context's contribution to its grant used to stay at its highest admitted
estimate until its last owner dropped. That estimate includes the build tables, the
intermediate results and the deduplication state of the query, so a held `QueryResult`
or a Python or Node result object reserved its query's peak long after the query
finished. The result cache stores plain columns without a context, so it was not
affected. A DISTINCT of a million-row product with a thousand rows
of answer kept about 40 MB of the pool while its result was alive.

When an eager SELECT or ASK finishes, `Ctx::shrink_retained` now lowers the contribution
to what the result keeps. That is the capacity its columns have allocated, which a
filter that compacted them in place leaves above their length, the terms computed into
the query's local vocabulary, its cached decoded values with their strings, the memos of
its blank nodes and every live charge. Each map entry counts its key, its strings and 48
bytes for its slot. `QueryMemoryLedger::shrink_eager` lowers the primary account to the
higher of its live bytes and that estimate, lowers every other account to its live bytes,
and returns the excess to each account's own grant outside the ledger's lock. A cursor
ledger keeps no eager contribution and a ledger with a running algorithm call keeps its
contribution, so both are left alone. A later charge on the context grows its
contribution again as before. A fast account whose contribution is within 1 MiB of the
retained estimate is left alone, so a small query keeps its lock-free account and takes
no ledger. The grant and the fast account are read first, so a query without a pool, or
whose contribution is within 1 MiB of its columns, takes none of the caches' locks. A larger one is handed to the ledger first, which makes the shrink and any
concurrent charge take the same lock. A context that already has a ledger, such as one
that called a graph algorithm, always shrinks.

The computed terms, cached values and blank-node memos were never charged in eager
execution, and the peak estimate covered them only by accident. Counting them keeps the
contribution from falling below them after the shrink, but a contribution that was
already below them stays where it was, as before. A CONSTRUCT or DESCRIBE result keeps its peak estimate, because its decoded
triples and quads are not measured when the query finishes.

`a_finished_eager_result_retains_only_what_it_holds` holds the thousand-row DISTINCT under
2 MiB of the pool where it held 40 MB, and checks that a million-row result keeps at least
its rows. `a_retained_small_result_leaves_its_peak_to_other_queries` runs that DISTINCT
twice in a pool that fits one peak and a half while the first result is alive, which
failed with `server-memory` before. `shrinking_a_finished_eager_query_keeps_live_and_result_bytes`
covers two accounts on different grants, live charges and a cursor ledger. A review
found that the first version counted only the rows of the result table and none of the
blank-node memos. `a_compacted_result_keeps_its_column_capacity_covered` holds two rows
whose filtered columns keep 2.4 MB of capacity, which the first version shrank to
112 bytes, and `a_result_keeps_its_blank_node_memo_covered` holds a COUNT over 100,000
`BNODE("x")` memo entries, which it shrank to 123 bytes.

### Phase 2a: batch kernels (2026-10-17)

The kernels of §5 replaced the cursor's per-row paths. They did not land behind an
`Optimizations` switch, as §5.3 planned, because every gated shape measured as fast or
faster and the old paths had no remaining use.

* The merge join (`cursor/merge.rs`) finds each left key's place in the right batch with
  the branch-free multi-lane `zipper::lower_bounds`, collects (left, right) row pairs and
  gathers each output column once. Semi, anti and MINUS joins build a keep mask and
  compact branch-free. A count over the join adds the run lengths of equal right keys
  without branching. That count is what X05 kept eager on AMD hosts, and it now measures
  0.79 to 0.88 times eager execution there.
* The hash join (`cursor/binary.rs`) groups build rows by key into one row vector with a
  map from key to start and count, sized before it is built, and probes a batch in two
  passes as §5.1 describes. An OPTIONAL with an expression evaluates it once per chunk of
  candidate pairs. The pre-sized map costs up to 18 bytes per build row when keys repeat.
* The grouping (`cursor/group.rs`) computes a slot per row of a batch and updates each
  aggregate column-wise. COUNT, SUM and AVG take typed loops, and the other aggregates keep
  the per-row path. A group's charge follows its layout. It had been charged about
  1.3 KiB, so 833,177 groups reported 1,092 MB where eager grouping reports 130 MB.
* DISTINCT keeps only the last row over input ordered on every output column. Otherwise
  it keeps one flat arena of rows behind a `hashbrown` table, or a set of ids for one
  column, and reserves room for a whole batch before it grows.

The §5.3 gate asked for streaming within 10% of eager on `optional-chain`, `group-avg`,
`minus`, `expr-bind-group` and `distinct-join` at 1.05M and 10.5M. On an 8-vCPU AMD EPYC
Namespace instance, reading every row with eager and cursor runs interleaved in one
process, the ratios of streaming to eager at 1.05M with one thread, 10.5M with one and
10.5M with eight were 0.90, 0.61 and 0.71 for `group-avg`, 0.88, 0.82 and 0.83 for
`minus`, 0.70, 0.66 and 0.78 for `expr-bind-group`, and 1.09, 0.95 and 0.99 for
`distinct-join`. `optional-chain` read 1.00, 1.33 and 0.92, and an earlier run on another
instance read 0.72 at 10.5M with one thread, so it is within the gate in two runs of three
and noisy in the third. [BENCHMARKS.md](../BENCHMARKS.md#batch-kernels-and-disk-spill)
has the full table. The second half of §5.3, no other query slower by more than the tie
threshold, is checked by the screen in the gate below.

Automatic execution widened with these measurements. It streams a grouping, under any of
Project, Slice, BIND, FILTER and ORDER BY, whose input is estimated at 20,000 rows or more
and whose operators all have cursor implementations, because every grouped query of the
screen measured 0.50 to 0.91 times eager execution at every size and thread count. Joins
without a grouping stay eager, since `star-join`, `regex-iri`, `x-join-out` and
`x-hash-join` measured 1.0 to 1.3. The decision table is
`automatic_execution_streams_groupings_over_large_inputs` in `cursor.rs`.

### Phase 2b: disk spill (2026-10-17)

`crate::spill` holds the files, and the sort, grouping and DISTINCT use them. A query
spills only when `QueryOptions::spill` holds a `Spill`, which `serve --spill on` opens
under `--spill-dir`. The tests are `tests/spill_sort.rs`, `tests/spill_group.rs`, the
unit tests of `spill.rs` and `http::budgets_tests`.

**Files.** §6.6 is built as written. A process directory `<boot-id>-<pid>-<random>` with
mode 0700 holds the flocked `SPARKLES-SPILL` marker. Files are `O_TMPFILE` or unlinked
at creation, so dropping an operator on completion, error, cancellation, panic or close
frees its space, and `Spill::open` sweeps directories of dead processes and leaves
anything else with a warning. The format is registered in `formats.rs` as the derived
kind `spill`, version 1. Frames hold at most 4,096 rows, and a sort writes smaller ones
when memory is short. Reservation takes 64 MiB extents from `--spill-max-mb`, checks
`--min-free-disk-mb` with `statvfs`, and fails with the new budget kind `spill` (`507`),
as do `ENOSPC`, `EDQUOT` and `EFBIG` on a write or when a file is created. Another I/O
error fails the query with that error. The extents are accounting, not preallocation, and
a query gives back the extents its open files no longer need whenever one of its files
closes. The process locks its marker before it writes the format into it, so a sweep by a
process starting at the same time never takes the directory for a dead one. The spill
directory is created with mode 0700 and refused unless the server's user owns it and no
other user can write it. A failed start removes its process directory, and a sweep also
removes a process directory whose marker is missing or empty once it is a minute old.
`serve --spill on` fails to start when the directory cannot be used or sits on tmpfs or
ramfs, where spilled state would take memory, and warns on a network file system.

**External sort.** The decisions below depart from §6.4.

* A run is written when the buffered input's sort would take half of the room the query
  had when the sort started, and when a charge for the buffer fails. Runs are sorted in
  pieces that the room left at that moment should hold, and a piece that still fails its
  budget is tried again at half its size while the input stays buffered. Estimates of
  the in-memory sort's footprint proved unreliable, and retrying a piece is not.
* Runs store ids only. The merge decodes each frame's keys into `SortKey`s as it reads
  the frame, instead of storing sort classes in the run, which keeps the format to plain
  id columns. A key that is a pure expression is evaluated again on each frame, so its
  computed values are never interned. A key that calls `RAND`, `BNODE`, `UUID` or
  `STRUUID` is evaluated once into a hidden column that the run keeps, so the merge
  compares what the run was sorted by. Those computed terms stay interned for the query,
  so such a sort does not shed their memory.
* Equal keys keep input order without sequence numbers. A run holds earlier input than
  every later run, the in-memory sort of an ORDER BY is stable, and the merge takes the
  earliest run on a tie. The id sort that feeds a merge join sorts its runs unstably,
  since only its sort variables carry an order. A pass that merges the first runs puts its output first.
* A run's frames are sized so that 64 of them share a quarter of the starting room, at
  least 64 rows, which replaces the fixed 256 KiB read buffers. The fan-in is the number
  of such frames that half the room holds, at most 64, and more runs merge in passes.

**Grouping and DISTINCT.** The decisions below depart from §6.5.

* A grouping does not write partial states. Once its groups hold half the starting room,
  or the room left falls under a quarter of it, or a new group's charge fails, the pass
  takes no new groups. Rows of the groups it holds are still aggregated, and rows of other
  groups go in input order to one of 16 files by a seeded hash of their keys. After the
  pass's groups are output and freed, each file is grouped by a pass of its own, which
  partitions again with the next seed. Every row of a group reaches the same pass in input
  order, so every admitted aggregate, `GROUP_CONCAT` order, DISTINCT and SAMPLE included,
  needs no merge rule, and each pass takes at least one group. The cost is that rows of
  passed groups are written whole rather than as partial states.
* A grouping without keys never spills, since it holds one group. Growth of a held group's
  state, such as a long `GROUP_CONCAT`, still fails at the budget.
* DISTINCT spills the same way with a hash of the whole row, and only over input with no
  order, because rows from the files come after the others. Input ordered on every output
  column needs no memory. Input ordered on some columns keeps its in-memory set, so that
  the order the cursor claims for its output holds. In `SELECT DISTINCT … ORDER BY` the
  DISTINCT sits above the sort, as SPARQL's algebra places it, so it also keeps its whole
  set in memory and fails with the `memory` budget when the set does not fit. The same
  holds above a ranking such as a text search, and when the ordered rows reach the
  DISTINCT through a join, OPTIONAL, MINUS, EXISTS, LATERAL or a path that passes an
  input's order on. The tests are `distinct_over_order_by_keeps_the_order_or_fails` and
  `distinct_over_a_join_of_an_ordered_subquery_keeps_the_order_or_fails`. Item 85 moved
  the DISTINCT of `SELECT DISTINCT … ORDER BY` below the sort, as the next section
  describes.

**Eager queries.** Both paths of §6.8 are built. With spill, automatic execution streams a
SELECT whose operators all have cursor implementations when the largest operator output
the plan estimates would take more than half of the memory left. An eager SELECT that
fails its `memory` budget runs once more as a cursor within the first run's deadline,
unless it calls SERVICE or a graph algorithm anywhere, inside an EXISTS included, or has
extension functions or initial bindings. The cursor plan counts `eagerMemoryRetry`, and
`/$/metrics` counts retries in `sparkles_spill_eager_retries_total`. The access log does
not record the retry separately.

**Not built here.** Hash-join spill (§6.3), the merge-join run spool and `--spill
encrypted` (§6.7) came with item 85, described in the next section. Spill stays off by
default until every A8 to A14 gate passes, as section 9 requires. The `NeedsSpill` signal of §4.1 is not
needed, since an operator switches on its own budget error or threshold. Spill for an
update's WHERE clause (open question 3) is not offered, because updates run eagerly.

**Gates.**

* **A8 to A10, A14.** Item 85 supplies these fixtures, as the next section records.
* **A11.** `spilled_order_by_matches_eager_order_with_ties_and_expressions` sorts 60,000
  rows of integers, strings, language strings, IRIs, decimals and dates, with OPTIONAL
  unbound values, under 768 KiB, and a total order matches eager row for row.
  `many_runs_merge_in_passes_and_report_them_in_the_plan` sorts 150,000 rows in more than
  one merge pass. At 10.5M on Namespace, a sort of 1.02M rows under 64 MiB held 45 MB and
  took 0.85 times the unlimited eager time at one thread and 2.24 times at eight. The
  20-million-row run of A11 was not made.
* **A12.** `spilled_groupings_match_eager_aggregates` groups 60,000 rows into 20,000
  groups with COUNT, SUM, MIN, MAX, AVG of an expression, SAMPLE, GROUP_CONCAT,
  COUNT(DISTINCT) and COUNT(DISTINCT *), and two keys with an OPTIONAL, under 4 MiB, and
  matches eager. `spilled_distinct_emits_each_row_once` covers DISTINCT under 256 KiB. At
  10.5M on Namespace, 833,177 groups held 38 MB under 64 MiB and took 1.54 times the eager
  time. The 10-million-group run was not made.
* **A13.** The tests are `spill_limits_fail_the_query_with_a_spill_budget` (reservation),
  the unit tests of `spill.rs` (injected write, read and reservation faults, an unknown
  version, kind, codec or encryption flag, a damaged frame, and the sweep of dead, live
  and foreign directories),
  `cancelled_spilled_sorts_remove_their_files` and
  `spilled_sorts_remove_their_files_when_dropped_early`. Files are nameless from creation,
  so a SIGKILL leaves only the empty directory that the next start removes, which the
  sweep test checks with a directory whose lock no process holds. A deadline during a
  merge takes the same path as cancellation.

* **Screen.** `mise run bench:nsc` of 0a81011c against 4391b74d, which has the kernels,
  spill and the wider automatic execution, ran 27 queries in four A,B,B,A rounds on an
  8-vCPU AMD EPYC instance. Both returned the same answers. At 1.05M the geometric mean
  of the server times was 1.012 and of the wall times 1.007, and no query was 1.06 times
  slower or more. At 10.5M the means were 0.972 and 0.957, `expr-agg-arg` fell to 0.51
  and `group-avg` to 0.83, and the slowest ratio was 1.057 for `subquery-agg`, whose
  servers spread by 34 to 40% between processes. No query reached the 1.10 flag, so no
  query under 0.3 ms needed a check on forge. The commits after 4391b74d change the
  charge of a group and the documentation.

### Phase 2b, item 85 (2026-10-18)

Item 85 added hash-join spill, the merge-join spool, a DISTINCT below its ORDER BY and
encrypted spill. These are enabled with `serve --spill on` or `encrypted`. The code is in
`sparql/cursor/joinspill.rs`, `binary.rs`, `merge.rs`, `distinct.rs` and `spill.rs`. The
tests are `tests/spill_join.rs`, the unit tests of `joinspill.rs`, `merge.rs` and
`spill.rs`, and `distinct_below_order_by_spills_and_keeps_the_order` in
`tests/spill_group.rs`.

**Hash join.** The decisions below depart from §6.3.

* A join that may spill stops reading its build input into memory once the input holds
  half of the room the query had when the build started, or when a charge for it fails,
  or when the room left would no longer cover what the input took to make its last
  batch. The last rule measures the rise of the query's peak while the input makes a
  batch, since a deep input such as a chain of merge joins holds several batches of its
  own and otherwise ran out of memory before the join could spill. These proactive
  thresholds apply only after a build row has a bound shared key. Disconnected builds
  and builds with every shared key unbound continue until a real memory charge fails.
* Partition 0 is not kept in memory, so every partition takes the same path. The hybrid
  join of §6.3 would save writing one partition, but it would also have to share the
  room with the partition writers and the probe input, which made the budgets of the
  first version fail under small limits.
* The join plans with half of the room it will have once its build rows are on disk. The
  fan-out is the estimated build bytes over half of that, as a power of two between 8 and
  64, and it halves while the writers' frames, of at least 64 rows each, would take more
  than a quarter of it. Frames replace the fixed 256 KiB buffers of §6.3.
* Each level picks a row's partition from the high bits of the key mixed with the
  level's seed through the SplitMix64 finalizer. The first version used a seeded FxHash,
  whose high bits barely change with the seed, so a partition's rows stayed together at
  the next level. At 10.5M a join under 4 MiB then went to the last level and read 856
  chunks with no heavy key, and the unit test `partitioning_again_divides_a_partition`
  now checks the spread. The spilled grouping and DISTINCT of item 7 had the same hash
  and now mix it the same way.
* A partition whose build rows exceed a chunk is partitioned again up to the third
  level. A partition that keeps nine tenths of its parent's rows holds a heavy key and
  goes straight to the block nested loop. A chunk holds half of the join's budget, or a
  quarter of the room left when that is more.
* Build rows whose key is unbound stay in memory, as §6.3 says, and join each partition
  in its first chunk. Probe rows whose key is unbound go to their own file, which is
  joined against every build file before the partitions' own jobs run. OPTIONAL, semi, anti and MINUS joins
  keep a bitmap of matched probe rows whenever a partition takes more than one chunk.
* §6.3 clears the cursor's order claims once a join spills. Claims are fixed when the
  cursor is built, before anything spills, so the build now passes down whether an
  operator above relies on an input's order. A projection, BIND or LIMIT passes on what
  its parent needs, the query root needs nothing, a hash join needs only its probe
  input's order and only when its own is needed, and every other operator is taken to
  need its inputs' order. A hash join whose order nothing relies on claims none when
  spill is on and emits its partitions in turn. One whose order is relied on numbers its
  probe rows, writes each partition's output in that order and merges the output files
  by number, up to 64 at a time, which restores the probe order at the cost of writing
  the output once more. Output files consolidate during production with the
  memory-derived fan-in. Equal generations merge first, with smallest-file merges
  bounding the total to 128 retained output files and one active writer. Reader
  frames, output buffers and temporary disk overlap are admitted before a merge.
  Consolidation reserves these buffers while the join is still live and can add
  intermediate writes. The earlier measurements do not measure this repair.

**Merge join.** A run of equal keys that outgrows a quarter of the room left, at least
16 KiB, or whose charge fails, is spooled to a file. The pair of runs is then joined
left-major, reading the right run again for each left row, so the output keeps the
merge join's order. Child schema widths are retained through EOF, so a spooled final
run from a nested join remains readable after its input buffer is released. A merge
cursor initializes its inputs before reserving an output batch, then limits its
emission capacity to loaded rows and current room for the output and zipper scratch.
The caller's batch size remains an upper bound, while child pulls keep their original
cap. Intermediate buffers admit sort-order metadata before replacing its backing and
retain its actual capacity charge. The plan counts `spooledRuns`. Reader admission uses the largest frame actually written,
including encrypted files, rather than an average that can omit a long first frame.

**DISTINCT below ORDER BY.** §6.5 numbers rows and merges partitions by number. The
build instead rewrites `Distinct(Project(OrderBy(X)))` into
`Project(OrderBy(Distinct(Project(X) with a row number)))` when spill is on, the sort
has no LIMIT, and every sort key is a pure expression of projected variables without
EXISTS. The DISTINCT keeps the first number of each row, the sort orders ties by the
number, and the projection drops it. Equal projected rows have equal keys, so the
answer and its order are those that sorting first gives, and the DISTINCT spills as an
unordered one does. Other shapes keep the DISTINCT above the sort with its whole set in
memory, as before. A numbered DISTINCT that may partition its input claims no
intermediate order, leaving the enclosing sort to restore it. Its row arena is
charged during relocation even when the projected row has only one column or the
hash table has room and only the arena grows.

**Encryption.** §6.7 is built as written with `aws-lc-rs` 1.18.1, which the server
already links through rustls and which `sparkles-backup` pins. A file's random 256-bit
AES-GCM key is made at creation and held only in memory, and its bytes are cleared when
the key drops. The nonce is the file's number and the frame's number, and the
associated data binds the frame to its kind, file, place and header, so a frame that
was changed, moved to another place or file, reordered or cut short fails with "spill
frame: it fails authentication". The CRC covers the sealed bytes, so damage still reads
as a CRC error first. Spill format version 2 adds the encryption and the file number to
the header, and a reader refuses version 1 and an encryption it does not expect. Spill
files never outlive their process, so the version bump needs no migration.

The initial implementation enabled spill before the full A11 and A12 runs were
made. Review restored the off default required by section 9 until every A8 to A14
gate passes. `serve --spill on` and `--spill encrypted` enable spill and fail startup
if the directory cannot be used. An ordinary start leaves the spill directory
unopened. Item 93 records the full-scale runs and tracks the review of enabling spill
by default and remaining execution work.

**Gates.** The fixtures below check current behavior. The 10.5M figures in this list
are historical results from the initial implementation, before the review corrections.
Those runs used `examples/modeprobe` on an 8-vCPU Namespace instance, with each spilled
cursor's rows checked against eager execution by an order-free digest of the decoded
rows.

* **A8.** `spilled_hash_joins_match_eager_at_every_batch_size` joins a build input more
  than four times its 1 MiB budget at batch sizes 1, 7 and 4,096 and checks the plan's
  partitions, levels and bytes. The unit test of the join covers batch size 1 in each
  join mode. At 10.5M a join of `foaf:knows` over a build input of a million rows,
  whose unlimited cursor holds 90.8 MB or 23 times the 4 MiB budget, returned the 2,497,620 eager rows within the budget after
  writing 157 MB over four levels.
* **A9.** `spilled_optional_and_minus_handle_unbound_keys` runs OPTIONAL with a filter
  and MINUS where one build row and one probe row in twenty leave the key unbound, under
  768 KiB and 1 MiB, and matches eager. At 10.5M an OPTIONAL with a filter and a MINUS
  over a million build rows matched eager under 4 MiB. At that scale an unbound probe key
  meets every build row, so a 5% rate of unbound keys was not repeated there.
* **A10.** `a_heavy_key_joins_as_a_block_nested_loop` joins inputs where one key holds
  nine tenths of the rows within 320 KiB through more than one chunk. At 10.5M the
  Namespace run joined 6,000 rows of 16 columns on each side, nine tenths on one key,
  into 29,170,859 rows within 1 MiB, in five chunks and 681 ms against 631 ms eager.
* **A14.** `encrypted_spill_files_hold_no_plain_bytes` reads the files of a running
  encrypted join through `/proc/self/fd` and finds none of the inline ids of the input
  and none of its 16-byte stretches. The unit tests of `spill.rs` refuse tampered,
  reordered, moved and truncated encrypted frames.
* **A13 for joins.** `failures_in_a_spilled_join_leave_nothing_behind` cancels a
  spilled join, drops it early, lets it time out, exhausts its spill budget and injects
  reservation, write and read faults, and each leaves no file and no reservation.

The W3C cursor harness accepts `SPARKLES_W3C_SPILL=plain` or `encrypted` to run its
query passes with a 1 MiB budget and a private spill directory per query. It checks
batch sizes 1, 2, 3 and 4,096 against the suite's expected answers. Eager passes keep
their ordinary options. This configuration enables spill during conformance, while
the integration fixtures above force physical spill.

**Measurements.** The following values precede the review corrections and are retained
as historical results. They are medians of three rounds with the instance's default
threads under `mimalloc`, beside the unlimited cursor and eager execution of the same
query. They do not measure bounded output consolidation or the later buffer admission
and metadata accounting repairs.

| Query at 10.5M | Eager | Cursor in memory | Spilled under 16 MiB | Spilled under 4 MiB | Written under 4 MiB |
|---|---|---|---|---|---|
| Join on `foaf:knows`, order relied on | 66 ms | 150 ms, 90.8 MB | 322 ms | 1,333 ms | 157 MB |
| OPTIONAL with a filter | 34 ms | 57 ms, 61.8 MB | 77 ms | 186 ms | 24.6 MB |
| MINUS | 21 ms | 36 ms, 61.8 MB | 59 ms | 165 ms | 24.6 MB |
| `SELECT DISTINCT … ORDER BY` | 615 ms | 569 ms, 1,016 MB | 1,421 ms | 3,028 ms | 60.1 MB |

The cursor's memory ledger stayed within each budget. Spilled joins wrote 95 to 150 MB
a second of compressed frames, which the encoding and the hashing bound rather than the
disk. Encryption added 1 to 6% to the bytes written, for the tags. It changed the times
under 4 MiB by −2% to 21%, most of them under 10%. Two runs under 16 MiB were 66% and
79% slower encrypted, but the eager run beside one of them slowed by 73% as well, so the
instance was noisy there. Process RSS is not a useful measure here, since the block cache of a 10.5M store
dominates it in both kinds of run.

A spilled join whose order is relied on writes its output twice. In the historical
measurements above, this made the `foaf:knows` join 8.9 times slower than in memory
under 4 MiB, where the OPTIONAL and MINUS joins took 3.3 and 4.6 times. A DISTINCT
over ordered input other than an ORDER BY keeps its whole set. Item 93 tracks these
execution limits, the performance investigation and the review of enabling spill
by default. Default enablement requires every A8 to A14 gate. The profile runs at 10.5M and on
DBpedia remain under item 1.

### Full-scale spill acceptance, item 93 (2026-10-21)

The manual `spill_acceptance` example now covers the full A11 and A12 cardinalities
in both plain and encrypted modes. The A11 sort processes 20 million rows with mixed
numbers, strings, dates with and without time zones and unbound keys under 64 MiB.
It matches every position of the unlimited sort and checks input-order stability
for ties on identical RDF terms.

The A12 matrix processes 10 million groups from 40 million input rows under 64 MiB.
It covers COUNT, SUM, AVG, MIN, MAX, SAMPLE, GROUP_CONCAT and COUNT(DISTINCT) both
separately and in one query containing all eight aggregates. Independent formulas
check exact RDF datatypes and numeric values, and a dense group-identity check
requires every group exactly once. SAMPLE matches the eager choice and its allowed
value set. GROUP_CONCAT matches the eager ordering and exact string spelling of
every component. Ordered DISTINCT checks its entire independently derived sequence
after both DISTINCT and ORDER BY physically spill.

All 22 plain and encrypted configurations passed. The bounded cursors finish
without eager fallback, their operator counts show the required full inputs and
outputs, and their charged query peaks stay within 64 MiB. Physical spill counters
are positive, reservations return to zero and query files are removed after close.
The tests release each unlimited reference before running its bounded cursor.
Process peak RSS includes that earlier reference and is separate from the query's
memory ledger. The fixture and oracle tests also reject wrong identities, RDF
datatypes, aggregate values, missing or extra columns, choice changes and malformed
GROUP_CONCAT spellings.

The earlier 10.5M tables remain historical measurements from before the reviewed
corrections. Spill remains opt-in. Item 93 retains the execution follow-ups,
performance investigation and review of enabling spill by default.

Completed native hash joins release their hash index, input buffers and spill readers
when they reach EOF. Their plans keep final spill counters and completion metadata,
and already returned batches keep their own storage. This does not resolve the known
large spill-pass count when a grouping consumes ordered spilled join output.
