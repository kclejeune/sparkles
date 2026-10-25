# Language bindings results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Comparison summary

Through Jena's own API, Sparkles answers 24 of 28 suite queries faster
  than TDB2 and than Jena's in-memory store. The Python package is faster than pyoxigraph
  on 25 of 28, and the Node.js package is faster than Oxigraph's JavaScript package on 24
  of 28.

## Bindings

The bindings embed the engine in a JVM, Python or Node.js process. The comparison of
`mise run bench:bindings` loads 105,302 synthetic triples (10,000 people) into each library
and runs the suite's 28 queries through each library's own API, consuming every row and
term. It ran on `atlas` with the processes pinned to CPUs 0–11, two processes per engine
and mode, one warm-up and five samples per case, and reports medians of the process
medians. Every engine's answers agree by value on all 33 cases.

### JVM

Jena's `QueryExec` runs on a `DatasetGraphSparkles`, on disk and in memory, against TDB2
on disk and Jena's in-memory TIM dataset.

| Case, ms | Sparkles on disk | Sparkles in memory | TDB2 | TIM |
|---|---:|---:|---:|---:|
| Load the N-Triples file (s) | **0.51** | 0.53 | 1.30 | 0.87 |
| `count-all` | 0.988 | **0.979** | 23.7 | 46.3 |
| `star-join` | 1.728 | **1.694** | 5.740 | 4.014 |
| `two-hop-count` | 1.239 | **1.122** | 56.3 | 32.6 |
| `group-avg` | 1.478 | **1.456** | 25.5 | 15.3 |
| `order-by-full` | **6.991** | 8.091 | 41.4 | 22.8 |
| `export-500k` | 28.0 | **25.2** | 54.0 | 44.5 |
| `knows-reach` | 0.570 | 0.591 | 0.565 | **0.449** |
| `star-lookup` | 0.739 | 0.800 | 0.739 | **0.426** |
| `values-star` | 0.775 | 0.952 | 0.662 | **0.463** |
| `employee-docs` | 0.626 | 0.707 | 0.509 | **0.391** |
| Peak RSS, load process (MiB) | **149** | 151 | 457 | 416 |
| Peak RSS, read process (MiB) | **345** | 346 | 472 | 590 |

Over all 28 queries, Sparkles on disk is faster than TDB2 on 24, with a geometric mean
8.1× faster, and Sparkles in memory is faster than TIM on 24, 5.8× faster. The four
losses are the small lookups in the last rows, where a query's fixed cost through Jena
dominates.

Calls that read a few triples go through hand-written JNI entry points. This table gives
their latency on one thread in microseconds, from the same dataset in memory, against
TDB2 and TIM.

| Operation, µs | Sparkles | TDB2 | TIM |
|---|---:|---:|---:|
| `Graph.contains` in its own read transaction | 1.68 | 2.47 | **0.53** |
| `Model.getProperty` | 2.64 | 3.46 | **0.68** |
| `find` on a subject | 5.20 | 5.59 | **1.71** |
| `ASK` | 42.69 | 28.75 | **26.18** |
| `SELECT ?o` on a subject | 42.61 | 27.48 | **23.92** |
| `star-lookup` | 233.7 | 209.8 | **115.5** |
| `values-star` | 195.8 | 101.7 | **88.9** |

On four threads, `contains` reaches 1,680,000 operations per second against TDB2's
678,000, `getProperty` 1,130,000 against 657,000, and `find` on a subject 704,000 against
446,000. A small `ASK` reaches 43,200 queries per second against TDB2's 45,500.

### Python

The native API runs against pyoxigraph 0.5.11, and the rdflib store plugin against
rdflib's in-memory store. The native export and four small-lookups rows use six fresh
processes per engine on forge with CPython 3.14.7, pinned to six distinct performance
cores, with ten warmups and thirty samples per process. The remaining rows and the
full-suite totals use the complete binding comparison. The [current-source assessment](../2026-10-25-python33-current-source-forge.json)
records the exact native build, all samples and answer checks.

| Case, ms | sparkles | pyoxigraph | rdflib on Sparkles | rdflib Memory |
|---|---:|---:|---:|---:|
| Load the N-Triples file (s) | 0.15 | **0.14** | 1.46 | 1.52 |
| `count-all` | **0.236** | 14.8 | 2.413 | 641.5 |
| `star-join` | **1.224** | 8.567 | 7.682 | 400.3 |
| `two-hop-count` | **0.368** | 27.9 | 4.295 | 600.5 |
| `group-avg` | **0.711** | 6.921 | 3.580 | 237.7 |
| `order-by-full` | **20.5** | 41.6 | 76.5 | 511.1 |
| `export-500k` | **140.3** | 300.3 | 881.9 | 2,234 |
| `knows-reach` | 0.063 | **0.042** | 1.819 | 2.060 |
| `star-lookup` | **0.209** | 0.303 | 3.928 | 4.625 |
| `values-star` | 0.131 | **0.120** | 3.887 | 3.671 |
| `employee-docs` | **0.122** | 0.134 | 2.337 | 3.234 |
| Peak RSS, load process (MiB) | 65 | 65 | 127 | 160 |
| Peak RSS, read process (MiB) | 135 | **97** | 197 | 291 |

Over all 28 queries the native API is faster than pyoxigraph on 25, with a geometric
mean 7.5× faster. Through rdflib's API the plugin answers every query faster than
rdflib's own store, by a geometric mean of about 42×.

### Node.js

`@sparkles-rdf/engine` runs against Oxigraph's JavaScript package and N3.js's `Store`
queried through Comunica.

| Case, ms | @sparkles-rdf/engine | oxigraph (JS) | N3.js and Comunica |
|---|---:|---:|---:|
| Load the N-Triples file (s) | **0.17** | 0.22 | 0.39 |
| `count-all` | **0.462** | 25.6 | 397.8 |
| `star-join` | **3.301** | 102.0 | 212.1 |
| `two-hop-count` | **0.606** | 37.5 | 278.1 |
| `group-avg` | **1.161** | 10.8 | 210.8 |
| `order-by-full` | **91.6** | 599.5 | 192.6 |
| `export-500k` | 586.8 | 3,900 | **406.8** |
| `knows-reach` | 0.340 | **0.308** | 2.932 |
| `star-lookup` | 1.033 | **0.757** | 28.0 |
| `values-star` | 0.564 | **0.488** | 8.004 |
| `employee-docs` | 0.613 | **0.516** | 27.1 |
| Peak RSS, load process (MiB) | **94** | 143 | 341 |
| Peak RSS, read process (MiB) | **238** | 739 | 459 |

Over all 28 queries the engine is faster than Oxigraph's package on 24, with a geometric
mean 9.9× faster, and faster than N3.js with Comunica on 27.

### Per-call costs

Calls that touch a few quads, such as a `has()` probe or a single add in a transaction,
cost more through a native binding than through a store written in the host language.
These runs use the same 105,000 triples, A/B/B/A order in fresh processes pinned to six
cores, and four to six process medians per arm.

| Python, per call | sparkles | pyoxigraph |
|---|---:|---:|
| Single `tx.add` | **4.856 µs** | 4.896 µs |
| `extend`, per quad | **4.599 µs** | 4.935 µs |
| `in` | **1.087 µs** | 1.887 µs |
| First object of `(s p ?)` | 2.030 µs | **1.359 µs** |
| `(s ? ?)`, per subject | 11.207 µs | **9.144 µs** |
| `(? p o)` over 2,982 matches | 3.052 ms | **2.910 ms** |

The native Python costs use the same quiet Forge assessment as the small-query rows.
Each write sample inserts 10,000 fresh triples. Single-add timings use a Sparkles
transaction and pyoxigraph's `extend`, while the bulk row uses Sparkles `extend` and
pyoxigraph `bulk_extend`. Single-add costs are close. All nine read cases agree by value,
and a separate full export checks every row and term. Decimal lexical spellings differ
on 1,020 salary values, such as `92059.20` and `92059.2`, while their values agree.
Actual-write checks confirm stored data, blank-node identity and iterator snapshots.
The assessment leaves the Python binding unchanged.

Through the rdflib plugin, against rdflib's Memory store, `(? p o)` takes 4.1 ms against
4.1 ms, iterating every triple 122 ms against 155 ms, a `contains` probe 4.0 µs against
3.1 µs and `Graph.value` 6.7 µs against 5.1 µs.

| Node.js, per call | @sparkles-rdf/engine | N3.js `Store` |
|---|---:|---:|
| Single add in a transaction | 7.6 µs | **5.1 µs** |
| `addAll`, per quad | 7.0 µs | **5.8 µs** |
| `has()` | 1.9 µs | **1.3 µs** |
| First object of `(s p ?)` | **6.5 µs** | 8.0 µs |
| `(s ? ?)`, per subject | **12 µs** | 21 µs |
| `(? p o)` over 2,983 matches | 2.5 ms | **2.4 ms** |
| Iterate every quad | **59 ms** | 123 ms |

On the same kind of instance, ordinary Jena use on Sparkles took 25 µs for an `ASK` on a
bound triple against TDB2's 27 µs, 22 µs for a `SELECT ?o` on a subject against 23 µs,
10 µs for an initial binding on a parsed query against 12 µs, and 52 µs for a DESCRIBE
of one resource against 36 µs. A DELETE/INSERT WHERE on one resource in the in-memory
store took 13 to 15 ms against TDB2's 2.3 to 3.2 ms.

*The rdflib, Node.js and Jena per-call figures use 8-vCPU AMD EPYC cloud instances.
Runs on different instances differed by up to 30%. The native Python figures use the
quiet Forge assessment described above.*

### Native term validation

Borrowing cached terms during native quad-position validation reduces the cost of hot
Node operations on forge. Both builds use Rust 1.98.1 and GCC 12.2.0 in the same container
and source path. Node 22.23.3 runs on the Intel Core i5-13500 with six worker threads.
Three interleaved A/B/B/A blocks give six fresh processes per build over 105,300 unique
quads. Each process checks two million cached probes with equal hits and misses, writes
10,000 fresh literals with reused subjects and predicate, and measures 10,000 empty
dataset creations and closes after warmup.

| Operation | Before | After | Change |
|---|---:|---:|---:|
| `has()` through the public API | 0.832 µs | 0.796 µs | 4.3% faster |
| Native contains call | 0.671 µs | 0.643 µs | 4.2% faster |
| Add a fresh literal in a transaction | 4.464 µs | 4.280 µs | 4.1% faster |
| Create and close an empty in-memory dataset | 76.104 µs | 73.897 µs | 2.9% faster |

All three blocks improve these operations. Query and export controls have smaller or
inconsistent effects. A five-row SELECT changes from 9.6% slower to 4.9% faster in a
warmed repeat, with wide variation between processes. Fully decoded exports change from
0.8% faster to 1.5% slower, and count-only exports from 1.0% to 0.4% slower. These small
export costs are accepted alongside the probe and write gains. Roadmap item 329 tracks
export variation and the unstable small-query control. The [raw samples and drivers](../2026-10-25-node33-borrowed-validation-forge.json)
record both batches, binary hashes, compiler versions and answer checks.
