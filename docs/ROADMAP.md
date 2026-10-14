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

Item numbers are stable identifiers, not a ranking: a new item takes the next free
number in whatever tier it belongs to. Impact is judged against the goal above. Tiers 1–4 are the main plan. Tiers 5–7 are
work that is wanted at a lower priority, and items move between tiers as results come in.

## How the work is organized

The tiers set the order of priority. Within a tier, most items can run in parallel.
Three tracks run through the tiers:

| Track | Items | Spec order |
|---|---|---|
| **Performance** | 1 → 2 → 3 → 4 → 7 → 16 | X06 → X08 → X09 → X07 → X08 Phase 2 → X11 |
| **Graph features** | 8 → 12 → 20 | F14 → C22 → F01 |
| **Reasoning** | 9 → 19 → 18, with 17 before F18 Phase 3 | F16 → F17 → F18 Phases 1–2 → F13 → F18 Phase 3 |

The UI items (11, 21, 27–31) fill time when a track is waiting on benchmark results.

Two decisions depend on what item 1 measures:

* If Sparkles already leads LDBC-style workloads, openCypher (item 20) moves up.
* If spill (item 7) turns out cheaper than expected, the incremental core (item 17)
  starts earlier, because the two share operator machinery.

## Tier 1: now

These items are cheap or foundational.

| # | Item | Spec | Size | Impact |
|---|---|---|---|---|
| 1 | **Benchmark coverage and refresh.** Adds BSBM, SP²Bench, LUBM, OWL2Bench, LDBC SNB and Graphalytics, Wikidata query logs, sustained writes with compaction, and data above 1.24B triples. Reruns the GeoSPARQL compliance benchmark, and compares vector recall and latency with other engines (today only Sparkles is measured). All comparisons are refreshed at the current head. | [X06](specs/X06-benchmark-coverage.md) | M | Very high |
| 2 | **Server-wide cache sizing and query memory pool.** Today each dataset's block and result caches are sized to the global limit, so memory grows with the number of datasets. Per-class concurrency caps already exist. The pool adds a shared byte budget with admission control. | [X08](specs/X08-disk-spill-and-server-memory.md) Phase 1, [C01](specs/C01-observability-and-budgets.md#roadmap-follow-ups-2026-10-12) | S, then M | High |
| 3 | **Group commit as the default.** Phases 1 and 2 are built: metrics, an idle fast path for a lone writer, sync-failure and user-space power-loss tests, and admission of messages, digests, preconditions, Graph Store, upload, RDF Patch, SHACL-guarded writes and writes during compaction. Left: full-text, spatial and embedding datasets and ShEx guards still commit one at a time, reader p99 at 10.5M and the section 9 gates need `forge`, and the 1,000-point power-loss runs on ext4 and XFS need a host with device-mapper `log-writes`. Then the default can switch. Commits stay synchronized before acknowledgement. | [X09](specs/X09-group-commit-by-default.md) | S–M | High |
| 4 | **Cold sparse lookups.** These are 2.3–3.1× slower than Oxigraph at 10.5M triples and 1.8–3.8× at 1.05M. Column-selective decoding and the sparse vocabulary index already exist. The remaining causes: whole 32,768-row block columns are read and decoded, query constants are looked up one at a time, and the switch between sparse and dense reads applies to the whole process. | [X07](specs/X07-cold-sparse-reads.md) | M | High |
| 5 | **Correctness fixes.** `allShortest` misses equal-cost paths over zero-weight edges. Small bulk commits into large datasets keep only counts in the change log. A read-only `gsp-direct-r` endpoint from a Fuseki configuration is served read-write, with only grants limiting writes. | [F07](specs/F07-path-search.md#roadmap-follow-ups-2026-10-12), [F06](specs/F06-snapshots-and-point-in-time.md#roadmap-follow-ups-2026-10-12), [G08](specs/G08-fuseki-configuration.md#roadmap-follow-ups-2026-10-12) | S | Medium |
| 6 | **Ask evaluation on public benchmarks.** QALD-9-plus first; LC-QuAD 2.0 once Wikidata can be loaded. `scripts/eval-ask` already runs the model matrix on the demo questions. | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12) | S–M | Medium |

## Tier 2: high-impact features and the scale foundation

| # | Item | Spec | Size | Impact |
|---|---|---|---|---|
| 7 | **Batch-oriented joins and grouping, then disk spill.** Also covers LIMIT propagation, running state for FOLD aggregates, parallel decoding and earlier first bytes. | [X08](specs/X08-disk-spill-and-server-memory.md) Phase 2, [X05](specs/X05-streaming-query-execution.md#roadmap-follow-ups-2026-10-12) | L | Very high |
| 8 | **Graph-algorithm library.** PageRank, centrality, components, Louvain and Leiden, similarity, and FastRP and node2vec embeddings | [F14](specs/F14-graph-algorithms.md) | M–L | Very high |
| 9 | **Materialization at scale.** `owl:sameAs` by representatives, rules over the compressed indexes instead of an in-memory copy, and a separate layer for derived facts. Semi-naive evaluation is already parallel; the single-threaded merge in `add_batch` and the barrier after each iteration are not. | [F16](specs/F16-materialization-at-scale.md), [C08](specs/C08-inference-freshness.md#roadmap-follow-ups-2026-10-12) | L | High |
| 10 | **Vector index maintenance.** Incremental HNSW inserts, graphs kept across compactions, background catch-up and build speed | [F04](specs/F04-vector-search.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 11 | **Time slider** over graphs, results, entities and the schema. It builds on the existing `at=` field, history diffs and Memory's "As of" selector. | [U01](specs/U01-time-slider.md) | M | High |
| 12 | **GraphRAG community summaries** for Ask and agent memory | [C22](specs/C22-graph-rag-community-summaries.md) | M | High |
| 13 | **Current shape standards.** SHACL 1.2 node expressions, ShEx 2.2, recursive ShEx typing that survives restarts, and richer drafted shapes | [G03](specs/G03-shaclc.md#roadmap-follow-ups-2026-10-12), [G02](specs/G02-shex.md#roadmap-follow-ups-2026-10-12), [C02](specs/C02-schema-discovery.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 14 | **Compaction.** Delta changes located per block, new vocabulary without a full rebuild, adaptive thresholds, and automatic compaction in embedded use | [C13](specs/C13-automatic-compaction.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 15 | **Ingestion.** Office documents, images and audio; more transcript adapters; CSV entity linking | [C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12) | M | Medium–high |
| 16 | **Predicate patterns (QLever's pattern trick),** FSST vocabulary compression, IRI encoding and index codecs | [X11](specs/X11-index-compression-and-predicate-patterns.md), [X01](specs/X01-compression-codecs.md#roadmap-follow-ups-2026-10-12) | L | High at scale |

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

## Tier 5: backlog

These items are smaller or narrower. Each has a design sketch in the linked spec.

| # | Item | Spec | Size |
|---|---|---|---|
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

## Tier 6: release work

This work is done when the first release is cut.

| # | Item | Spec | Size |
|---|---|---|---|
| 58 | Packages: publishing to PyPI, Maven Central and npm, Windows and Intel macOS builds, a musl JVM build, PyPy and free-threaded wheels, and Python async wrappers. The workflows exist, with their publish jobs switched off. | [X12](specs/X12-release-engineering.md), [P01](specs/P01-python-bindings.md#roadmap-follow-ups-2026-10-12), [P04](specs/P04-jvm-bindings.md#roadmap-follow-ups-2026-10-12), [P05](specs/P05-node-bindings.md#roadmap-follow-ups-2026-10-12) | M–L |
| 59 | Server binaries and published container images | [X12](specs/X12-release-engineering.md) | M |
| 60 | A stability and migration policy for the on-disk format and the APIs. The index refuses a format mismatch with "dump and reload". `dataset.json` has a minimum reader version, but `wal.log`, the catalog's `config.json` and most settings files have no version marker. | [X12](specs/X12-release-engineering.md) | M–L |
| 61 | Whole-server backup and restore, all datasets in one request, a remote backup CLI, and repository copy | [F05](specs/F05-snapshot-repositories.md#roadmap-follow-ups-2026-10-12) | M |
| 62 | Audit events for creating, deleting and cloning datasets and for creating and deleting branches (restores already emit them) | [C09](specs/C09-dataset-access-control.md#roadmap-follow-ups-2026-10-12) | S |
| 63 | Mutual TLS, OCSP, opaque-token introspection, front-channel logout and Cloudflare group lookup | [C09](specs/C09-dataset-access-control.md#roadmap-follow-ups-2026-10-12) | M |
| 64 | Binding the listener before datasets open, and degraded startup when a dataset fails to open | [C01](specs/C01-observability-and-budgets.md#roadmap-follow-ups-2026-10-12) | M |
| 65 | Encryption: key management workflows, Vault and cloud KMS, recovery export, and encryption of live datasets and derived indexes | [F11](specs/F11-encryption-at-rest.md#roadmap-follow-ups-2026-10-12) | L–XL |
| 66 | Replication: read replicas, preserved primary IDs, promotion, failover, synchronous replication, and a patch log or continuous archive | [F10](specs/F10-replication.md#roadmap-follow-ups-2026-10-12) | XL |
| 67 | MCP OAuth: RFC 9728 protected-resource metadata, audiences bound to the resource, and scope challenges. OIDC tokens are already audience-checked on every route. | [C11](specs/C11-mcp-server.md#roadmap-follow-ups-2026-10-12) | S–M |
| 68 | Assistant secrets sealed by default. `--secrets-key` sealing exists and seals existing secrets in place, but only in builds with the `backup-encryption` feature: the Linux Nix package has it, and the Dockerfile and default builds don't. | [C19](specs/C19-layered-settings.md#roadmap-follow-ups-2026-10-12) | S–M |

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
