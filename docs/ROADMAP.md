# Roadmap

This is the plan for making Sparkles the best graph database for performance across the
published benchmarks and for practical use. It also covers filling the real feature
gaps in RDF extensions, reasoning, GeoSPARQL, search, agent memory and integration. The
first public release is not the goal of this plan. Packaging and the release contract
are listed at the end and wait until a release is cut.

The order weighs implementation difficulty against impact. Each item links to the spec
that holds its design. New items have their own spec. Items that extend an existing spec
are designed in a **Roadmap follow-ups (2026-10-12)** section at the end of that spec's
Outcome. [FEATURES.md](FEATURES.md#known-gaps) remains the record of current limits,
and [BENCHMARKS.md](BENCHMARKS.md#where-sparkles-loses) remains the record of measured
losses.

Sizes are estimates:

| Size | Meaning |
|---|---|
| S | Days |
| M | One to two weeks |
| L | Several weeks |
| XL | A quarter or more |

Impact is judged against the goal above.

## How the work is organized

The tiers set the order of priority. Within a tier, most items can run in parallel.
Three tracks run through the tiers:

| Track | Items | Spec order |
|---|---|---|
| **Performance** | 1 → 2 → 3 → 4 → 7 → 16 | X06 → X08 → X09 → X07 → X08 Phase 2 → X11 |
| **Graph features** | 8 → 12 → 20 | F14 → C22 → F01 |
| **Reasoning** | 9 → 17 → 18 → 19 | F16 → F13 → F18 → F17 |

The UI items (11, 21, 27–31) fill time when a track is waiting on benchmark results.

Two decisions depend on what item 1 measures:

* If Sparkles already leads LDBC-style workloads, openCypher (item 20) moves up.
* If spill (item 7) turns out cheaper than expected, the incremental core (item 17)
  starts earlier, because the two share operator machinery.

## Tier 1: now

These items are cheap or foundational.

| # | Item | Spec | Size | Impact |
|---|---|---|---|---|
| 1 | **Benchmark coverage and refresh.** Adds BSBM, SP²Bench, LUBM, OWL2Bench, LDBC SNB and Graphalytics, Wikidata query logs, the GeoSPARQL compliance benchmark, vector recall and latency, sustained writes with compaction, and data above 1.24B triples. All comparisons are refreshed at the current head. | [X06]({X06}) | M | Very high |
| 2 | **Server-wide cache sizing and query memory pool.** Today each dataset's block and result caches are sized to the global limit, so memory grows with the number of datasets. | [X08]({X08}) Phase 1, [C01](specs/C01-observability-and-budgets.md#roadmap-follow-ups-2026-10-12) | S, then M | High |
| 3 | **Group commit, hardened and made the default.** Commits stay synchronized before acknowledgement. | [X09]({X09}) | S–M | High |
| 4 | **Cold sparse lookups.** These are 2.3–3× slower than Oxigraph at 10.5M triples. | [X07]({X07}) | M | High |
| 5 | **Correctness fixes.** `allShortest` misses equal-cost paths over zero-weight edges. Small bulk commits into large datasets keep only counts in the change log. | [F07](specs/F07-path-search.md#roadmap-follow-ups-2026-10-12), [F06](specs/F06-snapshots-and-point-in-time.md#roadmap-follow-ups-2026-10-12) | S | Medium |
| 6 | **Ask evaluation on QALD and LC-QuAD,** with published per-model results | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12) | S–M | Medium |

## Tier 2: high-impact features and the scale foundation

| # | Item | Spec | Size | Impact |
|---|---|---|---|---|
| 7 | **Batch-oriented joins and grouping, then disk spill.** Also covers LIMIT propagation, running state for FOLD aggregates, parallel decoding and earlier first bytes. | [X08]({X08}) Phase 2, [X05](specs/X05-streaming-query-execution.md#roadmap-follow-ups-2026-10-12) | L | Very high |
| 8 | **Graph-algorithm library.** PageRank, centrality, components, Louvain and Leiden, similarity, and FastRP and node2vec embeddings | [F14](specs/F14-graph-algorithms.md) | M–L | Very high |
| 9 | **Materialization at scale.** `owl:sameAs` by representatives, parallel semi-naive evaluation, rules over the compressed indexes, and a separate layer for derived facts | [F16]({F16}), [C08](specs/C08-inference-freshness.md#roadmap-follow-ups-2026-10-12) | L | High |
| 10 | **Vector index maintenance.** Incremental HNSW inserts, graphs kept across compactions, background catch-up and build speed | [F04](specs/F04-vector-search.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 11 | **Time slider** over graphs, results, entities and the schema | [U01](specs/U01-time-slider.md) | M | High |
| 12 | **GraphRAG community summaries** for Ask and agent memory | [C22](specs/C22-graph-rag-community-summaries.md) | M | High |
| 13 | **Current shape standards.** SHACL 1.2 node expressions, ShEx 2.2, recursive ShEx typing that survives restarts, and richer drafted shapes | [G03](specs/G03-shaclc.md#roadmap-follow-ups-2026-10-12), [G02](specs/G02-shex.md#roadmap-follow-ups-2026-10-12), [C02](specs/C02-schema-discovery.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 14 | **Compaction.** Delta changes located per block, new vocabulary without a full rebuild, adaptive thresholds, and automatic compaction in embedded use | [C13](specs/C13-automatic-compaction.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 15 | **Ingestion.** Office documents, images and audio; more transcript adapters; CSV entity linking | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 16 | **Predicate patterns (QLever's pattern trick),** FSST vocabulary compression, IRI encoding and index codecs | [X11]({X11}), [X01](specs/X01-compression-codecs.md#roadmap-follow-ups-2026-10-12) | L | High at scale |

## Tier 3: big bets

| # | Item | Spec | Size | Impact |
|---|---|---|---|---|
| 17 | **Incremental computation core.** Materialized SPARQL views, live queries that push result changes, and provenance per fact | [F13](specs/F13-incremental-views-and-provenance.md) | XL | Very high |
| 18 | **Explanations.** Derivation trees, "why not?" answers, entailment diffs on branches, and minimal justifications; protections and `reviewedOnly` that track provenance | [F18]({F18}) | M–L | High |
| 19 | **Query-time reasoning.** A cost-based choice between materialized and on-demand derivation (magic sets), Jena backward rules, and OWL 2 QL rewriting | [F17]({F17}) | L | High |
| 20 | **openCypher,** with graph algorithms as procedures and LDBC SNB as acceptance | [F01](specs/F01-cypher.md#roadmap-follow-ups-2026-10-12) | L–XL | High |
| 21 | **Editing forms generated from SHACL shapes,** writing to branches | [U02](specs/U02-shape-driven-forms.md) | M–L | Medium–high |
| 22 | **RML, R2RML and YARRRML loading,** CSVW standard mode and key checks, and Tarql functions | [F19](specs/F19-mappings-and-virtual-graphs.md) Phase 1, [C05](specs/C05-tabular-imports.md#roadmap-follow-ups-2026-10-12) | M–L | Medium–high |
| 23 | **Full-text parity.** Lucene analyzers, CJK segmentation, online reindexing, search after uncommitted writes, and historical search | [F03](specs/F03-full-text-search.md#roadmap-follow-ups-2026-10-12) | M each | Medium |
| 24 | **GeoSPARQL.** k-NN with arbitrary joins, polygon approximations, datum shifts and QLever's spatial syntax | [G01](specs/G01-geosparql.md#roadmap-follow-ups-2026-10-12) | M | Medium |
| 25 | **Callbacks in other languages** (Python, JavaScript and Java functions inside queries) | [P03](specs/P03-query-extensions.md#roadmap-follow-ups-2026-10-12) | M | Medium |
| 26 | **Local models.** GPU, ONNX and GGUF, rerankers, and models shared between processes | [F12](specs/F12-local-embeddings.md#roadmap-follow-ups-2026-10-12) | M–L | Medium |

## Tier 4: worth doing, not urgent

| # | Item | Spec | Size |
|---|---|---|---|
| 27 | Live query profiling with verified rewrite suggestions | [U04](specs/U04-live-query-profiling.md) | M |
| 28 | Query by example and Bloom-style search phrases | [U03](specs/U03-query-by-example.md) | M |
| 29 | Rendering very large graphs with WebGL, with server-side aggregation | [U06](specs/U06-large-graph-rendering.md) | L |
| 30 | Notebooks stored in the dataset | [U05](specs/U05-notebooks.md) | M |
| 31 | Data-quality timeline and schema diagram | [U07](specs/U07-data-quality-and-schema-diagram.md) | M |
| 32 | Parameterized reusable plans | [X10]({X10}), [P01](specs/P01-python-bindings.md#roadmap-follow-ups-2026-10-12) | L |
| 33 | Per-call overhead in the bindings: JNI, Node `has()` and export conversion | [P04](specs/P04-jvm-bindings.md#roadmap-follow-ups-2026-10-12), [P05](specs/P05-node-bindings.md#roadmap-follow-ups-2026-10-12) | M |
| 34 | Vector quantization and vector-result caching | [F04](specs/F04-vector-search.md#roadmap-follow-ups-2026-10-12) | M |
| 35 | Multi-field full-text entity documents | [F03](specs/F03-full-text-search.md#roadmap-follow-ups-2026-10-12) | M |
| 36 | Batched (chunked) updates and commit triggers | [C23](specs/C23-batched-updates-and-triggers.md) | M |
| 37 | GraphQL execution: child queries seeded with engine IDs and top-k per parent; later, persisted queries, mutations and subscriptions | [C03](specs/C03-graphql.md#roadmap-follow-ups-2026-10-12) | M |
| 38 | SERVICE enhancer: slice-aware cache, cache functions, disk cache and parallel requests | [G10](specs/G10-service-enhancer.md#roadmap-follow-ups-2026-10-12) | M |
| 39 | Stored updates, multi-value parameters and private stored queries | [C16](specs/C16-stored-queries.md#roadmap-follow-ups-2026-10-12) | S–M |
| 40 | Memory footprint: idle server RSS, Python process RSS and full-text index size | [X08]({X08}) Phase 3 | M |
| 41 | Text input in Similar, chunk offsets kept beside vectors, and NixOS embedding settings | [F08](specs/F08-embeddings-on-write.md#roadmap-follow-ups-2026-10-12) | S |
| 42 | MCP progress notifications, per-tool metrics and performance measurements | [C11](specs/C11-mcp-server.md#roadmap-follow-ups-2026-10-12) | S–M |
| 43 | Virtual graphs over SQL, Parquet and Iceberg | [F19](specs/F19-mappings-and-virtual-graphs.md) Phase 2 | XL |
| 44 | A local-first engine in WebAssembly that syncs through RDF Patch and branches | [F20](specs/F20-local-first-engine.md) | XL |

## Backlog

These are small or narrow items. They stay listed so they aren't lost, and their specs
already describe them.

* **Branches** ([F09](specs/F09-branches-and-merges.md#roadmap-follow-ups-2026-10-12)): a remote branch-relink CLI, and clones and archive transfer between servers.
* **Notifications** ([C21](specs/C21-outbound-notifications.md#roadmap-follow-ups-2026-10-12)): an email channel, an administration UI, broader coverage of backup failures, and delivery that doesn't let a slow channel delay the others.
* **Rust client** ([P02](specs/P02-rust-client.md#outcome)): typed bodies for the remaining administration calls, proactive rate-limit pacing, and integration tests with environment and saved credentials.
* **Formatter and linter** ([X02](specs/X02-formatter.md#outcome), [X04](specs/X04-linter.md#outcome)): SHACLC formatting, and linting of N-Triples, N-Quads, JSON-LD and embedded SHACL queries.
* **Write previews** ([C15](specs/C15-write-previews.md#outcome)): local dry-run flags and UI previews.
* **Restricted views** ([C12](specs/C12-graph-access-control.md#roadmap-follow-ups-2026-10-12)): token scopes limited to graphs, validation and statistics for restricted views, and text scoring local to what a caller can see.
* **ARQ extensions** ([G06](specs/G06-arq-query-extensions.md#outcome)): parallel LATERAL and user DESCRIBE handlers.
* **History** ([F06](specs/F06-snapshots-and-point-in-time.md#roadmap-follow-ups-2026-10-12)): pin rebasing, periods per quad, change feeds per graph, and reconstructing expired states from the change log.
* **Inference** ([C08](specs/C08-inference-freshness.md#roadmap-follow-ups-2026-10-12)): input-graph and RDFS settings in the UI, the remaining RDFS-on-read surfaces, and controls for the import policy.
* **ShEx** ([G02](specs/G02-shex.md#roadmap-follow-ups-2026-10-12)): combined SHACL and ShEx guards, schemas from a graph plus a file, and CLI outbound controls.
* **CLI and OpenAPI** ([X03](specs/X03-openapi-and-completions.md#outcome)): dynamic and Nushell completions, and an OpenAPI "try it" page.
* **MCP** ([C11](specs/C11-mcp-server.md#roadmap-follow-ups-2026-10-12)): legacy resource subscriptions.
* **Agent memory** ([C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12)): scratch-branch expiry settings and stdio expiry.

## Deferred until the first release

These matter for a release but not for the goal above.

* **Packages and binaries** ([P01](specs/P01-python-bindings.md), [P04](specs/P04-jvm-bindings.md), [P05](specs/P05-node-bindings.md)):
  * Publishing to PyPI, Maven Central and npm. The workflows exist, with their publish jobs switched off.
  * Windows and Intel macOS builds.
  * Server binaries and published container images.
* **Release contract:** a stability and migration policy for the on-disk format. Today a format mismatch is refused with "dump and reload".
* **Backups** ([F05](specs/F05-snapshot-repositories.md#outcome)): whole-server backup and restore, a backup of every dataset in one request, and a remote backup CLI.
* **Access control** ([C09](specs/C09-dataset-access-control.md#outcome)):
  * Audit events for creating, deleting and cloning datasets.
  * Mutual TLS, OCSP, opaque-token introspection, front-channel logout and Cloudflare group lookup.
* **Operations** ([C01](specs/C01-observability-and-budgets.md#roadmap-follow-ups-2026-10-12)): binding the listener before datasets open, degraded startup, and OTLP metric export.
* **Encryption** ([F11](specs/F11-encryption-at-rest.md#outcome)): key management workflows, Vault and cloud KMS, and encryption of live datasets and derived indexes.
* **Replication** ([F10](specs/F10-replication.md#outcome)): read replicas, failover and synchronous replication.
* **MCP** ([C11](specs/C11-mcp-server.md#roadmap-follow-ups-2026-10-12)): OAuth protected-resource metadata, audience validation and scope challenges.
* **Secrets** ([C19](specs/C19-layered-settings.md#outcome)): assistant secrets are stored in plaintext unless the server runs with `--secrets-key`.
* **Bindings** ([P01](specs/P01-python-bindings.md), [P04](specs/P04-jvm-bindings.md)): Python async wrappers, PyPy and free-threaded wheels, and a musl JVM build.

## Not planned

* **ARQ features** ([G06](specs/G06-arq-query-extensions.md#outcome)): JavaScript functions, `afn:context`, the Leviathan library, the `JSON` query form and locale collation. These are deliberate exclusions.
* **GeoSPARQL expected answers** ([COMPARISON.md](COMPARISON.md#geosparql)): Sparkles won't change to match every expected answer of the GeoSPARQL benchmarks where it made a deliberate semantic choice.

## Keeping this current

Update this document when an item ships, changes tier or gets a new spec. When an item
ships:

* Its spec's Outcome records what landed.
* [FEATURES.md](FEATURES.md) and [BENCHMARKS.md](BENCHMARKS.md) change in the same commit.
* The row here is removed or marked done.
