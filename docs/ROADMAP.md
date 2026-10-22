# Roadmap

This is the plan for making Sparkles the best graph database for performance across the
published benchmarks and for practical use. It also covers filling the real feature
gaps in RDF extensions, reasoning, GeoSPARQL, search, agent memory and integration. The
first public release is not the goal of this plan. Packaging and the release contract
are listed at the end and wait until a release is cut.

The order weighs implementation difficulty against impact. Each item links to the spec
that holds its design. New items have their own spec. Items that extend an existing spec
are designed in a **Roadmap follow-ups (2026-10-12)** section at the end of that spec's
Outcome.

This document and [FEATURES.md](FEATURES.md#known-gaps) are kept separate on purpose.
FEATURES describes what Sparkles does and does not do at the current commit. This
document describes what will be done and in what order. Every known gap in FEATURES
names the roadmap items that address it, and every gap is on this roadmap, in Tier 7 if
nothing else. [BENCHMARKS.md](BENCHMARKS.md#where-sparkles-loses) remains the record of
measured losses.

Sizes are estimates:

| Size | Meaning |
|---|---|
| S | Days |
| M | One to two weeks |
| L | Several weeks |
| XL | A quarter or more |

A partly done item has merged implementation with work or acceptance gates remaining.
Implementation completion does not mean that its performance gates have passed or that
an opt-in feature is enabled by default. Work on an unmerged branch stays open here.

Item numbers are stable identifiers, not a ranking: a new item takes the next free
number in whatever tier it belongs to. Impact is judged against the goal above. Tiers 1–4 are the main plan. Tiers 5–7 are
work that is wanted at a lower priority, and items move between tiers as results come in.

## How the work is organized

The tiers set the order of priority. Within a tier, most items can run in parallel.
Three tracks run through the tiers:

| Track | Items | Spec order |
|---|---|---|
| **Performance** | 3 → 2 → 1 → 7 and 86 → 4 → 16 | X09 → X08 → X06 → X08 Phase 2 and X09 Phase 4 → X07 → X11 |
| **Graph features** | 8 → 12 → 20 | F14 → C22 → F01 |
| **Reasoning** | 9 → 87 → 19 → 18, with 17 before F18 Phase 3 | F16 → F16 follow-up → F17 → F18 Phases 1–2 → F13 → F18 Phase 3 |

The UI items (11, 21, 27–31) fill time when a track is waiting on benchmark results.

Item 2's gates run before item 1's benchmark refresh, so that the refresh measures the
settled defaults. Item 86 and the rest of item 7 then run in parallel, since they change
different parts of the engine. The removal at open of a leftover generation, from item 14,
shipped on 2026-10-18 with the other durability follow-ups, and the rest of item 14 waits
for the sustained-write results of item 1.

Two decisions depend on what item 1 measures:

* If Sparkles already leads LDBC-style workloads, openCypher (item 20) moves up.
* If spill (item 7) turns out cheaper than expected, the incremental core (item 17)
  starts earlier, because the two share operator machinery.

## Tier 1: now

These items are cheap or foundational.

| # | Item | Spec | Size | Impact |
|---|---|---|---|---|
| 1 | The **benchmark refresh** has published its first forge batch and the Explore, full-text, WatDiv, DBpedia 50M and BSBM BI follow-ups. BI driver failures, answer disagreements, complete input provenance and later X06 batches remain open. It also covers F16 Phase 1 acceptance and X08 Phase 1a’s warm and cold profile checks at 10.5M and on DBpedia. | [X06](specs/X06-benchmark-coverage.md), [F16](specs/F16-materialization-at-scale.md), [X08](specs/X08-disk-spill-and-server-memory.md#33-the-default-and-small-profiles) §3.3 | M | Very high |
| 2 | **Done (2026-10-18), except the Phase 1a suite runs at 10.5M and on DBpedia, which item 1 takes. Server-wide cache sizing and query memory pool.** Shared caches are sized once per server, with a `small` profile. One query pool admits requests and adds retained eager estimates, including graph output, while cursor state releases promptly. Both gates were amended on 2026-10-18 and met on forge. The pool adds at most 0.5 µs to queries under 0.3 ms, and ten datasets use 0.946 and 1.027 times the anonymous memory that the A1 bound allows at 256 and 32 MiB of cache. Item 88 covers the remaining allocator overhead. [X08](specs/X08-disk-spill-and-server-memory.md) Phase 2 follows under item 7. | [X08](specs/X08-disk-spill-and-server-memory.md) Phase 1, [C01](specs/C01-observability-and-budgets.md#roadmap-follow-ups-2026-10-12) | S, then M | High |
| 3 | **Done (2026-10-17). Group commit as the default.** Persistent stores group concurrent commits by default, and `--no-group-commit` or `SPARKLES_GROUP_COMMIT=off` turns it off. Phases 1 and 2 add metrics, an idle fast path, modeled power-loss and failed-sync checks, and admission of most write kinds. Phase 3 meets the forge gates for serial parity, concurrency, fairness, mixed load and churn, passes the `kill -9` and 1,000-point `log-writes` runs on ext4 and XFS, adds the model-based stress test, and frames headerless logs strictly from a recorded offset. Indexed datasets kept the ordinary path until item 86. A headerless log that this release has never opened for writing keeps the mixed legacy rules for the bytes it holds. | [X09](specs/X09-group-commit-by-default.md) | S–M | High |
| 4 | **Partly done. Cold sparse lookups.** Historical cold lookups took 2.3–3.1× as long as Oxigraph at 10.5M triples and 1.8–3.8× at 1.05M. The built path includes selective columns, sparse vocabulary indexes, cooperative cache-bounded warmup, coverage-aware advice and vocabulary and numeric profiling before mapped access. Profile lifetime transitions preserve shared observations in a capped 65,536-page epoch with `pagesTruncated`, while corrected-source measurements remain pending. | [X07](specs/X07-cold-sparse-reads.md) Phases 1 and 2, with corrected-source performance gates open | M | High |
| 5 | **Done (2026-10-13). Correctness fixes.** `allShortest` includes equal-cost paths over zero-weight edges. Small bulk commits into large datasets record their changes within the change-log candidate limit. A `gsp-direct-r` endpoint from a Fuseki configuration refuses writes independently of grants. | [F07](specs/F07-path-search.md#roadmap-follow-ups-2026-10-12), [F06](specs/F06-snapshots-and-point-in-time.md#roadmap-follow-ups-2026-10-12), [G08](specs/G08-fuseki-configuration.md#roadmap-follow-ups-2026-10-12) | S | Medium |
| 6 | **Ask evaluation on public benchmarks.** The initial hosted demo pilot stopped on its first request because its gate refused a chunked reply, which left $1.002560 held at its bound. The harness now has a proxy whose cost ledger reconciles such a charge with the billed amount, scores failed Asks, and converts QALD-9-plus, Text2SPARQL'25 and LC-QuAD 2.0, with gold recomputed on DBpedia 2022.12.01 and CK25. The first paid run asked the demo set and CK25 with Haiku, Sonnet and Opus 5.5. Answering the pipeline's clarification questions in the evaluation, a paid run of the DBpedia sets and independent-role evaluation remain. The DBpedia sets now run in about two seconds an Ask (item 89), but most of their questions stop at a clarification between a resource and its namesakes, and LC-QuAD 2.0 waits for a Wikidata load from the full dump. | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12) | S–M | Medium |
| 89 | **Done (2026-10-19). Ask and `check_query` on large datasets.** On DBpedia 2022.12.01, about a billion triples, each `sparkles ask` took about 100 s and most checks failed on the 2 GiB query memory budget. Above ten million quads the tools now read schema reports from the index statistics, corrected exactly for the delta and for graphs outside the selection, and measure a predicate's objects on demand. The planner moves a VALUES table into the branches of a UNION it joins, which `link_entities` and the terms list needed. On DBpedia a `check_query` takes 0.3 s instead of 52.7 s and 11.1 GiB, and an Ask about 2 s with under 512 MiB of anonymous memory. A selection that leaves out graphs with more than a million quads is still scanned. No report is persisted, since the correction costs no more than a scan at any commit. Item 90 covers the shapes the VALUES rewrite misses. | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12) | M | Medium |

## Tier 2: high-impact features and the scale foundation

| # | Item | Spec | Size | Impact |
|---|---|---|---|---|
| 7 | **Partly done. Batch-oriented joins and grouping, then disk spill.** Also covers LIMIT propagation, running state for FOLD aggregates, parallel decoding, earlier first bytes and wider automatic selection. Automatic execution became the default over HTTP, in the CLI and in Node on 2026-10-16 and in the JVM library on 2026-10-17, and the UI's result table still asks for eager execution until it can show a streamed result's unknown total. The streaming parity work of 2026-10-17 closed the cursor's own gaps and widened automatic scans to a thousand rows. On 2026-10-17 the cursor's joins, counts, groupings and DISTINCT moved to column kernels, OPTIONAL counts and other groupings of 20,000 input rows or more stream automatically again, and sorts, groupings and unordered DISTINCT spill to disk, with the eager memory retry. Item 85 added hash-join and merge-join spill, a DISTINCT that runs below its ORDER BY and encrypted spill on 2026-10-18, and spill remains opt-in until all Phase 2b acceptance gates pass. On 2026-10-18 a cursor filter over an index scan began to read and filter the scan in parallel morsels, which brought CONTAINS, STRSTARTS and LANGMATCHES filters to 0.91 to 0.95 times eager execution at eight threads, and a REGEX over subject IRIs to 1.08. A cursor range top-k ranks its range in parallel morsels and holds only its best rows, at 0.89 to 1.06 times eager execution with eight threads over several runs. A filter over a scan in native JSON at one thread reached parity on the same day. These were measured on an 8-vCPU AMD Namespace instance, and the 12-thread measurement on an Intel host is still to be taken. EXISTS and DESCRIBE do not stream. Decoding ahead under many concurrent queries is unmeasured, a JVM cancellation can race the release of its query, and the eager `regex-iri` filter needs per-clone regular expression caches to stop depending on inlining. The Phase 1 follow-ups X08-P1-grant-routing (admit MCP and the other requests that never queue), X08-P1-nested-service (a SERVICE call to the same server), X08-P1-admit-after-body (admit after the request body is read, and wake only the waiters that fit) and X08-P1-retained-results (shrink an eager result's retained estimate to its size) are planned with it. | [X08](specs/X08-disk-spill-and-server-memory.md) Phase 2, [X05](specs/X05-streaming-query-execution.md#roadmap-follow-ups-2026-10-12) | L | Very high |
| 8 | **Partly done. Graph-algorithm library, remainder.** The review follow-ups are done. Every long loop over nodes, rows or heap entries checks cancellation within bounded work, sorts run as one step, every workspace is charged, write-back decodes one batch at a time, `GRAPH ?g` scopes each call to its graph and reads each graph once, and HTTP runs authorize before the body and wait for the memory pool. What remains is the Graphalytics harness with [X06](specs/X06-benchmark-coverage.md), the UI, a JVM handle, the C23 writer and openCypher procedures with F01. | [F14](specs/F14-graph-algorithms.md#outcome) | S–M | High |
| 9 | **Partly done. Materialization at scale.** Phase 1 is built, with rules that read the compressed indexes, a parallel merge, one memory budget per run and a kept closure without the explicit facts. Phase 1 acceptance belongs to item 1 and the cost of small incremental runs to item 87. The separate derived-fact layer and `owl:sameAs` representatives of Phases 2 and 3 remain unbuilt. | [F16](specs/F16-materialization-at-scale.md), [C08](specs/C08-inference-freshness.md#roadmap-follow-ups-2026-10-12) | L | High |
| 10 | **Done (2026-10-13). Vector index maintenance.** Committed rows join the HNSW graph in the background, compactions carry the graph over before they switch, and builds are about 2.2 times faster (63.5 s at 1M × 384, 1.27 times USearch). | [F04](specs/F04-vector-search.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 11 | **Done (2026-10-16). Time slider** over graphs, results, entities and the schema. It builds on the existing `at=` field, history diffs and Memory's "As of" selector. A browser compare of complete results, a bounded commit timeline and a server comparison inside the memory budget and pool shipped with it. A calendar-aware track by time, a branch-qualified selector and a server comparison across branches stay unbuilt, as the spec's Outcome records. | [U01](specs/U01-time-slider.md) | M | High |
| 12 | **GraphRAG community summaries** for Ask and agent memory | [C22](specs/C22-graph-rag-community-summaries.md) | M | High |
| 13 | **Done (2026-10-16). Current shape standards.** SHACL 1.2 node expressions, ShEx 2.2, recursive ShEx typing that survives restarts, and richer drafted shapes. The SHACL 1.2 Core additions that are not expression syntax, ShEx `RESTRICTS` and the `EXACTLY` reference syntax stay out, and the Outcome sections of the specs list them. | [G03](specs/G03-shaclc.md#roadmap-follow-ups-2026-10-12), [G02](specs/G02-shex.md#roadmap-follow-ups-2026-10-12), [C02](specs/C02-schema-discovery.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 14 | **Partly done. Compaction.** Delta changes located per block, new vocabulary without a full rebuild, adaptive thresholds and automatic compaction in embedded use remain open. The removal at open of a build that a power loss left behind shipped on 2026-10-18. | [C13](specs/C13-automatic-compaction.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 15 | **Ingestion.** *Partly done (2026-10-13).* Office documents, more transcript adapters and CSV entity linking shipped. Images and audio remain, with the follow-ups recorded in C18. | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 16 | **Predicate patterns (QLever's pattern trick),** FSST vocabulary compression, IRI encoding and index codecs | [X11](specs/X11-index-compression-and-predicate-patterns.md), [X01](specs/X01-compression-codecs.md#roadmap-follow-ups-2026-10-12) | L | High at scale |
| 86 | **Partly done (2026-10-18). Group commit for indexed datasets.** Datasets with a full-text, spatial or embedding index now group their commits, and no index's durable state names a commit above the durable log. The `kill -9` runs on ext4 and XFS pass with each index after the review's fixes, and the 1,000-point `log-writes` runs passed before them. Forge acceptance under section 17.9 remains open, including serial latency, concurrent throughput and reader latency during writes. | [X09 section 17](specs/X09-group-commit-by-default.md#17-phase-4-group-commit-for-indexed-datasets) | M–L | Medium–high |
| 87 | **Small incremental runs over a kept closure.** The kept closure remembers its lookups of the input graphs, and deletion rounds search in parallel. A change of 1,000 triples at 16 threads now takes 1.0 to 1.6 times its time before the F16 follow-up for RDFS and 1.5 to 2.4 times for OWL 2 RL, short of the 1.2 times goal. The first change's cold reads and the OWL 2 RL insertion remain. An Intel rerun on atlas found no cost in the 1-thread OWL 2 RL full run. | [F16](specs/F16-materialization-at-scale.md#roadmap-follow-ups-2026-10-18) | S–M | Medium |

## Tier 3: big bets

| # | Item | Spec | Size | Impact |
|---|---|---|---|---|
| 17 | **Incremental computation core.** Materialized SPARQL views, live queries that push result changes, and provenance per fact | [F13](specs/F13-incremental-views-and-provenance.md) | XL | Very high |
| 18 | **Explanations.** Derivation trees, "why not?" answers, entailment diffs on branches, and minimal justifications; protections that track provenance, and `reviewedOnly` (which Ask already has, but which doesn't cover the inferred graph) extended to derived facts. Phases 1–2 need only F17 Phase 1; Phase 3 needs F13. | [F18](specs/F18-explanations-and-entailment-diffs.md) | M–L | High |
| 19 | **Query-time reasoning.** A cost-based choice between materialized and on-demand derivation (magic sets), Jena backward rules (today `<-` rules parse but are skipped with a warning), OWL 2 QL rewriting, and part of the Datalog frontend that [specs/README.md](specs/README.md) lists as unspecified | [F17](specs/F17-query-time-reasoning.md) | L | High |
| 20 | **openCypher,** with graph algorithms as procedures and LDBC SNB as acceptance | [F01](specs/F01-cypher.md#roadmap-follow-ups-2026-10-12) | L–XL | High |
| 21 | **Editing forms generated from SHACL shapes,** writing to branches | [U02](specs/U02-shape-driven-forms.md) | M–L | Medium–high |
| 22 | **RML, R2RML and YARRRML loading,** CSVW standard mode and key checks, and tests that Tarql's functions work in templates (`apf:strSplit` already exists in the engine) | [F19](specs/F19-mappings-and-virtual-graphs.md) Phase 1, [C05](specs/C05-tabular-imports.md#roadmap-follow-ups-2026-10-12) | M–L | Medium–high |
| 23 | **Full-text parity.** Lucene analyzers, CJK segmentation, online reindexing, search after uncommitted writes, and historical search | [F03](specs/F03-full-text-search.md#roadmap-follow-ups-2026-10-12) | M each | Medium |
| 24 | **GeoSPARQL.** k-NN with arbitrary joins, better polygon approximations than the existing cell grid, QLever's spatial syntax, and geometry limits: geographic CRSs on datums other than WGS 84, grid-based datum shifts, exact Helmert shifts, curved GML segments and solids, 3D constructions, and lines split at crossings | [G01](specs/G01-geosparql.md#roadmap-follow-ups-2026-10-12) | M | Medium |
| 25 | **Callbacks in other languages** (Python and JavaScript functions inside queries, and native dispatch of Java functions, which today run through ARQ) | [P03](specs/P03-query-extensions.md#roadmap-follow-ups-2026-10-12) | M | Medium |
| 26 | **Local models.** GPU, ONNX and GGUF, rerankers, and models shared between processes | [F12](specs/F12-local-embeddings.md#roadmap-follow-ups-2026-10-12) | M–L | Medium |
| 81 | **Shared memory representation for agents and personal knowledge.** Define a common profile for documents, cited assertions, entities, notes, decisions and tasks, with distinct evidence and dependency relations, time and invalidation rules, and knowledge separated from instructions. This is deferred and has no active implementation lane. | [C17](specs/C17-agent-memory.md#shared-memory-representation), [C18](specs/C18-natural-language-questions-and-ingest.md#shared-memory-workflows) | M–L | High |

## Tier 4: worth doing, not urgent

| # | Item | Spec | Size |
|---|---|---|---|
| 27 | Live query profiling with verified rewrite suggestions | [U04](specs/U04-live-query-profiling.md) | M |
| 28 | Query by example and Bloom-style search phrases | [U03](specs/U03-query-by-example.md) | M |
| 29 | Rendering very large graphs with WebGL, with server-side aggregation | [U06](specs/U06-large-graph-rendering.md) | L |
| 30 | Notebooks stored in the dataset | [U05](specs/U05-notebooks.md) | M |
| 31 | Data-quality timeline and schema diagram | [U07](specs/U07-data-quality-and-schema-diagram.md) | M |
| 32 | Parameterized reusable plans | [X10](specs/X10-reusable-query-plans.md), [P01](specs/P01-python-bindings.md#roadmap-follow-ups-2026-10-12) | L |
| 33 | Per-call overhead in the bindings: JNI calls (3–4× Jena's in-memory store), Node `has()` (1.9 vs 1.3 µs published) and Node export conversion (1.44×) | [P04](specs/P04-jvm-bindings.md#roadmap-follow-ups-2026-10-12), [P05](specs/P05-node-bindings.md#roadmap-follow-ups-2026-10-12) | M |
| 34 | Vector quantization and vector-result caching | [F04](specs/F04-vector-search.md#roadmap-follow-ups-2026-10-12) | M |
| 35 | Multi-field full-text entity documents | [F03](specs/F03-full-text-search.md#roadmap-follow-ups-2026-10-12) | M |
| 36 | Batched (chunked) updates and commit triggers | [C23](specs/C23-batched-updates-and-triggers.md) | M |
| 37 | GraphQL execution: child queries seeded with engine IDs and top-k per parent; later, persisted queries, mutations and subscriptions | [C03](specs/C03-graphql.md#roadmap-follow-ups-2026-10-12) | M |
| 38 | SERVICE enhancer: slice-aware cache, cache functions, disk cache and parallel requests | [G10](specs/G10-service-enhancer.md#roadmap-follow-ups-2026-10-12) | M |
| 39 | Stored updates, multi-value parameters and private stored queries | [C16](specs/C16-stored-queries.md#roadmap-follow-ups-2026-10-12) | S–M |
| 40 | Memory footprint: idle server RSS, Python process RSS and full-text index size | [X08](specs/X08-disk-spill-and-server-memory.md) Phase 3 | M |
| 41 | Text input in Similar, chunk offsets kept beside vectors, and NixOS embedding settings | [F08](specs/F08-embeddings-on-write.md#roadmap-follow-ups-2026-10-12) | S |
| 42 | MCP progress notifications, per-tool metrics and performance measurements | [C11](specs/C11-mcp-server.md#roadmap-follow-ups-2026-10-12) | S–M |
| 43 | Virtual graphs over SQL, Parquet and Iceberg | [F19](specs/F19-mappings-and-virtual-graphs.md) Phase 2 | XL |
| 44 | A local-first engine in WebAssembly that syncs through RDF Patch and branches | [F20](specs/F20-local-first-engine.md) | XL |
| 72 | JSON query languages, and a Datalog surface syntax for rules and queries | [C24](specs/C24-json-query-languages.md), [F17](specs/F17-query-time-reasoning.md) | M–L |
| 78 | Incremental write-time validation of recursive SHACL shapes and SHACL-SPARQL constraints not anchored at the focus node | [C10](specs/C10-write-time-validation.md#roadmap-follow-ups-2026-10-12) | M–L |
| 82 | Cheaper vector catch-up and measured half-precision recall. Catch-up scans only new rows, binary16 recall is measured on Euclidean and dot-product data with an offset, a measurement picks the worker pool, and a test cancels a carry. | [F04](specs/F04-vector-search.md#roadmap-follow-ups-2026-10-12) | S–M |
| 83 | The ShEx guard checks the SHA-256 of its schema copy against `schema.sha256` in its `validation.json` when it loads, and refuses the guard or validates in full when they differ, so a copy edited outside Sparkles is not trusted with the stored baseline counts. | [C10](specs/C10-write-time-validation.md#known-gap-schema-copy-digest-2026-10-16) | S |
| 84 | Large query plans. The deadline checks, the plan JSON without ids, the iterative explanation reader and the teardown of refused queries shipped. An n-ary join, designed in X05, would let the 4000-level plan limit rise, and planning time still grows with the square of the number of triple patterns. A star of a variable that many patterns share remains slow, about 3 s at the limit on a dataset of 31,815 triples. | [X05](specs/X05-streaming-query-execution.md#large-basic-graph-patterns) | M |
| 85 | **Implementation done (2026-10-21). Hash-join spill, ordered DISTINCT spill and encrypted spill.** Review corrections bound retained files and batch admission, with tests for nested merge EOF, keyless builds and DISTINCT accounting. Spill remains opt-in, and item 93 records the full-scale acceptance matrix and remaining execution work. | [X08](specs/X08-disk-spill-and-server-memory.md) §6.3 and §6.7 | M |
| 93 | The 20-million-row mixed-type sort and 10-million-group aggregate and DISTINCT matrix pass in plain and encrypted modes under 64 MiB. The performance investigation and review of enabling spill by default remain open. Ordered join output incurs extra writes to restore probe order, and a DISTINCT over ordered input other than an ORDER BY still keeps its whole set. | [X08](specs/X08-disk-spill-and-server-memory.md#full-scale-spill-acceptance-item-93-2026-10-21) | M |
| 88 | Allocator overhead of the server-wide caches. The heap behind the result cache is about 1.2 times the bytes it counts, and evicted entries leave free pages in the heaps of the tokio and Rayon threads, which the amended A1 bound of X08 allows for. The idle release could collect on the tokio workers too, or the caches could charge the sizes that mimalloc reports. The item also covers about 3% of extra instructions on queries of a few microseconds between 899fb251 and 4d174d02, which profiles spread over scans, the index join, reallocation and the clock. | [X08](specs/X08-disk-spill-and-server-memory.md#gates) | S–M |
| 90 | VALUES pushdown in more shapes. The rewrite of item 89 fires only at a join the planner plans as a whole, so `GRAPH <g> { VALUES … { A } UNION { B } }` and joins reached while flattening a group keep the table outside. A branch FILTER that reads the table's variable, such as `FILTER(isLiteral(?l))`, also keeps the table outside it, so the branch still reads its whole pattern. | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12) | S–M |
| 91 | **SP²Bench planner losses are partly addressed.** A bounded UNION screen keeps cheap plans ordinary and cuts `q8` operator rows from 875,171 to 6,439 and `q12b` from 861,737 to 4,856 at 50k triples. A bounded ASK policy avoids over-budget equality candidates for `q12a`, while Forge acceptance at 10M and the `q4`, `q5a` and `q6` failures remain open. | [X06](specs/X06-benchmark-coverage.md#sp2bench-planner-follow-up-2026-10-21) | M |

### Spill performance investigation

For item 85, repeat the default-path `expr-order-key` and `distinct-join` comparisons
on a quiet Intel host with matched builds and inputs. Capture the executed plans and
profile shared cursor buffer accounting, join and DISTINCT work before assigning a
cause. Bisect reproducible slowdowns below the push gate as well as threshold failures.
The initial screen passed the gate but does not establish that performance is
unchanged across queries.

Measure ordered hash joins with a GROUP BY parent at tight and larger spill budgets,
including consolidation writes, merge passes, charged memory, result equality and
cleanup. Compare memory budgets within one build separately from comparisons between
builds, and retain coverage for partially ordered DISTINCT. These investigations belong
to [X08](specs/X08-disk-spill-and-server-memory.md), with reference measurements on forge
when the host is free.

## Tier 5: backlog

These items are smaller or narrower. Each has a design sketch in the linked spec.

| # | Item | Spec | Size |
|---|---|---|---|
| 92 | Per-crate Nix builds. Compare the working crate2nix prototype with Crane under matched build settings, measure which crates rebuild after source changes, and validate packaging and platform coverage before deciding whether to migrate. | [X13](specs/X13-per-crate-nix-builds.md) | S–M |
| 45 | Branches: a remote branch-relink CLI, and clones and archive transfer between servers | [F09](specs/F09-branches-and-merges.md#roadmap-follow-ups-2026-10-12) | M |
| 46 | Notifications: an email channel, an administration UI, broader backup-failure coverage, and delivery that doesn't let a slow channel delay the others | [C21](specs/C21-outbound-notifications.md#roadmap-follow-ups-2026-10-12) | M |
| 47 | Rust client: typed bodies for the remaining administration calls, proactive rate-limit pacing, and credential integration tests | [P02](specs/P02-rust-client.md#roadmap-follow-ups-2026-10-12) | S–M |
| 48 | SHACLC formatting; linting of N-Triples, N-Quads, JSON-LD and embedded SHACL queries; linting in the shapes editor; an MCP lint tool | [X02](specs/X02-formatter.md#roadmap-follow-ups-2026-10-12), [X04](specs/X04-linter.md#roadmap-follow-ups-2026-10-12) | M |
| 49 | Write previews: local dry-run flags, UI previews beyond the CSV import panel and merge page, `If-Match` on `/{ds}/update`, and the handling of read-only servers and costly bulk previews | [C15](specs/C15-write-previews.md#roadmap-follow-ups-2026-10-12) | S–M |
| 50 | Restricted views: token scopes limited to graphs, validation and statistics for restricted views, and text scoring local to what a caller can see | [C12](specs/C12-graph-access-control.md#roadmap-follow-ups-2026-10-12) | M |
| 51 | ARQ: parallel LATERAL, user DESCRIBE handlers, the remaining client options and builder methods, and `shortest(…)` evaluation | [G06](specs/G06-arq-query-extensions.md#roadmap-follow-ups-2026-10-12) | M |
| 52 | History: pin rebasing, periods per quad, change feeds per graph, reconstructing expired states, and history of settings | [F06](specs/F06-snapshots-and-point-in-time.md#roadmap-follow-ups-2026-10-12) | M |
| 53 | Inference: input-graph and RDFS settings in the UI, the remaining RDFS-on-read surfaces, and controls for the import policy | [C08](specs/C08-inference-freshness.md#roadmap-follow-ups-2026-10-12) | S–M |
| 54 | ShEx: combined SHACL and ShEx guards, schemas from a graph plus a file, and CLI outbound controls | [G02](specs/G02-shex.md#roadmap-follow-ups-2026-10-12) | M |
| 55 | Dynamic and Nushell completions, and an OpenAPI "try it" page with named schemas | [X03](specs/X03-openapi-and-completions.md#roadmap-follow-ups-2026-10-12) | S–M |
| 56 | Legacy MCP resource subscriptions | [C11](specs/C11-mcp-server.md#roadmap-follow-ups-2026-10-12) | S |
| 57 | Scratch-branch expiry as a dataset setting, and expiry under stdio (today only `--mcp-scratch-branch-ttl` on the server) | [C17](specs/C17-agent-memory.md#roadmap-follow-ups-2026-10-12) | S |
| 71 | An OWL 2 ontology object API, as Jena's `jena-ontapi` provides | [F21](specs/F21-ontology-api.md) | L |
| 73 | Format coverage: all formats in `load_str`, `dump_graph`, SPARQL `LOAD` and the JVM and Node bindings (which read none of Jena's formats), and loading Thrift, Protobuf, RDF/JSON and TriX without a temporary N-Quads copy | [G05](specs/G05-command-line-tools.md#roadmap-follow-ups-2026-10-12) | S–M |
| 74 | RDF Patch: patches with several transactions applied as several commits, blank-node identity across datasets, and patch logs | [F10](specs/F10-replication.md#roadmap-follow-ups-2026-10-12) | M |
| 75 | Access control: validation endpoints for callers whose grants cover part of a dataset, the change feed for data-dependent protections, and hiding IRIs that visible triples mention | [C12](specs/C12-graph-access-control.md#roadmap-follow-ups-2026-10-12), [C12b](specs/C12b-triple-access-control.md#roadmap-follow-ups-2026-10-12) | M–L |
| 76 | Jena compatibility: complete assembler bodies, direct Graph Store naming per dataset, and JVM metrics | [G08](specs/G08-fuseki-configuration.md#roadmap-follow-ups-2026-10-12), [P04](specs/P04-jvm-bindings.md#roadmap-follow-ups-2026-10-12) | S–M |
| 77 | Assistant ingestion: the Ingest panel stores uploads without extracting facts when server ingestion is off, without saying so; and server-side document ingestion has no redaction (memory import redacts with built-in and custom patterns) | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12), [C19](specs/C19-layered-settings.md#roadmap-follow-ups-2026-10-12) | S–M |
| 79 | Nonnegative weighted betweenness semantics. Define zero-weight path counting and validate an algorithm before extending the current finite, strictly positive domain. | [F14](specs/F14-graph-algorithms.md#nonnegative-weighted-betweenness-semantics) | M–L |
| 80 | Positive-weight numerical robustness. Extend the refused mixed-scale domain and define distance and path-count approximation guarantees for weighted betweenness. | [F14](specs/F14-graph-algorithms.md#positive-weight-numerical-robustness) | M–L |

## Tier 6: release work

This work is done when the first release is cut. The release waits until Tiers 1–5 have
landed (decided 2026-10-13), because leading on performance and features matters more
than releasing early.

| # | Item | Spec | Size |
|---|---|---|---|
| 58 | Packages: publishing to PyPI, Maven Central and npm, Windows and Intel macOS builds, a musl JVM build, PyPy and free-threaded wheels, and Python async wrappers. The workflows exist, with their publish jobs switched off. | [X12](specs/X12-release-engineering.md), [P01](specs/P01-python-bindings.md#roadmap-follow-ups-2026-10-12), [P04](specs/P04-jvm-bindings.md#roadmap-follow-ups-2026-10-12), [P05](specs/P05-node-bindings.md#roadmap-follow-ups-2026-10-12) | M–L |
| 59 | Server binaries and published container images | [X12](specs/X12-release-engineering.md) | M |
| 60 | A stability policy for the HTTP, CLI and library APIs, with its checks (X12 Phase 4). The on-disk side (Phase 3) is done: every file kind has a version marker in one registry, older formats inside the window open in place, `sparkles migrate` runs upgrade steps, and fixtures prove it. | [X12](specs/X12-release-engineering.md) | S–M |
| 61 | Whole-server backup and restore, all datasets in one request, a remote backup CLI, and repository copy | [F05](specs/F05-snapshot-repositories.md#roadmap-follow-ups-2026-10-12) | M |
| 62 | Audit events for creating, deleting and cloning datasets and for creating and deleting branches (restores already emit them) | [C09](specs/C09-dataset-access-control.md#roadmap-follow-ups-2026-10-12) | S |
| 63 | Mutual TLS, OCSP, opaque-token introspection, front-channel logout and Cloudflare group lookup | [C09](specs/C09-dataset-access-control.md#roadmap-follow-ups-2026-10-12) | M |
| 64 | Binding the listener before datasets open, and degraded startup when a dataset fails to open | [C01](specs/C01-observability-and-budgets.md#roadmap-follow-ups-2026-10-12) | M |
| 65 | Encryption: key management workflows, Vault and cloud KMS, recovery export, and encryption of live datasets and derived indexes | [F11](specs/F11-encryption-at-rest.md#roadmap-follow-ups-2026-10-12) | L–XL |
| 66 | Replication: read replicas, preserved primary IDs, promotion, failover, synchronous replication, and a patch log or continuous archive | [F10](specs/F10-replication.md#roadmap-follow-ups-2026-10-12) | XL |
| 67 | MCP OAuth: RFC 9728 protected-resource metadata, audiences bound to the resource, and scope challenges. OIDC tokens are already audience-checked on every route. | [C11](specs/C11-mcp-server.md#roadmap-follow-ups-2026-10-12) | S–M |
| 68 | **Done (2026-10-13).** Runtime secrets are sealed by default with a configured key, a systemd credential or a key that the server generates in its data directory, offline `unseal` and `rekey` commands move the values, and a build without the `backup` feature keeps plaintext. | [C19](specs/C19-layered-settings.md#roadmap-follow-ups-2026-10-12) | S–M |

## Tier 7: compatibility leftovers

These were excluded earlier on purpose. They stay on the list and are reconsidered when
users ask for them.

| # | Item | Spec | Size |
|---|---|---|---|
| 69 | ARQ: JavaScript functions, `afn:context`, the Leviathan library, the `JSON` query form and locale collation | [G06](specs/G06-arq-query-extensions.md#roadmap-follow-ups-2026-10-12) | M each |
| 70 | GeoSPARQL: an optional mode that reproduces Jena's answers where Sparkles diverges on purpose ([COMPARISON.md](COMPARISON.md#geosparql)) | [G01](specs/G01-geosparql.md#roadmap-follow-ups-2026-10-12) | M |

## Keeping this current

Update this document when an item ships, changes tier or gets a new spec. A new known gap in
FEATURES gets a roadmap item in the same commit. When an item ships:

* Its spec's Outcome records what landed.
* [FEATURES.md](FEATURES.md) and [BENCHMARKS.md](BENCHMARKS.md) change in the same commit, and the Known gaps bullet drops the item's number.
* The row here is removed or marked done.
