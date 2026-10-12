# X08: Disk spill and server-wide memory

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 is server-wide cache sizing, a smaller memory
> profile and the server-wide query memory pool with admission control. Phase 2 adds
> batch-oriented hash join and grouping kernels in the cursor engine, then disk spill for
> hash joins, sorts, grouping and DISTINCT. Phase 3 reduces the idle server footprint,
> the Python process footprint and the size of the full-text index.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

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

Each `Ctx` gets an optional `PoolGrant`. A grant holds a number of bytes reserved from the
pool, in granules of 4 MiB. `Charge::add` today does one `fetch_update` on `mem_live`
against `mem_limit` (`ctx.rs`, `impl Charge`). With a pool, the check becomes:

1. Add to `mem_live` as now, against the query's own `mem_limit`.
2. If the new `mem_live` exceeds the grant, ask the pool for enough granules to cover it,
   with one compare-and-swap on the pool's granted total. Success raises the grant.
3. If the pool cannot cover it, undo the addition and fail with kind `server-memory`, or,
   when the caller is a spillable operator, return `Error::NeedsSpill` so the operator can
   spill (§6).

The fast path is unchanged while `mem_live` stays inside the grant, so small queries pay
one extra comparison. `Charge::drop` and `RetainedCharge` release bytes to `mem_live` only.
The grant shrinks back to the granule above `mem_live` when the query releases more than
16 MiB, and to zero when the `Ctx` drops. A `QueryBatch` that the caller retains after the
cursor closes keeps its share of the grant until it drops, as it keeps its charge today.

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
| 1b | The pool, admission, `/$/server/memory`, metrics, embedded catalog pools | A4–A7. No regression above 2% on single-client latency. |
| 2a | Batch hash join and grouping kernels | §5.3 |
| 2b | Spill for hash join, sort, grouping, DISTINCT; the eager retry | A8–A14. Spill off by default until every gate passes, then on. |
| 3 | Footprint items of §8 | A15–A17 |

## 10. Acceptance examples

* **A1 — One cache.** A server with ten datasets of 1.05M triples each, `--cache-mb 256`,
  runs the suite against each in turn. The block cache never holds more than 256 MiB in
  total, `sparkles_block_cache_bytes` summed over datasets equals the cache's weight, and
  RSS after the run is at most 1.1 times that of a server with one dataset.
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

Not built. See [ROADMAP](../ROADMAP.md) items 2, 7 and 40.
