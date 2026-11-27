# Reference setup and methodology

This page defines the shared hardware, engine versions, timing contract and answer-checking
rules used across the published benchmark reports. Individual reports note any exception to
this reference setup. [BENCHMARKING.md](../../BENCHMARKING.md) documents how to run the
harnesses.

| Context | Reference setting |
|---|---|
| Hardware | `forge` has an Intel Core i5-13500 with 6 performance and 8 efficiency cores (20 threads), 16 GiB of RAM and an NVMe SSD. `atlas` has the same CPU and runs the language-binding comparisons. Other hosts appear only where a report says so. |
| OS and kernel | NixOS for the reference runs. Each measurement record identifies its kernel. |
| Sparkles | 0.1.0, release builds with default features, rustc 1.98 and 1.99. |
| Other engines | QLever 0.5.48, Fluree 4.2.2 (release binary), Oxigraph 0.5.11 (server, pyoxigraph and JavaScript package), Apache Jena TDB2 6.2.0 with Fuseki 5.1.0, Jena 5.6.0 (TDB2 and TIM in the JVM comparison), rdflib 7.6.0, N3.js 2.13.11 with Comunica 5.4.1. |
| Dates | Each table names its host, date and build revision. Sections without a date come from the runs of 2026-10-03 to 2026-10-09 on `forge`. |

Reference engine comparisons run alone under a 12 GiB memory limit, with the timing client
pinned to CPU 11 and result caches disabled. Full-data DBpedia, bindings, remote-host
measurements and model evaluations specify their own limits and placement.

## Methodology and reproduction

<a id="setup"></a>

### Setup

| Aspect | Contract |
|---|---|
| Data | `scripts/gen-data.py N` generates about 10.5 triples per person (people, organisations, documents, typed literals, `foaf:knows` and citation graphs, language-tagged titles and a small OWL class hierarchy). N = 100,000 produces 1.05M triples; N = 1,000,000 produces 10.5M triples (1.12 GB of N-Triples). |
| Queries | `scripts/bench.sh` runs 28 queries covering scans, counts, joins, OPTIONAL, property paths, subqueries, grouping, sorting, point lookups and expressions in BIND, ORDER BY and aggregate arguments. |
| Isolation | Each engine runs alone on the machine, one after another, inside a systemd scope with a 12 GiB memory limit and no swap, so an engine that exceeds the limit is killed on its own. The OS page cache is warm except in cold runs. |
| SSD state | The operator trims the SSD with `fstrim` before each batch of timed runs so free blocks do not stall loads or commits. Cold runs evict each engine's own files with `posix_fadvise` rather than dropping the whole page cache. Runs dated before 2026-10-23 relied on the host's weekly trim. |
| Carried-over engines | A refresh may time Sparkles alone and reuse another engine's results when that engine's version, binary and host are unchanged. `bench-record.py build --reuse` rejects any change, records the carried-over dates, host and harness under `manifest.reused`, and still checks Sparkles' new answers against the other engines. |
| Timing | hyperfine 1.20 times SPARQL protocol POSTs with TSV results, using 2 warm-ups and 10 timed runs. Times are mean ± σ in ms and include starting `curl` and the round trip (about 3.5 ms). The client runs on CPU 11 (the last performance core); servers are unpinned. |
| Writes | Update latency measures a single-triple `INSERT DATA` into a named graph preceded by an untimed `DELETE DATA`, with an ASK before and after. Throughput measures 160 `star-join` requests from 16 unpinned `curl` clients. Memory records server RSS 2 s after the throughput run and peak RSS during it. |
| Comparisons | A difference within 10% counts as a tie. No table cell is a significance test or confidence interval. |

No engine may answer from a result cache. Each engine runs with the following settings.

- Sparkles runs `sparkles serve --result-cache-mb 0` with every other setting at its default.
- Jena loads with `tdb2.tdbloader` and serves with Fuseki and `-Xmx8G`. Fuseki has no result cache.
- QLever runs with `-m 8G -j 16 --cache-max-size-single-entry 0B`, disabling result caching, and clears its cache before every timed run.
- Fluree 4.2.2 is the checksum-verified release binary downloaded from the release archive. The flake's package of the same archive (`nix/fluree.nix`) patches the Linux executable for nixpkgs glibc, so its SHA-256 differs from the downloaded binary's; the published figures used the downloaded release binary. Fluree loads with `fluree create bench --from data.nt --chunk-size-mb 16` and serves with `fluree server run`, `FLUREE_CACHE_MAX_MB=4096` and `FLUREE_PATH_MAX_VISITED=20000000` (the default 1M traversal cap rejects `knows-reach` at 10.5M). Fluree has no result cache.
- Oxigraph loads with `oxigraph load` followed by `oxigraph optimize`, and serves with `oxigraph serve --timeout-s 600`. The harness pipes N-Triples on standard input because `--file` in Oxigraph 0.5.11 parses in parallel chunks and splits blank node labels that appear across chunks.

### Answer checks

`scripts/bench-answers.py` fetches every engine's answer to every query as SPARQL JSON
before timing and fingerprints it by row count, by solution multiset under RDF term
identity, and by solution multiset with numbers compared by value to 12 significant
digits. An engine whose answer differs in value from the majority is marked † and left
unranked unless an independent data oracle accepts the difference. Majority agreement is a
screening rule rather than proof of correctness. Unresolved disagreements remain excluded,
including the full DBpedia `births-by-decade` counts of 165 and 171, which have no
retained independent data check. A ‡ marks equal values returned as different RDF terms.
`export-500k` uses LIMIT without ORDER BY, so only its row count is checked.

### Engine failures

The following peer failures occurred during measurements and are recorded as results
rather than harness errors.

- Fuseki 5.1 answers `knows-reach` with HTTP 500 after a `StackOverflowError` in its
  property-path engine at 1.05M and 10.5M. In one 10.5M run, that failure left TDB2's node
  table cache locked and every subsequent Fuseki query timed out.
- Fluree 4.2.2 exceeds the 12 GiB memory limit on `optional-chain` at 10.5M, both on the
  freshly loaded store and on the changed store in `updates` mode, and is killed by the
  kernel.
- QLever 0.5.48 keeps updates in memory. In a one-minute smoke run of the long write
  harness, its RSS reached 27 GiB after about 2,300 write requests and the kernel killed
  it, so long write runs enforce `SERVER_MEM_MAX`.
- QLever and Fluree were killed during SP²Bench `q4` at 10M triples during the answer
  check that ran all five servers concurrently.
- QLever 0.5.48 answers RDF/XML requests with HTTP 406, so it cannot run BSBM's Explore
  mix, whose driver requests DESCRIBE and CONSTRUCT results only as RDF/XML. In BSBM smoke
  runs with 500 products on a development machine, QLever's server crashed twice with
  SIGSEGV in Boost.Asio's epoll reactor (once with one client in Explore and once with
  four clients in Business Intelligence).

### What the benchmarks do not cover

| Gap | Details |
|---|---|
| Larger scale | The largest measured dataset is English DBpedia at 1.24 billion triples. QLever targets 10⁹–10¹¹ triples (Wikidata, UniProt), where lazy evaluation, FSST vocabulary compression and IRI encoding matter more. Sparkles' FSST option is measured up to the DBpedia 50M slice. Sparkles spills cursor sorts, groupings, DISTINCT and joins when spill is enabled, while queries whose eager state outgrows its budget still fail. |
| Data larger than RAM | Every measured store fits in RAM except the 28.7 GiB DBpedia index under a 14 GiB limit. |
| Other standard benchmarks | SP²Bench at 10M, LUBM(100) and BSBM Explore and BI at 10M have reference-host comparisons, though BI's driver cells have not been rerun on the current engine. The 100M BSBM and SP²Bench scales, LUBM(1000) and OWL2Bench query comparisons remain open. LDBC, WatDiv stress testing and the Wikidata query log have no harness, and the 28 synthetic queries are hand-picked over a regular graph shape. |
| Long write runs | Hours of sustained writes and compaction under load have not been compared across engines; `scripts/bench-longwrite.sh` has run only at smoke scale. |
| Spatial and query-time reasoning | GeoSPARQL workloads and query-time reasoning have no peer comparisons. |
| Result caches | Every published comparison runs with result caches disabled. |
| Other hardware | Most comparisons use the reference Intel host. Cloud and development-laptop measurements are identified separately; broader hardware coverage remains open. |

### Reproducing

[BENCHMARKING.md](../../BENCHMARKING.md) documents the harnesses, modes and outputs,
including local and remote regression comparisons. The commands below reproduce the
published comparisons.

- `mise run bench [people] [workdir]` or `scripts/bench.sh` runs the synthetic suite.
  `--engines NAME` re-measures one engine and merges it into an existing run, and
  `--answers-only` checks every engine's answers without timing (`ENGINES` and
  `ANSWERS_ONLY=1` for `scripts/bench.sh`).
- `mise run bench:watdiv [scale]` runs WatDiv, and `scripts/bench-text.sh` runs the
  full-text comparison.
- `mise run bench:sp2b 10m`, `mise run bench:lubm 100` and
  `SPARKLES_ALLOW_GPL_BENCHMARK=1 mise run bench:bsbm 10m` run the standard suites. Each
  writes a result record and run manifest naming the Sparkles commit, engine versions and
  data checksums, and `scripts/bench-regressions.py compare` compares two records.
- `mise run bench:billion full` runs DBpedia. Run it on a quiet machine right after
  `fstrim` so an untrimmed SSD does not stall loads or slow cold lookups.
  `scripts/bench-billion.sh` records the last trim, free space and I/O pressure, and warns
  if the trim is more than two days old or less than 30% of the file system is free.
- `mise run bench:bindings` runs the JVM, Python and Node.js comparison, and
  `mise run bench:jena` runs ordinary Jena use cases.
- `mise run bench:backup` runs backup measurements, and `scripts/geosparql-benchmark.sh`
  runs the GeoSPARQL Compliance Benchmark.
- Run each engine alone. Stop every server by its PID or port before starting the next,
  and keep builds and other benchmarks off the host.
