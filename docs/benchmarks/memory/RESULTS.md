# Memory and disk results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Memory and disk

Sparkles uses more memory than QLever in exchange for speed, and less than Jena and
Oxigraph. The decoded-block cache, eager execution and mimalloc each trade memory for
speed, and each tradeoff is stated with its cost below.

### Server memory and index size

These are server resident sets (VmRSS, and VmHWM for peaks) and index sizes on disk from
the synthetic suite. Lower is better.

| 1.05M triples | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| RSS 2 s after start (MiB) | 66 | 251 | 38 | 48 | **23** |
| RSS after every query ran once (MiB) | 189 | 1092 | 172 | 2291 | **75** |
| RSS after the run (MiB) | 408 | 1882 | **310** | 3077 | 1231 |
| Peak RSS during throughput (MiB) | 567 | 1889 | **339** | 3077 | 1241 |
| Index size on disk | 28 MiB | 132 MiB | **26 MiB** | 37 MiB | 147 MiB |

| 10.5M triples | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| RSS 2 s after start (MiB) | 67 | 256 | 47 | 257 | **27** |
| RSS after every query ran once (MiB) | 668 | 3510 | 528 | — | **408** |
| RSS after the run (MiB) | **780** | 4923 | 795 | 2357 | 2074 |
| Peak RSS during throughput (MiB) | 1339 | 4930 | **939** | 2445 | 2074 |
| Index size on disk | 287 MiB | 1.3 GiB | **276 MiB** | 398 MiB | 1.4 GiB |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-18 and 2026-10-19.*

Fluree has no reading after the 10.5M answer check, because its server had been killed at
the memory limit. Of Sparkles' 408 and 780 MiB after the run, 63 and 379 MiB are the block
cache. Readings after the run vary between sessions, and four interleaved runs of two
builds in one session gave 402 to 432 MiB after the run and 545 to 582 MiB at the peak at
1.05M. At 10.5M the largest per-query rises in peak RSS are `order-by-full` at 241 MiB,
`two-hop-count` at 202 MiB, `star-join` at 175 MiB and `knows-reach` at 94 MiB, which
include cache growth as well as intermediate results.

| DBpedia, 1.24B lines | sparkles | qlever | fluree |
|---|---:|---:|---:|
| Server RSS after the run (MiB) | 4,480 | **1,426** | killed at 14 GiB |
| Index size on disk | **28.7 GiB** | 33.4 GiB | 69.7 GiB |

The DBpedia RSS figures do not measure the same memory. Sparkles maps its index, so the
clean file pages it touches count toward its RSS and the kernel can reclaim them. QLever
reads with `pread`, so the file cache does not count toward its RSS. The numeric column
adds 20.7 MB on disk and 4.3 MB of resident memory to the Sparkles index.

<a id="memory-and-the-speed-it-buys"></a>

### Memory and the speed it buys

**The decoded-block cache** keeps decompressed index blocks, up to 1 GiB per dataset by
default (`--cache-mb`). These runs measured the 10.5M suite with four cache sizes. The
first three memory columns come from a fresh-server probe that runs all 28 queries once
and then three rounds of 160 concurrent star joins.

| Block cache | Probe RSS after the queries | Probe peak RSS | Suite RSS after the run | Held by the cache | Throughput (q/s) | Sum of 28 query means |
|---|---:|---:|---:|---:|---:|---:|
| 0 MiB | 150 MiB | 614 MiB | 304 MiB | 0 MiB | 127 | 3,548 ms |
| 256 MiB | 577 MiB | 985 MiB | 713 MiB | 255 MiB | 237 | 1,056 ms |
| 1024 MiB (default) | 724 MiB | 1101 MiB | 851 MiB | 378 MiB | 234 | 1,058 ms |
| 4096 MiB | 724 MiB | 1121 MiB | 849 MiB | 378 MiB | 231 | 1,056 ms |

Turning the cache off saves 547 MiB of RSS after the run, below QLever's 653 MiB, and
costs about 46% of throughput and a 3.4× slower suite. A 256 MiB cache saves 138 MiB at
the same speed on this workload, which touches about 378 MiB of blocks. A larger cache
adds nothing here.

**The `small` memory profile** sets a 256 MiB block cache and smaller result, SERVICE and
history caches and query budgets. A prefetch request may decode up to a quarter of its
block cache in parallel, with a floor of 64 MiB. On forge, with the result cache off, the
profile's geometric means are within 1% of the default profile's warm and cold at 1.05M and
10.5M and on the DBpedia 50M slice, and its cold `star-lookup` takes 18.5 ms against
18.1 ms. After every 10.5M query ran three times the server held 709 to 720 MiB of RSS
against 775 to 794 MiB with the default profile.
[X08's Outcome](../../specs/X08-disk-spill-and-server-memory.md#small-profile-retune-and-the-phase-1a-control-item-97-2026-10-23)
has the runs.

**The result cache** answers repeated queries from memory, up to 512 MiB per dataset
(`--result-cache-mb`). It was off in every run on this page.

<a id="streaming-execution"></a>

**Automatic execution** is the default over HTTP, in the CLI, in Node and in the JVM
library. Eager execution materializes each operator's result within the query's row and
memory budgets, and streaming execution returns bounded batches and charges growing
operator state to the budget, while sorts and some operators still need their full input.
Automatic execution streams plain scans from an estimated thousand rows, groupings and
the FILTER EXISTS and NOT EXISTS that a key set answers from an estimated 20,000 input rows,
and a plan whose eager output would outgrow the memory the query may use while the
cursor's own state fits. It runs every other plan eagerly, because eager is faster on
several shapes at 12 threads.

Each case below is one request to a fresh server process at 10.5M, which reports the
query's allocation estimate in the native metadata and its peak RSS in `VmHWM`.

| 10.5M, one request | Eager | Automatic |
|---|---:|---:|
| Full scan, first 10 rows, allocation estimate | 241.0 MiB | **1.0 to 1.1 MiB** |
| Full scan, first 10 rows, server peak RSS | 804 MiB | **63 to 66 MiB** |
| Full scan, first 10 rows, wall time | 240 to 320 ms | **2 to 5 ms** |
| Full scan, 10.5M rows as TSV, server peak RSS | 881 to 889 MiB | **580 to 582 MiB** |
| Full scan, 10.5M rows as TSV, wall time | 5.20 to 5.69 s | 4.87 to 5.46 s |
| Name scan, 1.02M rows, allocation estimate | 16.3 MB | **5.0 MB** |
| Name scan, 1.02M rows, server peak RSS | 140 MiB | **121 MiB** |

*Measured on forge.*

A streamed answer stopped after `send` rows reports an unknown total, where the eager
answer counted all 10.5M rows. The automatic decision costs a small query nothing that
`perf stat` can see, with user-space instructions per request within −0.2% to +0.1% of an
eager-only build over 1,000 requests each of `star-lookup`, `values-star`, `employee-docs`
and `star-join` at 1.05M.

A probe binary that interleaves the modes in one process measured streaming time divided
by eager time at 10.5M, where under 1 means streaming is faster
([X05](../../specs/X05-streaming-query-execution.md#streaming-parity-2026-10-17)).

| 10.5M, ratio of streaming to eager | 12 threads | 1 thread |
|---|---:|---:|
| TSV, 500,000 rows | **0.86** | **0.83** |
| TSV, name scan of 1.02M rows | **0.89** | **0.91** |
| Range top-k, rows only | 1.56 | **0.94** |
| Expression ordering, rows only | 1.08 | **0.89** |
| Substring filter, rows only | 1.45 | 1.02 |
| Regular expression filter, rows only | 1.50 | 1.07 |

*Measured on forge on 2026-10-17.*

Small scans stream faster too, which is why automatic execution streams scans from an
estimated thousand rows. The scans read `foaf:name` with `LIMIT` and `ex:city`, which has
20,000 triples. The first table measures the probe binary in one process, and the second
measures automatic execution over HTTP, each line the median of three rounds of the median
wall time of 60 TSV requests from curl to a fresh server at 10.5M with one Rayon thread, the
server and client pinned to their own cores. A first run sets `SPARKLES_NO_IN_PLACE`, so
that every request is planned and runs as a first one does. A repeat lets a query that
answered in under 10 ms run eagerly in place, which a streamed answer also earns. The wall
times include a constant floor of about 1.97 ms in the client path, which hides
differences at 1,000 and 5,000 rows.

| 10.5M, TSV, ms | Eager, 12 threads | Streaming, 12 threads | Eager, 1 thread | Streaming, 1 thread |
|---|---:|---:|---:|---:|
| 1,000 triples | 0.332 | **0.259** | 0.335 | **0.260** |
| 5,000 triples | 1.604 | **1.126** | 1.610 | **1.229** |
| 50,000 triples | 15.590 | **8.057** | 15.676 | **12.034** |
| 100,000 names | 19.151 | **16.888** | 30.216 | **23.228** |

| 10.5M, TSV over HTTP, ms | First run | Repeated |
|---|---:|---:|
| 1,000 rows | 2.014 | 2.013 |
| 5,000 rows | 3.149 | 3.155 |
| 20,000 rows | 5.461 | 6.443 |
| 50,000 rows | 14.007 | 14.004 |

*Measured on forge.*

<a id="batch-kernels-and-disk-spill"></a>

#### Batch kernels and disk spill

The cursor's merge and hash joins, counts, groupings and
DISTINCT run on column kernels, and sorts, groupings, unordered DISTINCT, hash joins,
merge-join runs and a DISTINCT below its ORDER BY can spill (X08 Phase 2). Spill is
opt-in. A probe binary interleaved eager and cursor runs of each query in one process,
reading every row. The table gives the cursor's time divided by the eager time, the median
of three to five rounds, where under 1 means streaming is faster.

| Ratio of streaming to eager | 1.05M, 1 thread | 10.5M, 1 thread | 10.5M, 8 threads |
|---|---:|---:|---:|
| `expr-agg-arg`, a grouped SUM of an expression | **0.50** | **0.51** | **0.51** |
| `group-avg`, grouped AVG and COUNT then top 10 | **0.90** | **0.61** | **0.71** |
| `expr-bind-group`, a COUNT by a computed key | **0.70** | **0.66** | **0.78** |
| `x-group-out`, 2,000 groups of a join | **0.91** | **0.83** | **0.75** |
| `x-group-knows`, a COUNT per subject | **0.65** | **0.70** | **0.79** |
| `optional-count`, COUNT over OPTIONAL | **0.88** | **0.80** | **0.79** |
| `minus`, COUNT over MINUS | **0.88** | **0.82** | **0.83** |
| `x-distinct-out`, DISTINCT of a scan | **0.90** | **0.93** | **0.96** |
| `distinct-join`, DISTINCT of a join | 1.09 | **0.95** | **0.99** |
| `optional-chain`, two OPTIONAL joins | 1.00 | 1.33 | **0.92** |
| `x-hash-join`, a hash join's 1.75M rows | 1.10 | 1.02 | **0.99** |
| `x-join-out`, an index join's 1M rows | 1.28 | 1.03 | **0.98** |
| `star-join` | 1.03 | 1.09 | 1.16 |
| `regex-iri` | 1.26 | 1.26 | 1.30 |
| `order-by-full`, a sort of 1.02M rows | 1.01 | **0.69** | **0.82** |

*Measured on an 8-vCPU AMD EPYC Namespace instance on 2026-10-17.*

Every grouped query streamed faster at every size and thread count, so automatic
execution streams groupings from an estimated 20,000 input rows, counts over OPTIONAL
and MINUS included. Joins without a grouping stay eager, because they measured slower or
mixed. The cursor also holds much less. At 10.5M its ledger peaked at 2.2 MB for
`optional-count` against 19.8 MB eagerly, and 2.6 MB for `distinct-join` against
102.1 MB. A full sort stays eager by default, because the in-memory cursor sort holds its
whole input with decoded keys, 580 MB in the ledger against 31 MB eagerly.

<a id="automatic-exists"></a>

#### Groupings with EXISTS

The cursor answers a FILTER EXISTS or NOT EXISTS from a key set
of its pattern. This comparison of eager and automatic execution ran with one Rayon thread,
four A, B, B, A rounds and fresh servers per round, and both modes returned the same
answers. The table gives the median server execution time of automatic execution divided
by that of eager execution.

| Ratio of automatic to eager | 1.05M | 10.5M |
|---|---:|---:|
| `not-exists-knows`, COUNT of `knows` edges without a reverse edge | **0.91** | **0.68** |
| `not-exists`, the bench query | 0.96, eager at this size | **0.96** |
| `exists-join`, the bench query | 1.01, eager at this size | **0.98** |
| `exists-group`, a grouped COUNT over FILTER EXISTS, top 10 | 1.01 | 0.99 |
| `exists-bind`, a COUNT grouped by BIND(EXISTS …) | 1.07 | 1.15 |

*Measured on an 8-vCPU AMD EPYC Namespace instance on 2026-10-23.*

At 1.05M `not-exists` and `exists-join` filter fewer than 20,000 estimated rows and stay
eager, so their ratios compare a mode with itself. An EXISTS in BIND, in an OPTIONAL
condition or inside a larger expression is evaluated once per distinct outer key, which
was slower in the cursor, so plans with one stay eager.

Spill was measured with the cursor under a 64 MiB budget and the eager run unlimited, at
10.5M, with the spill directory on the instance's local disk.

| 10.5M, ms | Eager, unlimited | Cursor with spill, 64 MiB, 1 thread | Eager, 8 threads | Cursor with spill, 64 MiB, 8 threads |
|---|---:|---:|---:|---:|
| `order-by-full`, 1.02M rows by name | 773 | **658** | **238** | 535 |
| ORDER BY age then person, 1M rows | 1,677 | **1,294** | **298** | 563 |
| ORDER BY an expression, 1M rows | 1,809 | **1,297** | **307** | 600 |
| GROUP BY person, 833,177 groups | **127** | 195 | **124** | 205 |
| DISTINCT over a UNION, 986,333 rows | 56 | **51** | 69 | **56** |

*Measured on an 8-vCPU AMD EPYC Namespace instance on 2026-10-17.*

The spilling sorts held 45 to 54 MB in the ledger, where the in-memory cursor sort held 580
to 708 MB and the eager sort reported 31 MB. The merge runs on one thread, so a spilling
sort is faster than eager execution at one thread and about twice as slow at eight. The
grouping held 38 MB against 130 MB eagerly and took 1.5 to 1.7 times as long, and at
16 MiB it held 11 MB and took 2.8 times as long. DISTINCT held 14.5 MB against 152 MB, and
at 16 MiB 4.4 MB at 1.7 times the eager time. A budget near a tenth of the state's size
thus costs 1.7 to 2.8 times the time, and one near half of it costs little.

The same probe ran joins and an ordered DISTINCT under a budget beside the unlimited cursor
and eager execution, and checked that the spilled rows matched the eager ones.

| 10.5M, ms | Eager | Cursor, unlimited | Cursor, 16 MiB | Cursor, 4 MiB | Written at 4 MiB |
|---|---:|---:|---:|---:|---:|
| Join on `foaf:knows` under a merge join, 2.5M rows | **66** | 150 | 322 | 1,333 | 157 MB |
| OPTIONAL with a filter, 300,191 rows | **34** | 57 | 77 | 186 | 24.6 MB |
| MINUS, 90,161 rows | **21** | 36 | 59 | 165 | 24.6 MB |
| `SELECT DISTINCT … ORDER BY`, 917,941 rows | 615 | **569** | 1,421 | 3,028 | 60.1 MB |

*Measured on an 8-vCPU AMD EPYC Namespace instance on 2026-10-18, before later corrections
to join spill that bound retained output files and repaired merge and DISTINCT accounting.
These figures do not establish the cost of those corrections.*

The unlimited cursors held 61.8 to 90.8 MB for the joins and 1,016 MB for the DISTINCT,
and every spilled run stayed within its budget. A join of 29.2M rows where one key holds
nine tenths of each input took 681 ms under 1 MiB as a block nested loop, against 487 ms
for the unlimited cursor, which held 34.3 MB, and 631 ms eagerly. The `foaf:knows` join
sits under a merge join that relies on its order, so a spilled run writes its output a
second time to merge it back, which is why it slows more than the OPTIONAL and MINUS joins
at the root. The spilled joins wrote 95 to 150 MB a second. `--spill encrypted` added 1 to
6% to the bytes written and −2 to 21% to the times under 4 MiB, most of them under 10%.

<a id="parallel-cursor-filters"></a>

#### Parallel cursor filters

A cursor filter over an index scan reads the scan in morsels
of whole blocks and filters each morsel in parallel
([X05](../../specs/X05-streaming-query-execution.md#parallel-cursor-filters-2026-10-18)). The
table gives the cursor's time divided by the eager time over 10.5M triples, the median of
seven rounds, reading every row.

| Ratio of streaming to eager | 8 threads | 1 thread |
|---|---:|---:|
| CONTAINS on names, 49,837 rows | **0.91** | 1.00 |
| REGEX on subject IRIs, 1,000 rows | 1.08 | 1.15 |
| STRSTARTS on names, 49,837 rows | **0.94** | **0.90** |
| LANGMATCHES on titles, 500,000 rows | **0.95** | **0.88** |

*Measured on an 8-vCPU AMD EPYC Namespace instance on 2026-10-18.*

The cursor's ledger peaked at 13.5 MB for the CONTAINS filter with eight threads, against
15.6 MB eagerly, and at 6.2 MB with one thread, since each worker holds its own morsel
scratch. The cursor's range top-k reads the parts of its range without value order in
parallel morsels and keeps only its best rows
([X05](../../specs/X05-streaming-query-execution.md#streaming-range-top-k-2026-10-18)). Over the
salaries above 150,000 at 10.5M it takes 0.89 to 1.06 times its eager time at eight threads
and 0.88 at one, with a ledger peak of 6.6 to 7.1 MB at eight threads and 3.5 MB at one. In
native JSON at one thread, the STRSTARTS filter over names takes 1.01 to 1.03 times its
eager time and the streamed name scan of 1.02M rows 0.88 to 0.90 times
([X05](../../specs/X05-streaming-query-execution.md#native-json-of-a-streamed-filter-2026-10-18)).

In the JVM binding comparison at 1.05M on forge, the JNI export of 500,000 rows takes
130 ms in automatic mode against 126 ms eagerly on disk and 129 ms against 124 ms in
memory. No other query, `find`, `contains` or `getProperty` moved by more than run-to-run
noise between the modes, and the small-query latencies at one thread stayed within 2%.

**mimalloc** keeps freed memory in per-thread heaps, and the server hands idle memory back
to the OS after a second (`--idle-release-ms`). On the development laptop, a fresh 10.5M
server with mimalloc held 920 MiB after concurrent rounds against 838 MiB for glibc with
idle release, and peaked at 1,682 MiB against 1,439 MiB. In exchange it served 190 q/s
against 144, and `star-join` took 38 ms against 72 ms. The bindings make the same choice
case by case. The Python package and the JVM library use mimalloc, which raises the
Python benchmark process's peak RSS by about 20% and a mixed JVM run's from 653 to
962 MiB. The Node.js addon uses mimalloc as well, because single adds, bulk adds and
`has()` took 15 to 20% less time with it, and the process keeps more memory in exchange
([P05](../../specs/P05-node-bindings.md#outcome)).

The server and the bindings call mimalloc through its plain entry points for buffers
aligned to 8 bytes or less (`crates/sparkles-alloc`). The aligned entry point, which the
`mimalloc` crate uses for every request, puts a power-of-two buffer of 64 KiB or more into
the next size class, so a full decoded block column of 256 KiB would take 320 KiB. The X08
A1 gate measured the difference with ten datasets of 1.05M triples, after the suite ran
against each, three repeats each and medians below. `scripts/x08-gates/a1.sh` reproduces
it.

| `--cache-mb` | Datasets | Cache bytes | `RssAnon`, aligned entry | `RssAnon`, plain entry | VmRSS, aligned entry | VmRSS, plain entry |
|--:|--:|--:|--:|--:|--:|--:|
| 256 | 1 | 60.6 MiB | 132.5 MiB | 120.7 MiB | 195.7 MiB | 176.5 MiB |
| 256 | 10 | 379 MiB | 572.5 MiB | 492.9 MiB | 702.8 MiB | 612.8 MiB |
| 32 | 1 | 44.2 MiB | 120.7 MiB | 111.0 MiB | 184.2 MiB | 167.5 MiB |
| 32 | 10 | 160 MiB | 274.8 MiB | 264.5 MiB | 405.3 MiB | 387.3 MiB |

*Measured on forge.*

Most of the cache bytes at `--cache-mb 32` belong to the result cache, which is sized
separately and whose heap stays about 1.2 times the bytes it counts. [Roadmap](../../ROADMAP.md)
item 88 covers that overhead.

<a id="compression-105m-triples"></a>

### Compression of exports and backups

HTTP responses and backups can be compressed. Times are hyperfine means over 5 runs at
10.5M, including `curl`, and server CPU is the server's user and system time per request.

| Response | Encoding | Bytes | Ratio | Wall (s) | Server CPU (s) |
|---|---|---:|---:|---:|---:|
| Full N-Quads export | identity | 1,121,335,687 | 1.00 | 5.141 | 5.427 |
| | gzip-6 | 74,937,009 | 14.96 | 9.478 | 14.745 |
| | br-4 | 96,641,420 | 11.60 | 5.295 | 10.453 |
| | zstd-3 | 83,261,471 | 13.47 | 5.139 | 6.650 |
| | zstd-1 | 79,063,209 | 14.18 | 5.145 | 6.325 |
| `SELECT * … LIMIT 500000` as TSV | identity | 45,411,223 | 1.00 | 0.089 | 0.253 |
| | gzip-6 | 2,919,848 | 15.55 | 0.432 | 0.672 |
| | br-4 | 3,490,551 | 13.01 | 0.266 | 0.517 |
| | zstd-3 | 3,134,195 | 14.49 | 0.093 | 0.307 |
| | zstd-1 | 2,905,885 | 15.63 | 0.093 | 0.290 |

zstd streams the export as fast as identity at a 13–14× smaller size, while gzip takes
about 1.8× the wall time and 2.7× the server CPU. `/$/backup` writes an N-Quads dump to a
file, medians of 3 runs:

| Codec | Seconds | Size |
|---|---:|---:|
| none | 5.66 | 1,121.3 MB |
| gzip-6 | 45.11 | 74.9 MB |
| zstd-3 | 9.70 | 81.5 MB |
| brotli-5 | 17.69 | 119.0 MB |
| lz4 | 6.12 | 161.9 MB |
