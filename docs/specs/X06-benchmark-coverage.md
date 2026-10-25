# X06: Benchmark coverage and refresh

> **Status:** partly shipped
>
> **Phases:** Phase 1 has shipped its tooling and part of its measurements. The result record,
> the run manifest, `scripts/bench-record.py`, the `compare` subcommand and the BSBM
> (Explore and BI), SP²Bench and LUBM drivers are in the repository. Published forge records
> cover the synthetic suite, SP²Bench, LUBM, Explore, full-text, WatDiv, DBpedia 50M and
> BI, with their answer and provenance limits recorded in BENCHMARKS.md. A standard
> refresh at one head, resolution of the open failures and answer disagreements, and
> regeneration of the BENCHMARKS.md tables from records remain incomplete.
> Phase 2 adds comparative spatial and vector runs, OWL2Bench and long write runs
> with compaction. Its OWL2Bench driver, the Jena rule reasoner comparison and the long
> write harness are built and ran at smoke scale, and its measurements are not done. Phase 3 adds LDBC SNB Interactive and BI and the Wikidata query-log
> replay above 1.24 billion triples. Phase 4 adds LDBC Graphalytics once the graph
> algorithms of F14 exist, and the wider hardware matrix.
>
> **User docs:** [BENCHMARKING.md](../BENCHMARKING.md) documents the harnesses and the commands
> of the full runs, and [BENCHMARKS.md](../BENCHMARKS.md) the methodology.
>
> The sections before the Outcome are the design as written before implementation. The
> [Outcome](#outcome) section at the end records how it landed.

This design was written from the published descriptions of the Berlin SPARQL Benchmark,
SP²Bench, LUBM, OWL2Bench, the LDBC Social Network Benchmark and LDBC Graphalytics, the
Wikidata SPARQL query logs and the paper that describes them, the GeoSPARQL Compliance
Benchmark paper, the ANN-Benchmarks paper, the public documentation of the engines it
compares, and the Sparkles benchmark harnesses and documents:
[BENCHMARKING.md](../BENCHMARKING.md), [BENCHMARKS.md](../BENCHMARKS.md),
`scripts/bench*.sh`, `scripts/bench-watdiv/watdiv.nix`, `scripts/bench-regressions.py`
and `docs/benchmarks/2026-10-04-regressions.json`. It builds on full-text search
([F03](F03-full-text-search.md)), vector search ([F04](F04-vector-search.md)), path
search ([F07](F07-path-search.md)), GeoSPARQL ([G01](G01-geosparql.md)), automatic
compaction ([C13](C13-automatic-compaction.md)), inference ([C08](C08-inference-freshness.md))
and the streaming executor ([X05](X05-streaming-query-execution.md)). The sources are
listed in §17.

## 1. Summary, goals, non-goals

[BENCHMARKS.md](../BENCHMARKS.md) compares Sparkles with QLever, Fluree, Oxigraph and
Jena TDB2 with Fuseki on a generated suite of 28 queries at 1.05M and 10.5M triples, on
WatDiv at 10.97M triples and on English DBpedia at up to 1.24 billion triples. It also
has full-text comparisons with Fuseki and QLever, and Sparkles-only measurements of
vector search, path search, spatial queries and the GeoSPARQL Compliance Benchmark.
Its section "What the benchmarks do not cover" names the gaps: no data above 1.24
billion triples, LUBM, BSBM, SP²Bench, WatDiv's stress testing and the Wikidata query
log not run, no long runs of writes and compaction against other engines, no spatial or
reasoning comparison with other engines, and one machine.

The roadmap's aim is to lead on every published benchmark that fits an RDF store. That
claim needs the standard benchmarks run the same way as the existing suite, and kept
current as the code changes. This spec defines the suites, the engines each one
compares, the fairness rules, the pinning of data and tools, a common result record,
the refresh protocol and the regression tracking.

**Goals**

1. Run the standard RDF benchmarks (BSBM, SP²Bench, LUBM, OWL2Bench), the graph
   benchmarks that an RDF store can answer (LDBC SNB Interactive and BI, LDBC
   Graphalytics), a replay of real queries (the Wikidata logs), the GeoSPARQL
   Compliance Benchmark against other engines, a vector recall and latency comparison
   in the manner of ANN-Benchmarks, long write runs with compaction, and data above
   1.24 billion triples.
2. Keep the rules of the current suite for every new one: each engine runs alone, no
   result cache, every answer is checked before anything is timed, and every loss above
   10% is listed under "Where Sparkles loses".
3. Make every run reproducible from the repository: generators and data pinned by
   checksum, engines pinned by version and checksum, seeds fixed, and the commands in
   [BENCHMARKING.md](../BENCHMARKING.md).
4. Write every result in one machine-readable record, so that runs at different heads
   can be compared by a script and regressions found without reading tables.
5. Refresh the published figures at a defined cadence and when a head changes a measured
   path.

**Non-goals**

- Audited LDBC results. Runs are labelled as derived from the LDBC workloads (§3.6).
- Benchmarks of features Sparkles lacks, beyond recording that they are missing. A suite
  that needs a missing feature waits for it (Graphalytics waits for F14).
- Tuning other engines beyond their documented settings for the workload.
- Continuous benchmarking on every pull request. The A/B harness `scripts/nsc-bench.sh`
  already covers relative checks of one change (§8).
- A dashboard service. The record is JSON in the repository and the tables are Markdown.

## 2. Current coverage

| Harness | Data | Engines | Status |
|---|---|---|---|
| `scripts/bench.sh` (modes default, `updates`, `mixed`, `cold`) | `scripts/gen-data.py`, 1.05M and 10.5M triples | Sparkles, Fuseki/TDB2, QLever, Fluree, Oxigraph | Published |
| `scripts/bench-watdiv.sh` | WatDiv v0.6, pinned by `watdiv.nix`, scale 100 | The same five | Published; basic testing only |
| `scripts/bench-billion.sh` | English DBpedia 2022.12, checksums in `scripts/bench-billion/dbpedia-2022.12.tsv` | Sparkles and QLever by default; Fluree, Jena and Oxigraph with `--engines` | Published |
| `scripts/bench-text.sh` | The synthetic data | Sparkles, Fuseki with jena-text, QLever | Published |
| `scripts/bench-vector.sh` | Generated clustered vectors | Sparkles only | Published |
| `scripts/bench-geo.sh`, `scripts/gen-geo.py` | Generated geometries | Sparkles only, index on and off | Published |
| `scripts/geosparql-benchmark.sh` | The GeoSPARQL Compliance Benchmark, fetched at a pinned commit when `SPARKLES_ALLOW_GPL_BENCHMARK=1` | Sparkles only | 187 of 206 |
| `scripts/bench-writes.py` and the durable-commit harness | The synthetic data | Sparkles and Oxigraph, the churn on all five | Published |
| `scripts/bench-regressions.py` | Store copies | One Sparkles binary against another | One record, `docs/benchmarks/2026-10-04-regressions.json` |
| `scripts/nsc-bench.sh` | The synthetic data | Two Sparkles builds on a Namespace instance | Not published; A/B only |
| `mise run bench:reasoner`, `bench:shacl`, `bench:bindings`, `bench:backup` | Generated | Sparkles; Jena, pyoxigraph and others in the bindings run | Published |

Each harness writes its own result files (`results/*.json` from hyperfine and the
harness, `results/summary.md`). The 2026-10-04 regression record has its own schema.
There is no common record and no history of runs.

## 3. Suites

Each suite gets a script `scripts/bench-<suite>.sh`, a directory `scripts/bench-<suite>/`
for its pins, mappings and query texts, and a `mise run bench:<suite>` task, in the
pattern of `bench-watdiv.sh`. Each sources `scripts/bench-lib.sh` for the engines'
servers and memory readings, and checks answers with `scripts/bench-answers.py`.

### 3.1 BSBM (Berlin SPARQL Benchmark)

- **Workload.** The Explore use case (a query mix that simulates a customer browsing
  products) and the Business Intelligence use case (analytic queries with grouping).
  The Explore and Update mix is run as a separate measurement under §3.9's write rules.
- **Data.** The BSBM generator at product counts that give about 10M and 100M triples,
  with a fixed seed. Run 1B triples in Phase 3 if the generator's time allows.
- **Engines.** The five of `bench.sh`.
- **Metrics.** Query mixes per hour with 1, 4 and 16 clients, as BSBM reports them, and
  the median and p95 of each query template. The test driver's warm-up runs are kept.
- **Notes.** The driver issues parameterized queries over HTTP, so it measures the
  protocol path the existing suite measures.

### 3.2 SP²Bench

- **Workload.** The 17 SP²Bench queries over DBLP-like data, which stress OPTIONAL,
  FILTER, UNION, negation and large intermediate results.
- **Data.** The SP²Bench generator at 1M, 10M and 100M triples. The generator is
  deterministic for a given size.
- **Engines.** The five of `bench.sh`.
- **Metrics.** Per-query times warm and cold, with a timeout of 600 s. A timeout is a
  reported failure, as in the existing suite.

### 3.3 LUBM

- **Workload.** The 14 LUBM queries. Several need RDFS or OWL inference to return their
  complete answers.
- **Data.** The UBA generator at LUBM(100) and LUBM(1000), with seed 0 and a fixed start
  index, plus the univ-bench ontology.
- **Two measurements.**
  1. *Inference.* Sparkles materializes with `sparkles infer --profile owl-rl` (the
     reasoner's `Profile::OwlRl`) and with `--profile rdfs`. The time, the peak RSS and
     the number of derived triples are reported. Jena's rule reasoners are run where they
     finish within the memory limit. Engines without a reasoner are listed as such.
  2. *Queries.* Every engine loads the same materialized N-Triples that Sparkles wrote,
     so the query comparison is over equal data. The answers are checked against the
     LUBM reference answers where they are published for the scale, and against the
     majority otherwise.
- **Engines.** The five of `bench.sh` for queries.

### 3.4 OWL2Bench

- **Workload.** The OWL2Bench university ontology and data in its OWL 2 RL profile, and
  its SPARQL queries. The EL, QL and DL profiles are run only where the RL subset
  materialization gives a defined answer, and the report says which.
- **Data.** The OWL2Bench generator at its documented university counts, seeded.
- **Engines.** Sparkles' materialization against Jena's OWL rule reasoner. Query times
  over the materialized data on all five engines, as in §3.3.
- **Purpose.** It measures reasoning cost, the input to F16 and F17.

### 3.5 GeoSPARQL

- **Compliance.** `scripts/geosparql-benchmark.sh` gains `--engines`, and runs the
  benchmark against Jena's GeoSPARQL Fuseki and against any engine in the matrix whose
  documentation claims GeoSPARQL functions. Scores are per requirement, as in
  BENCHMARKS.md today. The benchmark stays fetched at run time behind
  `SPARKLES_ALLOW_GPL_BENCHMARK=1`, since it is GPL-2.0, and none of it is committed.
- **Performance.** The queries of `scripts/gen-geo.py` run on Sparkles with its spatial
  index, Jena GeoSPARQL Fuseki with its spatial index, and QLever's spatial joins where
  its syntax can express the query. A query an engine cannot express is marked, not
  rewritten into a different query.

### 3.6 LDBC SNB Interactive and BI

- **Data.** The LDBC SNB Datagen (Apache-2.0) at SF1 and SF10, and SF100 in Phase 4,
  pinned by release and checksum. Datagen writes CSV. A checked-in mapping
  (`scripts/bench-ldbc/mapping/`) turns it into RDF with `sparkles csv` and CSVW
  metadata ([C05](C05-tabular-imports.md)). The mapping uses one IRI per entity, one
  predicate per property, and RDF 1.2 reifiers for the creation dates of `knows` and
  `likes` edges, as F01 §2.4 reads relationship properties.
- **Queries.** SPARQL versions of the Interactive complex reads (IC1 to IC14), short
  reads (IS1 to IS7) and updates (IU1 to IU8), and of the BI queries and their
  insert and delete batches, written for this repository from the specification's
  descriptions. Shortest-path queries (IC13, IC14 and the BI path queries) use
  `SERVICE path:search` (F07). Each query file states which choke points of the
  specification it exercises. Once F01 lands, the Cypher versions run too, and the
  difference between the two is reported.
- **Engines.** Sparkles, Jena, QLever, Oxigraph and Fluree on the SPARQL texts. Property
  graph engines on their own Cypher texts, where the license allows a local run and a
  published comparison (§4): Neo4j Community Edition, Memgraph and Kùzu are the
  candidates. Each answer is checked against the LDBC validation parameters and expected
  results shipped with the driver where they exist for the scale factor.
- **Metrics.** For Interactive, the throughput the driver sustains at a fixed time
  compression ratio and the latency per query type. For BI, the power test (each query
  alone) and the throughput test, with the per-query times.
- **Labelling.** LDBC's fair-use rules reserve the benchmark's name for audited results.
  Published figures are labelled as runs of workloads derived from LDBC SNB, with the
  scale factor and the deviations listed.

### 3.7 LDBC Graphalytics

- **Workload.** BFS, PageRank, weakly connected components, community detection by
  label propagation, local clustering coefficient and single-source shortest paths, on
  the Graphalytics datasets that fit the host.
- **Precondition.** Sparkles answers these through the graph-algorithm library of F14,
  called by `SERVICE`. The suite starts with F14's Phase 1. Until then the report says
  that the workload is not supported.
- **Validation.** Outputs are compared with the reference outputs the benchmark ships,
  with its tolerance for PageRank and the clustering coefficient.
- **Engines.** The property-graph engines of §3.6 that ship these algorithms in their
  free editions, under the same license rule.

### 3.8 Wikidata query-log replay

- **Data.** A Wikidata "truthy" N-Triples dump of a fixed date, pinned by URL and
  checksum in `scripts/bench-wikidata/dump.tsv`, at full size (several billion triples)
  and at a subject-hash sample of about 1B triples made the way `bench-billion.sh`
  samples DBpedia. This is the run above 1.24 billion triples.
- **Queries.** The published Wikidata SPARQL query logs (CC0), organic queries only. A
  fixed-seed sample of 10,000 queries is stratified by the log's query shapes. The
  sample is checked in as a list of log line ids, with the script that extracts them.
  Queries that use `SERVICE wikibase:label`, `wikibase:mwapi` or other Blazegraph-specific
  services are rewritten only where a standard equivalent exists (the label service as
  `OPTIONAL` label patterns with a language order). Otherwise they are excluded, and the
  exclusions are counted by reason.
- **Engines.** Sparkles and QLever at full size. Jena, Oxigraph and Fluree at the 1B
  sample where they load it within the host's limits.
- **Metrics.** The share of queries answered within 60 s, the median and p95 latency
  over the queries every engine answered, answer agreement, and the loss table.

### 3.9 Sustained writes with compaction

- **Workload.** One hour of a mixed load per engine on the 10.5M and DBpedia 50M stores,
  and one four-hour run of Sparkles on the 10.5M store: 16
  readers running a rotating set of suite queries and one to four writers committing a
  mix of single-triple and 100-triple changes generated by `scripts/bench-writes.py`
  with a fixed seed. Automatic compaction (C13) stays at its defaults.
- **Engines.** The five of `bench.sh`, each with its documented maintenance: Oxigraph's
  RocksDB compaction runs as configured, TDB2 runs `tdb2.tdbcompact` on its documented
  schedule if the run requires it, QLever keeps updates in memory (§5.1).
- **Metrics.** Per minute: committed writes, read and write p50 and p99, RSS, disk use,
  and Sparkles' compaction events (start, kind, duration, writer lock time). At the end:
  the state checked against the expected final state, after a SIGKILL and a restart, and
  after a normal stop and a restart.

### 3.10 Vector search in the manner of ANN-Benchmarks

- **Data.** Two ANN-Benchmarks datasets with their ground truth (one Euclidean, one
  angular), and a set of 384-dimension sentence embeddings, at 1M vectors. Each loads as
  `spk:vector` literals as `scripts/bench-vector.sh` does now.
- **Measurement.** For each index setting, recall@10 against the ground truth and
  queries per second with one and 16 clients, plotted as a recall–throughput curve, plus
  build time, index size and peak RSS. HTTP and in-process numbers are kept apart: the
  Sparkles in-process number goes through the Rust API, so that it compares with
  libraries.
- **Engines.** Sparkles' HNSW index; USearch directly (the library F04 §8 recommends),
  as the bound on what the index could reach; a vector database that runs locally under
  a permissive license (Qdrant is the candidate); and pgvector. A hybrid query (a vector
  top-k joined to a structural filter) is run on the engines that can express it.

## 4. Engines and licenses

| Engine | License | Source in the harness | Suites |
|---|---|---|---|
| Sparkles | The repository's | The build under test | All |
| Apache Jena TDB2 and Fuseki, GeoSPARQL Fuseki | Apache-2.0 | nixpkgs, as today | All RDF suites, GeoSPARQL |
| QLever | Apache-2.0 | nixpkgs, as today | All RDF suites |
| Oxigraph | MIT or Apache-2.0 | nixpkgs, as today | All RDF suites |
| Fluree | BUSL-1.1 | The checksum-verified release binary, as today | All RDF suites |
| Neo4j Community Edition | GPL-3.0 | Release archive pinned by checksum, run as a separate process | LDBC SNB, Graphalytics |
| Memgraph | Source-available; its terms are checked before Phase 3 | Release package pinned by checksum | LDBC SNB, Graphalytics |
| Kùzu | MIT | Release binary pinned by checksum, while a maintained release exists | LDBC SNB, Graphalytics |
| USearch | Apache-2.0 | Python package pinned by version | Vectors |
| Qdrant | Apache-2.0 | Release binary pinned by checksum | Vectors |
| pgvector | PostgreSQL License | nixpkgs | Vectors |

Rules:

1. Other engines are run as separate binaries. Their source code is never read for this
   work, vendored or linked. A GPL-licensed engine or benchmark is fetched only when
   `SPARKLES_ALLOW_GPL_BENCHMARK=1` is set, as `geosparql-benchmark.sh` does today.
2. An engine whose license or terms forbid publishing benchmark results is not run.
   Commercial editions (Neo4j Enterprise, GraphDB, Stardog, RDFox, Virtuoso's commercial
   edition) are out of scope unless their terms allow it and a free license for the run
   exists. The license check is recorded in the run manifest (§6).
3. Each engine's configuration file or command line is checked in with the suite, and
   every setting that differs from the engine's default is justified in a comment that
   cites the engine's documentation.

## 5. Fairness rules

These extend [BENCHMARKS.md § Setup](../BENCHMARKS.md#setup).

### 5.1 Durability parity

Sparkles acknowledges a write after its WAL record and new terms are synchronized.
Oxigraph writes its RocksDB WAL without synchronizing it, QLever keeps updates in
memory, and TDB2 synchronizes. Every write table carries a *durability* column with one
of `synced`, `written` (in the OS cache) or `memory`, taken from each engine's
documentation. The long write run checks the class from the device counters. Before each
run it records the writes of the device that holds the store and the flushes of the disks
under it for 60 seconds with the server idle, and it reports the run's counts net of that
rate, per committed write request. With one writer, an engine that synchronizes each commit
makes at least one device write per commit beyond the idle rate, and an engine that leaves
its log in the OS cache makes far fewer. The flushes alone do not decide the class, because
a write with forced unit access, such as an `O_DIRECT` and `O_DSYNC` write, is durable
without a flush. With several writers, group commit lets concurrent commits share one write,
so a `synced` engine may show fewer than one write per commit, and the ratio is read only
from a single-writer run. Rankings of write throughput and
latency are made within a durability class. A comparison across classes is printed, but
it is not counted as a loss or a win in the summary, and the loss table names the class.

Sparkles is never run with synchronization off for a published table.

### 5.2 Warm and cold

- *Warm*: the engine has answered every query of the suite once, and the timed runs
  follow warm-ups, as today.
- *Cold*: the server is stopped, its files are evicted from the page cache with
  `POSIX_FADV_DONTNEED` on an isolated store copy, and it is started again before each
  sample, as `bench-regressions.py --cold` does. The time to ready is reported apart.
- *Device state*: cold runs at DBpedia scale and above run after `fstrim`, with the trim
  age and free space recorded, as `bench-billion.sh` does.

### 5.3 Caches and memory

- No result cache for any engine, as today. Block caches and page caches stay at their
  defaults, and the Sparkles block cache size is recorded.
- Every engine runs in a systemd scope with the same memory limit and no swap
  (`SERVER_MEM_MAX`). The limit is part of the record.
- Each engine runs alone. The client is pinned to a CPU the servers do not use.

### 5.4 Queries

- The query texts are the same for every SPARQL engine. A dialect change that an engine
  needs (for example a QLever spatial `SERVICE` or a Fluree setting) lives in a separate
  file per engine with a comment, and the answers must still agree.
- No engine-specific rewriting that changes the algorithm, such as adding a hint that
  only one engine understands, unless every engine gets its own equivalent hint.
- Answers are checked first with `scripts/bench-answers.py`. An engine whose answer
  differs from the majority is marked and not ranked, as today. For suites with
  reference answers, the reference is the majority.

### 5.5 Failures and timeouts

A timeout, a crash or an out-of-memory kill is a result. It is printed in the table,
counted in the summary, and never replaced by a rerun with other settings unless the
rerun is reported too.

## 6. Pinning and the run manifest

- **Generators.** A generator built from source gets a Nix derivation next to its
  script, pinned by checksum, with patches that make its output a function of its
  inputs, in the pattern of `scripts/bench-watdiv/watdiv.nix` (fixed seeds, fixed dates,
  pinned word lists). A Java generator (BSBM, the LDBC driver) is pinned by its release
  jar's checksum and run on the JDK from `flake.lock`.
- **Data.** Downloaded data is listed in a TSV of URL, size and SHA-256, like
  `scripts/bench-billion/dbpedia-2022.12.tsv`. Generated data records its generator
  hash, parameters and the SHA-256 of its normalized N-Triples.
- **Licenses.** Each pin records the generator's and data's license. A generator or
  query set without a license, or with one that forbids redistribution, is fetched at
  run time and never committed. The DBpedia SPARQL Benchmark templates are already
  handled this way: the templates are not copied.
- **Manifest.** Each run writes `manifest.json`: the Sparkles commit and binary
  SHA-256, every engine's version and binary hash, the host (CPU, RAM, disk model,
  kernel, file system, mount options), the memory limit, the CPU pinning, the data
  hashes, the start and end times, and the license check of §4.

## 7. The result record

Every suite writes one JSON file per run in the same shape, which
`scripts/bench-record.py` (new) builds from the harness's own outputs:

```json
{
  "record": 1,
  "suite": "bsbm-explore",
  "scale": "10m",
  "manifest": { "sparkles": { "commit": "…", "sha256": "…" }, "host": "forge", "…": "…" },
  "settings": { "warmup": 2, "runs": 10, "timer": "hyperfine+curl", "resultCache": false },
  "results": [
    {
      "engine": "sparkles",
      "query": "q05",
      "mode": "warm",
      "clients": 1,
      "durability": null,
      "samples_ms": [12.1, 12.4, 11.9],
      "median_ms": 12.1,
      "p95_ms": 12.4,
      "answer": { "rows": 10, "hash": "…", "agrees": true },
      "status": "ok"
    }
  ],
  "memory": { "sparkles": { "rss_after_mib": 412, "peak_mib": 600, "cache_mib": 64 } }
}
```

`status` is `ok`, `timeout`, `error`, `oom` or `wrong-answer`. Throughput results use
`qps` or `qmph` in place of the sample list. Long runs (§3.9) write a time series as
`series` with one entry per minute.

Published records go to `docs/benchmarks/<date>-<suite>-<host>.json`, compressed with
zstd when larger than 1 MiB. The existing 2026-10-04 record stays as it is. The
Markdown tables in BENCHMARKS.md are produced from the records by
`scripts/bench-summary.py` and its successors, so every published figure has a record
behind it.

## 8. Refresh protocol

A *head* is the commit at which a refresh starts. A refresh has three levels.

| Level | When | What | Where |
|---|---|---|---|
| A/B | A change that touches the executor, the store or the writer | `scripts/nsc-bench.sh` against the change's base, plus `bench-regressions.py` on forge for cold paths | Namespace, forge |
| Standard | Every two weeks, and before BENCHMARKS.md is edited | The synthetic suite (all modes), WatDiv, full-text, BSBM, SP²Bench, LUBM(100), the GeoSPARQL comparison and vectors | forge |
| Full | Every quarter, and when a roadmap item from Tier 2 or 3 lands | Everything, including DBpedia full, Wikidata, LDBC SF10, LUBM(1000), OWL2Bench and the four-hour write runs | forge and the large host |

Rules:

1. A refresh runs every engine at the head, not only Sparkles. A competitor's figures
   may be reused from an earlier run only when its version and the host are unchanged,
   and the record says so.
2. Competitor versions are updated at the start of a full refresh to the latest release
   in nixpkgs or upstream, and the change is listed.
3. BENCHMARKS.md is updated from the records of one refresh, with the measurement
   window in its context table, as now.
4. A loss found in a refresh goes into "Where Sparkles loses" in the same change, with
   a link to the roadmap item or spec that addresses it, or a statement that none does.

## 9. Regression tracking

`scripts/bench-regressions.py` becomes the comparison tool for records:

```sh
scripts/bench-regressions.py compare docs/benchmarks/2026-11-01-bsbm-explore-forge.json \
    docs/benchmarks/2026-11-15-bsbm-explore-forge.json
```

It pairs results by suite, scale, engine, query, mode and clients, and prints the ratio
of medians and whether the sample ranges overlap. A Sparkles query is a *regression*
when its median is more than 10% slower (the tie threshold of BENCHMARKS.md) and the
ranges do not overlap, or more than 5% slower in two consecutive refreshes. A
regression blocks the BENCHMARKS.md update until it is explained in the change or
fixed. A changed answer hash is always reported, whatever the timing.

The existing measurement modes stay: the warm and cold store-copy mode, the CPU pinning
and the memory cap.

## 10. Hardware matrix

| Host | Purpose | Notes |
|---|---|---|
| `forge` (Intel Core i5-13500, 16 GiB, NVMe) | Standard and full refreshes up to DBpedia | The current reference machine |
| `atlas` (the same CPU) | Bindings | As today |
| A large host, at least 128 GiB of RAM and 4 TB of NVMe | Wikidata at full size, LDBC SF100, data larger than RAM by design | Rented by the hour for full refreshes |
| A Namespace `linux/amd64:8x16` instance | A/B checks | As today. Its noise limits it to changes of about 10% or more (BENCHMARKING.md) |
| An aarch64 server | The standard suites on ARM | The bindings already build for aarch64 |
| Network block storage | Cold reads on cloud disks, where latency per read is higher than NVMe | The same suite, cold mode only |

Each published table names its host. Figures from different hosts are never placed in
one ranking.

## 11. Architecture

- `scripts/bench-lib.sh` gains engine entries for the property-graph engines and the
  vector engines, with the same start, stop, ready and RSS functions.
- New directories: `scripts/bench-bsbm/`, `scripts/bench-sp2b/`, `scripts/bench-lubm/`,
  `scripts/bench-owl2bench/`, `scripts/bench-ldbc/`, `scripts/bench-graphalytics/`,
  `scripts/bench-wikidata/`, `scripts/bench-longwrite/` and
  `scripts/bench-ann/`, each with its pins (`*.nix` or `*.tsv`), queries or query-list
  extractors, and a `summary.py` if the suite's metrics differ from per-query times.
- `scripts/bench-record.py` (new) converts a results directory into the record of §7.
  `scripts/bench-regressions.py` gains the `compare` subcommand of §9 and keeps its
  current store-copy mode.
- `mise.toml` gains `bench:<suite>` tasks next to the existing ones.
- `docs/BENCHMARKING.md` documents each harness. `docs/BENCHMARKS.md` gains a section
  per suite and keeps its summary, loss table and methodology.
- No engine code changes are part of this spec. A suite that exposes a missing feature
  or a defect files it against the owning spec.

## 12. Phases

| Phase | Content | Cost | Gate |
|---|---|---|---|
| 1 | The record (§7), the manifest (§6), `bench-record.py`, `compare`, the refresh protocol; BSBM Explore and BI, SP²Bench, LUBM(100); a standard refresh of every existing suite at the head | M | A1–A5 |
| 2 | GeoSPARQL compliance and performance against other engines; vectors (§3.10); OWL2Bench; LUBM(1000); one-hour write runs per engine and one four-hour Sparkles run; the aarch64 host | M | A6–A9 |
| 3 | LDBC SNB Interactive and BI at SF1 and SF10; the Wikidata replay at the 1B sample and at full size on the large host | L | A10–A12 |
| 4 | Graphalytics after F14; LDBC SF100; network block storage; Cypher texts after F01 | M | A13 |

## 13. Acceptance examples

- **A1 — Reproducible generator.** Two runs of `mise run bench:bsbm 10m` on different
  days produce data with the same SHA-256 and the same query parameters.
- **A2 — Answer checks first.** In BSBM and SP²Bench, every engine's answer to every
  query is fingerprinted before timing. A deliberately wrong engine answer (a test
  fixture) is marked and not ranked.
- **A3 — One record per run.** `bench-record.py` turns each of the existing harnesses'
  result directories (synthetic suite, WatDiv, DBpedia, full-text) into a record that
  validates against the schema, and `bench-summary.py` regenerates the current
  BENCHMARKS.md tables from those records without a numeric change.
- **A4 — Regression found.** Comparing a record with a copy in which one Sparkles median
  is 15% higher and its range does not overlap reports exactly that query as a
  regression. A changed answer hash is reported as such.
- **A5 — Refresh at a head.** A standard refresh at one commit produces records for
  every suite in its row of §8, a manifest naming that commit, and a BENCHMARKS.md diff
  whose figures all come from those records.
- **A6 — Durability classes.** The long write run's tables put Sparkles and TDB2 in the
  `synced` class and Oxigraph in `written`. A single-writer run confirms it by the device
  writes per commit net of the idle baseline (§5.1), at least one for the `synced` engines
  and well below one for Oxigraph, with the flushes reported beside them. The summary does
  not rank across classes.
- **A7 — GeoSPARQL comparison.** The compliance benchmark runs against Sparkles and Jena
  GeoSPARQL Fuseki from one command, gated by `SPARKLES_ALLOW_GPL_BENCHMARK=1`, and
  reports per-requirement scores for both.
- **A8 — Vector curve.** The ANN run produces recall@10 and queries per second for at
  least five `ef` settings per engine on two datasets, with ground truth from the
  dataset, not from Sparkles' exact search.
- **A9 — Long run.** The four-hour run records one point per minute for every metric of
  §3.9, the compactions that ran, and a final state check and reopen that pass.
- **A10 — LDBC labelling.** The SNB report states the scale factor, that the run is not
  audited, and every deviation from the specification.
- **A11 — Wikidata exclusions.** The replay reports how many log queries were sampled,
  rewritten and excluded, by reason, and the share answered within 60 s per engine.
- **A12 — Above 1.24B.** A load and the replay of the full Wikidata truthy dump on the
  large host complete for Sparkles and QLever, with load time, peak RSS and index size.
- **A13 — Graphalytics validation.** Sparkles' outputs for BFS, WCC and PageRank on one
  Graphalytics dataset validate against the reference outputs.

## 14. Testing and measurement

- The harness scripts are checked by `shellcheck` and run in CI in an `ANSWERS_ONLY`
  smoke mode at the smallest scale for Sparkles alone, so a broken script is found before
  a refresh. The smoke mode times nothing. It leaves out every suite whose benchmark or
  engine is GPL-licensed: `scripts/geosparql-benchmark.sh` refuses to fetch the
  GeoSPARQL Compliance Benchmark without `SPARKLES_ALLOW_GPL_BENCHMARK=1`, and CI never
  sets it.
- `bench-record.py` and `compare` have unit tests on fixture result directories.
- Each suite's first published run includes a control: two runs of the same Sparkles
  binary, compared with `compare`, to measure the noise floor on that host. The noise
  floor is printed next to the suite's tables.

## 15. Rejected alternatives

- **Reusing third-party harness code from GPL projects.** Not done. Designs are taken
  from the benchmark papers and specifications, and the scripts are written here.
- **Publishing only Sparkles-favourable suites.** Every suite in §3 is published with
  its losses, under the same 10% rule.
- **Disabling Sparkles' WAL synchronization for write comparisons.** Durability is part
  of the product's contract. §5.1 separates the classes instead.
- **Normalizing every engine to one cache size.** Engines size caches differently and
  some have none. Defaults are kept, recorded, and the memory used is reported next to
  the speed, as BENCHMARKS.md does today.
- **One aggregate score.** A geometric mean over unlike suites hides losses. Each suite
  reports its own metrics, and the summary lists wins and losses per suite.
- **A hosted benchmarking service.** JSON records in the repository suffice and keep the
  history reviewable.

## 16. Open questions

1. Whether BSBM's generator and test driver, SP²Bench's generator and OWL2Bench's
   generator have licenses that allow committing their query texts, or whether they are
   fetched at run time like the GeoSPARQL benchmark.
2. Whether Memgraph's current license terms allow publishing comparative results.
3. Which large host to rent, and whether the full refresh runs there every quarter or
   only for Wikidata and SF100.
4. Whether the Wikidata label-service rewrite counts as a dialect change (§5.4) or as an
   exclusion.
5. Whether to add WatDiv's stress-testing mode, which BENCHMARKS.md lists as not run, to
   Phase 1 or leave it after BSBM and SP²Bench.

## 17. Sources

- C. Bizer and A. Schultz, "The Berlin SPARQL Benchmark", International Journal on
  Semantic Web and Information Systems 5(2), 2009.
- M. Schmidt, T. Hornung, G. Lausen and C. Pinkel, "SP²Bench: A SPARQL Performance
  Benchmark", ICDE 2009.
- Y. Guo, Z. Pan and J. Heflin, "LUBM: A benchmark for OWL knowledge base systems",
  Journal of Web Semantics 3(2–3), 2005.
- G. Singh, S. Bhatia and R. Mutharaju, "OWL2Bench: A Benchmark for OWL 2 Reasoners",
  ISWC 2020.
- O. Erling et al., "The LDBC Social Network Benchmark: Interactive Workload", SIGMOD
  2015, and G. Szárnyas et al., "The LDBC Social Network Benchmark: Business
  Intelligence Workload", PVLDB 16(4), 2022, with the LDBC SNB specification and the
  LDBC fair-use policies.
- A. Iosup et al., "LDBC Graphalytics: A Benchmark for Large-Scale Graph Analysis on
  Parallel and Distributed Platforms", PVLDB 9(13), 2016.
- S. Malyshev, M. Krötzsch, L. González, J. Gonsior and A. Bielefeldt, "Getting the Most
  out of Wikidata: Semantic Technology Usage in Wikipedia's Knowledge Graph", ISWC 2018,
  for the query logs.
- M. Jovanovik, T. Homburg and M. Spasić, "A GeoSPARQL Compliance Benchmark", ISPRS
  International Journal of Geo-Information 10(7), 2021.
- M. Aumüller, E. Bernhardsson and A. Faithfull, "ANN-Benchmarks: A Benchmarking Tool
  for Approximate Nearest Neighbor Algorithms", Information Systems 87, 2020.
- G. Aluç, O. Hartig, M. T. Özsu and K. Daudjee, "Diversified Stress Testing of RDF Data
  Management Systems", ISWC 2014, for WatDiv.
- The public documentation of QLever, Oxigraph, Apache Jena (TDB2, Fuseki, GeoSPARQL),
  Fluree, Neo4j, Memgraph, Kùzu, USearch, Qdrant and pgvector, for their settings and
  licenses.
- The Sparkles benchmark scripts and documents named above, and the specs C05, C08,
  C13, F01, F03, F04, F07, F14, G01 and X05.

## Outcome

Phase 1's tooling is built. The first batch of the standard refresh ran on the reference
host on 2026-10-18 and 2026-10-19, and BENCHMARKS.md has its synthetic, SP²Bench and LUBM
figures. BENCHMARKS.md also includes the BSBM Explore comparison at 10M from
2026-10-19, the isolated full-text queries at 1.05M from 2026-10-20 and BSBM BI at
10M from 2026-10-20 to 2026-10-21, with
versioned records of their samples, answer fingerprints and engine failures. On
2026-10-23 Sparkles was measured again in all of these suites at the settled head
6699f27d, as the settled-head refresh below describes. The rest of the refresh is listed
under "Left for Phase 1".

**Record and manifest.** `scripts/bench-record.py` has four subcommands. `manifest` writes
the run manifest of §6 with the commit of the scripts' checkout, the Sparkles binary's
checksum and its own commit when the caller knows it (`SPARKLES_COMMIT`), every engine's
version, binary checksum and license, the Nix store path of an engine taken from nixpkgs, the host, the memory limit, the client CPU, the data checksums
and the license check. `finish` adds the end time. `build` turns the results directory of
`scripts/bench.sh` or of a suite into the record of §7, and `validate` checks a record
against the shape described in the script's header. The record also carries the load
numbers and the memory readings, and the LUBM record carries the materialization costs
under `inference`. `build --pool-instances` merges query instances named `<template>-<i>`
into one result per template, with the samples pooled. The agreement of an answer is judged
against the majority of the engines that answered, as in `bench-summary.py`, and a result
that disagrees is recorded as `wrong-answer`. An intentionally unchecked DESCRIBE answer has
no fingerprint in the record and does not vote for the majority. The collector no
longer treats matching unchecked markers as proof of agreement. The durability class of §5.1 is filled in for
update results only, and no suite has a long write run yet.

**Comparison.** `scripts/bench-regressions.py compare` takes two or more records, pairs
results by suite, scale, engine, query, mode and clients, and applies the rules of §9 to the
compared engine. It reports a regression when the median is more than 10% slower and the
sample ranges are disjoint, or more than 5% slower in two consecutive comparisons of a
chain. It also reports a changed answer hash and a result that stopped being `ok`, and it
exits with status 1 when it reports anything. The old store-copy mode is unchanged. Unit
tests for both tools run in `mise run test:bench-scripts`, which is part of `mise run ci`.
They cover A4 on fixture records and the marking of a wrong answer from A2.

**Suites.** `scripts/bench-suite-lib.sh` is the common run of the three suites. It loads one
N-Triples file into the selected engines, serves them, checks every answer, times each query
with hyperfine, optionally measures cold restarts, and writes the manifest, the record and
`summary.md`. The timed server of each engine first answers every selected query once, so
it is as warm as the one that was checked, and then each engine's queries and query mixes
run with no other server up. To share this code with the existing harness, the load, the server start-up,
the answer check and the cold mode moved from `scripts/bench.sh` into `scripts/bench-lib.sh`.
`bench.sh` was checked afterwards in its default and cold modes. `bench-answers.py` now
also fingerprints ASK, CONSTRUCT and DESCRIBE results. A CONSTRUCT or DESCRIBE answer is a
set of triples compared by term identity before the value digest normalizes the terms, so
`1` as an integer and `1.0` as a decimal stay two triples. An `order` digest is recorded only
for ORDER BY columns whose order SPARQL defines, and rows that tie at a LIMIT or OFFSET
boundary are compared by their key values only when a second request shows that the tie
crosses the cut. `bench-lib.sh` picks the Accept type of a
timed request from the query form. `nixbin` now builds engines from the nixpkgs
revision in `flake.lock`, so engine versions follow the repository and the manifest records
them. This can change the Jena, QLever and Oxigraph versions of an existing harness run
compared with the older registry lookup.

* *SP²Bench* (`scripts/bench-sp2b.sh`). The generator is built from the checksum-pinned v1.01
  release by `scripts/bench-sp2b/sp2b.nix` as a 64-bit binary. Two runs of the same size gave
  the same file. The 17 queries come from the same release. All five engines loaded the 50k
  document and agreed on the first queries, and the Sparkles-only smoke run completed.
* *LUBM* (`scripts/bench-lubm.sh`). The generator and ontology are downloaded at run time
  against `scripts/bench-lubm/pins.tsv` because they carry no license text. The queries are
  written out from the paper in `scripts/bench-lubm/queries.txt`. Two generations of
  LUBM(1) in different directories gave the same N-Triples file. That needed two
  normalizations after conversion. The two header triples that name each generated file by
  its local path are dropped, and the ontology's blank nodes are relabeled in order of
  appearance. The data has 103,380 triples with the ontology, and `sparkles infer` derives
  43,557 triples with the `rdfs` profile and 84,748 with `owl-rl`. Query 13 returns one row
  at LUBM(1), and a direct query
  over the generated data confirms that only one person has a degree from University0. The
  answers have not been compared with published reference counts, because the scripts use the
  majority of the engines as the reference. Jena's reasoners are not run. Fluree's public
  documentation describes query-time reasoning, which the script does not enable, so Fluree
  answers over the same materialized file as the other engines. Timing it with its own
  reasoning on is not done.
* *BSBM* (`scripts/bench-bsbm.sh`). The GPL-3.0 tools run as separate processes and are
  fetched only with `SPARKLES_ALLOW_GPL_BENCHMARK=1`. Generation is deterministic. The
  test driver's own query mixes are recorded through a stub endpoint
  (`scripts/bench-bsbm/instantiate.py`) to get the query instances for the answer check and
  the per-template timings, and the driver itself measures query mixes per hour with 1, 4
  and 16 clients. The Explore and BI use cases both instantiate. The Explore and Update mix
  is not built. The scale labels follow BSBM's published dataset sizes, which are 2,785
  products for 1m, 28,480 for 10m and 284,826 for 100m, and the manifest records the triple
  count that was generated. At 1m the generator gives 1,000,312 triples, against 1,000,313
  in the V2.0 specification. The query mix check compares only runs in which the driver
  succeeded, gives each engine one vote, and does not compare runs with different mix
  counts.

**Smoke runs.** SP²Bench at 50k, LUBM(1) and BSBM with 2000 products ran end to end with
Sparkles alone on a development machine. SP²Bench at 50k also ran with all five engines,
with a 20 second request timeout, on a machine that was compiling at the same time, so its
times mean nothing. The run showed what the harness is for. Oxigraph and QLever returned
other answers than the majority for several queries (q4, q5a, q5b, q6 and q8) and were
marked and left out of the ranking, and Jena, Fluree and Oxigraph timed out on some
queries and were recorded as `timeout`.

The Oxigraph disagreements came from the harness. `oxigraph load --file` parses N-Triples
in parallel chunks and splits a blank node label that occurs in two chunks into two nodes,
so it held 4,727 blank nodes for SP²Bench at 50k triples instead of 4,198, and 109,384
instead of 86,870 at 1M. The harness now loads Oxigraph from standard input with
`--format nt`, which keeps the labels whole and takes about 3x as long to load at 1M
(6.6 s against 2.3 s). With that load Sparkles, Oxigraph and QLever agree on q4, q5a, q5b
and q6. QLever still differs on q6 (1,959 rows against 1,769) and q8 (0 rows against 264),
which looks like the engine's own behavior. The synthetic data and WatDiv contain no blank
nodes, and the DBpedia load already read standard input, so no published Oxigraph result
needs a rerun. Sparkles was slow on q5a and q12a at this size, which a run on the
reference host should confirm before it is listed under "Where Sparkles loses".

**Phase 2 tooling.** Built on 2026-10-18 under roadmap item 1, with no published figures
yet.

* *Materialization.* The LUBM and OWL2Bench drivers share `suite_infer` and `jena_reason`
  in `scripts/bench-suite-lib.sh`. Each materialization records, beside its time, peak RSS
  and derived triples, the reasoner's own peak heap estimate and thread count from
  `sparkles infer --status`, and the SHA-256 of the sorted derived triples, which are what
  F16's acceptance run A1 compares between the head and the commit before F16's Phase 1.
  `INFER_PROFILES` limits the materializations, which LUBM(1000) needs for disk space.
  Jena's rule reasoners (`rdfs`, `owl-micro`, `owl-mini`, `owl`) run through `sparql` over
  an inference model of an in-memory copy of the data, with the start of the JVM and the
  parse measured apart, within a time and heap limit. A run that hits a limit is recorded
  with its status. On LUBM(1) the `rdfs` profile derives 43,557 triples with a peak heap
  estimate of 11 MiB.
* *OWL2Bench* (`scripts/bench-owl2bench.sh`). The generator and the TBoxes are Apache-2.0
  and the queries CC-BY-4.0, so open question 1 is settled for OWL2Bench, and both are
  still fetched at run time. The current release jar of the generator, from July 2026,
  writes its ABox in the namespace `https://kracr.iiitd.edu.in/OWL2Bench#` while the TBoxes
  use `http://benchmark/OWL2Bench#`, so no TBox axiom applies to its data. The script pins
  the jar of 2020 that made the paper's datasets, runs it on Java 8 from nixpkgs, and refuses
  data in the other namespace. Two generations of one university gave the same file of
  54,815 triples. The 18 queries whose profile list contains RL run over the `owl-rl`
  materialization. At one university Sparkles derived 1,399,968 triples in 5 to 6 seconds
  with a peak heap estimate of 499 MiB on a loaded development machine. Of these, 1,311,932
  are `hasSameHomeTownWith`, a symmetric and transitive property whose closure is a clique
  per home town. All five engines loaded the materialized file and agreed on every query.
  Jena's `rdfs` reasoner finished in about a second, and its OWL Micro reasoner did not
  finish within 28 minutes on the same data. Queries q5, q9, q13 and q14 return no rows on
  every engine, because the RL TBox does not define the classes or values they ask for.
* *Long writes* (`scripts/bench-longwrite.sh`, `scripts/bench-longwrite/longrun.py`). The
  writers generate their own triples with a fixed seed rather than through
  `scripts/bench-writes.py`, so that the expected final state follows from the
  acknowledged requests of each writer. The harness records the per-minute series of §3.9,
  Sparkles' compaction events and counters, the device writes per write request and the
  disk flushes per write request, and the state checks at the end of the run, after a
  SIGKILL and after a normal restart. The record gets the mode `longrun`, which needs
  record version 2, and `compare` refuses versions it does not know. The kernel counts
  flushes only on whole disks. On the development machine 500 `fdatasync` calls on a file of
  an LVM volume gave 500 flushes on the NVMe disk and none on its partition or on the
  device-mapper nodes, so the harness maps the store's device to the disks under it and
  counts their flushes. An earlier version read the wrong field of `/proc/diskstats` on the
  device-mapper node and reported one flush for the 500 calls, and that figure is
  withdrawn. Flushes alone do not confirm the `synced` class for A6, because a write with
  forced unit access is durable without one. Sparkles writes its log with `O_DIRECT` and
  `O_DSYNC`, and in a one-minute smoke run it made 1.6 device writes but 0.03 disk flushes
  per commit. A6 and §5.1 therefore read the class from the device writes per commit net
  of a 60-second idle baseline, in a single-writer run, with the flushes beside them. The
  smoke figures below are gross counts on a busy laptop, and Jena's 107 writes per commit
  are mostly other processes' I/O divided by its low commit rate. A two-minute
  Sparkles smoke run on 210,509 triples with 4 readers and 2 writers committed 121,585
  write requests (6.1 million triples written), ran 3 automatic compactions, and passed the
  state check before and after the restart. Its store grew from 16 MiB to 557 MiB with
  39,330 live triples of the writers. In a shorter repeat, 90% of the store was the change
  log under `changes/`, which keeps the commits for point-in-time reads, so the long runs
  report the disk use with that history kept at its default. The user set the published
  runs at one hour per engine, plus one four-hour run of Sparkles on the 10.5M store, and
  approved BSBM with `SPARKLES_ALLOW_GPL_BENCHMARK=1`.
* The review of 2026-10-18 led to these changes in the long write harness. The workers
  die with the load generator, back off after errors and bound the writes they hold as
  uncertain. The generator runs in its own session under a time limit, and `stop` in
  `scripts/bench-lib.sh` sends SIGKILL to a server that ignores SIGTERM. A run's status is
  `error` when no write succeeded, more than 1% of the requests failed, the server went
  away, or a synced or written engine lost acknowledged writes after a SIGKILL or a
  restart. The state check reads SPARQL JSON results and rejects malformed values. Disk use
  is the allocated size, read away from the sampling thread. The readers' queries are
  answered on every engine's loaded store before the run, and an engine whose answers
  differ from the majority is not run. `SKIP_QUERIES` in `bench.sh` keeps a query's
  results only when they hold every engine of the run. The materialization digest is
  streamed from the dump through `sort`, and its line count must equal the derived triples
  that `infer` reported.
* A one-minute smoke run of the revised harness on 210,509 triples with 4 readers and 2
  writers ran all five engines. Sparkles, Jena, Fluree and Oxigraph passed the state
  checks at the end of the run, after the SIGKILL and after the restart. Per commit,
  Sparkles made 1.57 device writes and 0.03 disk flushes, Jena 107 and 32, Fluree 9.3 and
  1.0, and Oxigraph 0.03 and 0.00, which puts Oxigraph in the `written` class and the
  other three in `synced`. QLever's RSS reached 27 GiB after about 2,300 write requests
  and the kernel killed it, so its run was recorded as `error`, and the published runs
  set `SERVER_MEM_MAX`.
* `scripts/bench-lib.sh` runs Jena's tools without `JAVA_HOME`, because Jena's scripts
  prefer it to the JDK of the nixpkgs wrapper, and a developer's `JAVA_HOME` may name a JDK
  older than the jars need.

**First refresh batch (2026-10-18 to 2026-10-19).** The batch ran on forge with the release
binary of commit 81478fbe, every engine alone under a 12 GiB memory limit. The harness was
that of commit 81478fbe for the synthetic runs at 1.05M and of commit 77032452 for the
rest. Its records are built with `bench-record.py build` from the result directories.

* The synthetic suite at 1.05M and 10.5M in the default, cold, `updates` and mixed modes,
  SP²Bench at 10M triples and LUBM(100) with the `owl-rl` closure and cold runs completed,
  and BENCHMARKS.md has their figures. Sparkles is fastest on 27 of 28 warm queries at
  1.05M and on all 28 at 10.5M, on 11 of 14 LUBM queries, and on 11 of the 13 SP²Bench
  queries it answers. Its default query memory budget refuses SP²Bench `q4` with 503 and
  `q5a`, `q6` and `q12a` with 507, and it loses `q8` and `q12b` by more than two orders of
  magnitude.
* The 10.5M records failed to build on forge, because `build` took the list in
  `restarts.json` for a query's results. `restarts.json` is now an auxiliary file, and
  the records were built afterwards from the copied results.
* WatDiv and full-text search failed on a QLever build from an unpinned nixpkgs. The
  runners now take the engines from this flake's outputs. The DBpedia slice of 50M
  triples stalled after its load, because `bench-billion.sh` waited for its pressure
  sampler, which is now disowned. These three runs were restarted on 2026-10-19 with the
  harness of commit 0cc691f6 and the same binary and engine packages.
* BSBM Explore at 10M and BI at 10M failed. The test driver asks for DESCRIBE and
  CONSTRUCT results as RDF/XML only, and QLever 0.5.48 answered 406, so its mixes failed.
  The suites' ports from 48300 lie in the kernel's range of ephemeral client ports, and a
  client connection of the driver in TIME-WAIT held Oxigraph's port when its server
  started, so Oxigraph and the following BI run could not bind. On 2026-10-19 the default
  ports moved to 28300 and 28400, a server starts only once its port can be bound, the
  driver's stub takes a free port, an engine that does not start is left out instead of
  ending the run, a server that died before a query or during a driver run is restarted,
  and Explore gains a second mix without its DESCRIBE and CONSTRUCT queries that runs on
  every engine. The byte counts of those two queries are left out of the mix check. The
driver now opens a new connection per request, because on Java's reused connections
QLever's answers waited about 40 ms each for a delayed TCP acknowledgement. A run
  of both use cases with 500 products on a development machine completed on all five
  engines. BENCHMARKING.md describes the mixes. The cold eviction now advises every file
  from one process, which took 0.11 to 0.14 s instead of 31 to 74 s for a Fluree store of
  10,074 files.
* Engine failures seen in the batch are measured results, listed in BENCHMARKS.md. Fuseki
  5.1 fails `knows-reach` with a `StackOverflowError` at both sizes, and in a first attempt
  at 10.5M the failure left TDB2's node table cache locked. Fluree's `optional-chain` at
  10.5M was killed at the memory limit. QLever's and Fluree's servers were killed during
  SP²Bench `q4` in the answer check, which runs all five servers at once.

**Settled-head refresh (2026-10-23).** Roadmap item 1 measured the release binary of
6699f27d on forge in the synthetic suite in all its modes at both scales, SP²Bench at 10M,
LUBM(100), WatDiv at scale 100, the full-text queries at 1.05M, BSBM Explore and BI at 10M
and the DBpedia 50M slice. Sparkles ran alone with the published settings, and the other
engines' results were carried over under rule 1 of §8, since their versions, binaries and
the host were unchanged. `bench-record.py build --reuse ENGINE=MANIFEST` writes such a
record. It refuses a change of host or of an engine's version or checksum, names each
carried-over engine under `manifest.reused` with its run's dates, host and harness, and
marks its results `reused`. The answer checks compared Sparkles' new answers with the
carried-over ones in every suite, and all agreed except where BENCHMARKS.md explains a
difference. The SSD was trimmed before each batch. `sudo -n fstrim` works only from an
operator session whose forwarded agent authorizes sudo, and fails inside a systemd user
unit, so the operator trims before launching. The same holds for `drop_caches`, and the
cold runs evict each engine's files with `posix_fadvise`, which needs no privilege.

The refresh resolved the recorded answer questions. `scripts/bench-bsbm/bi-float-oracle.py`
computes exact means of the float-rounded prices of BSBM BI's q4 and q5 and accepts a value
within the error bound of float summation in any order. With each engine's raw answers,
which `bench-answers.py` now keeps under `ANSWERS_RAW_DIR`, all five engines pass it. On the
DBpedia slice, Jena and Oxigraph count three fewer triples because they merge `xsd:float`
literals with different lexical forms and the same value. The DBpedia manifest now names
all 38 inputs with their hashes, since `--data-path` names a file by its base name. The BI
driver failures come from a Sparkles budget refusal of one generated q4 instance, which
roadmap item 96 takes. No query regressed by 10% or more against the published figures
in interleaved runs of the published binary and this one.

**Left for Phase 1.**

BSBM Explore at 10M completed on 2026-10-19, and its figures and record are in
BENCHMARKS.md. Full-text queries at 1.05M were rerun one engine at a time on
2026-10-20 using the retained indexes, and their record includes the earlier
index-build measurements. WatDiv completed on 2026-10-20 with all 100 template
and engine results passing the answer checks. Its record includes the raw instance
timings and load measurements. In an earlier attempt, WatDiv's
last query was interrupted when QLever crashed, and its record included unfinished
hyperfine output as an extra query. The collector now excludes those temporary
files, and the shared timing helper prepares each engine separately so QLever's
cache operation cannot prevent a later engine's measurement. A server that dies
during timing records an engine failure without publishing its partial samples.
A broken preparation command against a responsive server still fails the harness.
DBpedia's warm phase failed after QLever timed out and restarted on another port,
because cache preparation still used its old endpoint. Preparation now resolves
the endpoint after the query probes and restarts, including for later throughput.
DBpedia also kept every answer-check server alive during warm measurements. It now
stops them, then warms, times and samples RSS for each engine alone, applying the
configured systemd memory limit without swap. CLIENT_CPU can pin its timed clients.
BSBM BI exhausted its eight-hour batch limit while Jena's driver continued after
repeated request timeouts. Each engine, mix and client count now has a configurable
1800 second wall budget, with bounded process-group cleanup and an explicit timeout
record without partial throughput. Successful runs retain their requested mix
counts. These repairs have regression coverage. WatDiv completed with the corrected
harness. The DBpedia 50M query rerun also completed on 2026-10-20, reusing the
2026-10-19 stores and load measurements. Its record contains all 155 warm cells,
155 cold cells and ten throughput cells. All timed requests succeeded. Jena and
Oxigraph disagree with the other three engines on two aggregate queries, and
Oxigraph’s separate answer check for `top-linked` exceeded its 300 second limit.
The timing limit was 600 seconds. These limitations are explicit in BENCHMARKS.md,
and the aggregate disagreements remain unresolved. The bounded BI rerun completed
on 2026-10-21 and its 55-cell record is published. Sparkles' generated driver
queries encounter HTTP 503 budget refusals, while QLever fails and the Jena and
Oxigraph drivers time out. Fluree's two completed driver cells have no successful
competitor for the result-count check and are not ranked. Fixed q4 and q5 have
unresolved answer disagreements, and QLever lacks complete fixed-instance answer
checks on q4 through q8. QLever is 12.6 times faster on the agreeing q2 instances
and 1.37 times on q3. These are observations of the 81478fbe binary, not a
comparison with the current head.
The runner leaves JAVA unset for Jena 6’s loader
so its pinned Java 21 is used, while BSBM keeps its separate Java 17 driver.

* The later batches of the standard refresh (A5). The settled-head refresh of
  2026-10-23 measured Sparkles at one head in every published suite. The
  regeneration of the BENCHMARKS.md tables from records without a numeric change (A3)
  remains open. `bench.sh` starts,
  checks and times one engine at a time in its default and `updates` modes since
  2026-10-18, as `suite_run` does. The published runs invoked `bench.sh` once per engine,
  so each engine ran alone there too. `bench-record.py` reads
  the results of `bench.sh` and of a small `bench-watdiv.sh` run, and both produce valid
  records. The runner also normalizes the full-text files into a common record. The
  DBpedia 50M record and figures are published, and `bench-summary.py` still reads raw
  files. Equals signs in sampled-input filenames were parsed as argument separators,
  collapsing 38 files into 28 manifest entries without hashes or byte sizes.
  The settled-head refresh fixed the input provenance and explained the two aggregate
  answer disagreements.
* A noise-floor control for each standard suite (§14) remains, and its commands are in
  BENCHMARKING.md. BSBM BI's driver cells wait for roadmap item 96.
* BSBM at 100M triples has not been tried, and the generator's heap defaults may need
  raising. SP²Bench at 100M triples will meet the 600 second timeout in its slower queries.
* Open question 1 is settled for LUBM and BSBM only in practice. The BSBM tools and the LUBM
  generator are fetched at run time, the SP²Bench release is BSD and also fetched, and only
  the LUBM queries, which come from a published paper, are committed.
* A CI smoke job in `ANSWERS_ONLY` mode (§14) is not wired in. BSBM would stay out of it
  because of its license.
* Phases 2 to 4 are untouched.

See [ROADMAP](../ROADMAP.md) item 1.


<a id="sp2bench-planner-follow-up-2026-10-21"></a>

### SP²Bench planner follow-up (2026-10-21)

Roadmap item 91 traced the `q8` and `q12b` losses to a selective author pattern
joined outside a UNION. Each branch planned its coauthor joins without that
restriction and generated unrelated walks before joining the author. On baseline
`a6e91187`, a fixture with 64 unrelated papers made the two queries produce 54,368
and 54,367 operator rows while returning two names and a true boolean.

The planner now distributes an eligible basic graph pattern into pure UNION
branches before choosing their join order. It builds the ordinary plan once and
reuses it whenever a bounded screen declines alternative planning. A borrowed shape
check and the existing cost and relation screen precede property catalogue and
variable-set allocation. Every anchor
variable must already occur in every branch. An ordinary branch must join scans
of the same unrestricted index relation with different variable sets, and the
ordinary estimated cost must exceed 32 cost units per branch. That work floor
uses the existing anchor row limit rather than host latency. Algebra and plan
inspection use iterative walks with explicit node limits. Hidden anchor columns,
cheap plans and branches without repeated unbound reads keep the ordinary path
before any alternative algebra is copied or planned.

For an admitted candidate, it plans the anchor to require an
estimate of at most 32 rows, limits it to eight triples and allows at most eight
branches. The extra anchor plan does no query execution. It also compares the complete
ordinary and distributed plan costs, choosing distribution only when its estimate
is lower. A small intersection that reads large inputs can therefore stay outside
the UNION. A branch may contain a
basic graph pattern and deterministic FILTER wrappers. A moved variable read by a filter must already be bound by that filter's
original basic graph pattern. This
preserves expression errors, BOUND and bag multiplicities. Property functions,
application callbacks, volatile expressions, EXISTS, OPTIONAL, SERVICE, LATERAL,
subqueries and assignments keep their original boundaries. Any compiled outer
filter retains the ordinary join. This preserves its evaluation count and placement
without assuming that extension aliases or dynamic calls are pure.

The same release queries over the checksum-pinned SP²Bench v1.01 generator's
50k fixture return the same 264 names and true boolean after the correction.
Their executed row counts fall from 875,171 to 6,439 for `q8` and from 861,737 to
4,856 for `q12b`. These local counts establish the removed work. They do not
replace the published Forge latency figures. The 10M performance acceptance and
the budget failures on the other item 91 queries remain open.

Regression tests cover irrelevant coauthor components, both join orientations,
duplicate hidden anchor bindings, repeated UNION branches, empty anchors and
filters whose variables would become newly bound. Explicit cursors check the
answers at batch sizes 1, 7 and 4,096 without eager materialization. Guard tests
keep effectful or scope-sensitive branches outside the transformation and bound
its expansion. A broad-anchor plan stays outside the UNION, as does a one-row anchor whose
large intersection would be repeated in every branch. Outer FILTER controls cover
native volatile functions, EXISTS, Jena UUID aliases, dynamic calls and a deterministic
expression in both join orientations. Test-only observations verify that cheap
shared-anchor self-joins and rejected high-read anchors do not enter eligibility
validation or alternative planning. Native timing diagnostics distinguish planning and total server time
from execution time, which excludes the optimizer's work.

### SP²Bench ASK candidate follow-up (2026-10-22)

The default-budget failure of `q12a` comes from an equality FILTER above a
Cartesian product. Eager ASK evaluates growing prefixes, but each prefix still
reserves its candidate pairs before applying that filter. The cursor can stop at
the first qualifying pair without reserving the whole candidate table.

Automatic execution now selects that existing ASK cursor for a bounded native
shape when the ordinary candidate estimate exceeds the query's row or memory
budget. The shape is a single variable equality over an independent product,
under ASK's limit and transparent projection. Both compared variables must be
certainly bound by their respective inputs. A bounded inspection admits only
plain scans, values, projections, sorts and joins over an immutable snapshot.
Extension calls, effects, other expressions and unsupported inputs keep the
ordinary path. Direct scans whose exact counts prove that the existing eager
prefix fits their budgets stay ordinary. Graph and repeated-variable discounts
cannot establish that bound.
An admitted product builds the smaller estimated input, as eager limited
execution does. The prepared plan is reused, and the boolean, timing,
cancellation and ownership behavior follows the explicit ASK cursor. Runtime
failures keep their partial execution plan and mark unfinished nodes incomplete,
including scans whose output accounting fails after input exhaustion.
Repeated HTTP ASK requests retain automatic admission even after their completed
body is remembered as quick. In-place scheduling does not establish that eager
candidate storage fits the next request's budget.

Regression cases preserve numeric, date and language value equality without
renaming variables. They distinguish a first qualifying solution from a false ASK
that genuinely consumes more candidate rows than its budget. Memory and produced
row limits remain enforced. Small ordinary ASK and SELECT policies are unchanged.
The historical Forge tables remain unchanged. Reference acceptance at 10M and the
remaining `q4`, `q5a` and `q6` failures are still open.


### SP²Bench string equality candidate follow-up (2026-10-22)

The `q5a` failure has the same equality-filtered Cartesian product as `q12a`,
but SELECT must retain every matching binding. A fixture using the original six
triple patterns produces a 6,144-row product, a 192-row matching bag and 32
DISTINCT answers. The ordinary product exceeds a 512-row limit before its filter
runs.

Eager execution can prune that product when the original intermediate row,
produced-row or memory budget cannot hold it. The filter must be one variable
equality over independent inputs, with both variables certainly bound. The inputs
must contain only scans, values, projections, variable sorts and inner joins.
Every compared runtime value must be a simple or `xsd:string` literal. A charged
lexical string index emits matching pairs in their original order and preserves
duplicates and term ids. The index, the matching pairs and the output are charged
against the memory budget before they are allocated, and every long loop checks
for cancellation. The original SPARQL equality filter still runs on the pruned
pairs, and a pruned subset never uses the unfiltered product's result-cache key.

Numeric promotion, language-tagged values, datatype errors, unbound variables and
mixed domains retain the ordinary kernel. No equality is replaced by RDF-term
unification, and input expressions, OPTIONAL, SERVICE and extension operators are
outside this admission. Products that fit their existing budgets stay ordinary.
Explicit streaming retains its existing operators. The fixture establishes
removed intermediate rows without a latency claim. The separate `q4` and `q6`
failures remain open.

The 10M acceptance ran on forge on 2026-10-23 with the original 10,000,091-triple
input and unchanged defaults. These were an 8 GiB query budget, a 200M-row limit, a
60 s deadline, automatic execution, the result cache off and spill off. The server
ran on one CPU with one Rayon thread. `q5a` returned 385,208 rows in 4.23 to 4.25 s
over three runs. Each answer matched an independent oracle, up to blank-node
relabeling. The pruned product emitted 1,137,394 rows, which equals the oracle's
count before DISTINCT, and query memory peaked at 166 MB. The previous build refused
the query because its 924,047,440,470-row product exceeded the row limit. The guards
`q8`, `q12b` and repeated automatic `q12a` gave the same answers and times on both
builds. These are diagnostic timings without the forge trim protocol. Item 1's
refresh of 2026-10-23 took the reference numbers with the trim protocol, and `q5a`
answered in a mean of 3.9 s over ten runs. The product itself runs on one thread,
and it takes about 3.4 s of the 3.8 s execution.

<a id="sp2bench-q4-and-q6-follow-up-2026-10-23"></a>

### SP²Bench `q4` and `q6` follow-up (2026-10-23)

The `q6` failure came from an OPTIONAL that shares no variable with the pattern
before it. Its only link is the condition `?author = ?author2 && ?yr2 < ?yr`, so the
generic left join formed the product of its sides before testing the condition. At
10M triples that product has 1,577,754,551,569 rows, and the row limit refused the
query in 0.3 s. The planner now builds a keyed left join when an OPTIONAL's sides
share no variable and its condition has a top-level conjunct `?a = ?b` with one
variable from each side. Equality between two different ids can hold only when
neither is an IRI or a blank node, so a left row whose value is one meets only the
right rows with the same id. A left row with another kind of value meets every right
row whose value is bound and is neither an IRI nor a blank node. An unbound value
meets no row, because `=` then raises an error and the conjunction cannot hold. The
whole condition is tested on these candidates 64K pairs at a time, so the answer and
its bag multiplicity are the product's. At 10M the candidate pairs number 7,439,035.
A cursor runs the same join natively. It indexes the right side's equality column by
id, keeps the right rows of other bound kinds in a list of their own, and tests the
condition on the rows that a left value reaches. The planner keeps the generic left
join when the query calls an application extension, because a registered function
could observe how many times the condition runs.

The `q4` failure has two parts. Its plan joins 88,192,293 pairs of articles of the
same journal before `FILTER(?name1 < ?name2)` and DISTINCT, and eager execution holds
that join at a peak of 5.69 GB. Item 1's reference run capped the server at 12 GiB,
which makes the query pool 5,344 MiB, so the pool refused the reservation with 503.
An automatic SELECT whose eager run exceeded its memory budget already ran again as a
cursor, but only when the query could spill. Item 96's automatic admission streams a plan
without spill when its eager output is expected to outgrow half of the memory the query
may use and the cursor's estimated state fits in that half. The planner estimates
26.7 million rows for the `q4` join, so admission keeps the query eager. The retry now
asks the same question as admission about the cursor, so one rule decides both paths.
An eager SELECT that fails its own budget or the shared pool runs again as a cursor
without spill when every operator streams natively and the cursor's estimated state
fits. The retry covers the plans whose estimates missed the eager failure, and it starts
no cursor that admission would have judged too large. The cursor holds 1.99 GB for
`q4`. A retried cursor that still outgrows the budget fails within it.

The other part is the answer's size. `q4` at 10M returns 41,073,618 rows, and their
TSV takes 1,955,839,207 bytes. That is larger than the default response budget of
1 GiB, so with the default settings the response stops at 1 GiB and the client sees an
incomplete body. No default budget can hold this answer, and `q4` keeps its error in
the benchmark table. With `--max-result-mb 4096` it answers within the other default
budgets. The access log recorded that truncated response as a complete answer, with the
outcome `ok` and all 41,073,618 rows. Item 99 fixed this on 2026-10-24. A streamed eager
body that reaches the budget sends what it had buffered, aborts the chunked transfer as
a cursor's body does, and is logged when it ends with the outcome `budget`, the bytes
sent and the rows written whole.

The comparison in `q4` was also slow. An eager filter decoded both name columns before
comparing them, and at one thread that took 2.0 s of the 2.4 s eager run at 1M. Two
stored simple literals now compare without decoding. The base vocabulary sorts keys
`"` lexical 0xFF, which places a string after the strings it is a proper prefix of, so
id order alone does not decide their order. Each string's floor, the first id whose key
starts with its lexical form, does. A smaller id is the smaller string unless it lies
at or above the larger string's floor. An eager filter computes the floor once per
distinct id in a cache of each worker, which holds at most 2^20 ids and is charged to
the query. A cursor batch compares the two keys directly and charges the keys it
reads, as its decoding does. Other kinds of values keep the generic comparison.

The answers were checked against other engines and an independent oracle. At 10M the
`q6` answer has 906,313 rows and the same exact and value digests as Jena's answer in
item 1's reference run. At 1M Sparkles and Jena agree on `q6`, with 62,948 rows, and
Oxigraph timed out at 600 s. At 1M Sparkles and Oxigraph agree on `q4`, with 2,583,025
rows, and Jena timed out. A Python oracle that reads the N-Triples file and pairs the
names of each journal's authors gives the same 2,583,025 pairs as the eager and the
streaming answers. At 10M the sorted `q4` answers of the base build and of this build,
eager and streaming, are identical.

Regression tests cover the q6 fixture against an oracle, an OPTIONAL whose product
exceeds the row limit while its keyed pairs fit, and values that mix IRIs, blank
nodes, numbers equal by value with different ids, language strings, unknown datatypes
and unbound values, compared with the generic left join. A q4 fixture fails eagerly
within a 1 MiB budget and answers through the retried cursor without spill. String
order tests cover prefixes, empty strings and non-ASCII strings below and above the
4,096 rows where the eager kernel starts, with numbers, a language string and an IRI
among them.

An A/B on forge ran the 17 queries at 10M with SERVER_MEM_MAX=12G and the default
budgets, two interleaved rounds of 10 runs after 2 warm-ups for each build. The base was
1c4583e9. `q6` answered in a mean of 948 ms, and `q4` kept its error for the response
size. The other 15 queries stayed within 4% of the base, with `q5a` at 1.015, `q8` at
0.965, `q12a` at 1.001 and `q12b` at 0.975 times the base time. Single runs of `q4` on one
thread without the server cap and with `--max-result-mb 4096` measured the string
comparison. Eager execution fell from 46.3 s to 9.9 s, and a streamed response from
62.2 s to 30.4 s of wall time. These gains cost no memory. The eager `q4` peak
stays at 5.69 GB and the streamed one at 1.99 GB, and the keyed join holds its right
side's index of 24 bytes a row.

After the review fixes, three runs of `q6` in each execution mode on forge returned the
same sorted answer as before. Automatic and eager execution took 1.19 to 1.20 s of wall
time with a peak of 267 MB, and a streamed response took 1.25 s with a peak of 191 MB
through the cursor's keyed join.

Codex reviewed the work, and all five findings were confirmed and fixed. The keyed join
tested its condition on fewer pairs than the product, which an application function
could observe, so such queries keep the generic join. A cursor's string comparison did
not charge the keys it rebuilt, the keyed join did not charge its chunk of pairs, and
the eager floor cache could outgrow its charge. The keyed join first had no cursor
implementation, so strict streaming refused plans that it had run before.
A second review found that a finished cursor keyed join kept its equality index until
the cursor ended and that the index's list of other values grew without charging the
old buffer during a move. The index is now released with the join's other working sets,
and the list is counted, charged and allocated once. A third review asked for the
cancellation check that the list's fill pass had lost, and it was restored.
After the rebase onto item 96, a review of the shared rule found that the retry's test of
the cursor ran before every automatic eager run and planned each EXISTS pattern, which
slowed successful queries with many EXISTS filters. The test now runs only after an eager
memory failure, on a fresh plan that keeps the first run's deadline.

A forge A/B after the merge, over four interleaved rounds at 10M with a 16 GiB server cap
and `--max-result-mb 4096`, found that the fix for the first review's uncharged keys had
cost the cursor most of its comparison gain. Streamed `q4` took 53.8 s on the merged
build, 60.2 s on main before it and 25.7 s on the lane build before that fix, while eager
`q4` took 15.9 s, 35.9 s and 16.2 s. Each key of a cursor comparison had its own scratch
charge, and that charge took the query ledger about six times per comparison. The
comparison now reserves its keys within the cursor's decoding envelope, which covers a
key of the largest size seen so far on every worker, is charged once when it grows and
is released when the cursor ends. On atlas at 1M with four threads, an approximate
measurement on a shared host, streamed `q4` execution fell from about 2.7 s to 1.4 s,
eager execution stayed at about 0.4 s, and the answers stayed the same. A test counts the
ledger operations of 2,000 cursor comparisons against a filter that compares nothing.

### Remaining competitive benchmark losses

Roadmap item 307 follows the competitive query and durable-write improvements. It
rechecks the published losses with the resulting build before choosing further
optimizations. Its work is separate from refreshing the comparison records in item 1.
The remaining LUBM q2 gap belongs to the preceding competitive query work.

The first step rebuilds the loss inventory from the full benchmark tables as well as
[Where Sparkles loses](../BENCHMARKS.md#where-sparkles-loses). A case already brought
within the comparison's ten-percent tie band needs a matching measurement and an updated
record. A loss that remains needs a profile and a fix, followed by the comparison again.
An internal A/B improvement alone does not establish that Sparkles leads the other engine.

| Work | Published evidence and required coverage |
|---|---|
| DBpedia | Recheck the 50M slice's `births-by-decade`, where QLever leads by 4.9 times warm and 1.7 times cold, and cold `outlink-classes-2` at 1.24 billion triples. Keep both dataset sizes so a win at full scale does not hide a smaller-scale regression. |
| Remaining BSBM queries | Complete the investigation of BI q3 and Explore q7 and q8. Profiles of these cases do not count as closed competitor gaps. |
| Cold reads | Recheck cold point lookups against Oxigraph and small cold scans against Fluree at 1.05M and 10.5M. Coordinate changes with item 4 and X07, including their warm-query and dense-read guards. |
| Bindings | Recheck JVM and Python small queries, rdflib calls, Node adds and probes, and Node `export-500k`. Item 33 coordinates this work, including fresh quiet-host measurements of JVM resource descriptions and DELETE/INSERT WHERE, whose shared-host timings show further possible losses. |
| Mixed reads and writes | Recheck the write-rate gap under concurrent readers as well as serial commits, while preserving durable acknowledgements. Compare the read rates and latency tails alongside the write rate, and keep the engines' synchronization policies explicit. |
| Smaller query losses | Recheck WatDiv L1-1 and changed-store `two-hop-count`, whose published gaps are near ten percent. Use enough rounds to distinguish a persistent loss from measurement variation. |

The binding comparisons include Python's small VALUES query and rdflib's `contains`
and `Graph.value` calls. The JVM microbenchmarks include the fixed cost of entering the
native binding and constructing Jena results. A repair must preserve transaction,
term-conversion and iterator behavior rather than remove work that the public API needs.

Each comparison uses the same input digest, query, result format and qualified answers,
with matched compiler and cache settings where appropriate. Published numbers come from
quiet forge runs. Warm and cold cases remain distinct, and every cold comparison records
its eviction method. The write comparisons retain Sparkles' acknowledgement durability
and report a competitor's unsynchronized commits as a different policy.

The loss inventory also retains the documented memory and index-size differences under
item 40. Mapped file pages, reclaimable caches and private heap must be distinguished,
and text indexes must be compared with their predicate scope and features stated. These
are separate footprint tradeoffs. This follow-up does not silently reduce caches or
remove features to improve a ranking, and the standing priority remains speed unless a
memory cost is egregious.

Implement and review changes in independent groups, with correctness tests and broad
regression screens. Update README, BENCHMARKS and the corresponding spec outcomes only
when measurements support the new comparison. Record any accepted slowdown as roadmap
work and any measured rejected optimization in PERFORMANCE-EXPERIMENTS, retaining its
commit and revert. Item 307 remains open while confirmed speed losses remain, unless the
user explicitly accepts a tradeoff or removes a case from scope.

### Keyed inner groups and short ASKs

BSBM BI `q2` links two disconnected groups with a FILTER equality. They now use a
keyed inner join instead of materializing their product. Candidate keys preserve value
equality for numeric and temporal datatypes, language tags and triple terms, and the
whole condition still tests each candidate. Eager LIMIT and ASK bound the larger input
in growing prefixes rather than evaluating both inputs completely.

Large native ASKs over these joins alternate input reads and stop at the first matching
pair. This retains prefixes of both inputs while both can produce rows, which can use
more memory than an ordinary hash join. When one input ends, the other input's old
history is discarded and only its active probe batch remains. Both index growth and
temporary buffers must fit the query budget before allocation. Automatic admission
keeps a cheap eager prefix only when exact scan counts prove that it fits, including
when named-graph statistics discount estimates.

A singleton triple pattern can supply a constant to other patterns and arithmetic
FILTER bounds in the same group. The planner reads at most two rows from a pattern
with one free variable and at most four estimated rows, and substitutes only a proved
single result. The output variable is reconstructed with its original RDF term.
Volatile filters, application callbacks and occurrences across algebra boundaries keep
the ordinary binding. This narrows BSBM Explore `q5` and SP²Bench `q12b` without changing
their answers. STRLEN comparisons with integer constants can test vocabulary keys
directly, preserving Unicode scalar counts, language strings and STR coercion.

Adversarial coverage exercises native ASK at batch sizes one, seven and 4096, empty and
disjoint inputs, late matches after EOF, graph-discounted estimates, value-equality
errors, unbound values, cancellation, deadlines and resource limits. Singleton tests
cover graph scope, duplicate solutions, RDF term identity, blank nodes, numeric bounds,
OPTIONAL, subqueries, paths, EXISTS and snapshots with a delta. Published comparison
refreshes remain under roadmap item 91.

### Short-query checks after keyed inner groups

The Namespace guard against `a5847311` used matching compiler signatures and returned
the same answers for every query. It passed the regression threshold while accepting
small increases for `class-predicates`, `exists-join` and `two-hop-count`. These effects
remain unconfirmed on Intel. Roadmap item 299 tracks matched Intel checks and a fix for
any confirmed cost. The screen does not establish current parity with other engines.
