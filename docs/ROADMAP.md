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
| 6 | **Ask evaluation on public benchmarks.** The evaluation should answer the pipeline's clarification questions, which stop most DBpedia questions at a choice between a resource and its namesakes. A paid run of the DBpedia sets and an evaluation with independent roles remain, and LC-QuAD 2.0 waits for a Wikidata load from the full dump. | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12) | S–M | Medium |

## Tier 2: high-impact features and the scale foundation

| # | Item | Spec | Size | Value |
|---|---|---|---|---|
| 7 | **Batch-oriented execution and disk spill.** Spill stays opt-in until X08 Phase 2b's acceptance gates pass, and several cases still run eagerly. The [remaining work](#remaining-work-for-item-7) is listed below. | [X08](specs/X08-disk-spill-and-server-memory.md) Phase 2, [X05](specs/X05-streaming-query-execution.md#roadmap-follow-ups-2026-10-12) | L | Very high |
| 8 | **Graph-algorithm library.** The full Graphalytics harness with X06, the UI, a JVM handle, the C23 writer and openCypher procedures with F01 remain. | [F14](specs/F14-graph-algorithms.md#outcome) | S–M | High |
| 9 | **Materialization at scale.** The separate derived-fact layer of Phase 2 and the `owl:sameAs` representatives of Phase 3 remain. | [F16](specs/F16-materialization-at-scale.md), [C08](specs/C08-inference-freshness.md#roadmap-follow-ups-2026-10-12) | L | High |
| 12 | **GraphRAG community summaries.** The paid acceptance runs wait for approval, and the evaluation of §9 follows them. Counting changes from an F13 view waits for item 17. | [C22](specs/C22-graph-rag-community-summaries.md) | M | High |
| 14 | **Compaction.** Delta changes located per block, new vocabulary without a full rebuild, adaptive thresholds and automatic compaction in embedded use remain. | [C13](specs/C13-automatic-compaction.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 15 | **Ingestion of images and audio.** The remaining follow-ups are recorded in C18. | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 86 | **Group commit for indexed datasets.** The p90 of spatial serial inserts at 10.5M misses its gate, and the reader gates are not established. Items 110 to 112 follow up, and the text and spatial mixed cells rerun after item 110. | [X09 section 17](specs/X09-group-commit-by-default.md#17-phase-4-group-commit-for-indexed-datasets) | M–L | Medium–high |
| 113 | **Small changes and undos over a kept closure.** Changes of 100 triples at 16 threads and undos at 1 thread remain slower than before the F16 follow-up, and a profile of the full run should find or rule out a cost of about 5%. An opt-in in-memory index of the input facts, charged to the memory budget, is one option. | [F16](specs/F16-materialization-at-scale.md#roadmap-follow-ups-2026-10-18) | M | Low |

### Remaining work for item 7

* Spill stays opt-in until all X08 Phase 2b acceptance gates pass, which item 93 tracks.
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
| 19 | **Query-time reasoning.** Phase 2 adds `auto` with its cost model, the dataset setting and hybrid rules. Phase 3 adds OWL 2 QL rewriting, reuse of tables across commits, the MCP argument, the UI and tables kept current by F13. Items 190 to 193 cover what Phase 1 left. | [F17](specs/F17-query-time-reasoning.md#outcome) | L | High |
| 18 | **Explanations.** Derivation trees, "why not?" answers, entailment diffs on branches and minimal justifications, with protections that track provenance and `reviewedOnly` extended to derived facts. Phases 1 and 2 need only F17 Phase 1, and Phase 3 needs F13. | [F18](specs/F18-explanations-and-entailment-diffs.md) | M–L | High |
| 17 | **Incremental computation core.** Materialized SPARQL views, live queries that push result changes, and provenance per fact. | [F13](specs/F13-incremental-views-and-provenance.md) | XL | Very high |
| 23 | **Full-text parity.** Lucene analyzers, CJK segmentation, online reindexing, search after uncommitted writes, and historical search. | [F03](specs/F03-full-text-search.md#roadmap-follow-ups-2026-10-12) | M each | Medium |
| 24 | **GeoSPARQL.** k-NN with arbitrary joins, better polygon approximations than the cell grid and QLever's spatial syntax. The geometry limits to lift are datums other than WGS 84, grid-based and exact Helmert shifts, curved GML segments and solids, 3D constructions, and lines split at crossings. | [G01](specs/G01-geosparql.md#roadmap-follow-ups-2026-10-12) | M | Medium |
| 27 | **Live query profiling** with verified rewrite suggestions. | [U04](specs/U04-live-query-profiling.md) | M | Not rated |
| 28 | **Query by example** and Bloom-style search phrases. | [U03](specs/U03-query-by-example.md) | M | Not rated |
| 21 | **Editing forms generated from SHACL shapes.** Phase 3 adds nested forms for blank nodes, the list editor and `sh:or` and `sh:xone` choices. Validation of nodes more than two triples from the edited resource, in a check without a guard, also remains. | [U02](specs/U02-shape-driven-forms.md#outcome) | M–L | Medium–high |
| 22 | **RML, R2RML and YARRRML loading,** CSVW standard mode and key checks, and tests that Tarql's functions work in templates. | [F19](specs/F19-mappings-and-virtual-graphs.md) Phase 1, [C05](specs/C05-tabular-imports.md#roadmap-follow-ups-2026-10-12) | M–L | Medium–high |
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
| 91 | **SP²Bench planner losses.** Jena stays 12 times faster on `q12a` and Oxigraph 1.7 times faster on `q12b`. The `q4` answer is 1.96 GB of TSV and needs a `--max-result-mb` above the default. | [X06](specs/X06-benchmark-coverage.md#sp2bench-planner-follow-up-2026-10-21), [follow-up](specs/X06-benchmark-coverage.md#sp2bench-q4-and-q6-follow-up-2026-10-23) | M |
| 93 | **Spill execution costs and the default.** The full-scale acceptance matrix passes. Grouping cost above ordered spilled joins, the extra writes that restore probe order, a DISTINCT over ordered input that keeps its whole set, the [performance investigation](#spill-performance-investigation) and a decision on enabling spill by default remain. | [X08](specs/X08-disk-spill-and-server-memory.md#full-scale-spill-acceptance-item-93-2026-10-21) | M |
| 109 | **`order-by-full` with one codegen unit for sparkles-core.** The sort ran slightly slower under this build setting. Profile the sort and its comparator under both settings to find what lost its inlining. | [Benchmarking](BENCHMARKING.md#release-build-settings) | S |
| 110 | **Write stall of the locked full-text checkpoint.** All writers stop for 70 to 150 ms about every 5.4 s under steady writes, probably while a checkpoint holds the writer lock through its file sync. Confirm and bound the stall, then rerun the text and spatial mixed cells to settle item 86's reader gates. | [X09 Outcome](specs/X09-group-commit-by-default.md#phase-4) | S–M |
| 111 | **Spatial serial slowdowns of group commit at 10.5M.** Single geometry inserts and IRI inserts run a few percent slower at p90 with grouping. Find the cause, then fix it or state the tradeoff. | [X09 Outcome](specs/X09-group-commit-by-default.md#phase-4) | S–M |
| 112 | **Fence cost of group commit on indexed datasets.** A fence costs about half as much again as an ordinary commit and carries only about two commits with four writers. Find why and whether it can carry more, and measure it on a drive in its fast state too. | [X09 Outcome](specs/X09-group-commit-by-default.md#phase-4) | M |
| 114 | **Client time of large results in `optional-chain`.** The query got faster on the server but slower at the client, so the time is spent writing or sending its rows. Profile the result serialization and the response path of a large SELECT. | [Benchmarking](BENCHMARKING.md) | S |
| 160 | **FSST single-term lookups.** Looking up one term at a time with FSST costs more than decoding each earlier key in full, which slows `star-join` and cold long-literal lookups. Find a lookup path that keeps the partial decode for long literals without the cost on short keys. | [X11](specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads) | S |
| 161 | **Front coding on warm DBpedia after item 108.** A single warm round measured front coding about 2% slower on short queries. Rerun the warm DBpedia suite on forge in several pinned rounds and fix any query that stays slower. | [X11](specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads) | S |
| 180 | **Plan pattern references on small queries.** The references of U04 Phase 4 add instructions and a few percent of server time to small queries. Build them only for a plan that is described, or reuse the parsed query's terms, and measure with two builds from one host. | [Benchmarks](BENCHMARKS.md#the-cost-of-plan-pattern-references), [U04](specs/U04-live-query-profiling.md#phase-4-2026-10-23) | S–M |
| 181 | **Compiler provenance in the Namespace query screen.** `scripts/nsc-bench.sh` caches a binary by commit alone. Record each binary's compilers as `scripts/nsc-writes.sh` does and refuse to compare binaries built with different ones. | [Benchmarking](BENCHMARKING.md#benchmark-scripts) | S |
| 182 | **`subquery-agg` slowdown.** The Namespace screen measured `subquery-agg` about 6% slower after the U02 forms, U11 Phase 3 and item 94 merges. Check it on forge with pinned rounds, and bisect those merges if it holds. | [Benchmarking](BENCHMARKING.md) | S |

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
| 140 | **Linked views remainder in the UI.** Colour the embedding map by a kept Leiden run, count date facets by month for short spans, give the facets a full-screen tab on phones, and let Explore's map and facets read a branch. | [U11](specs/U11-linked-views.md#outcome) | M |
| 141 | **Frame times of the linked views on a GPU.** Run U11's A9 frame-rate and lasso targets in a Playwright performance project on a GPU runner, since they were measured only in software rendering. | [U11](specs/U11-linked-views.md#outcome) | S |
| 142 | **Projections on disk and for partial callers.** Decide whether to write projections beside the index in a versioned file, and whether to serve a projection filtered to what a caller can read instead of answering `403`. | [U11](specs/U11-linked-views.md#10-open-questions) | M |
| 150 | **Bidirectional weighted path search.** A weighted search runs Dijkstra's algorithm from the source alone and is far slower than an unweighted one. Search from both ends, run Yen's spur searches on it, and bound them by the best cost found so far. | [F07](specs/F07-path-search.md), [U09](specs/U09-path-finder.md#outcome) | M |
| 105 | **Canvas lenses, Phase 3.** First an explanation search over a materialized closure, then the derivation tree of an inferred edge on the canvas. | [U10](specs/U10-canvas-lenses.md) Phase 3 | L |

Item 105 Phase 3 comes last because its explanation search is the most open engine work
of the set, and U10's open question 1 asks whether to build it before F17 or wait for
item 18.

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
| 66 | **Replication.** Read replicas, preserved primary IDs, promotion, failover, synchronous replication, and a patch log or continuous archive. | [F10](specs/F10-replication.md#roadmap-follow-ups-2026-10-12) | XL |
| 67 | **MCP OAuth.** RFC 9728 protected-resource metadata, audiences bound to the resource, and scope challenges. | [C11](specs/C11-mcp-server.md#roadmap-follow-ups-2026-10-12) | S–M |

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
- **106.** Validation overlay. [U07](specs/U07-data-quality-and-schema-diagram.md#11-phase-4-a-validation-overlay-on-the-graph-2026-10-23) Phase 4
- **107.** Linked views. [U11](specs/U11-linked-views.md#outcome)
- **108.** Faster FSST reads. [X11](specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads)

## Keeping this current

Update this document when an item ships, changes tier or gets a new spec. A new known gap
in FEATURES gets a roadmap item in the same commit. When an item ships, its spec's Outcome
records what landed, [FEATURES.md](FEATURES.md) and [BENCHMARKS.md](BENCHMARKS.md) change
in the same commit, and the Known gaps bullet drops the item's number. The item's row then
moves to a one-line entry under [Completed](#completed). When part of an item ships, its
row is cut down to the part that remains.
