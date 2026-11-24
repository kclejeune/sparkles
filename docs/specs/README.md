# Feature specs

These design specs cover Sparkles capabilities and application data profiles beyond its
core port of Apache Jena, Fuseki and QLever's engine. Each spec is written before
implementation from public standards, published papers, the Sparkles codebase, and the
documentation and permissively licensed code of other projects. The
[provenance index](PROVENANCE.md) locates each spec's source and dependency records.

Each spec has a landing page giving its status, shipped phases and links to user guides.
Its design closes with an **Outcome** section summarizing what shipped, material
departures from the design and remaining work. Larger delivery records live in a linked
`OUTCOME.md` with implementation decisions, conformance results, measurements and deferred
work. Original designs are kept as written; where code and design diverge, the code,
[the API reference](../API.md) and [the feature list](../FEATURES.md) describe current
behaviour, and the Outcome explains the difference.

[ARCHITECTURE.md](../ARCHITECTURE.md) connects the implemented decisions across these
specs with an overview of the library, storage, execution and server boundaries.

## Spec layout

Every new spec uses a directory named for its stable ID and descriptive name, with
`README.md` and `SPEC.md` as the default files. This applies to small specs too.

```text
docs/specs/<ID-and-name>/
  README.md
  SPEC.md
```

`README.md` is the engineering landing page. It owns the spec's current status and
phase summary, and links to the design, relevant user guides and remaining roadmap
items. Keep it brief and scannable rather than repeating the design.

`SPEC.md` holds the design, rationale, contracts and acceptance criteria. Its closing
Outcome section gives a concise delivery overview, important departures from the design
and links to remaining work. Small specs can keep their complete outcome there.

Add `OUTCOME.md` when the implementation record benefits from its own sections, such as
multiple delivered phases, substantial design decisions or detailed validation and
measurements. Organize that record by phase or topic, keep the Outcome overview in
`SPEC.md` linked to it, and link back from `OUTCOME.md` to the overview. Supporting
diagrams and fixtures live beside these files when needed.

Keep short source and adoption records in a Provenance section in `SPEC.md`. When a
record needs substantial detail, keep a summary and link there and put the complete
record in the bundle's `PROVENANCE.md`, linking back to the spec. Update the
[central provenance index](PROVENANCE.md) when adding or moving a record.
[Shared records](shared/PROVENANCE.md) cover dependencies and sources shared by several
specs. Preserve source, licensing and adoption history separately from current delivery
status, including earlier draft records that differ from final decisions.

Existing single-file specs keep their paths until migrated. Their opening status block
serves as their landing page, and their Outcome section holds the delivery record. A
migration preserves the spec ID and original design, separates the current summary from
the detailed record, and updates repository links and heading anchors in the same change
because a Markdown stub does not redirect an old heading anchor.

User-facing capability guides hold current capabilities, configuration, usage and feature
roadmaps, and link to the relevant specs here. One spec may support several feature
guides.

## How to use them

* **Design rationale.** A spec explains why a feature works the way it does, covering
  goals, non-goals, rejected alternatives, compatibility choices (usually with Jena or
  Fuseki), failure modes and acceptance examples. Read the spec before changing the
  feature.
* **Kept with the code.** When a change alters behaviour, update the spec's status,
  Outcome overview and detailed delivery record in the same change while leaving the
  original design intact. A new feature of similar size gets a new spec bundle with a
  new ID and its own provenance record linked from the provenance index.
* **Cross-references.** Specs refer to each other by ID and section, such as
  [C10 §6.2](C10-write-time-validation.md). A `§` number names a section of that spec,
  and labels such as `A1` and `A2` number acceptance examples.

## IDs

| Prefix | Group |
|---|---|
| `A` | Application data profiles, source mappings and acceptance contracts built over shared evidence and temporal resources without new native engine semantics. |
| `CI` | Durable commit identity: the commit sequence, dataset IDs, receipts, change digests and change feed that later features build on. |
| `C` | Server, governance and agent capabilities: observability, schema discovery, GraphQL, tabular imports, cloning, inference freshness, access control, write-time validation, MCP, compaction, previews, stored queries, agent memory, natural-language workflows, settings, prefixes, notifications, GraphRAG, triggers, JSON query languages, evidence, durable jobs, valid time, resource catalogs and audit. |
| `F` | Larger storage, query and reasoning features: Cypher, full-text and vector search, path search, local and remote embeddings, backup repositories, history and point-in-time reads, branches and merges, replication, encryption at rest, incremental views, graph algorithms, large-scale and query-time reasoning, explanations, mappings and virtual graphs, local-first WASM, ontology API, analysis lifecycles and ML integration. |
| `G` | Apache Jena compatibility gaps beyond the first release: GeoSPARQL, ShEx, SHACL Compact Syntax, command-line tools, ARQ extensions, Fuseki configuration import and the SERVICE enhancer. |
| `P` | Language bindings, clients and embedding APIs: Python, Rust HTTP client, query extensions, JVM (Apache Jena), Node.js and TypeScript, and the shared library administration API. |
| `X` | Internal engine and engineering architecture: compression codecs, formatter, linter, OpenAPI and completions, streaming execution, benchmark coverage, cold reads, disk spill and memory pools, group commit, reusable plans, index compression, release engineering, unified execution, join planning, storage consolidation, tiered segments, fragment composition, charged buffers and formal verification. |
| `U` | Web UI and desktop features spanning server capabilities: time slider, shape-driven forms, query by example, live query profiling, notebooks, large-graph rendering, data-quality timeline and schema diagram, visual query builder, path finder, canvas lenses, linked views, ontology workbench and desktop application. |

Numbers are stable. Gaps in the numbering correspond to roadmap items without a spec,
such as Cypher writes and property-graph storage. G01 replaced an earlier narrower
geospatial item, F17 and C24 replaced the Datalog frontend, and F15 is unused.
[ROADMAP.md](../ROADMAP.md) orders specified and unbuilt work by priority.

## Specs

A spec's status describes delivery within its named scope. *Implemented* means the listed
phase contract shipped. *Implemented in part* means usable functionality shipped with
listed phases or follow-up items still open. *In progress* is reserved for active
unshipped implementation rather than a design or unfinished validation run. *Specified*
means the design is written and unimplemented. These correspond to Complete, Partially
complete and Planned in the [capability overview](../FEATURES.md#capability-status). A
completed phase does not imply every later extension or compatibility goal is built.

| Spec | Summary | Status |
|---|---|---|
| [A01](A01-shared-entity-representation/README.md) | Versioned entity profiles, cross-domain fixtures and bounded offline replay over evidence and temporal records. | implemented in part |
| [CI](CI-commit-identity.md) | Durable dataset IDs, gap-free commit sequences, commit receipts, `/$/commits`, `sparkles log`, change digests, ETags and the change feed. | implemented (Phases 1–3) |
| [C01](C01-observability-and-budgets.md) | Request IDs, access logs, Prometheus metrics, readiness probes, OpenTelemetry tracing, memory and result budgets, and disconnect cancellation. | implemented in part |
| [C02](C02-schema-discovery.md) | Server-computed dataset schema at `GET /$/schema/{ds}` with VoID exports, SHACL constraints, class profiles, commit diffs, drafted shapes, CLI and UI browser. | implemented |
| [C03](C03-graphql/README.md) | Read-only GraphQL endpoint per dataset with `@rdf`-mapped schemas drafted from shapes or data, compiled to batched SPARQL under caller views and budgets. | implemented in part (Phase 1) |
| [C05](C05-tabular-imports.md) | CSV and TSV imports using default mappings, W3C CSVW metadata or Tarql-style CONSTRUCT templates in `sparkles load`, `sparkles csv` and HTTP uploads. | implemented |
| [C06](C06-clone-to-sandbox.md) | Cloning a consistent dataset snapshot into a new, independent sandbox dataset. | implemented in part (Phases 1–2, part of Phase 3) |
| [C08](C08-inference-freshness.md) | Materialized inference freshness tracking, incremental re-runs, inconsistency diagnostics, `owl:imports` input graphs and RDFS on read. | implemented (Phases 1–4) |
| [C09](C09-dataset-access-control.md) | Authentication (Basic, API tokens, OIDC, trusted proxies, Cloudflare Access, CLI logins, native TLS) and per-dataset authorization. | implemented (Phases 1–2; Phase 3 as C12) |
| [C10](C10-write-time-validation.md) | Per-dataset SHACL or ShEx write guard that validates post-write state before commit, with UI configuration and rejected-write reports. | implemented (Phases 1–3) |
| [C11](C11-mcp-server.md) | Model Context Protocol server over stdio (`sparkles mcp`) and HTTP (`/$/mcp`) with query, schema, search, validation, formatting, opt-in update and stored-query tools. | implemented (Phases 1–3) |
| [C12](C12-graph-access-control.md) | Per-graph and per-endpoint access grants enforced through a filtered dataset view in the engine. | implemented |
| [C12b](C12b-triple-access-control.md) | Triple-level protections that hide triples by predicate, subject class or caller-bound SPARQL pattern, lifted by grants through a masked snapshot. | implemented |
| [C13](C13-automatic-compaction.md) | Background compaction triggered by delta size, log size, age or idle time while writes continue. | implemented |
| [C15](C15-write-previews.md) | Dry runs for SPARQL updates, Graph Store writes, uploads, RDF Patches and MCP writes that report changes, validation, quota and preconditions before rolling back. | implemented |
| [C16](C16-stored-queries.md) | Versioned named queries per dataset with typed parameters, execution by name, MCP tools and saved queries in the UI. | implemented |
| [C17](C17-agent-memory.md) | Server-side MCP tools for agent knowledge graphs: schema-checked queries, entity linking, cited fact recall, RDF 1.2 provenance writes, duplicate checks and scratchpad branches. | implemented in part (Phases 1a–1c) |
| [C18](C18-natural-language-questions-and-ingest/README.md) | Natural-language Ask queries, cited document ingestion, query plan explanations, verified rewrites, memory review inbox and coding-agent transcript imports. | implemented in part (Phases 1–5) |
| [C19](C19-layered-settings.md) | Layered dataset and server settings merging defaults, declared files and runtime overrides, with field locks, `/$/settings`, `sparkles settings`, UI controls and provider presets. | implemented |
| [C20](C20-declared-prefixes.md) | Global and per-dataset declared prefixes managed via settings files, HTTP, CLI and UI, with locks, shadow warnings and editor/LSP completion. | implemented |
| [C21](C21-outbound-notifications.md) | Outbound notifications over signed webhooks, ntfy and email with retries, deduplication, repeats, test sends and server UI settings. | implemented |
| [C22](C22-graph-rag-community-summaries.md) | Hierarchical Leiden community summaries written as cited RDF triples, refreshed within a token budget and used by Ask, recall and memory briefs. | implemented in part (Phase 1, parts of Phases 2–3) |
| [C22b](C22b-report-configurations.md) | Named community-report configurations scoped by principal, projection and language with shared budget enforcement. | specified |
| [C23](C23-batched-updates-and-triggers.md) | Resumable batched updates from SELECT results and commit triggers that run SPARQL updates or webhooks with ordering, recursion limits and a run log. | specified |
| [C24](C24-json-query-languages.md) | JSON-LD node-pattern queries and JSON-LD 1.1 frames compiled to SPARQL algebra, plus n-ary Datalog queries evaluated via F17 demand transformation. | specified |
| [C25](C25-evidence-and-assessments.md) | Shared sources, unasserted claims, evidence references, review decisions, reversible identity matches and managed accepted projections. | implemented in part (Phase 1, local Phase 2) |
| [C26](C26-durable-jobs-and-delivery.md) | Durable dataset jobs, checkpoints, fenced worker leases and atomic output and outbox publication with restart recovery. | implemented in part (Phase 1, `quads` kind) |
| [C27](C27-valid-time-profiles.md) | Validity intervals, uncertain temporal bounds, observation availability, temporal query helpers and an event-time UI control. | implemented in part (Phase 1 A4 open) |
| [C28](C28-chatgpt-model-access.md) | Personal ChatGPT-plan model connections with local sign-in, protected credentials, Responses streaming and delegated background access. | specified |
| [C29](C29-bounded-view-preparation/README.md) | Request-scoped memory, cancellation and deadlines for historical snapshot replay and permission-filtered view preparation. | implemented in part (Phases 1–2) |
| [C30](C30-resource-catalog/README.md) | Server-level catalog of versioned sources, vocabularies and datasets with a content-addressed source store, recipes, pinned imports and licence records. | implemented in part (Phase 1) |
| [C31](C31-checks-and-protected-merges/README.md) | RDF-defined gate suites of SHACL, ShEx, lint, reasoning and quality checks, required checks on protected branches, breaking-change analysis and review workflows. | in progress |
| [C32](C32-linked-data-publishing/README.md) | Content negotiation for hosted namespaces, generated term and ontology documentation pages, catalog-wide term search and an OLS API subset. | specified |
| [C33](C33-agent-platform-controls/README.md) | MCP tool profiles, shape templates, agent update approval queues, change resources, composite tools, branch handoff, framework adapters and A2A skills. | implemented in part |
| [C34](C34-audit-and-erasure/README.md) | Tamper-evident access and agent audit streams, personal-data erasure across history and backups, field-level encryption, PII detection and verifiable credentials. | specified |
| [F01](F01-cypher.md) | Read-only openCypher subset compiled to SPARQL algebra over a property-graph view of RDF using RDF 1.2 reifiers, served at `/{ds}/cypher` and `sparkles cypher`. | specified |
| [F03](F03-full-text-search/README.md) | BM25 full-text search over literals with Tantivy via Jena's `text:query`, supporting Lucene query syntax, highlighting and per-language stemming. | implemented in part (Phases 1–2, most of Phase 3) |
| [F04](F04-vector-search.md) | `spk:vector` literals, similarity functions, `spk:vectorSearch`, per-predicate HNSW vector indexes and hybrid ranking with full-text search. | implemented in part (Phases 1, 1b, 2, most of Phase 3) |
| [F05](F05-snapshot-repositories/README.md) | Incremental, deduplicated backup repositories on local filesystems or S3 with restore, verification, retention policies and garbage collection. | implemented in part (Phase 1, most of Phase 2) |
| [F06](F06-snapshots-and-point-in-time.md) | Point-in-time reads with `?at=`, named snapshots, retention windows, JSON and RDF Patch diffs, change feeds, change logs and history queries. | implemented in part (Phases 1–3) |
| [F07](F07-path-search.md) | Shortest, all-shortest, k-shortest and bounded-length path search via `SERVICE path:search` with predicate filters, directions and reifier weights. | implemented in part (Phases 1–2) |
| [F08](F08-embeddings-on-write.md) | Post-commit vector embeddings computed from selected literals via OpenAI-compatible endpoints, with catch-up, chunking, rate limits and metrics. | implemented (Phases 1–2) |
| [F09](F09-branches-and-merges.md) | Persistent and in-memory branches over shared immutable indexes with three-way and replayed merges, recursive virtual bases, relinking and a commit graph. | implemented in part (Phases 1–2, parts of Phase 3) |
| [F10](F10-replication.md) | Fuseki `patch` operations with `H prev` concurrency checks, plus read replicas that pull RDF Patch commits, bootstrap from backups and promote manually. | implemented in part (Phase 1; Phases 2–3 deferred) |
| [F11](F11-encryption-at-rest.md) | Client-side encrypted backup repositories with keyed blob IDs and chunking, plus AES-256-GCM encryption at rest for datasets under KMS-wrapped keys. | implemented in part (part of Phase 1) |
| [F12](F12-local-embeddings.md) | In-process local embedding models using Candle (`embed-local`), a verified model store via `sparkles models`, the `local` provider kind and Nix helpers. | implemented |
| [F13](F13-incremental-views-and-provenance.md) | Z-set incremental view engine over commit diffs, materialized views via `SERVICE view:read`, live query streams, semiring provenance and per-caller views. | implemented in part (Phase 1) |
| [F14](F14-graph-algorithms.md) | Graph algorithms via `SERVICE algo:…` (PageRank, centrality, components, Louvain, Leiden, similarity, triangles, k-core, BFS, FastRP, node2vec) with budgets and write-back. | implemented in part (Phases 1–2, Phase 3 embeddings) |
| [F16](F16-materialization-at-scale.md) | Large-scale rule materialization directly over index permutations with parallel semi-naive evaluation, a separate derived tier, canonical `owl:sameAs` and incremental maintenance. | implemented in part (Phase 1) |
| [F17](F17-query-time-reasoning.md) | Demand-driven query-time reasoning with magic sets, cost-based `auto` selection, cached snapshot tables, Jena backward and hybrid rules, and OWL 2 QL rewriting. | implemented in part (Phases 1–2) |
| [F18](F18-explanations-and-entailment-diffs.md) | Derivation trees for inferred triples, bounded "why not?" hypotheses, entailment diffs in previews, minimal inconsistency justifications and visibility-safe inference. | implemented in part (Phases 1–2) |
| [F19](F19-mappings-and-virtual-graphs/README.md) | R2RML, RML and YARRRML mappings over CSV, JSON, Parquet and SQL sources, plus virtual graphs that unfold SPARQL into SQL over external tables. | implemented in part (most of Phase 1) |
| [F20](F20-local-first-engine.md) | WebAssembly engine over browser OPFS and edge runtimes with offline local branches, RDF Patch sync and F09 conflict-aware merges. | specified |
| [F21](F21-ontology-api.md) | OWL 2 ontology API over RDF triples with axiom and annotation read/write, EL/QL/RL profile checks, and Rust, Python, Node.js and JVM (`jena-ontapi`) surfaces. | implemented |
| [F22](F22-analysis-lifecycle.md) | Reproducible analysis runs, read dependencies, alternative evidence supports, freshness tracking, guarded publication and maintained assessments. | specified |
| [F23](F23-reasoner-core/README.md) | Specialized closure operators, complete OWL 2 RL rules, cost-based rule planning, a columnar derived tier, compact provenance, an n-ary Datalog core and shared maintenance. | implemented in part |
| [F24](F24-owl-classification/README.md) | OWL 2 EL classifier and external DL reasoner bridge over the F21 axiom model with inferred-axiom export, justifications and repair suggestions. | implemented in part (OWL 2 EL classifier) |
| [F25](F25-git-sync-and-remotes/README.md) | Canonical git serialization, branch export and import from git refs, and fetch/push/pull remotes between Sparkles servers. | specified |
| [F26](F26-temporal-paths/README.md) | Time-respecting paths and temporal projections over valid time, custody-chain checks and time-slice algorithm results with change-point detection. | specified |
| [F27](F27-impact-and-identity-primitives/README.md) | Generic entity-resolution primitives, tier and concentration metrics, decaying weighted reachability, recommendation explanations and survivorship rules. | specified |
| [F28](F28-ml-and-probabilistic-integration/README.md) | Integration with ML pipelines, model registries and probabilistic engines via Croissant, Parquet, MLflow, Hugging Face Hub, ONNX, KServe, Triton, Feast, OpenLineage, PSL and ProbLog. | specified |
| [G01](G01-geosparql/README.md) | OGC GeoSPARQL 1.1 functions, Jena spatial extensions, a spatial index, spatial joins and map views in the web UI. | implemented in part (Phases 1–2, part of Phase 3) |
| [G02](G02-shex/README.md) | ShEx validation in ShExC, ShExJ and ShExR, shape maps, `POST /{ds}/shex`, write-time ShEx guards and dataset ShExR graphs. | implemented in part (Phases 1–2, most of Phase 3) |
| [G03](G03-shaclc.md) | SHACL Compact Syntax (`text/shaclc`) read and write support across shape endpoints and SHACL 1.2 list constraints. | implemented in part (Phase 1 and follow-ups) |
| [G05](G05-command-line-tools.md) | Jena-compatible CLI utilities (`convert`, `qparse`, `uparse`, `compare`, `iri`, `langtag`, `rsparql`, `rupdate`, `rset`, `rdfpatch`) and IRI/language-tag validation. | implemented |
| [G06](G06-arq-query-extensions.md) | Jena ARQ extensions (`LATERAL`, property path ranges `{n,m}`, `GRAPH` in CONSTRUCT templates) and configurable DESCRIBE strategies. | implemented (Phases 1–3) |
| [G08](G08-fuseki-configuration.md) | `sparkles config import fuseki` and `serve --fuseki-config` to translate Fuseki assembler files and Shiro auth into Sparkles scripts and settings. | implemented |
| [G10](G10-service-enhancer.md) | Jena SERVICE enhancer (`loop`, `bulk`, `cache`), correlated SERVICE via `LATERAL`, per-caller result caching and `urn:x-arq:self`. | implemented |
| [P01](P01-python-bindings.md) | Embedded `sparkles` Python package with sync and async queries, bounded cursors and transactions, built with PyO3 and maturin as abi3 wheels. | implemented in part (Phase 1, most of Phase 2) |
| [P02](P02-rust-client.md) | Async and blocking `sparkles-client` crate with streaming `oxrdf` results, receipts, Graph Store Protocol, `Retry-After` retries and OpenAPI conformance tests. | implemented |
| [P03](P03-query-extensions.md) | Per-query registered scalar functions, property functions and aggregates with budgets, access-filtered reads and batched callbacks. | implemented in part (Rust registries) |
| [P04](P04-jvm-bindings/README.md) | `sparkles-jena` Kotlin library over UniFFI embedding Sparkles behind Jena's `DatasetGraph` and query/update engines, with ARQ fallback, Fuseki assembler and TDB2 import. | implemented in part (core and administration) |
| [P05](P05-node-bindings.md) | Node.js and TypeScript napi-rs embedded addon with async batched iteration, `AbortSignal` transactions, RDF/JS terms, a Comunica source and a typed remote client. | implemented in part (Phases 1–2) |
| [P06](P06-library-admin-api.md) | Shared Rust embedding administration API (`Dataset`, `Catalog`, `Control`) backing server routes and language bindings with OpenAPI parity tests. | implemented (Phases 1–3) |
| [X01](X01-compression-codecs.md) | Zstd and Brotli alongside gzip and LZ4 for inputs, HTTP responses, dumps, backups and UI assets. | implemented in part (Phase 1) |
| [X02](X02-formatter/README.md) | Opinionated SPARQL and RDF formatter in `sparkles fmt`, `POST /$/format`, `sparkles lsp` and the web UI. | implemented |
| [X03](X03-openapi-and-completions.md) | Route-checked OpenAPI 3.1 schema at `/$/openapi.json`, `sparkles openapi`, shell completions and man pages. | implemented (Phases 1–2) |
| [X04](X04-linter.md) | SPARQL, Turtle and TriG linter (`sparkles lint`, `POST /$/lint`, `sparkles lsp`, UI editor) with configurable severities and safe quick fixes. | implemented |
| [X05](X05-streaming-query-execution.md) | Snapshot-owning streaming cursors, resumable joins, budget-charged operator state, HTTP/binding backpressure and automatic execution mode selection. | implemented in part |
| [X06](X06-benchmark-coverage.md) | Reproducible benchmark suites (BSBM, SP²Bench, LUBM, OWL2Bench, LDBC SNB/Graphalytics, Wikidata 1.24B, spatial, vector, writes) with immutable JSON records and regression checks. | implemented in part (Phase 1 published) |
| [X07](X07-cold-sparse-reads.md) | Per-query I/O accounting, batched constant resolution, plan-time prefetch, per-scan read policies, segmented index blocks and optional prewarm for cold point lookups. | implemented in part (Phases 1–4) |
| [X08](X08-disk-spill-and-server-memory.md) | Server-wide shared caches, a `small` memory profile, byte-admitted query memory pools, batch join/grouping kernels and crash-safe disk spill with optional encryption. | implemented in part (Phases 1–2) |
| [X09](X09-group-commit-by-default.md) | Group commit enabled by default for persistent stores (`--no-group-commit` opt-out) across all write paths, with latency, durability and compaction gates. | implemented in part |
| [X10](X10-reusable-query-plans.md) | Parsed-query and parameterized plan caches with cardinality-class re-planning, compaction invalidation and prepared queries across language bindings. | specified |
| [X11](X11-index-compression-and-predicate-patterns.md) | QLever-style predicate patterns (`spk:hasPredicate`), FSST vocabulary compression, inline numeric IRI suffixes and versioned compressed index blocks. | implemented in part (Phase 1, optional Phase 2a) |
| [X12](X12-release-engineering.md) | Unified release workflow, multi-arch binaries and container images with provenance attestations, cross-platform wheels, format compatibility policy and `sparkles migrate`. | implemented in part (Phase 3) |
| [X13](X13-per-crate-nix-builds.md) | Evaluation of per-crate Nix builds with crate2nix versus Crane under matched conditions before any packaging migration. | specified (deferred investigation) |
| [X14](X14-unified-executor/README.md) | Single morsel-driven pull executor where eager execution is a consumption policy, with unified memory accounting, shared operator kernels and one execution-policy entry point. | implemented in part |
| [X15](X15-join-algorithms-and-estimation/README.md) | Correlation-aware cardinality estimation, worst-case-optimal joins, scalable join enumeration, adaptive re-optimization, OPTIONAL reordering, reachability indexes and factorized results. | implemented in part (Phase 1 estimation) |
| [X16](X16-storage-consolidation/README.md) | First-release format cutoff, unified budgeted `Control` readers, shared commit publish tail, single load plan and partitioned vocabularies. | implemented in part (Phases 1–2) |
| [X17](X17-tiered-segments/README.md) | Immutable generations offloaded to S3-compatible storage after compaction, cached locally as memory-mapped segments with lazy dataset open and idle unload. | specified |
| [X18](X18-fragment-composition/README.md) | Immutable fragment generations indexing each source version once, composed into datasets by k-way merging fragment vocabularies and sorted permutations. | specified |
| [X19](X19-charged-buffers/README.md) | Cursor operator buffers whose memory charge tracks allocation directly, paired with a heap audit covering every cursor operator. | implemented in part |
| [X20](X20-formal-verification/README.md) | Protocol models, bounded code checks and mechanized proofs for storage, concurrency, admission, access control, reasoning and query rewrites. | specified |
| [U01](U01-time-slider.md) | Time slider across query results, Explore graphs, entities and schemas with commits, snapshots, branches, compare mode, time-lapse and graph diffs. | implemented (Phases 1–4) |
| [U02](U02-shape-driven-forms/README.md) | Editing forms generated from SHACL shapes and DASH hints with client/server validation, C15 previews, branch saves and field-level conflict checks. | implemented in part |
| [U03](U03-query-by-example.md) | Visual query construction from selected Explore nodes and edges, parameter conversion, stored-query search phrases and future Cypher output. | specified |
| [U04](U04-live-query-profiling.md) | Live per-operator row counts and states via `GET /$/running/{id}` (JSON/SSE), hot-spot plan trees, stop-and-explain, verified rewrites and pattern-graph overlays. | implemented in part (Phase 4) |
| [U05](U05-notebooks.md) | RDF-backed notebooks of Markdown, SPARQL, parameter, Ask, chart and map cells with history, branches, commit pinning, MCP tools and Jupyter/Markdown export. | specified |
| [U06](U06-large-graph-rendering.md) | WebGL graph renderer using sigma.js and ForceAtlas2 for up to 100,000 nodes, class-level overviews, server-side community grouping and semantic zoom. | implemented in part (Phases 1–2) |
| [U07](U07-data-quality-and-schema-diagram.md) | Per-commit SHACL/ShEx quality timeline tracking introducing commits, plus a schema diagram with observed/declared cardinalities, Mermaid export and graph badges. | implemented in part (Phase 4) |
| [U08](U08-visual-query-builder.md) | Two-way visual query canvas synced with the SPARQL editor via text splices, schema-backed predicates and locked blocks for unsupported syntax. | implemented in part (Phases 1–2) |
| [U09](U09-path-finder.md) | Shortest, all-shortest and k-shortest path finder animated in Explore and query graphs, with reifier weights, excluded predicates and hub degree limits. | implemented |
| [U10](U10-canvas-lenses.md) | Graph canvas lenses for node size, colour, badges, dashed inferred edges, cached F14 algorithm metrics and inferred-edge derivation trees. | implemented (Phases 1–3) |
| [U11](U11-linked-views.md) | Selection-linked map arcs, server-computed facet histograms and vector-index embedding maps whose brushes and lassos filter the query and graph. | implemented in part (Phases 1–3, partial) |
| [U12](U12-ontology-workbench/README.md) | Ontology and thesaurus editors, a DL Query tab, a visual SHACL shape editor and an editable schema diagram. | implemented in part |
| [U13](U13-desktop-application/README.md) | Standalone desktop application for local datasets and remote servers, with framework selection gated on performance and web UI parity. | specified |

### Interop and query surfaces

Roadmap item 465 groups remaining interoperability and query-surface work. It has no
separate spec because each item extends the spec that owns its surface, recorded under
Roadmap follow-ups in each spec's Outcome.

| Surface | Spec |
|---|---|
| GraphQL aggregates, edge annotations, mutations, response assembly and keyset cursors | [C03](C03-graphql/OUTCOME.md#interop-and-query-surfaces-workstream) |
| Interactive multi-request transactions and blank-node-aware graph sync | [C15](C15-write-previews.md#interop-and-query-surfaces-workstream) |
| A SQL surface over shape-derived tables | [C24](C24-json-query-languages.md#interop-and-query-surfaces-workstream) |
| GQL, SQL/PGQ and the Bolt protocol | [F01](F01-cypher.md#interop-and-query-surfaces-workstream) |
| Arrow and Parquet results, DuckDB, CSV in virtual graphs, ontology rewriting and reconciliation | [F19](F19-mappings-and-virtual-graphs/OUTCOME.md#interop-and-query-surfaces-workstream) |
| Geometry validity and GIS file import | [G01](G01-geosparql/OUTCOME.md#interop-and-query-surfaces-workstream) |
| The remaining ShEx conformance cases | [G02](G02-shex/OUTCOME.md#interop-and-query-surfaces-workstream) |
| SHACL 1.2 Core and its Working Group suite | [G03](G03-shaclc.md#interop-and-query-surfaces-workstream) |
| Archive inputs, HDT and `--check` severities | [G05](G05-command-line-tools.md#interop-and-query-surfaces-workstream) |
| Fuseki status codes and Linked Data Fragments endpoints | [G08](G08-fuseki-configuration.md#interop-and-query-surfaces-workstream) |
| `SERVICE` over another local dataset | [G10](G10-service-enhancer.md#interop-and-query-surfaces-workstream) |
| RDF/XML formatting | [X02](X02-formatter/OUTCOME.md#interop-and-query-surfaces-workstream) |
| The SHACL query prefix contract | [X04](X04-linter.md#interop-and-query-surfaces-workstream) |

Two surfaces stay out of scope:

* **LDP containers and the Solid Protocol.** Sparkles does not serve LDP containers, the
  Solid Protocol, WebID or WAC and ACP documents. The Graph Store protocol and the access
  model of C09 and C12 cover the same data without a second resource model.
* **Gremlin server and traversal API.** Sparkles does not serve Gremlin traversals.
  openCypher (item 20) and GQL after it are the property-graph query languages it adopts.

### How the graph-interaction features are grouped

Ten graph-interaction features are organized across existing and dedicated UI specs. Three
extend specs that already own their data: the plan drawn on the query's pattern is U04
Phase 4 (plan views and redactions), the validation overlay is U07 Phase 4 (validation
results and access rules), and the time-lapse and graph diff are U01 Phase 4 (slider,
compare mode and marks).

The other seven form four dedicated specs. The visual query builder is U08 rather than a
phase of U03, because U03 starts from selected Explore data without a round trip from
query text, whereas U08 edits query text directly. The path finder stands alone as U09
with its own server phase. Algorithm lenses and inferred-edge explanations share one lens
model in U10, which U07 Phase 4 also uses for validation badges. Selection-linked views
beside the canvas—map arcs, facet histograms and the embedding map—are grouped in U11.

The [provenance index](PROVENANCE.md) locates the records for these specs, the allocator,
the vendored spargebra and development tools.
