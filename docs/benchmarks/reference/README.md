# Reference setup and methodology

This page preserves the shared context of the imported reports. Individual reports can
specify different hosts, revisions, timing contracts or limits.

This page reports how fast Sparkles loads, queries and writes RDF, how much memory and
disk it uses for that speed, and how its Python, JVM and Node.js bindings compare with
the libraries they replace. The comparisons run the same data and queries on every engine,
check every engine's answers before timing them, and list every case where Sparkles loses.
[BENCHMARKING.md](../../BENCHMARKING.md) explains how to run the harnesses. The reference setup
below applies unless a section specifies another host, limit or timing contract.

| Measurement context | |
|---|---|
| Hardware | `forge` has an Intel Core i5-13500 with 6 performance and 8 efficiency cores and 20 threads, 16 GiB of RAM and an NVMe SSD. `atlas` has the same CPU and runs the bindings. A development laptop or a Namespace instance is used only where a section says so. |
| OS and kernel | NixOS for the reference runs. Each measurement record identifies its kernel. |
| Sparkles | 0.1.0, release builds with default features, rustc 1.98 and 1.99 |
| Other engines | QLever 0.5.48, Fluree 4.2.2 (the release binary), Oxigraph 0.5.11 (server, pyoxigraph and the JavaScript package), Apache Jena TDB2 6.2.0 with Fuseki 5.1.0, Jena 5.6.0 (TDB2 and TIM in the JVM comparison), rdflib 7.6.0, N3.js 2.13.11 with Comunica 5.4.1 |
| Dates | Each table names the host, the date and the build it comes from. A section that names no date comes from the runs of 2026-10-03 to 2026-10-09 on forge. |

Reference engine comparisons run alone under a 12 GiB memory limit, with the timing
client on CPU 11 and result caches off. Full-data DBpedia, bindings, cloud measurements
and model evaluations specify their own limits and placement. [Methodology and
reproduction](README.md#methodology-and-reproduction) gives the settings and commands for each
comparison.

## Methodology and reproduction

<a id="setup"></a>

### Setup

* **Data.** `scripts/gen-data.py N` generates about 10.5 triples per person: people,
  organisations, documents, typed literals, `foaf:knows` and citation graphs,
  language-tagged titles and a small OWL class hierarchy. N = 100,000 gives 1.05M triples,
  and N = 1,000,000 gives 10.5M triples, 1.12 GB of N-Triples.
* **Queries.** `scripts/bench.sh` has 28 queries covering scans, counts, joins, OPTIONAL,
  property paths, subqueries, grouping, sorting, point lookups and expressions in BIND,
  ORDER BY and aggregate arguments.
* **Engines.** No engine may answer from a result cache.
  * Sparkles runs as `sparkles serve --result-cache-mb 0` with every other setting at its
    default.
  * Jena loads with `tdb2.tdbloader` and serves with Fuseki and `-Xmx8G`. Fuseki has no
    result cache.
  * QLever runs with `-m 8G -j 16 --cache-max-size-single-entry 0B`, which stops it from
    caching results, and its cache is cleared before every timed run.
  * Fluree 4.2.2 is the release binary, downloaded from the release archive and verified
    against a checksum. The flake's package of the same archive, which `nix/fluree.nix`
    pins by SHA-256, rewrites the Linux executable to load the glibc of the locked
    nixpkgs, so its SHA-256 in result manifests differs from the downloaded binary's. The
    published figures were not measured with the packaged executable. Fluree loads with
    `fluree create bench --from data.nt --chunk-size-mb 16` and serves with
    `fluree server run`, `FLUREE_CACHE_MAX_MB=4096` and
    `FLUREE_PATH_MAX_VISITED=20000000`, because the default traversal cap of 1M nodes
    rejects `knows-reach` at 10.5M. Fluree has no result cache.
  * Oxigraph loads with `oxigraph load` and then `oxigraph optimize`, the compaction its
    loader recommends before read-heavy workloads, and serves with
    `oxigraph serve --timeout-s 600`. The harness reads the N-Triples from standard input,
    because with `--file` Oxigraph 0.5.11 parses N-Triples in parallel chunks and turns a
    blank node label used in two chunks into two nodes.
* **Isolation.** Each engine runs alone on the machine, one after another, in a systemd
  scope with a 12 GiB memory limit and no swap, so an engine that runs out of memory is
  killed on its own. The OS page cache is warm except in the cold runs.
* **SSD state.** The SSD is trimmed with `fstrim` before each batch of timed runs, so free
  blocks do not slow the writes of loads and commits. The trim needs root, so the operator
  trims before launching a batch. The cold runs evict each engine's own files with
  `posix_fadvise`, which needs no privilege, instead of dropping the whole page cache. Runs
  dated before 2026-10-23 had only the host's weekly trim.
* **Carried-over engines.** A refresh may time Sparkles alone and keep another engine's
  results when that engine's version and binary and the host are unchanged.
  `bench-record.py build --reuse` refuses a change of either, writes the carried-over
  run's dates, host and harness under `manifest.reused` and marks each carried-over
  result. The answer checks still compare Sparkles' new answers with the other engines'.
* **Timing.** hyperfine 1.20 times SPARQL protocol POSTs with TSV results, with 2 warm-ups
  and 10 runs. Times are mean ± σ in ms and include starting `curl` and the round trip,
  about 3.5 ms. The client runs on CPU 11, the last performance core, and the servers are
  not pinned.
* **Writes.** Update latency is a single-triple `INSERT DATA` into a named graph, preceded
  by an untimed `DELETE DATA` so that each request inserts, with an ASK before and after.
  Throughput is 160 `star-join` requests from 16 unpinned `curl` clients. Memory is the
  server's RSS 2 s after the throughput run, and its peak during it.
* **Comparisons.** A difference within 10% is a tie. No figure is a significance test or
  a confidence interval.

### Answer checks

`scripts/bench-answers.py` fetches every engine's answer to every query as SPARQL JSON
before timing and fingerprints it by the row count, by the solution multiset under RDF
term identity, and by the same multiset with numbers compared by value to 12 significant
digits. An engine whose answer differs in value from the majority is marked † and not
ranked unless an independent data oracle accepts the difference. A majority is a screening
rule, not proof that its answer is correct. Unresolved disagreements remain excluded,
including the full DBpedia births-by-decade counts of 165 and 171, which have no retained
independent data check. A ‡ marks equal values returned as different RDF terms. `export-500k` uses LIMIT
without ORDER BY, so only its row count is checked.

### Engine failures

These failures of other engines were seen in the measurements and are results, not
harness errors.

* Fuseki 5.1 answers `knows-reach` with HTTP 500 after a `StackOverflowError` in its
  property path engine, at 1.05M and 10.5M. In one run at 10.5M the failure also left
  TDB2's node table cache locked, and every later Fuseki query ran into the request limit.
* Fluree 4.2.2 grows past the 12 GiB memory limit on `optional-chain` at 10.5M, on the
  loaded store and on the changed store of the `updates` mode, and the kernel kills it.
* QLever 0.5.48 keeps updates in memory. In a one-minute smoke run of the long write
  harness its RSS reached 27 GiB after about 2,300 write requests and the kernel killed
  it, so the long write runs set `SERVER_MEM_MAX`.
* QLever and Fluree were killed during SP²Bench `q4` at 10M triples, in the answer check
  that runs all five servers at once.
* QLever 0.5.48 answers a request for RDF/XML with 406, so it cannot run BSBM's Explore
  mix, whose driver asks for DESCRIBE and CONSTRUCT results in that format only. In BSBM
  smoke runs with 500 products on a development machine, QLever's server crashed with
  SIGSEGV twice while the test driver ran, once with one client in Explore and once with
  four clients in Business Intelligence. The core dump shows the fault in Boost.Asio's
  epoll reactor.

### What the benchmarks do not cover

* **Larger scale.** The largest dataset is English DBpedia at 1.24 billion triples. QLever
  is built for 10⁹–10¹¹ triples and runs Wikidata and UniProt, and its lazy evaluation,
  FSST vocabulary compression and IRI encoding matter more as data grows. Sparkles'
  FSST option was measured only up to the DBpedia 50M slice. Sparkles spills cursor sorts,
  groupings, DISTINCT and joins when spill is enabled. A query still fails when its eager
  state outgrows its budget.
* **Data larger than RAM.** Every store fits in memory except the DBpedia index of
  28.7 GiB under a 14 GiB limit.
* **Other standard benchmarks.** SP²Bench at 10M triples, LUBM(100) and BSBM Explore and
  BI at 10M have been compared on the reference machine. BI's driver cells have not been
  rerun with the current engine. The 100M BSBM and SP²Bench scales and the query
  comparisons at LUBM(1000) and OWL2Bench remain open. LDBC, WatDiv's stress testing and
  the Wikidata query log have no harness. The synthetic suite has a regular shape, and its
  28 queries are hand-picked.
* **Long runs.** Hours of sustained writes and compaction under load were not compared
  with other engines. `scripts/bench-longwrite.sh` runs that comparison and has run only
  at smoke scale.
* **Spatial and query-time reasoning.** GeoSPARQL workloads and query-time reasoning were
  not compared with other engines.
* **Result caches.** All runs had result caches off.
* **Other hardware.** Most engine comparisons use the reference Intel host. Cloud
  per-call and development-laptop model evaluations are identified separately. Broad
  hardware comparisons remain open.

### Reproducing

[BENCHMARKING.md](../../BENCHMARKING.md) documents the harnesses, modes and outputs, including
local and cloud regression comparisons. The commands below reproduce the published
comparisons.

* `mise run bench [people] [workdir]` or `scripts/bench.sh` runs the synthetic suite.
  `--engines NAME` re-measures one engine and merges it into an existing run, and
  `--answers-only` checks every engine's answers without timing. `scripts/bench.sh` takes
  the same settings as `ENGINES` and `ANSWERS_ONLY=1`.
* `mise run bench:watdiv [scale]` runs WatDiv, and `scripts/bench-text.sh` the full-text
  comparison.
* `mise run bench:sp2b 10m`, `mise run bench:lubm 100` and
  `SPARKLES_ALLOW_GPL_BENCHMARK=1 mise run bench:bsbm 10m` run the standard suites. Each
  writes a result record and a run manifest that name the Sparkles commit, the engine
  versions and the data checksums, and `scripts/bench-regressions.py compare` compares two
  records. A refresh puts the figures of its records into this document, with the
  measurement dates in each table's environment line.
* `mise run bench:billion full` runs DBpedia. Run it on a quiet machine right after
  `fstrim`, because an untrimmed SSD stalls the load's writes and slows cold lookups.
  `scripts/bench-billion.sh` records the last trim, the free space and the I/O pressure,
  and warns when the trim is more than two days old or less than 30% of the file system
  is free.
* `mise run bench:bindings` runs the JVM, Python and Node.js comparison, and
  `mise run bench:jena` the ordinary Jena use cases.
* `mise run bench:backup` runs the backup measurements, and
  `scripts/geosparql-benchmark.sh` the GeoSPARQL Compliance Benchmark.
* Engines must run alone. Stop each server by its PID or port before starting the next,
  and keep builds and other benchmarks off the machine.
