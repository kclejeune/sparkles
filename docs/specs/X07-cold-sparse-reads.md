# X07: Cold sparse reads

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 is per-query I/O accounting and a profile of every
> cold loss in BENCHMARKS.md. Phase 2 adds batched resolution of a query's constants,
> plan-time prefetch and a read policy chosen per scan instead of per process. Phase 3
> adds a segmented block layout that lets a point lookup read and decode part of a
> block. Phase 4 is an optional prewarm of the pages a server used before it stopped.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

This design was written from the Sparkles code (`crates/sparkles-core/src/index.rs`,
`vocab.rs`, `sparql/indexjoin.rs`, `sparql/results.rs`, `sparql/ctx.rs`,
`store/partial.rs`), the cold-start section and the loss table of
[BENCHMARKS.md](../BENCHMARKS.md#cold-starts), the Linux `madvise(2)`, `mincore(2)`,
`getrusage(2)` and `proc(5)` manual pages, the PAX paper by Ailamaki et al. on the
layout of columns inside a page, the RocksDB and PostgreSQL documentation on block
sizes and on prewarming, and the specs [X01](X01-compression-codecs.md),
[X05](X05-streaming-query-execution.md), [C13](C13-automatic-compaction.md) and
[X06](X06-benchmark-coverage.md). The sources are listed in §14.

## 1. Summary, goals, non-goals

After a restart with an empty page cache, Sparkles answers small lookups slower than
Oxigraph. At 10.5M triples, cold medians from [BENCHMARKS.md](../BENCHMARKS.md#cold-starts)
are:

| Query, cold, 10.5M | Sparkles (ms) | Oxigraph (ms) | Ratio |
|---|---:|---:|---:|
| `star-lookup` | 47.8 | 15.7 | 3.05× |
| `values-star` | 12.9 | 4.8 | 2.71× |
| `employee-docs` | 29.3 | 12.8 | 2.29× |
| `path-plus` | 5.3 | 1.9 | 2.77× |

At 1.05M the same queries lose 1.64× to 3.83×, `employee-docs` to Fluree, and Fluree
also wins four small cold scans at 1.05M (`count-all`, `star-join`, `contains` and
`lang-filter`) by 1.15× to 2.64×. Warm, Sparkles wins all of
these. The losses come from the first reads of index blocks and vocabulary data and from
decoding them. Several features already limit those reads (§2). The remaining cost is
not yet measured per query, and the existing switches trade sparse speed against dense
speed for the whole process.

**Goals**

1. Measure, per query, what a cold run reads and decodes, by file and by phase, and
   publish the breakdown for every cold loss.
2. Bring the four cold losses above within 10% of Oxigraph at 10.5M and 1.05M (§9).
3. Remove the process-wide tradeoff between sparse and dense reads: each scan gets the
   read policy that suits it, so that the 1.01B rows of the switch table in
   BENCHMARKS.md are each at least as fast as the better of its two columns.
4. Keep warm performance, index size and load time within 5% of today.

**Non-goals**

- Replacing memory mapping with a buffer pool. Mapping stays; §12 explains.
- Changes to the delta, the WAL or the full-text, vector and spatial indexes.
- Cold performance of a store larger than RAM under sustained load. That belongs to the
  scale work of X08 and X11.
- A new compression codec for index columns. X01 and X11 own codecs. This spec changes
  the unit of compression, not the codec.

## 2. Current read path

These are the parts of the code a cold lookup goes through.

**Permutation files.** `index.rs` writes each of the seven permutations as
`<perm>.dat` and `<perm>.meta`. A block holds up to `BLOCK_ROWS` = 32,768 rows. Each of
its four key columns is delta and zig-zag varint encoded and then compressed with LZ4 as
one frame (`encode_column`), and the columns follow each other in the file. Each block
has a 100-byte metadata record (`META_BYTES`: first and last key, offset, four column
lengths, row count, first row number). `PermIndex::open` reads every record into memory,
so a scan with a bound prefix finds its blocks without reading the data file. At 1.01B
quads the seven permutations have about 216,000 blocks and 21.6 MB of records.

**Decoding.** `BlockCache` keeps decoded columns keyed by permutation instance, block
and column. `get_cols` decodes only the columns in a `ColMask`
(`Optimizations::selective_columns`). `PermIndex::decode_col` asks the kernel for the
column's bytes and those of the block's later columns (`will_need`) and decompresses the
whole column into 32,768 values, even when the scan needs one row.

**Read hints.** Index and vocabulary data are mapped with `MADV_RANDOM` (`map_random`)
when `SPARKLES_IO_HINTS` is on, which is the default, so a page fault reads one page
rather than the device's read-ahead window. Readers then ask for the ranges they need
with `MADV_WILLNEED` in windows of 256 KiB (`will_need`).
`PermIndex::prefetch` asks for the columns of a block before it is decoded. An index join
with between 2 and 16,384 key ranges (`PREFETCH_RANGES`) prefetches all of their blocks
first (`indexjoin.rs`, `prefetch`), so the reads overlap.

**Vocabulary.** `vocab.dat` holds the terms front-coded in blocks of 16 keys
(`FC_BLOCK`), and `vocab.off` the offset of each block. The sparse index `vocab.idx`
holds, in memory, the first key and data offset of every 128th block (`IDX_BLOCKS`), so a
group covers 2,048 terms. `Vocab::find` locates the group from memory, asks for the
whole group's offsets and data once in the life of the mapping (`Sparse::first_hint`),
then binary-searches the group's blocks and scans one. Results of fewer than 256 rows
prefetch the exact front-coded blocks of their terms, and larger ones the groups
(`results.rs`, `prefetch_answer`). Numeric comparisons read 8-byte values from
`vocab.num`.

**Constants.** The planner resolves each constant of a query with `Ctx::intern_key`,
which calls `Snapshot::lookup_key`, one after another. On a cold server each lookup in a
new group waits for that group's read before the next constant is looked up.

**Switches.** `SPARKLES_IO_HINTS=off` and `SPARKLES_SPARSE_VOCAB=off` turn the hints and
the sparse index off for the process. BENCHMARKS.md shows that turning both on makes
sparse lookups 2.4× faster at 1.01B (`entity-facts-1`, 36.0–38.3 ms to 14.6–15.2 ms) and
makes `label-regex` (66.4–68.5 ms to 119.5–130.8 ms) and `export-1m` (1,231–1,239 ms to
1,883–1,956 ms) slower, because a dense pass over a random-access mapping loses the
kernel's automatic read-ahead.

## 3. Where the time is likely to go

These are hypotheses for Phase 1 to confirm or reject. None has been measured per query.

1. **Serial constant lookups.** `star-lookup` resolves `ex:worksFor`, `org/7`,
   `foaf:name`, `foaf:age` and `ex:salary`. Each can be in a different vocabulary group,
   and each group is read before the next lookup starts.
2. **Whole-column decodes for a few rows.** Each employee of `org/7` is a subject in a
   different place of SPO. Reading the employee's name, age and salary decodes columns
   of up to 32,768 rows and reads their compressed bytes, where Oxigraph reads a few
   small pages of its RocksDB tables.
3. **Reads after the plan is known.** Index blocks of the first scan are requested only
   when the scan starts, after all constants are resolved.
4. **Result terms.** `prefetch_answer` overlaps the result's term reads, but terms read
   during execution (FILTER on strings, ORDER BY on non-numeric values) are read one
   front-coded block at a time.
5. **Property paths.** `path-plus` expands a frontier per index sweep. Each sweep's few
   keys hit few blocks, so the join prefetch (which needs at least two ranges) may not
   apply, and each block is decoded whole.

## 4. Phase 1: I/O accounting

### 4.1 Counters

A query gets an `IoStats` record in its `Ctx`, filled by the readers:

| Counter | Where it is counted |
|---|---|
| Index columns decoded and their decoded bytes, per permutation | `BlockCache::get_cols` on a miss |
| Index bytes requested with `WILLNEED`, per file | `will_need`, `PermIndex::prefetch` |
| Vocabulary groups hinted, front-coded blocks scanned, `find` calls | `Vocab::find`, `scan_block`, `prefetch_*` |
| Numeric-column pages requested | `numeric::NumColumn::get_many` |
| Major page faults of the query's threads | `getrusage(RUSAGE_THREAD)` before and after each operator on the worker thread, summed |
| Time spent in vocabulary lookups, block decodes and waiting for faults | Timers around the same calls, in the profile mode only |

The counters cost a relaxed atomic add each and are always on. The timers and the fault
counts run only when the request asks for a profile.

### 4.2 Pages actually read

In the profile mode, the reader also records the mapped ranges it touched, and at the
end of the query calls `mincore(2)` on them to count the pages that are now resident.
Compared with a `mincore` taken before the query (on the same ranges, recorded lazily
at the first touch), this gives the pages the query brought in, by file. This is a
diagnostic: `mincore` on large ranges is not free, so it is never on in production.

### 4.3 Surfaces

- `application/x-sparkles+json` gains an `io` object in its metadata when the request
  has `profile=io`, next to `execMs`.
- `scripts/bench-regressions.py --cold --io` requests it for every sample and stores it
  in the record of [X06 §7](X06-benchmark-coverage.md#7-the-result-record).
- The process-wide counters are added to `/$/stats` and to the metrics as
  `sparkles_index_columns_decoded_total`, `sparkles_index_readahead_bytes_total` and
  `sparkles_vocab_groups_read_total`, by dataset.

### 4.4 Deliverable

A table in this spec's Outcome for each cold loss in BENCHMARKS.md at 1.05M and 10.5M:
the time in vocabulary lookups, index reads, decoding and result terms, the pages and
bytes read per file, and the columns decoded. Phases 2 and 3 are ordered by what this
table shows. A hypothesis of §3 that the table rejects removes the fix that depends on
it.

## 5. Phase 2: batching and policy

### 5.1 Batched constant resolution

Before the planner resolves any constant, it collects every IRI and literal key of the
algebra (patterns, VALUES, FILTER constants, graph names) and calls a new
`Vocab::prefetch_keys(&[&[u8]])`. With a sparse index, it finds each key's group in
memory and asks for every group not yet hinted, in one pass over sorted groups, then
returns. The lookups that follow find their pages arriving together. The delta
vocabulary is in memory and needs nothing. `Ctx::intern_key` is unchanged.

Without a sparse index (older generations, or `SPARKLES_SPARSE_VOCAB=off`), the batch
does nothing.

### 5.2 Plan-time prefetch

Once the join order is chosen, the executor knows the first scan's permutation and bound
prefix. It asks for that scan's blocks (`PermIndex::prefetch` over
`block_range(prefix)`, the columns of the scan's mask) before it builds the operators.
For a star joined on the first scan's subjects, the existing join prefetch then covers
the later scans. The lower bound of two ranges in `indexjoin.rs`'s `prefetch` stays for
the join, and the first scan's single range is covered by this step.

### 5.3 Read policy per scan

A new `ReadPolicy` is chosen per scan and per vocabulary pass:

| Policy | Chosen when | Index reads | Vocabulary reads |
|---|---|---|---|
| `Sparse` | The scan's estimated rows are less than an eighth of a block, or it is a join probe with fewer keys than blocks it touches | Request only the needed columns (or segments, §6) of each block | Request the exact front-coded blocks |
| `Dense` | An export, a full scan, a scan whose estimate covers most of its blocks, a regex or `CONTAINS` over a vocabulary range | Request ahead in a sliding window of 4 MiB per stream, refilled when half is consumed | Request ahead in the same window over `vocab.dat` and `vocab.off` |

The mappings stay `MADV_RANDOM`. A dense reader does its own read-ahead with
`MADV_WILLNEED`, so concurrent sparse and dense queries on the same file do not change
each other's mapping advice. The window size is swept in Phase 2 (1, 4 and 16 MiB) at
10.5M and 1.01B.

`Vocab::ahead_for` already turns read-ahead on for dense sorted id passes. The policy
moves that choice to the planner for the scans that know their density, and keeps
`ahead_for` as the fallback when no policy is given.

`SPARKLES_IO_HINTS` and `SPARKLES_SPARSE_VOCAB` remain for A/B runs. With the policy in
place, their documented tradeoff disappears, and BENCHMARKS.md's table gains a third
column, "per-scan policy".

### 5.4 Terms read during execution

An operator that needs the keys of many ids before it can continue (a string FILTER over
a column, an ORDER BY on strings, `GROUP_CONCAT`) calls `prefetch_terms` or
`prefetch_sorted` on the ids of its input batch before it decodes the first, in the way
`exec.rs` already does for some sorts. Each such call site gets a flag in
`Optimizations` so it can be measured on and off.

## 6. Phase 3: segmented blocks

### 6.1 Layout

A block keeps its 32,768 rows and its metadata record. Its data becomes a sequence of
*segments* of `SEG_ROWS` rows (1,024 proposed, swept from 512 to 4,096). Within a
segment, the four columns are encoded as today (delta and zig-zag varints, LZ4) and
stored one after another, so the rows of one segment are contiguous on disk, as in the
PAX layout. Deltas restart at each segment.

A new file `<perm>.seg` holds, for each block, a directory: per segment, its first key
(32 bytes) and the compressed length of each of its four columns (4 × 4 bytes). That is
48 bytes per segment and 1.5 KiB per full block. The file is mapped with `MADV_RANDOM`.
An offset table of one `u64` per block, at the start of `<perm>.seg`, locates each
block's directory. At 1.01B quads that table is 1.7 MB and is read into memory at open
with the metadata. The directories stay on disk.

The metadata record gains a format flag, so a generation written before this phase is
read as today and needs no rewrite. `sparkles check` verifies the directories against
the data. The builder (`PermWriter::flush_block`) writes segments when the generation
format is new, and `PermWriter::push_raw`, which partial compaction uses to copy
unchanged blocks (`store/partial.rs`), copies the block's directory with its bytes.

### 6.2 Reads

- **Point and short-range reads** (`Sparse` policy, and `count_between` at a block's
  edge): find the block in memory, read its directory (usually one page), find the
  segments that can hold the range by their first keys, request those segments' bytes,
  and decode only them. A lookup that hits one segment reads about 1/32 of a block.
- **Long ranges and full blocks** (`Dense` policy): request the block's whole byte range
  and decode all segments. The decoded result is the same `Block` of four columns.
- **Column masks.** A segment stores its columns separately, so a scan that needs two
  of four columns still decodes two. It reads all four columns' bytes of the segments
  it visits, since they are contiguous. Phase 3 measures whether that costs dense scans
  more than 5%. If it does, the dense path reads only the columns it needs, at the cost
  of one request per column per segment.

### 6.3 Cache

`BlockCache` keys become (permutation instance, block, column, segment), where a
segment index of `u16::MAX` means the whole column, so a full-block decode and a
segment decode never shadow each other. A full decode that finds some segments cached
decodes only the rest and assembles the column. The weigher is unchanged.

### 6.4 Costs to measure

- Index size: smaller LZ4 frames and restarted deltas compress worse. Target: no more
  than 5% growth at 10.5M and on DBpedia, directories included.
- Build time: no more than 5% slower loads.
- Warm decoding: a warm full-block scan decodes 32 frames instead of one per column.
  Target: within 5% on the warm suite.

## 7. Phase 4: optional prewarm

A server can record, at shutdown and every few minutes, which vocabulary groups and
index blocks its block cache and lookups used, as a list of ranges per file in the
dataset directory. With `--prewarm`, a start reads those ranges in the background after
it is ready, as PostgreSQL's `pg_prewarm` does with its auto-prewarm worker. It is off by
default. A cold benchmark runs with it off, and a separate row reports it on, with the
time to ready unchanged and the background read time stated.

## 8. Architecture

| Area | Files and types | Change |
|---|---|---|
| Accounting | `sparql/ctx.rs` (`Ctx`, `Optimizations`), `index.rs` (`BlockCache`, `will_need`), `vocab.rs` | `IoStats`, counters, profile mode |
| HTTP | the query handler and `application/x-sparkles+json` writer in `sparkles-server` | `profile=io` and the `io` metadata |
| Metrics | `sparkles-server/src/obs.rs` | Three counters by dataset |
| Batched constants | `vocab.rs` (`Vocab::prefetch_keys`, `Sparse`), the planner's entry in `sparql/mod.rs` | Collect and hint keys before resolution |
| Plan-time prefetch | `sparql/indexjoin.rs`, `sparql/exec.rs` | First-scan prefetch |
| Read policy | `index.rs` (`PermIndex` readers, `for_each_key_range_*`), `vocab.rs` (`get_sorted_with_ahead`, `ahead_for`), `sparql/plan.rs` estimates | `ReadPolicy` passed to readers, sliding window |
| Segments | `index.rs` (`PermWriter`, `PermIndex`, `BlockMeta`, `decode_col`), `builder`, `store/partial.rs`, `store/compaction.rs`, `check.rs` | `<perm>.seg`, segment decode, cache key |
| Prewarm | `store.rs` open and close, `sparkles-server` CLI | Range lists, background reader |

No public Rust API changes except new `Optimizations` fields and the `profile=io`
request parameter.

## 9. Targets

Measured with `scripts/bench.sh --mode cold` (median of 3 per query, as published), and
with `bench-regressions.py --cold` (7 samples) for the A/B gates, on `forge`.

| Query, cold | 1.05M target (ms) | 10.5M target (ms) | Basis |
|---|---:|---:|---|
| `star-lookup` | ≤ 16.4 | ≤ 17.3 | 1.1 × Oxigraph (14.9, 15.7) |
| `values-star` | ≤ 6.1 | ≤ 5.3 | 1.1 × Oxigraph (5.5, 4.8) |
| `employee-docs` | ≤ 11.2 | ≤ 14.1 | 1.1 × the faster engine (Fluree 10.2, Oxigraph 12.8) |
| `path-plus` | ≤ 2.1 | ≤ 2.1 | 1.1 × Oxigraph (1.9, 1.9) |

Other gates:

1. The 1.05M cold scans Fluree wins (`count-all`, `star-join`, `contains`,
   `lang-filter`) are reported. Matching Fluree on them is a stretch target, not a gate.
2. At 1.01B, `entity-facts-1` stays at or below 15.2 ms, `label-regex` at or below
   68.5 ms and `export-1m` at or below 1,239 ms, cold.
3. No cold query that Sparkles wins today becomes more than 5% slower.
4. Warm: the 28-query suite at 1.05M and 10.5M within 5% per query on the median
   (`scripts/nsc-bench.sh` for the screen, forge for the result).
5. Index size and load time within 5% at 10.5M and on DBpedia (Phase 3).

A competitor's cold figures are re-measured in the same refresh (X06 §8), so the targets
move with them.

## 10. Phases

| Phase | Content | Size | Gate |
|---|---|---|---|
| 1 | §4: counters, profile mode, metrics, the breakdown table | S | A1, A2 |
| 2 | §5: batched constants, first-scan prefetch, per-scan read policy, execution-time term prefetch | M | A3–A6 |
| 3 | §6: segmented blocks, cache keys, partial compaction, check | M–L | A7–A9 |
| 4 | §7: prewarm | S | A10 |

Phase 3 starts only if the Phase 2 results leave any of the four targets unmet and the
Phase 1 breakdown attributes the remainder to block reads or decoding.

## 11. Acceptance examples

- **A1 — Breakdown.** A cold `star-lookup` at 10.5M with `profile=io` reports the
  vocabulary groups read, the index columns decoded per permutation and the bytes
  requested per file, and the sum of the phase timers is within 10% of `execMs`.
- **A2 — Counter cost.** With the counters on and the profile mode off, the warm suite is
  within 2% of a build without them.
- **A3 — Batched constants.** A query with five constants in five different vocabulary
  groups issues its five group hints before the first `find`, and the cold time of the
  constant resolution falls by at least half against Phase 1 at 10.5M.
- **A4 — First-scan prefetch.** A cold `employee-docs` requests the first scan's blocks
  before its operators are built, visible in the profile's order of events.
- **A5 — Dense policy.** `export-1m` at 1.01B, cold, with hints on, takes no longer than
  with hints off (1,231–1,239 ms), and `entity-facts-1` stays at or below 15.2 ms in the
  same build.
- **A6 — Concurrent policies.** A dense export and 16 cold point lookups on the same
  store run together. Neither changes the other's mapping advice, and the lookups'
  median is within 10% of running alone.
- **A7 — Segment reads.** A cold point lookup on a segmented generation decodes one
  segment per column it needs, and reads less than a quarter of the block's bytes.
- **A8 — Format compatibility.** A store built before Phase 3 opens and answers the
  suite identically. After a compaction, the new generation is segmented, and `sparkles
  check` passes on both. A partial compaction copies segmented blocks with their
  directories.
- **A9 — Size and build.** Index size and load time stay within 5% at 10.5M and on
  DBpedia.
- **A10 — Prewarm.** With `--prewarm`, the server is ready as fast as without it, and a
  cold `star-lookup` issued 5 s after start runs at its warm time.

## 12. Rejected alternatives

- **Smaller blocks.** Cutting `BLOCK_ROWS` to 1,024 would give a point lookup the same
  read size as a segment, but the in-memory metadata grows with the block count: at
  1.01B quads, from 21.6 MB to about 690 MB. Segments keep the in-memory records per
  32,768 rows and move the finer directory to disk.
- **A buffer pool with `pread` instead of mapping.** It would give exact control of reads,
  but it means a second cache next to the page cache, copies, and a rewrite of every
  reader. The policy and segments get the read sizes right with the mapping kept.
- **`MADV_SEQUENTIAL` on ranges for dense scans.** Mapping advice is state of the
  mapping, shared by every query on the file. A dense query would change what a
  concurrent sparse query reads. Explicit `WILLNEED` windows have no shared state.
- **`MADV_NORMAL` by default and hints only for sparse reads.** This is the state before
  the hints were added, and BENCHMARKS.md measured it as slower for sparse lookups at
  every scale.
- **Turning the hints off when a query is estimated dense.** The switch is per process
  and per mapping. A per-scan policy reaches the same result without the switch.
- **Loading the whole vocabulary offset file into memory.** `vocab.off` is 8 bytes per
  16 terms. At 1.01B quads it is far larger than `vocab.idx`, and the sparse index
  already narrows a lookup to one range of it.
- **Prewarm on by default.** It reads data a restarted server may never need and hides
  the cold cost in benchmarks. It stays opt-in.

## 13. Open questions

1. The segment size. 1,024 rows is a guess from the 4 KiB page and the observed column
   sizes. Phase 3 sweeps it.
2. Whether `<perm>.seg` is a separate file or a footer in `<perm>.dat`. A separate file
   keeps old readers simple and partial compaction's copy straightforward.
3. Whether the sliding window should adapt to the device (larger on network block
   storage, X06 §10).
4. Whether the read policy belongs in the query plan explanation that C18 shows users.
5. Whether `getrusage(RUSAGE_THREAD)` is precise enough under the rayon pool, or the
   profile mode should run the query on one thread.

## 14. Sources

- A. Ailamaki, D. J. DeWitt, M. D. Hill and M. Skounakis, "Weaving Relations for Cache
  Performance", VLDB 2001, for the PAX layout of columns inside a page.
- The Linux manual pages `madvise(2)`, `mincore(2)`, `getrusage(2)` and `proc(5)`.
- The RocksDB documentation on block-based tables and block size, for the small read
  unit of an LSM store such as Oxigraph's.
- The PostgreSQL documentation of `pg_prewarm` and its auto-prewarm worker.
- [BENCHMARKS.md](../BENCHMARKS.md), sections "Cold starts" and "Where Sparkles loses".
- The Sparkles code named in §2 and §8, and the specs X01, X05, X06 and C13.

## Outcome

Not built. See [ROADMAP](../ROADMAP.md) item 4.
