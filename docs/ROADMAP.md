# Roadmap

This roadmap says what work is planned for Sparkles and in what order. Its goal is to make
Sparkles the fastest graph database across the published benchmarks and in practical use,
and to close the real feature gaps in RDF extensions, reasoning, GeoSPARQL, search, agent
memory and integration. Packaging and the release contract come last and wait until a
first release is cut.

[FEATURES.md](FEATURES.md#known-gaps) describes what Sparkles does and does not do today,
and every known gap there names the roadmap item that addresses it.
[BENCHMARKS.md](BENCHMARKS.md#where-sparkles-loses) records the measured results and the
workloads where Sparkles loses. This document holds only the work that remains.

## Contents

- [How the roadmap is organized](#how-the-roadmap-is-organized)
- [Tracks and decisions](#tracks-and-decisions)
- [Tier 1: now](#tier-1-now)
- [Tier 2: high-impact features and the scale foundation](#tier-2-high-impact-features-and-the-scale-foundation)
- [Tier 3: big bets](#tier-3-big-bets)
- [Tier 4: worth doing, not urgent](#tier-4-worth-doing-not-urgent)
- [Tier 5: backlog](#tier-5-backlog)
- [Tier 6: release work](#tier-6-release-work)
- [Tier 7: compatibility leftovers](#tier-7-compatibility-leftovers)
- [Completed](#completed)
- [Keeping this current](#keeping-this-current)

## How the roadmap is organized

The work is grouped into seven tiers in order of priority. Tiers 1 to 4 are the main plan,
and Tiers 5 to 7 hold work that is wanted at a lower priority. The order weighs the effort
of an item against its impact on the goal above, and items move between tiers as results
come in. Within a tier, most items can run in parallel.

Each item has a number that stays fixed for its lifetime. Specs, FEATURES and code
comments refer to items by these numbers, so a number is never reused or changed. The
numbers are not a ranking, and a new item takes the next free number whatever its tier.

Each row says what remains to be done and links to the spec that holds the design. Items
that extend an existing spec are designed in a "Roadmap follow-ups" section at the end of
that spec's Outcome, and the Outcome also records what has already landed. An item that
is partly finished stays in its tier, and its row describes only the remaining part.
Finished items are listed under [Completed](#completed).

The Size column estimates the remaining effort.

| Size | Meaning |
|---|---|
| S | Days |
| M | One to two weeks |
| L | Several weeks |
| XL | A quarter or more |

The Value column in Tiers 1 to 3 rates the impact of an item on the goal. Lower tiers are
ordered by their tier alone.

## Tracks and decisions

Three tracks run through the tiers. Each track lists its open items in the order they
should run.

| Track | Open items | Specs |
|---|---|---|
| **Performance** | 1, then 7 and 86 in parallel, then 4 | X06, X08 Phase 2, X09 Phase 4, X07 |
| **Graph features** | 8 → 12 → 20 | F14 → C22 → F01 |
| **Reasoning** | 9 → 19 → 18 → 17, with 17 before F18 Phase 3 | F16 → F17 → F18 Phases 1 and 2 → F13 → F18 Phase 3 |

The UI items fill time when a track is waiting on benchmark results. The rest of item 14
waits for the sustained-write results of item 1.

Two decisions depend on what item 1 measures.

* If Sparkles already leads LDBC-style workloads, openCypher (item 20) moves up.
* If spill (item 7) turns out cheaper than expected, the incremental core (item 17)
  starts earlier, because the two share operator machinery.

## Tier 1: now

These items are cheap or foundational.

| # | Item | Spec | Size | Value |
|---|---|---|---|---|
| 1 | **Benchmark refresh.** Later X06 batches, BSBM at 100M, a rerun of the BSBM BI driver cells and the regeneration of the published tables remain. | [X06](specs/X06-benchmark-coverage.md), [F16](specs/F16-materialization-at-scale.md), [X08](specs/X08-disk-spill-and-server-memory.md#33-the-default-and-small-profiles) §3.3 | M | Very high |
| 4 | **Cold sparse lookups.** Phase 4 adds an optional prewarm of the pages a server used before it stopped. Cold `star-join` at 10.5M is slower with index format 3 and that cost should be removed. The Oxigraph targets need measuring again, and the 1.01B dense-read gate and its window sweep are not measured. | [X07](specs/X07-cold-sparse-reads.md) | M | High |
| 6 | **Ask evaluation on public benchmarks.** Runs of local and OpenAI-compatible models and an evaluation with independent roles remain, and LC-QuAD 2.0 waits for a Wikidata load from the full dump. | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12) | S–M | Medium |
| 263 | **Ask on DBpedia.** Question-aware grounding has mixed measured accuracy across models. A complete visibility-safe candidate evidence contract and context-sensitive namesake resolution remain, with genuine ambiguity kept as a clarification. Further changes need offline checks and end-to-end accuracy evaluation. | [C18](specs/C18-natural-language-questions-and-ingest.md#ask-grounding-accuracy-item-263) | M | High |
| 318 | **Ask median latency.** The retained 107-question repeat has higher median wall time for Haiku and Sonnet, while p95 is nearly unchanged. Profile grounding and model time separately and reduce avoidable work while preserving caller visibility and required clarification. Provider variability and different GCC versions leave the cause unconfirmed. | [C18](specs/C18-natural-language-questions-and-ingest.md#ask-grounding-accuracy-item-263), [Benchmarks](BENCHMARKS.md#ask-on-dbpedia) | M | High |
| 307 | **Remaining competitive benchmark losses.** Recheck the published losses after the query optimizations, then close the confirmed gaps in DBpedia, cold reads, bindings, mixed writes, WatDiv and queries after updates. This includes BI q3 and Explore q7 and q8, with cold-read work coordinated through item 4 and binding work through item 33. | [X06](specs/X06-benchmark-coverage.md#remaining-competitive-benchmark-losses), [Benchmarks](BENCHMARKS.md#where-sparkles-loses) | L | High |

## Tier 2: high-impact features and the scale foundation

| # | Item | Spec | Size | Value |
|---|---|---|---|---|
| 7 | **Batch-oriented execution and disk spill.** Spill stays opt-in until X08 Phase 2b's acceptance gates pass, and several cases still run eagerly. The [remaining work](#remaining-work-for-item-7) is listed below. | [X08](specs/X08-disk-spill-and-server-memory.md) Phase 2, [X05](specs/X05-streaming-query-execution.md#roadmap-follow-ups-2026-10-12) | L | Very high |
| 8 | **Graph-algorithm library.** The full Graphalytics harness with X06, the UI, a JVM handle, the C23 writer and openCypher procedures with F01 remain. | [F14](specs/F14-graph-algorithms.md#outcome) | S–M | High |
| 9 | **Materialization at scale.** The separate derived-fact layer of Phase 2 and the `owl:sameAs` representatives of Phase 3 remain. | [F16](specs/F16-materialization-at-scale.md), [C08](specs/C08-inference-freshness.md#roadmap-follow-ups-2026-10-12) | L | High |
| 12 | **GraphRAG community summaries.** Counting changes from an F13 view waits for item 17. | [C22](specs/C22-graph-rag-community-summaries.md) | M | High |
| 14 | **Compaction.** Delta changes located per block, new vocabulary without a full rebuild, adaptive thresholds and automatic compaction in embedded use remain. | [C13](specs/C13-automatic-compaction.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 15 | **Ingestion of images and audio.** The remaining follow-ups are recorded in C18. | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 86 | **Group commit for indexed datasets.** The p90 of spatial serial inserts at 10.5M misses its gate. The spatial reader gate at 1.05M is not established, because the ordinary writer missed the schedule lag requirement. Items 111, 112 and 171 follow up. | [X09 section 17](specs/X09-group-commit-by-default.md#17-phase-4-group-commit-for-indexed-datasets) | M–L | Medium–high |
| 113 | **Small changes and undos over a kept closure.** Changes of 100 triples at 16 threads and undos at 1 thread remain slower than before the F16 follow-up, and a profile of the full run should find or rule out a cost of about 5%. The options are an opt-in in-memory index of the input facts, charged to the memory budget, and per-thread counters in the block cache. | [F16](specs/F16-materialization-at-scale.md#roadmap-follow-ups-2026-10-18) | M | Low |

### Remaining work for item 7

* Spill stays opt-in until all X08 Phase 2b acceptance gates pass, which item 93 tracks.
* LIMIT propagation into fallbacks, running state for FOLD and registered aggregates,
  and earlier first bytes remain from the X05 follow-ups.
* An EXISTS evaluated once per outer key, in BIND or an OPTIONAL condition, stays eager
  because the cursor was slower, and DESCRIBE does not stream.
* The UI's result table still asks for eager execution until it can show a streamed
  result's unknown total.
* The parallel cursor filters and range top-k were measured on 8 vCPUs and still need a
  12-thread measurement on an Intel host. Decoding ahead under many concurrent queries is
  unmeasured.
* The eager `regex-iri` filter keeps `regex::Regex`, and its 5% inlining sensitivity on
  forge is open.

## Tier 3: big bets

The reasoning track leads this tier in its own order. Full-text parity, GeoSPARQL, live
query profiling and query by example lead the items that fill time around it, and
openCypher comes last.

| # | Item | Spec | Size | Value |
|---|---|---|---|---|
| 19 | **Query-time reasoning.** Phase 2 adds `auto` with its cost model, the dataset setting, hybrid Jena rules, OWL 2 QL rewriting, reuse of tables across commits, the MCP argument, the UI and the G08 mapping. Phase 3 keeps tables current through F13 and adds `why` provenance on tables for F18. Items 190 to 193 cover what Phase 1 left. | [F17](specs/F17-query-time-reasoning.md#outcome) | L | High |
| 18 | **Explanations.** Entailment diffs on branches and in write previews, minimal justifications, and provenance-aware protections with `reviewedOnly` over derived facts. Demand derivation trees and bounded why-not searches are available. Phases 1 and 2 need only F17 Phase 1, and Phase 3 needs F13. | [F18](specs/F18-explanations-and-entailment-diffs.md) | M–L | High |
| 17 | **Incremental computation core.** Phase 2 adds `graph` views, maintenance inside the commit, the WebSocket, MCP, the Node and JVM APIs, the UI, views per branch, `keepCommits`, ordered windows and data-dependent protections compiled to views. Phase 3 adds provenance per fact, recursion and GraphQL subscriptions. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | XL | Very high |
| 23 | **Full-text parity.** Online enabling and reconfiguring, search after uncommitted writes, and historical search remain. The analyzers that follow Lucene's and the dictionary analyzers are built. | [F03](specs/F03-full-text-search.md#roadmap-follow-ups-2026-10-12) | M each | Medium |
| 24 | **GeoSPARQL.** k-NN with arbitrary joins, better polygon approximations than the cell grid and QLever's spatial syntax. The geometry limits to lift are datums other than WGS 84, grid-based and exact Helmert shifts, curved GML segments and solids, 3D constructions, and lines split at crossings. | [G01](specs/G01-geosparql.md#roadmap-follow-ups-2026-10-12) | M | Medium |
| 27 | **Live query profiling** with verified rewrite suggestions. | [U04](specs/U04-live-query-profiling.md) | M | Not rated |
| 28 | **Query by example** and Bloom-style search phrases. | [U03](specs/U03-query-by-example.md) | M | Not rated |
| 21 | **Editing forms generated from SHACL shapes.** Phase 3 adds nested forms for blank nodes, the list editor and `sh:or` and `sh:xone` choices. Validation of nodes more than two triples from the edited resource, in a check without a guard, also remains. | [U02](specs/U02-shape-driven-forms.md#outcome) | M–L | Medium–high |
| 25 | **Callbacks in other languages.** Python and JavaScript functions inside queries, and native dispatch of Java functions, which today run through ARQ. | [P03](specs/P03-query-extensions.md#roadmap-follow-ups-2026-10-12) | M | Medium |
| 26 | **Local models.** GPU, ONNX and GGUF, rerankers, and models shared between processes. | [F12](specs/F12-local-embeddings.md#roadmap-follow-ups-2026-10-12) | M–L | Medium |
| 81 | **Shared memory representation for agents and personal knowledge.** A common profile for documents, cited assertions, entities, notes, decisions and tasks, with evidence and dependency relations, time and invalidation rules, and knowledge kept apart from instructions. This item is deferred. | [C17](specs/C17-agent-memory.md#shared-memory-representation), [C18](specs/C18-natural-language-questions-and-ingest.md#shared-memory-workflows) | M–L | High |
| 20 | **openCypher,** with graph algorithms as procedures and LDBC SNB as acceptance. | [F01](specs/F01-cypher.md#roadmap-follow-ups-2026-10-12) | L–XL | High |

## Tier 4: worth doing, not urgent

This tier has three groups. The features are user-facing work, the performance follow-ups
investigate measured costs and planner losses, and the graph-interaction UI finishes a set
of canvas features.

### Features

| # | Item | Spec | Size |
|---|---|---|---|
| 29 | **Rendering very large graphs.** Phase 3 adds server layouts, the binary transfer format and GPU frame-rate tests. | [U06](specs/U06-large-graph-rendering.md#outcome) | M |
| 30 | **Notebooks** stored in the dataset. | [U05](specs/U05-notebooks.md) | M |
| 31 | **Data-quality timeline and schema diagram.** | [U07](specs/U07-data-quality-and-schema-diagram.md) | M |
| 32 | **Parameterized reusable plans.** | [X10](specs/X10-reusable-query-plans.md), [P01](specs/P01-python-bindings.md#roadmap-follow-ups-2026-10-12) | L |
| 34 | **Vector quantization** and vector-result caching. | [F04](specs/F04-vector-search.md#roadmap-follow-ups-2026-10-12) | M |
| 35 | **Multi-field full-text entity documents.** | [F03](specs/F03-full-text-search.md#roadmap-follow-ups-2026-10-12) | M |
| 36 | **Batched updates and commit triggers.** | [C23](specs/C23-batched-updates-and-triggers.md) | M |
| 37 | **GraphQL execution.** Child queries seeded with engine IDs and top-k per parent, and later persisted queries, mutations and subscriptions. | [C03](specs/C03-graphql.md#roadmap-follow-ups-2026-10-12) | M |
| 38 | **SERVICE enhancer.** A slice-aware cache, cache functions, a disk cache and parallel requests. | [G10](specs/G10-service-enhancer.md#roadmap-follow-ups-2026-10-12) | M |
| 39 | **Stored updates,** multi-value parameters and private stored queries. | [C16](specs/C16-stored-queries.md#roadmap-follow-ups-2026-10-12) | S–M |
| 41 | **Embedding conveniences.** Text input in Similar, chunk offsets kept beside vectors, and NixOS embedding settings. | [F08](specs/F08-embeddings-on-write.md#roadmap-follow-ups-2026-10-12) | S |
| 42 | **MCP observability.** Progress notifications, per-tool metrics and performance measurements. | [C11](specs/C11-mcp-server.md#roadmap-follow-ups-2026-10-12) | S–M |
| 43 | **Virtual graphs** over SQL, Parquet and Iceberg. | [F19](specs/F19-mappings-and-virtual-graphs.md) Phase 2 | XL |
| 44 | **A local-first engine** in WebAssembly that syncs through RDF Patch and branches. | [F20](specs/F20-local-first-engine.md) | XL |
| 72 | **JSON query languages,** and a Datalog surface syntax for rules and queries. | [C24](specs/C24-json-query-languages.md), [F17](specs/F17-query-time-reasoning.md) | M–L |
| 78 | **Incremental write-time validation** of recursive SHACL shapes and of SHACL-SPARQL constraints not anchored at the focus node. | [C10](specs/C10-write-time-validation.md#roadmap-follow-ups-2026-10-12) | M–L |
| 82 | **Cheaper vector catch-up and measured half-precision recall.** Catch-up should scan only new rows, binary16 recall needs measuring on Euclidean and offset dot-product data, and a measurement should pick the worker pool. A test should cancel a carry. | [F04](specs/F04-vector-search.md#roadmap-follow-ups-2026-10-12) | S–M |
| 83 | **Digest check on the ShEx guard's schema copy.** The guard should compare its schema copy with `schema.sha256` when it loads, and refuse the guard or validate in full when they differ. | [C10](specs/C10-write-time-validation.md#known-gap-schema-copy-digest-2026-10-16) | S |
| 100 | **Several GraphRAG configurations per dataset,** so that readers with different views each get reports built from their own view. The build state and ledger would be kept per configuration under a new format version. | [C22](specs/C22-graph-rag-community-summaries.md#13-open-questions) | M |
| 250 | **Chinese segmentation after SmartChineseAnalyzer,** and Kuromoji's normal and extended modes and user dictionaries for `ja-dict` and `ko-dict`. `zh-dict` segments by jieba's frequencies today. `ja-dict` and `ko-dict` should also cut a run of unknown characters inside a surrogate pair where Lucene does, which they do before the pair. | [F03](specs/F03-full-text-search.md#roadmap-follow-ups-2026-10-12) | M |
| 251 | **Analyzers for the other languages of jena-text,** such as Greek, Arabic, Persian and Hindi, that reproduce Lucene's tokens as the analyzers for nine languages do. | [F03](specs/F03-full-text-search.md#roadmap-follow-ups-2026-10-12) | M |
| 222 | **View access by grant containment.** A caller reads a view only with full access or the principal's exact access. Containment of grant sets, a cache per caller and view, and a `view:{name}` grant that declassifies a view with an audit record are not built. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | M |

### Performance follow-ups

The measurements behind these items are in [BENCHMARKS.md](BENCHMARKS.md) and the linked
spec sections.

| # | Item | Spec | Size |
|---|---|---|---|
| 33 | **Per-call overhead in the bindings.** JNI calls cost 3 to 4 times Jena's in-memory store, and Node `has()` and export conversion are slower than published targets. | [P04](specs/P04-jvm-bindings.md#roadmap-follow-ups-2026-10-12), [P05](specs/P05-node-bindings.md#roadmap-follow-ups-2026-10-12) | M |
| 40 | **Memory footprint.** Idle server RSS, Python process RSS and full-text index size. | [X08](specs/X08-disk-spill-and-server-memory.md) Phase 3 | M |
| 84 | **Large query plans.** An n-ary join would let the 4000-level plan limit rise, and planning time still grows with the square of the number of triple patterns. A star of a variable that many patterns share remains slow at the limit. | [X05](specs/X05-streaming-query-execution.md#large-basic-graph-patterns) | M |
| 88 | **Allocator overhead of the server-wide caches.** The result cache's heap is about 1.2 times the bytes it counts, and evicted entries leave free pages on the tokio and Rayon threads. The item also covers about 3% more instructions on queries of a few microseconds. | [X08](specs/X08-disk-spill-and-server-memory.md#gates) | S–M |
| 90 | **VALUES pushdown in more shapes.** The rewrite misses a VALUES inside `GRAPH`, joins reached while flattening a group, and branches whose FILTER reads the table's variable. | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12) | S–M |
| 91 | **SP²Bench `q12b` and large answers.** `q12b` remains 1.33× slower than Oxigraph after keyed ASK joins and singleton bindings. Profile the remaining cost and retain the `q12a` improvement. The `q4` answer is 1.96 GB of TSV and needs a `--max-result-mb` above the default. | [X06](specs/X06-benchmark-coverage.md#matched-query-comparisons), [follow-up](specs/X06-benchmark-coverage.md#sp2bench-q4-and-q6-follow-up-2026-10-23) | M |
| 93 | **Spill execution costs and the default.** The full-scale acceptance matrix passes. Grouping cost above ordered spilled joins, the extra writes that restore probe order, a DISTINCT over ordered input that keeps its whole set, the [performance investigation](#spill-performance-investigation) and a decision on enabling spill by default remain. | [X08](specs/X08-disk-spill-and-server-memory.md#full-scale-spill-acceptance-item-93-2026-10-21) | M |
| 111 | **Spatial serial slowdowns of group commit at 10.5M.** Single geometry inserts and IRI inserts run a few percent slower at p90 with grouping. Find the cause, then fix it or state the tradeoff. | [X09 Outcome](specs/X09-group-commit-by-default.md#phase-4) | S–M |
| 112 | **Fence cost of group commit on indexed datasets.** A fence costs about half as much again as an ordinary commit and carries only about two commits with four writers. Find why and whether it can carry more, and measure it on a drive in its fast state too. | [X09 Outcome](specs/X09-group-commit-by-default.md#phase-4) | M |
| 160 | **FSST single-term lookups.** Looking up one term at a time with FSST costs more than decoding each earlier key in full, which slows `star-join` and cold long-literal lookups. A lookup that plans every key's need from the last key back measured 7% slower on single lookups, because the lookups wait on memory more than on decoding. Profile the cold lookups and find a path that keeps the partial decode for long literals without the cost on short keys. | [X11](specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads) | S |
| 170 | **Reader tail of the parallel full-text checkpoint.** Since a checkpoint synchronizes its files from eight threads, the mixed full-text reader p99 at 1.05M measured about 4% higher, within the spread of the runs. Measure it with more rounds, and if the checkpoint's share holds, try fewer threads or a lower I/O priority for the synchronizing threads. | [X09 Outcome](specs/X09-group-commit-by-default.md#phase-4) | S |
| 171 | **Calibrate the spatial mixed cell for the drive's slow state.** The ordinary writers of the spatial mixed cell at 1.05M fell behind their schedule because the unpaced calibration averaged the drive's fast and slow states. Calibrate the rate against the ordinary writer in the slow state, or probe the flush time before each process, and rerun the cell to settle item 86's reader gate. | [X09 Outcome](specs/X09-group-commit-by-default.md#phase-4) | S |
| 276 | **Unweighted path searches after item 150.** Forge measured the unweighted `ps-pair`, `ps-pair-all`, `ps-pair-k5` and `ps-values10` 3 to 4% slower in every round and weighted `path:allShortest` 2.3% slower, although item 150 does not touch their searches. Compare their code generation in both builds and profile them to find what changed. | [Benchmarks](BENCHMARKS.md#weighted-pair-searches) | S |
| 223 | **Parallel view passes.** Each dataset runs its view passes on one worker thread, which falls behind a fast writer with 100 views. `--view-threads`, a shared worker pool and joins whose large deltas split across threads are not built. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | M |
| 225 | **The F13 benchmarks and tests.** The SNB update streams with live short reads and views, BSBM's mix with views, the fan-out test of 1,000 subscribers, a grammar-driven query generator for the differential tests and injected crashes for recovery are not built. A grouped COUNT's pass is only 15 to 35 times cheaper than the query on synthetic data, short of the 100 times target. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | M |
| 226 | **The cost of the change ring on commits.** While a view or live query is subscribed, each commit copies its changes into a ring, which adds about 4% to a two-quad commit on atlas. It needs a pinned forge measurement and a cheaper copy. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | S |
| 240 | **Buffered output of streamed answers.** The cursor writers of streamed SELECT and graph answers still write a few bytes at a time through the response writer and are compiled with the server. Give them the engine-side buffer that eager answers use, with the same count of the rows the client received. | [Benchmarking](BENCHMARKING.md#release-build-settings) | S |
| 241 | **Front coding on three warm DBpedia queries.** Since item 108, `pat-film-predicates` and `pat-film-predicate-list` take about 4% longer with front coding and `top-linked` 1.4% longer, in all 16 pinned rounds on forge. Count their instructions on both builds and remove the cost, or show that it comes from code placement. | [X11](specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads) | S |
| 243 | **Cheaper row accounting in the result buffer.** The buffer checks the row count at every write so that an answer cut partway reports the rows the client received. Serializers write one character at a time, so the check costs 7% of the instructions of `optional-chain` and 14% of its client time on forge, 150.8 ms against 132.0 ms. A row log that the serializers fill themselves measured no cheaper. Find a way to record row ends that costs nothing per write. | [Benchmarks](BENCHMARKS.md#the-cost-of-result-serialization) | S |
| 265 | **The commit path without views.** Forge measurements of inferred-view rounds are published, but the persistent commit comparison varies between 0.27 and 1.61 ms even within one build after SSD trimming. Syscall tracing locates large variation inside WAL and vocabulary writes in both builds. Repeat the comparison with matched durable-write latency before assigning a cost to the inference change. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | S |
| 290 | **LUBM query and durable commit costs.** Reduce the secondary-key probe fanout in LUBM `q2` and the overhead of serial durable commits. Compare the query with pinned QLever on the verified closure and keep synchronous acknowledgements when investigating the single-write gap with Oxigraph's unsynced commits. | [X06](specs/X06-benchmark-coverage.md#matched-query-comparisons) | M |
| 299 | **Short-query checks after keyed inner groups.** Verify the small Namespace guard increases for `class-predicates`, `exists-join` and `two-hop-count` on a quiet Intel host with matching compilers. Profile and remove any confirmed cost while retaining the keyed ASK and singleton-binding improvements. | [X06](specs/X06-benchmark-coverage.md#short-query-checks-after-keyed-inner-groups) | S |
| 306 | **Tiny queries after the combined feature changes.** In 16 pinned forge rounds, `count-all` server time rose from 5.3 to 6.0 µs and `distinct-obj` from 3.6 to 4.2 µs, while client time stayed within 1.2%. The later build runs about 11% fewer instructions across complete HTTP requests, and intermediate builds between `142cd28f` and `8798c2db` do not isolate one responsible change. Measure execution and its plan bookkeeping separately from serialization, distinguish instruction cost from code placement, and remove the cost. | [Benchmarks](BENCHMARKS.md#the-cost-of-result-serialization) | S |

### Spill performance investigation

This work belongs to item 93 and to [X08](specs/X08-disk-spill-and-server-memory.md).
Repeat the default-path `expr-order-key` and `distinct-join` comparisons on a quiet Intel
host with matched builds and inputs. Capture the executed plans and profile shared cursor
buffer accounting, join and DISTINCT work before assigning a cause, and bisect
reproducible slowdowns that stay below the push limit as well as clear failures.

Measure ordered hash joins with a GROUP BY parent at tight and larger spill budgets,
including consolidation writes, merge passes, charged memory, result equality and
cleanup. Compare memory budgets within one build separately from comparisons between
builds, and keep coverage for partially ordered DISTINCT.

### Graph-interaction UI

These items finish a set of graph-interaction features whose first phases have landed.
Rows that need backend work name it first.

| # | Item | Spec | Size |
|---|---|---|---|
| 101 | **Visual query builder, Phase 3.** `UNION`, `GRAPH`, inline `VALUES`, `MINUS` and `FILTER NOT EXISTS` join the editable subset, and **Build a query** opens the pane from a class of the Explore overview. | [U08](specs/U08-visual-query-builder.md#outcome) | L |
| 120 | **The validation overlay on the quality record.** Once item 31 Phase 1 records the guard's results per commit, add `focus=` to `GET /$/validation/{ds}/results` and read the record at the head instead of validating. | [U07](specs/U07-data-quality-and-schema-diagram.md#outcome) §11 | S |
| 121 | **Conclusive previews of a suggested fix.** Let a dry run name focus nodes to validate in full against the candidate state, so that the overlay can say a fix resolves a result while other results remain. | [U07](specs/U07-data-quality-and-schema-diagram.md#outcome) §11, [C15](specs/C15-write-previews.md) | S |
| 140 | **Linked views remainder in the UI.** Colour the embedding map by a kept Leiden run and open it with `Shift+M`, count date facets by month for short spans, give the facets a full-screen tab on phones, and let Explore's map and facets read a branch. | [U11](specs/U11-linked-views.md#outcome) | M |
| 141 | **Frame times of the linked views on a GPU.** Run U11's A9 frame-rate and lasso targets in a Playwright performance project on a GPU runner, since they were measured only in software rendering. | [U11](specs/U11-linked-views.md#outcome) | S |
| 142 | **Projections on disk and for partial callers.** Decide whether to write projections beside the index in a versioned file, and whether to serve a projection filtered to what a caller can read instead of answering `403`. | [U11](specs/U11-linked-views.md#10-open-questions) | M |
| 275 | **Weighted shortest paths under a length limit.** A weighted shortest search with `path:maxLength` lists paths by cost until one is short enough, so a pair whose short paths are all dear runs to the visit limit, and the path finder always sends a limit. Search over pairs of node and length, keeping a label only when no other label at the node is both cheaper and shorter, and use the same search for Yen's spurs. | [F07](specs/F07-path-search.md#weighted-searches-from-both-ends-2026-10-24), [U09](specs/U09-path-finder.md#outcome) | M |
| 277 | **Weighted `path:allShortest` from both ends.** Every cheapest path of one pair without `path:maxLength`, cycles through the source and searches to several targets still run Dijkstra's algorithm from the source alone. Keep every tied parent on both ends and join the halves at every cheapest meeting. | [F07](specs/F07-path-search.md#weighted-searches-from-both-ends-2026-10-24) | S–M |

| 285 | **Explain intensional query result cells.** Offer a Why? action on a result row when the query's triple pattern was intensional, carrying the row bindings into the existing explanation panel. | [F18](specs/F18-explanations-and-entailment-diffs.md#outcome) | S–M |
| 286 | **Why-not explanations for unbound query patterns.** Extend target patterns beyond a ground triple so `why_empty` can explain a failing intensional pattern with unbound variables, without inventing constant values. | [F18](specs/F18-explanations-and-entailment-diffs.md#outcome) | M |

## Tier 5: backlog

These items are smaller or narrower. Each has a design sketch in the linked spec.

| # | Item | Spec | Size |
|---|---|---|---|
| 45 | **Branches.** A remote branch-relink CLI, and clones and archive transfer between servers. | [F09](specs/F09-branches-and-merges.md#roadmap-follow-ups-2026-10-12) | M |
| 46 | **Notifications.** An email channel, an administration UI, broader backup-failure coverage, and delivery that a slow channel cannot delay. | [C21](specs/C21-outbound-notifications.md#roadmap-follow-ups-2026-10-12) | M |
| 47 | **Rust client.** Typed bodies for the remaining administration calls, proactive rate-limit pacing, and credential integration tests. | [P02](specs/P02-rust-client.md#roadmap-follow-ups-2026-10-12) | S–M |
| 48 | **Formatter and linter.** SHACLC formatting, linting of N-Triples, N-Quads, JSON-LD and embedded SHACL queries, linting in the shapes editor, and an MCP lint tool. | [X02](specs/X02-formatter.md#roadmap-follow-ups-2026-10-12), [X04](specs/X04-linter.md#roadmap-follow-ups-2026-10-12) | M |
| 49 | **Write previews.** Local dry-run flags, UI previews beyond the CSV import panel and merge page, `If-Match` on `/{ds}/update`, and handling of read-only servers and costly bulk previews. | [C15](specs/C15-write-previews.md#roadmap-follow-ups-2026-10-12) | S–M |
| 50 | **Restricted views.** Token scopes limited to graphs, validation and statistics for restricted views, and text scoring local to what a caller can see. | [C12](specs/C12-graph-access-control.md#roadmap-follow-ups-2026-10-12) | M |
| 51 | **ARQ.** Parallel LATERAL, user DESCRIBE handlers, the remaining client options and builder methods, and `shortest(…)` evaluation. | [G06](specs/G06-arq-query-extensions.md#roadmap-follow-ups-2026-10-12) | M |
| 52 | **History.** Pin rebasing, periods per quad, change feeds per graph, reconstruction of expired states, and history of settings. | [F06](specs/F06-snapshots-and-point-in-time.md#roadmap-follow-ups-2026-10-12) | M |
| 53 | **Inference settings.** Input-graph and RDFS settings in the UI, the remaining RDFS-on-read surfaces, and controls for the import policy. | [C08](specs/C08-inference-freshness.md#roadmap-follow-ups-2026-10-12) | S–M |
| 54 | **ShEx.** Combined SHACL and ShEx guards, schemas from a graph plus a file, and CLI outbound controls. | [G02](specs/G02-shex.md#roadmap-follow-ups-2026-10-12) | M |
| 55 | **Completions and OpenAPI.** Dynamic and Nushell completions, and an OpenAPI "try it" page with named schemas. | [X03](specs/X03-openapi-and-completions.md#roadmap-follow-ups-2026-10-12) | S–M |
| 56 | **Legacy MCP resource subscriptions.** | [C11](specs/C11-mcp-server.md#roadmap-follow-ups-2026-10-12) | S |
| 57 | **Scratch-branch expiry** as a dataset setting and under stdio. Today only the server's `--mcp-scratch-branch-ttl` sets it. | [C17](specs/C17-agent-memory.md#roadmap-follow-ups-2026-10-12) | S |
| 71 | **An OWL 2 ontology object API,** as Jena's `jena-ontapi` provides. | [F21](specs/F21-ontology-api.md) | L |
| 73 | **Format coverage.** All formats in `load_str`, `dump_graph`, SPARQL `LOAD` and the JVM and Node bindings, and loading Thrift, Protobuf, RDF/JSON and TriX without a temporary N-Quads copy. | [G05](specs/G05-command-line-tools.md#roadmap-follow-ups-2026-10-12) | S–M |
| 74 | **RDF Patch.** Patches with several transactions applied as several commits, blank-node identity across datasets, and patch logs. | [F10](specs/F10-replication.md#roadmap-follow-ups-2026-10-12) | M |
| 75 | **Access control.** Validation endpoints for callers whose grants cover part of a dataset, the change feed for data-dependent protections, and hiding IRIs that visible triples mention. | [C12](specs/C12-graph-access-control.md#roadmap-follow-ups-2026-10-12), [C12b](specs/C12b-triple-access-control.md#roadmap-follow-ups-2026-10-12) | M–L |
| 76 | **Jena compatibility.** Complete assembler bodies, direct Graph Store naming per dataset, and JVM metrics. | [G08](specs/G08-fuseki-configuration.md#roadmap-follow-ups-2026-10-12), [P04](specs/P04-jvm-bindings.md#roadmap-follow-ups-2026-10-12) | S–M |
| 77 | **Assistant ingestion.** The Ingest panel should say when it stores uploads without extracting facts because server ingestion is off, and server-side document ingestion needs redaction. | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12), [C19](specs/C19-layered-settings.md#roadmap-follow-ups-2026-10-12) | S–M |
| 79 | **Nonnegative weighted betweenness.** Define zero-weight path counting and validate an algorithm before extending the current finite, strictly positive domain. | [F14](specs/F14-graph-algorithms.md#nonnegative-weighted-betweenness-semantics) | M–L |
| 80 | **Positive-weight numerical robustness.** Extend the refused mixed-scale domain and define approximation guarantees for distances and path counts in weighted betweenness. | [F14](specs/F14-graph-algorithms.md#positive-weight-numerical-robustness) | M–L |
| 92 | **Per-crate Nix builds.** Compare the crate2nix prototype with Crane under matched settings, measure which crates rebuild after source changes, and validate packaging and platforms before deciding whether to migrate. | [X13](specs/X13-per-crate-nix-builds.md) | S–M |
| 190 | **Per-query cost of derivation on demand.** Selective LUBM queries under `reasoning=demand` miss F17's target of twice the time over a materialization, mostly in session setup, per-query program building and the merge of derived facts. Cache the specialized rules and programs per view, merge derived facts as a sorted run, and measure on forge. | [F17](specs/F17-query-time-reasoning.md#outcome) | M |
| 191 | **Schema facts that follow from derived instance facts under demand.** The schema pass reads only stored instance facts, so some `rdfs:Class`, `rdf:Property` and `owl:sameAs` facts are missed. Derive them from the demand program's own facts, or decide them by an existence test per class and property. | [F17](specs/F17-query-time-reasoning.md#outcome) | M |
| 192 | **Budget accounting of the derived-table cache.** The cache is a process-wide 256 MiB budget that no flag sets. Charge it to the result cache, or add a `serve` flag and report its use in `/$/stats`. | [F17](specs/F17-query-time-reasoning.md#outcome) | S |
| 193 | **DESCRIBE under derivation on demand.** `reasoning=demand` refuses DESCRIBE with `501`. Turn each described resource into demands for the facts the dataset's DESCRIBE mode follows before the description runs. | [F17](specs/F17-query-time-reasoning.md#outcome) | S |
| 210 | **One pass over the unbound build rows of a spilled outer join.** OPTIONAL, semi, anti and MINUS joins read that file once per partition. A bitmap of probe rows shared between the jobs would let one job read it once, as the inner join does. | [X08](specs/X08-disk-spill-and-server-memory.md#spilled-join-amplification-item-94-2026-10-24) | S |
| 220 | **Backups and clones of view definitions.** The backup layout has no path for `views/{name}/view.json`, so a restored or cloned dataset has no views. The layout needs a versioned way to carry nested settings files. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | S |
| 221 | **Incremental views over inferred data.** A view or live query on a dataset with RDFS on read or materialized inferences evaluates its whole query again at each commit that touches it. Incremental maintenance needs circuit scans that read the inferred graph, and the RDFS rewrite compiled into the circuit again when the schema changes. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | M |
| 224 | **A server-wide limit of live queries.** `--max-live-queries` and `--max-live-queries-per-principal` count per dataset, where the design counts over the server. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | S |
| 227 | **Disclosure guards in circuits.** Circuits treat the graphs a C22 guard protects as unreadable, because a request decides a guard at its own snapshot. A view principal or live reader whose footprints prove a reports graph does not see it. Circuits need a guard decision that a pass can update as the footprints change. | [F13](specs/F13-incremental-views-and-provenance.md#outcome) | M |
| 230 | **PostgreSQL and DuckDB sources for mappings.** The `postgres` and `duckdb` connection kinds of F19 §4.4 with secret references, the outbound policy for PostgreSQL hosts, the R2RML test cases on PostgreSQL in CI, and acceptance examples A1 and A10. | [F19](specs/F19-mappings-and-virtual-graphs.md#outcome) | M |
| 231 | **Mappings in the JVM and Node.js bindings.** The stored mappings handle and `run_mapping`, which the Rust library and Python have. | [F19](specs/F19-mappings-and-virtual-graphs.md#outcome) | S |
| 232 | **RML join conditions written as expression maps,** and the IRI checks of RMLTC0027b. Six RML-Core test cases are excluded until then. | [F19](specs/F19-mappings-and-virtual-graphs.md#outcome) | S–M |
| 233 | **A Mappings page in the web UI** that lists stored mappings, edits them, runs them with a dry run first and shows the last run. | [F19](specs/F19-mappings-and-virtual-graphs.md#8-stored-mappings) | M |
| 234 | **Stored mapping runs as tasks.** Runs longer than 10 seconds become tasks of `/$/tasks` with progress and cancellation, as F19 §2.2 describes. A run is one request. | [F19](specs/F19-mappings-and-virtual-graphs.md#outcome) | S–M |
| 235 | **The W3C CSVW test suite,** vendored as test data for the RDF tests in minimal and standard mode. | [F19](specs/F19-mappings-and-virtual-graphs.md#131-conformance-and-equivalence) | S–M |
| 236 | **The remaining SPARQL functions in function maps,** such as the date, numeric and term functions, as `urn:x-sparkles:fn:` functions. | [F19](specs/F19-mappings-and-virtual-graphs.md#outcome) | S |
| 237 | **Parquet sources in object stores** (`s3`, `gcs`, `azure`) through `object_store`, as F19 §4.4 lists. Parquet files are read from the local disk. | [F19](specs/F19-mappings-and-virtual-graphs.md#outcome) | M |
| 238 | **Template lookups that keep the dataset's blank nodes.** A lookup that binds an existing blank node writes a new one, because the loader scopes blank node labels per file. | [F19](specs/F19-mappings-and-virtual-graphs.md#outcome) | S–M |
| 239 | **JSON numbers with their source lexical form,** as F19 §4.2 asks. serde_json prints a non-integral number in its own form. | [F19](specs/F19-mappings-and-virtual-graphs.md#outcome) | S |
| 260 | **Person-checked GraphRAG evaluation samples.** No person checked the 30 generated global questions or judged a sample of the Opus judgements, both of which §9 asks for. | [C22](specs/C22-graph-rag-community-summaries.md#paid-acceptance-and-evaluation-2026-10-24) | S |
| 261 | **GraphRAG evaluation on more sets.** §9 also names meeting transcripts and a DBpedia subset about one domain, and a text baseline that retrieves by vector search rather than BM25 over 20 documents. The effect of §7.3 on C18 and the refresh tokens per thousand changed facts remain unmeasured. | [C22](specs/C22-graph-rag-community-summaries.md#paid-acceptance-and-evaluation-2026-10-24) | M |
| 262 | **Classifying whole-graph questions.** `auto` mode sent 14 of 30 generated global questions to local mode, where most ended in a clarification or as unanswerable. The draft's scope decision needs examples or a rule for questions about the whole dataset. | [C22](specs/C22-graph-rag-community-summaries.md#paid-acceptance-and-evaluation-2026-10-24) | S |
| 264 | **Explicit property order in the other model schemas.** Constrained decoding writes members in the order a schema lists them, and only the global map and reduce schemas name an order. The draft, report and ingest schemas reach providers sorted by name, and a measured run should decide whether a reasoning-first order helps them. | [C22](specs/C22-graph-rag-community-summaries.md#paid-acceptance-and-evaluation-2026-10-24) | S |
| 270 | **Scope challenges inside MCP tools.** `/$/mcp` challenges a missing `sparkles:read` or `sparkles:update` before a message runs, but an operation inside a tool that needs admin, such as deleting a protected branch, is refused as a tool error without naming `sparkles:admin`. The tools need a way to return the challenge, or the message check needs to know which arguments need admin. | [C11](specs/C11-mcp-server.md#oauth-for-the-mcp-endpoint-item-67-2026-10-24) | S |

## Tier 6: release work

This work is done when the first release is cut. The release waits until Tiers 1 to 5
have landed, because leading on performance and features matters more than releasing
early.

| # | Item | Spec | Size |
|---|---|---|---|
| 58 | **Packages.** Publishing to PyPI, Maven Central and npm, Windows and Intel macOS builds, a musl JVM build, PyPy and free-threaded wheels, and Python async wrappers. The workflows exist with their publish jobs switched off. | [X12](specs/X12-release-engineering.md), [P01](specs/P01-python-bindings.md#roadmap-follow-ups-2026-10-12), [P04](specs/P04-jvm-bindings.md#roadmap-follow-ups-2026-10-12), [P05](specs/P05-node-bindings.md#roadmap-follow-ups-2026-10-12) | M–L |
| 59 | **Server binaries and published container images.** | [X12](specs/X12-release-engineering.md) | M |
| 60 | **API stability policy.** X12 Phase 4 adds a stability policy for the HTTP, CLI and library APIs, with its checks. The on-disk side is in place. | [X12](specs/X12-release-engineering.md) | S–M |
| 61 | **Whole-server backup and restore.** All datasets in one request, a remote backup CLI, and repository copy. | [F05](specs/F05-snapshot-repositories.md#roadmap-follow-ups-2026-10-12) | M |
| 62 | **Audit events** for creating, deleting and cloning datasets and for creating and deleting branches. | [C09](specs/C09-dataset-access-control.md#roadmap-follow-ups-2026-10-12) | S |
| 63 | **Authentication additions.** Mutual TLS, OCSP, opaque-token introspection, front-channel logout and Cloudflare group lookup. | [C09](specs/C09-dataset-access-control.md#roadmap-follow-ups-2026-10-12) | M |
| 64 | **Startup resilience.** Binding the listener before datasets open, and degraded startup when a dataset fails to open. | [C01](specs/C01-observability-and-budgets.md#roadmap-follow-ups-2026-10-12) | M |
| 65 | **Encryption.** Key management workflows, Vault and cloud KMS, recovery export, and encryption of live datasets and derived indexes. | [F11](specs/F11-encryption-at-rest.md#roadmap-follow-ups-2026-10-12) | L–XL |
| 280 | **Replication spec refresh.** Answer F10's open questions 15 to 23 before Phase 2 starts. They cover derived state on replicas, branches, encryption, the fencing and data-loss guarantees of lease-based failover, Kubernetes, scope, the estimate, how a peer joins and exchanges credentials, and one account of high availability with recovery targets and client routing. | [F10](specs/F10-replication.md#11-open-questions) | S |
| 66 | **Replication.** Read replicas, preserved primary IDs, promotion, failover, synchronous replication, and a patch log or continuous archive. | [F10](specs/F10-replication.md#roadmap-follow-ups-2026-10-12) | XL |

## Tier 7: compatibility leftovers

These were left out on purpose. They stay on the list and are reconsidered when users ask
for them.

| # | Item | Spec | Size |
|---|---|---|---|
| 69 | **ARQ extras.** JavaScript functions, `afn:context`, the Leviathan library, the `JSON` query form and locale collation. | [G06](specs/G06-arq-query-extensions.md#roadmap-follow-ups-2026-10-12) | M each |
| 70 | **Jena-compatible GeoSPARQL mode** that reproduces Jena's answers where Sparkles diverges on purpose, as [COMPARISON.md](COMPARISON.md#geosparql) describes. | [G01](specs/G01-geosparql.md#roadmap-follow-ups-2026-10-12) | M |

## Completed

These items are finished. [FEATURES.md](FEATURES.md) describes what they deliver, and the
linked spec Outcome records how they landed.

- **2.** Server-wide cache sizing and query memory pool. [X08](specs/X08-disk-spill-and-server-memory.md) Phase 1
- **3.** Group commit as the default. [X09](specs/X09-group-commit-by-default.md)
- **5.** Correctness fixes in path search, change logs and Fuseki endpoints. [F07](specs/F07-path-search.md#roadmap-follow-ups-2026-10-12), [F06](specs/F06-snapshots-and-point-in-time.md#roadmap-follow-ups-2026-10-12), [G08](specs/G08-fuseki-configuration.md#roadmap-follow-ups-2026-10-12)
- **10.** Vector index maintenance. [F04](specs/F04-vector-search.md#roadmap-follow-ups-2026-10-12)
- **11.** Time slider. [U01](specs/U01-time-slider.md)
- **13.** Current shape standards. [G03](specs/G03-shaclc.md#roadmap-follow-ups-2026-10-12), [G02](specs/G02-shex.md#roadmap-follow-ups-2026-10-12)
- **16.** Index compression after predicate patterns. [X11](specs/X11-index-compression-and-predicate-patterns.md#outcome)
- **22.** RML, R2RML and YARRRML loading. [F19](specs/F19-mappings-and-virtual-graphs.md#outcome) Phase 1
- **67.** MCP OAuth. [C11](specs/C11-mcp-server.md#oauth-for-the-mcp-endpoint-item-67-2026-10-24)
- **68.** Sealed runtime secrets. [C19](specs/C19-layered-settings.md#roadmap-follow-ups-2026-10-12)
- **85.** Hash-join, ordered DISTINCT and encrypted spill. [X08](specs/X08-disk-spill-and-server-memory.md) §6.3 and §6.7
- **87.** Small incremental runs over a kept closure. [F16](specs/F16-materialization-at-scale.md#roadmap-follow-ups-2026-10-18)
- **89.** Ask and `check_query` on large datasets. [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12)
- **94.** Spilled join amplification. [X08](specs/X08-disk-spill-and-server-memory.md#spilled-join-amplification-item-94-2026-10-24)
- **95.** Close-only locks on the database directory. [P06](specs/P06-library-admin-api.md), [F05](specs/F05-snapshot-repositories.md#23-restore)
- **96.** Streaming NOT EXISTS over a Cartesian product. [X08](specs/X08-disk-spill-and-server-memory.md#automatic-streaming-without-spill-item-96-2026-10-23)
- **97.** Retuned `small` memory profile. [X08](specs/X08-disk-spill-and-server-memory.md#small-profile-retune-and-the-phase-1a-control-item-97-2026-10-23)
- **98.** Materialization limit and F16 acceptance at scale. [F16](specs/F16-materialization-at-scale.md#outcome)
- **99.** Truncated eager responses logged as truncated. [X06](specs/X06-benchmark-coverage.md#sp2bench-q4-and-q6-follow-up-2026-10-23)
- **102.** The plan drawn on the query's pattern. [U04](specs/U04-live-query-profiling.md#11-phase-4-the-plan-drawn-on-the-querys-pattern-2026-10-23) Phase 4
- **103.** Path finder. [U09](specs/U09-path-finder.md#outcome)
- **104.** Time-lapse and commit diffs on the canvas. [U01](specs/U01-time-slider.md#phase-4-time-lapse-and-commit-diffs-on-the-canvas-2026-10-23) Phase 4
- **105.** Canvas lenses and the derivation tree of an inferred edge. [U10](specs/U10-canvas-lenses.md#outcome)
- **106.** Validation overlay. [U07](specs/U07-data-quality-and-schema-diagram.md#11-phase-4-a-validation-overlay-on-the-graph-2026-10-23) Phase 4
- **107.** Linked views. [U11](specs/U11-linked-views.md#outcome)
- **108.** Faster FSST reads. [X11](specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads)
- **109.** `order-by-full` with one codegen unit for sparkles-core, which runs the same instructions and keeps the setting. [Benchmarking](BENCHMARKING.md#release-build-settings)
- **110.** Bounded write stall of the locked full-text checkpoint. [X09](specs/X09-group-commit-by-default.md#phase-4)
- **135.** Partial derivation trees within search budgets. [F18](specs/F18-explanations-and-entailment-diffs.md#outcome)
- **136.** Explanations through generalized facts and default geometries. [F18](specs/F18-explanations-and-entailment-diffs.md#outcome)
- **137.** Rule bindings and passed builtins in derivation trees. [F18](specs/F18-explanations-and-entailment-diffs.md#outcome)
- **138.** Interruptible RDFS schema builds for queries and explanations. [F18](specs/F18-explanations-and-entailment-diffs.md#outcome)
- **139.** Mixed materialized and RDFS-on-read derivations. [F18](specs/F18-explanations-and-entailment-diffs.md#outcome)
- **150.** Weighted path searches from both ends. [F07](specs/F07-path-search.md#weighted-searches-from-both-ends-2026-10-24)
- **114.** Client time of large results, from eager serializers compiled with the engine and buffered. [Benchmarking](BENCHMARKING.md#release-build-settings), [Benchmarks](BENCHMARKS.md#the-cost-of-result-serialization)
- **161.** Front coding on warm DBpedia after item 108, which forge measured at a geometric mean of 1.004. [X11](specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads)
- **180.** Plan pattern references only for a shown plan. [U04](specs/U04-live-query-profiling.md#phase-4-2026-10-23) Phase 4
- **181.** Compiler provenance in the Namespace query screen. [Benchmarking](BENCHMARKING.md#namespace-query-screens)
- **182.** `subquery-agg` after the U02 forms, U11 Phase 3 and item 94 merges, which forge measured unchanged. [Benchmarking](BENCHMARKING.md#namespace-query-screens)
- **242.** `two-hop-count` after the result buffer, which forge measured at 0.998 of the build before the buffer in 24 pinned rounds. [Benchmarks](BENCHMARKS.md#the-cost-of-result-serialization)

## Keeping this current

Update this document when an item ships, changes tier or gets a new spec. A new known gap
in FEATURES gets a roadmap item in the same commit. When an item ships, its spec's Outcome
records what landed, [FEATURES.md](FEATURES.md) and [BENCHMARKS.md](BENCHMARKS.md) change
in the same commit, and the Known gaps bullet drops the item's number. The item's row then
moves to a one-line entry under [Completed](#completed). When part of an item ships, its
row is cut down to the part that remains.
