# Feature specs

These are the design specs for Sparkles capabilities and application data profiles,
including features beyond its port of Jena, Fuseki and QLever's engine. Each spec was
written before the feature was built. The sources were public standards, published
papers, the Sparkles code, and the documentation
and permissively licensed code of other projects. The [provenance index](PROVENANCE.md)
locates each spec's source and dependency records.

Each spec has a landing page that gives its status, the phases shipped and links to the
user docs. Its design closes with an **Outcome** overview of what was delivered,
important departures and the work that remains. A substantial delivery record has its
own linked document with implementation decisions, test and conformance results,
measurements, and deferred or rejected work. The design remains as it was written.
Where the design and the code disagree, the code,
[the API reference](../API.md) and [the feature list](../FEATURES.md) describe the current
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
phase summary and links to the design, relevant user guides and remaining roadmap
items. Keep it brief and useful rather than repeating the design.

`SPEC.md` holds the design, rationale, contracts and acceptance criteria. Its final
Outcome section gives a concise delivery overview, the important departures from the
design and links to remaining work. A small spec can keep its complete outcome there.

Add `OUTCOME.md` when the implementation record benefits from its own sections, such as
several delivered phases, substantial design decisions or detailed validation and
measurements. Organize that record by phase or topic. Keep the Outcome overview in
`SPEC.md` and link it to the detailed record. The detailed record links back to the
overview. Diagrams, fixtures and further supporting files can live beside these files
when needed.

Keep short source and adoption records in a Provenance section in `SPEC.md`. When a
record needs substantial detail, keep a summary and link there and put the complete
record in the bundle's `PROVENANCE.md`. Link back to the spec from that document.
Update the [central provenance index](PROVENANCE.md) when adding or moving a record.
[Shared records](shared/PROVENANCE.md) cover dependencies and sources with several
spec owners. Preserve source, licensing and adoption history separately from current
delivery status, including earlier draft records that differ from the final decisions.

Existing single-file specs retain their current paths until migrated. Their opening
status block serves as their landing page, and their Outcome section keeps the delivery
record. A migration preserves the spec ID and original design, separates the current
summary from the detailed record, and updates repository links and heading anchors in
the same change. A Markdown stub does not automatically redirect an old heading anchor.

When adding user-facing feature folders, put capabilities, configuration, usage and
feature roadmap pages there and link to the relevant specs here. A spec may support
several feature pages.

## How to use them

* **As the design rationale.** A spec explains why a feature works the way it does. It
  covers the goals and non-goals, the alternatives that were rejected, the compatibility
  choices (usually with Jena or Fuseki), the failure modes, and the acceptance examples
  the tests were written from. Read the spec before you change the feature.
* **Kept with the code.** When a change alters a feature's behaviour, update its spec in
  the same change. Update its current status, Outcome overview and detailed delivery
  record when present. Leave the original design as it was, so the record of how the
  feature came about stays intact. A new feature of similar size gets a new spec bundle
  here, with a new ID and its own provenance section linked from the provenance index.
* **Cross-references.** Specs refer to each other by ID and section, such as
  [C10 §6.2](C10-write-time-validation.md). A `§` number refers to a section of the spec
  it names. Inside a spec, the labels `A1`, `A2` and so on number its acceptance examples.

## IDs

| Prefix | Group |
|---|---|
| `A` | Application data profiles, source mappings and acceptance contracts that specialize shared evidence and temporal resources without adding native engine semantics. |
| `CI` | Durable commit identity. The commit sequence, dataset ids and receipts that most later features build on. |
| `C` | Smaller server and engine capabilities: observability and budgets, schema discovery, a GraphQL adapter, CSV and TSV imports, cloning, inference freshness, access control, write-time validation, the MCP server, automatic compaction, write previews, stored queries, agent memory over MCP, questions and ingestion in natural language, layered settings, declared prefixes, outbound notifications, GraphRAG community summaries, batched updates with commit triggers, JSON query languages, shared evidence, durable jobs, valid-time profiles and personal ChatGPT-plan model access. |
| `F` | Larger data features: a Cypher frontend, full-text and vector search, path search, embeddings computed on write, local embedding models, backups to object storage, retained history and point-in-time reads, branches and merges, replication, encryption at rest, incremental views with provenance, graph algorithms, reasoning at scale and at query time, explanations, mapping languages with virtual graphs, a local-first engine, an ontology API, and maintained analysis lifecycles. |
| `G` | Gaps against Apache Jena that were out of scope for the first version: GeoSPARQL, ShEx, the SHACL Compact Syntax, the command-line tools, the converter of Fuseki configurations and the SERVICE enhancer. |
| `P` | Bindings and clients for other programs. These are the Python package that embeds the engine, the Rust client of remote servers, the JVM library that embeds the engine behind Apache Jena's API, the Node.js packages for the embedded engine and a remote client, and the administration API that all of them share. |
| `X` | Internal engineering that does not derive from any other database product: compression codecs, the formatter, the linter, the OpenAPI description with shell completions, streaming query execution, benchmark coverage, cold reads, disk spill and server memory, group commit, reusable plans, index compression, and release engineering. |
| `U` | Web UI features that span several server capabilities: the time slider, forms generated from shapes, query by example, live query profiling, notebooks, large-graph rendering, the data-quality timeline with a schema diagram, a two-way visual query builder, a path finder, lenses on the graph canvas, and linked views beside the graph. |

Numbers are stable. Gaps in the numbering are roadmap items without a spec: writes and a
property-graph representation for the Cypher frontend. G01 replaced the narrower
geospatial item on that list, and F17 and C24 replaced the Datalog frontend. F15 is
unused. [ROADMAP.md](../ROADMAP.md) orders the specified and unbuilt work by priority.

## Specs

A spec's status describes delivery within its named scope. *Implemented* means the
listed phase contract shipped. *Implemented in part* means usable functionality shipped
with the listed phases or items still missing. *Specified* means the design is written
and unimplemented. These correspond to Complete, Partially complete and Planned in the
[capability overview](../FEATURES.md#capability-status). In progress is reserved for
active unshipped implementation, rather than a design or an unfinished validation run.
A completed phase does not imply every later extension or compatibility goal is built.

| Spec | Summary | Status |
|---|---|---|
| [A01](A01-shared-entity-representation/README.md) | Versioned entity profiles, shapes and fixed cross-domain fixtures over evidence and temporal records. Offline replay, source mappings and managed integration remain. | implemented in part (Phase 1 resources) |
| [CI](CI-commit-identity.md) | Durable dataset ids, a gap-free commit sequence, commit receipts and headers, `/$/commits` and `sparkles log`, commit messages, change digests, entity tags, the catalog horizon and the change feed | implemented (Phases 1–3) |
| [C01](C01-observability-and-budgets.md) | Request ids, access log, Prometheus metrics, readiness, OpenTelemetry, memory and result-size budgets, cancellation on disconnect | implemented in part |
| [C02](C02-schema-discovery.md) | `GET /$/schema/{ds}` with paginated class and predicate listings computed on the server, a VoID export, a SHACL constraints layer, subject classes per predicate, class profiles, schema diffs between commits, reports kept up to date from the changes, shapes drafted from the data, `sparkles schema`, and the UI's schema browser | implemented |
| [C03](C03-graphql/README.md) | A read-only GraphQL endpoint per dataset, with a schema mapped to classes and predicates by `@rdf` directives and drafted from SHACL shapes or the data, compiled to batched SPARQL algebra under the caller's view and budgets | implemented in part (Phase 1) |
| [C05](C05-tabular-imports.md) | CSV and TSV imports with a default mapping, W3C CSVW metadata or Tarql-style CONSTRUCT templates, in `sparkles load`, `sparkles csv` and uploads | implemented |
| [C06](C06-clone-to-sandbox.md) | Cloning a consistent snapshot of a dataset into a new, independent dataset | implemented in part (Phases 1 and 2, part of Phase 3) |
| [C08](C08-inference-freshness.md) | Whether materialized inferences are current, re-running them incrementally, inconsistency diagnostics, input graphs with `owl:imports`, and RDFS on read | implemented (Phases 1–4) |
| [C09](C09-dataset-access-control.md) | Authentication (Basic, API tokens, OIDC with access tokens and back-channel logout, trusted proxies, Cloudflare Access, CLI logins, native TLS) and per-dataset permissions | implemented (Phases 1 and 2, Phase 3 as C12) |
| [C10](C10-write-time-validation.md) | A per-dataset SHACL or ShEx guard that validates the state after each write before the write commits, with editable UI configuration and rejected-write reports | implemented (Phases 1–3) |
| [C11](C11-mcp-server.md) | `sparkles mcp` and `/$/mcp`, a Model Context Protocol server over stdio and HTTP with query, schema, search, validation and formatting tools, an opt-in update tool, resources, prompts and stored queries as tools | implemented (Phases 1–3) |
| [C12](C12-graph-access-control.md) | Grants limited to some named graphs or endpoints of a dataset, enforced by a filtered dataset view in the engine | implemented |
| [C12b](C12b-triple-access-control.md) | C12 Phase 2: protections that hide triples by predicate, subject class or a SPARQL pattern with the caller bound, lifted by grants and enforced by a masked snapshot in the engine | implemented |
| [C13](C13-automatic-compaction.md) | Background compaction triggered by the delta's size, the log's size, age and idle time, with writes continuing during the build | implemented |
| [C15](C15-write-previews.md) | Dry runs of updates, Graph Store writes, uploads, RDF Patches and the MCP write tool, which report the commit, the changes, validation, quota and preconditions and roll back | implemented |
| [C16](C16-stored-queries.md) | Named queries per dataset with typed parameters bound as terms, versions, runs by name, MCP tools and the UI's saved queries | implemented |
| [C17](C17-agent-memory.md) | MCP tools for agents that keep knowledge in a dataset, with no model in the server: query checks against the schema with suggestions, stored queries as examples, entity linking, recall of facts with citations, and writes with RDF 1.2 provenance, duplicate checks, validation, previews and supersession, plus branches as scratchpads | implemented in part (Phases 1a, 1b and 1c) |
| [C18](C18-natural-language-questions-and-ingest/README.md) | Questions in natural language turned into checked, read-only SPARQL, through an MCP agent's handoff link or the UI's Ask bar backed by Ollama, OpenAI-compatible or Anthropic providers in the server, with editable queries, table and graph views and summaries that cite rows; documents turned into facts with cited spans reviewed on a branch; agent facts usable at once and marked unreviewed; a memory browser and a review inbox; ordered provider and model pairs per use with escalation on verified failures; plain-language explanations of any query's plan; suggested rewrites verified to return the same results faster; and the import of Claude Code, Codex and other coding agents' memory files and transcripts, with the `sparkles memory` commands and a cited brief for each session | implemented in part (Phases 1, 2, 2b, 3, 3m-a, 3m-b, 4 and 5) |
| [C19](C19-layered-settings.md) | Dataset settings computed from built-in defaults, an operator's declared settings file and runtime changes, with fields the operator can lock, one set of `/$/settings` routes, `sparkles settings`, a settings tab in the UI, declared datasets that cannot be deleted at runtime, and server-wide model providers, role lists, provider presets, per-provider TLS options and write-only API keys that server administrators can change | implemented |
| [C20](C20-declared-prefixes.md) | Prefixes declared in the settings file for every dataset or one dataset, changed and removed at runtime through the settings routes, `sparkles settings` and the UI, with locks, warnings for prefixes that shadow well-known ones, Fuseki's prefixes service kept on the runtime layer, and completion of prefixes in the query editor and `sparkles lsp` | implemented |
| [C21](C21-outbound-notifications.md) | Notifications pushed to webhooks with Standard Webhooks signatures and to ntfy topics, configured as a server-wide settings kind with locks and secret references, with retries, repeats of lasting conditions, a test send and delivery status, for memory waiting for review, failed backup policies and spent model budgets | implemented in part (Phase 1) |
| [C22](C22-graph-rag-community-summaries.md) | Hierarchical communities from F14's Leiden with reports written by a `community` model role, stored as triples that cite their facts with provenance and review status, refreshed after commits within a token budget, and used by Ask for global questions and by `recall` and the memory brief | implemented in part (Phase 1 and parts of Phases 2 and 3) |
| [C22b](C22b-report-configurations.md) | Named report configurations for different principals, projections and languages, with explicit selection and shared budget enforcement | specified |
| [C23](C23-batched-updates-and-triggers.md) | Batched updates that feed a SELECT's solutions to an update and commit every N, as tasks with progress, cancellation and resumption, a repeating form, and commit triggers that match a commit's changes and run a SPARQL Update or a webhook, with ordering, recursion limits, validation and a run log | specified |
| [C24](C24-json-query-languages.md) | A JSON query language of JSON-LD node patterns with variables, compiled to the SPARQL algebra, with nested selections that return JSON-LD trees; JSON-LD 1.1 frames as queries; and Datalog queries and relations of any arity as an extension of Jena's rule syntax, evaluated through F17's demand transformation | specified |
| [C25](C25-evidence-and-assessments.md) | Shared sources, unasserted claims, evidence references, review decisions, reversible identity matches and managed accepted projections | partially implemented (Phase 1 resources and local Phase 2 preparation) |
| [C26](C26-durable-jobs-and-delivery.md) | Durable jobs, checkpoints, fenced workers and atomic output and outbox publication with restart recovery | specified |
| [C27](C27-valid-time-profiles.md) | Validity intervals, uncertain temporal bounds, observation availability, temporal query helpers and an event-time control | implemented in part; Phase 1 A4 remains open |
| [C28](C28-chatgpt-model-access.md) | Personal ChatGPT-plan model connections, local sign-in, protected credentials, Responses streaming and explicitly delegated background access | specified |
| [C29](C29-bounded-view-preparation/README.md) | Request-scoped memory, cancellation and deadlines for historical replay, permission preparation and bounded caller adoption | implemented in part. Phase 1 bounded history is available. Controlled permissions and guarded adoption remain. |
| [F01](F01-cypher.md) | A read-only openCypher subset compiled to the SPARQL algebra over a property-graph view of RDF, with relationship identity and properties through RDF 1.2 reifiers, `/{ds}/cypher` and `sparkles cypher` | specified |
| [F03](F03-full-text-search/README.md) | BM25 full-text search over literals with Tantivy, through Jena's `text:query`, with Lucene's query syntax, highlighting and stemming per language | implemented in part (Phases 1 and 2, most of Phase 3) |
| [F04](F04-vector-search.md) | `spk:vector` literals, similarity functions, `spk:vectorSearch`, HNSW vector indexes per predicate, and hybrid ranking with full-text search | implemented in part (Phases 1, 1b and 2, most of Phase 3) |
| [F05](F05-snapshot-repositories/README.md) | Incremental, deduplicated backups to a file system or S3, restore, verification, policies and GC | implemented in part (Phase 1, most of Phase 2) |
| [F06](F06-snapshots-and-point-in-time.md) | Point-in-time reads with `?at=`, named snapshots, a retention window, diffs between commits as JSON or RDF Patch, a change feed, a change log and history queries | implemented in part (Phases 1–3; full-text search at pins and pin rebasing deferred) |
| [F07](F07-path-search.md) | Paths as solutions through `SERVICE path:search`: one, all or the k shortest paths, or every path up to a length, over chosen predicates and directions, with ends bound by the query and weights on reifiers | implemented in part (Phases 1 and 2) |
| [F08](F08-embeddings-on-write.md) | Vectors computed from selected literals by an OpenAI-compatible embeddings endpoint after each commit, with catch-up, re-embedding, status, metrics, chunking, rate limits and text queries | implemented (Phases 1 and 2) |
| [F09](F09-branches-and-merges.md) | Persistent and memory branches, shared immutable indexes, three-way and replayed merges, recursive virtual bases, explicit relinking from Rust and the offline CLI, independent branch backups and the commit graph | implemented in part (Phases 1–2 and parts of Phase 3) |
| [F10](F10-replication.md) | Applying RDF Patch through Fuseki's `patch` operation, with `H prev` as a concurrency check, then read replicas that pull commits as RDF Patch, keep the primary's commit ids, bootstrap from a generation copy or a backup, and are promoted by hand | implemented in part (Phase 1; Phases 2 and 3 deferred) |
| [F11](F11-encryption-at-rest.md) | Client-side encrypted backup repositories with keyed blob ids, content-defined chunking, and AES-256-GCM encryption at rest for dataset files under KMS-wrapped per-dataset keys | implemented in part (part of Phase 1) |
| [F12](F12-local-embeddings.md) | Local embedding models in the server with Candle behind the `embed-local` feature, a verified store of pinned model snapshots with `sparkles models`, the `local` provider kind, vector indexes that name a provider, and a Nix helper for read-only stores | implemented |
| [F13](F13-incremental-views-and-provenance.md) | One incremental core over Z-sets fed by each commit's changes, with materialized SPARQL views read through `SERVICE view:read` or kept in named graphs, live queries that stream result diffs over server-sent events, WebSocket, MCP and the bindings, per-fact provenance as semiring annotations, a fallback to re-evaluation, checkpoints, access per caller and views per branch | implemented in part (Phase 1) |
| [F14](F14-graph-algorithms.md) | Graph algorithms through `SERVICE algo:…` on projections defined by predicates, classes, weights on reifiers or nested patterns: PageRank, betweenness, components, label propagation, Louvain and Leiden, node similarity, triangles, k-core, BFS and shortest distances, and FastRP and node2vec embeddings for vector indexes, streamed as rows or written back to a named graph by tasks, with budgets, a thread pool and LDBC Graphalytics as acceptance | implemented in part (Phases 1 and 2, and embeddings from Phase 3) |
| [F16](F16-materialization-at-scale.md) | Reasoning over whole large graphs: rule evaluation over the store's index permutations instead of an in-memory copy, a parallel merge on top of the parallel semi-naive evaluation, a derived-fact layer kept apart from the base indexes, `owl:sameAs` through canonical representatives with expansion at query time, incremental maintenance with fewer full-run fallbacks, the `dt-not-type` check, and LUBM and OWL2Bench measurements | implemented in part (Phase 1 with scale acceptance and later follow-ups) |
| [F17](F17-query-time-reasoning.md) | Reasoning at query time: derivation on demand through the magic-sets transformation of the rule program, a cost-based choice per pattern between fresh materialized facts and derived ones, tables cached per snapshot and later kept current by F13, Jena backward and hybrid rules, OWL 2 QL query rewriting, budgets, and Jena's rule language as the Datalog syntax for rules over triples | implemented in part (Phase 1) |
| [F18](F18-explanations-and-entailment-diffs.md) | Explanations of reasoning: derivation trees for inferred triples, bounded "why not?" with verified hypotheses, entailment diffs between commits and branches in merge and write previews, minimal justifications for inconsistencies and unsatisfiable classes, and protections and `reviewedOnly` that keep the inferred triples a caller's visible facts support, through HTTP, the CLI, MCP and the UI | implemented in part (Phases 1 and 2) |
| [F19](F19-mappings-and-virtual-graphs/README.md) | R2RML, RML and YARRRML mappings over CSV, JSON, Parquet and SQL databases loaded into a dataset, CSVW's standard mode with key checks, Tarql's functions, templates that read the dataset, stored mappings and CSV in the Graph Store protocol, then virtual graphs that answer SPARQL over SQL, Parquet and Iceberg by unfolding queries into SQL | implemented in part (most of Phase 1) |
| [F20](F20-local-first-engine.md) | The engine compiled to WebAssembly over a storage layer without mapped files, replicas of a dataset or some of its graphs in the browser's Origin Private File System, offline commits on a local branch synced through RDF Patch with `H prev` and F09 merges with conflict reports, the UI's local mode, and a variant for Node.js, Deno and edge runtimes | specified |
| [F21](F21-ontology-api.md) | An OWL 2 ontology API over the triples of a dataset: entities, class and property expressions, axioms and annotations read and written through the OWL 2 mapping to RDF graphs, OWL 2 EL, QL and RL profile checks, Rust, Python, Node.js and JVM (through Jena's `jena-ontapi`) surfaces, and its relation to materialized and query-time reasoning | specified |
| [F22](F22-analysis-lifecycle.md) | Reproducible analysis runs, read dependencies, alternative evidence supports, freshness, guarded publication and maintained assessments | specified |
| [G01](G01-geosparql/README.md) | GeoSPARQL 1.1 functions, Jena's spatial extensions, a spatial index, spatial joins and the UI's maps | implemented in part (Phases 1–2, part of Phase 3) |
| [G02](G02-shex/README.md) | ShEx validation: ShExC, ShExJ and ShExR, shape maps, `POST /{ds}/shex` and write-time ShEx, with schemas in ShExR graphs of the dataset | implemented in part (Phases 1–2 and most of Phase 3, including the supported ShEx 2.2 subset) |
| [G03](G03-shaclc.md) | The SHACL Compact Syntax (`text/shaclc`) read and written wherever shapes go in or come out, and the SHACL 1.2 list constraints | implemented in part (Phase 1 and two of the later items) |
| [G05](G05-command-line-tools.md) | `convert` (`riot`), `qparse`, `uparse`, `compare` (`rdfdiff`), `iri`, `langtag`, `rsparql`, `rupdate`, `rset` and `rdfpatch`, and IRI and language-tag warnings in `convert --check`, `load --check` and `/$/validate/iri` | implemented |
| [G06](G06-arq-query-extensions.md) | Jena ARQ's syntax extensions: `LATERAL`, property path ranges `{n,m}` and CONSTRUCT templates with `GRAPH`, and DESCRIBE as a concise bounded description, its symmetric form or the resource's own triples, chosen per dataset or per request | implemented (Phases 1–3) |
| [G08](G08-fuseki-configuration.md) | `sparkles config import fuseki`, which turns a Fuseki configuration and its `shiro.ini` or password file into a `serve` script, a data migration script, an auth configuration and dataset settings with a report of each element, and `serve --fuseki-config` | implemented |
| [G10](G10-service-enhancer.md) | Jena's SERVICE enhancer: `loop`, `bulk` and `cache` options in front of a SERVICE IRI, correlated SERVICE on the `LATERAL` operator, bulk requests as VALUES blocks or Jena's UNION, a per-dataset cache of remote results kept apart per caller, and `urn:x-arq:self` | implemented |
| [P01](P01-python-bindings.md) | The `sparkles` Python package with async queries, bounded cursors and transactions beside the synchronous API, built with PyO3 and maturin as abi3 wheels | implemented in part (Phase 1, most of Phase 2) |
| [P02](P02-rust-client.md) | `sparkles-client`, an async and blocking Rust client for Sparkles and any SPARQL endpoint, with streaming results as `oxrdf` terms, receipts, the Graph Store Protocol, retries that honour `Retry-After`, and a test that checks it against the OpenAPI description | implemented |
| [P03](P03-query-extensions.md) | Per-query registered scalar functions, property functions and aggregates, with budgets, access-filtered reads and batched binding callbacks | implemented in part (Rust scalar, aggregate and property registries) |
| [P04](P04-jvm-bindings/README.md) | `sparkles-jena`, a Kotlin library over UniFFI bindings that embeds the engine behind Jena's `DatasetGraph`, transactions and query and update engines, with a per-query fallback to ARQ, an assembler type for Fuseki, an in-process TDB2 import and native libraries in the jar | implemented in part (integration and primary administration) |
| [P05](P05-node-bindings.md) | Node.js and TypeScript packages: the engine as a napi-rs addon with prebuilt binaries, async iteration of results in batches off the JavaScript thread, transactions on the single writer with `AbortSignal`, RDF/JS terms and a Comunica source, a remote client typed from the OpenAPI description, and what a browser build would take | implemented in part (Phases 1 and 2) |
| [P06](P06-library-admin-api.md) | The embedding API's administration surface: `Dataset` handles for snapshots, history, backups, indexes, schema, stored queries, reasoning, validation, settings and GraphQL, a `Catalog` of datasets in one data directory with a directory lock, one `Control` for cancellation and progress, the server's handlers on that API, tests that map every OpenAPI operation to a library call and every library call to each binding, and the Python bindings' missing methods | implemented (Phases 1–3) |
| [X01](X01-compression-codecs.md) | zstd and brotli next to gzip and LZ4 for inputs, responses, dumps, backups and the UI's assets | implemented in part (Phase 1) |
| [X02](X02-formatter/README.md) | `sparkles fmt`, `POST /$/format`, `sparkles lsp` and the UI's Format button for SPARQL and RDF | implemented |
| [X03](X03-openapi-and-completions.md) | An OpenAPI 3.1 description at `/$/openapi.json`, kept equal to the route table by a test and checked in, `sparkles openapi`, shell completions and man pages | implemented (Phases 1 and 2) |
| [X04](X04-linter.md) | `sparkles lint` for SPARQL, Turtle and TriG, with severities per rule, safe fixes, diagnostics and quick fixes in `sparkles lsp`, `POST /$/lint` and the query editor | implemented |
| [X05](X05-streaming-query-execution.md) | Cursors over SELECT and graph results that own their snapshot, resumable joins, operator state charged to the query budget, backpressure from the writer and HTTP responses, bindings, and automatic selection of streaming where it was measured to help | implemented in part, with opt-in spill under X08 and automatic execution defaults |
| [X06](X06-benchmark-coverage.md) | BSBM, SP²Bench, LUBM, OWL2Bench, LDBC SNB Interactive and BI, LDBC Graphalytics, a Wikidata query-log replay above 1.24 billion triples, GeoSPARQL and vector comparisons with other engines and long write runs with compaction, with durability classes, pinned generators and engines, one JSON result record, a refresh protocol and regression comparison | partly shipped, with published Phase 1 records and later acceptance work open |
| [X07](X07-cold-sparse-reads.md) | Per-query I/O accounting, batched resolution of a query's constants, plan-time prefetch, a read policy chosen per scan instead of per process, segmented index blocks that let a point lookup read part of a block, and an optional prewarm, to bring cold point lookups within 10% of Oxigraph | implemented in part (Phases 1, 2 and 3) |
| [X08](X08-disk-spill-and-server-memory.md) | Block, result, SERVICE and history caches sized once per server instead of per dataset, a `small` memory profile, a server-wide query memory pool with admission on bytes, batch hash join and grouping kernels for cursors, disk spill for hash joins, sorts, grouping and DISTINCT with reserved disk, crash-safe temporary files and ephemeral encryption, and reductions of idle, Python and full-text footprint | implemented in part. Phases 1 and 2 shipped with spill opt-in. Execution/default follow-ups and Phase 3 remain. |
| [X09](X09-group-commit-by-default.md) | Group commit on by default for persistent stores with `--no-group-commit` to opt out, after serial-latency and reader-tail gates, power-loss and failed-sync tests, metrics, and admission of commit messages, preconditions, guards, Graph Store, RDF Patch, index hooks and writes during compaction | implemented in part |
| [X10](X10-reusable-query-plans.md) | Plans reused across executions: a cache of parsed queries by exact text, a cache of plans by query shape with constants, `VALUES` cells and bindings as parameter slots, re-planning when a constant's count changes class, invalidation on compaction and planning settings, and prepared queries in Rust, Python, the JVM and Node | specified |
| [X11](X11-index-compression-and-predicate-patterns.md) | QLever-style predicate patterns per subject with `spk:hasPredicate` and pattern counts in the planner, FSST-compressed vocabulary suffixes, IRIs with numeric suffixes encoded in the id, frame-of-reference and zstd index blocks with dictionaries, and versioned per-generation features | implemented in part (Phase 1 and optional Phase 2a) |
| [X12](X12-release-engineering.md) | One release workflow and version for the server, crates and bindings, server binaries and multi-arch images with checksums and provenance attestations, Windows, Intel macOS, musl JVM, PyPy and free-threaded wheels, on-disk format versions with a compatibility window, older-format fixtures and `sparkles migrate`, an API compatibility policy with checks, and install smoke tests | partially implemented (Phase 3) |
| [X13](X13-per-crate-nix-builds.md) | An investigation of per-crate Nix builds with crate2nix, measured against Crane under matched settings and checked for packaging, feature and platform parity before a migration decision | investigation planned |
| [U01](U01-time-slider.md) | A time slider over query results, the Explore graph, an entity's properties and memory, and the schema tab, built on the existing `at` field and history views: commits, snapshots and branches on one track, compare mode with added and removed rows, nodes and edges marked, play with animation, a bucketed commit timeline, a term filter on diffs and result diffs on the server, then a time-lapse from each commit's changes, a commit's diff drawn as a graph and the overview through time | implemented (Phases 1–4) |
| [U02](U02-shape-driven-forms/README.md) | Editing forms generated from a dataset's SHACL shapes and DASH editor hints, with a form model built on the server, checks in the browser and against the shapes on the server, C15 previews, saves with field-level conflict checks to `main` or an edit branch, and review through the merge page | implemented in part. Bounded affected-target checks are available. Nested forms, list and choice editors and remaining producer admission stay under item 21. |
| [U03](U03-query-by-example.md) | Queries built from nodes and edges selected in Explore's graph, with constants turned into variables, classes, parameters or filters, a match count, saving as a stored query with typed parameters, Bloom-style search phrases on stored queries matched without a model, and Cypher output once F01 exists | specified |
| [U04](U04-live-query-profiling.md) | Per-operator row counts and states while a query runs, through a progress sink in the executors, `GET /$/running/{id}` as JSON or server-sent events and cancellation by request id, drawn in the query page's plan tree with hot spots, **Stop and explain**, a running-queries card, C18's verified rewrites, and estimated and actual rows drawn on the query's pattern graph | implemented in part (Phase 4) |
| [U05](U05-notebooks.md) | Notebooks of Markdown, SPARQL, parameter, Ask, chart and map cells kept as RDF in a system named graph per notebook, so they have history, branches, merges and graph grants, pinned to a commit for reproducible runs, with `/$/notebooks` routes, MCP tools and a resource, and Jupyter and Markdown export | specified |
| [U06](U06-large-graph-rendering.md) | A WebGL graph view with sigma.js and a ForceAtlas2 layout in a web worker for results of up to 100,000 nodes, a class-level overview from class profiles, server-side grouping by F14 communities or property values with semantic zoom, and server layouts with a binary transfer format | implemented in part (Phases 1 and 2) |
| [U07](U07-data-quality-and-schema-diagram.md) | A record of the write-time guard's SHACL or ShEx results for every validated commit, drawn as a quality timeline with the commit that introduced each current result, backfill and a search for older introducing commits, and a schema diagram with observed and declared cardinalities, Mermaid export, and SHACL and ShEx results badged on the graph with suggested fixes | implemented in part (Phase 4) |
| [U08](U08-visual-query-builder.md) | A canvas beside the query editor that draws any query's pattern and edits basic graph patterns, filters, optional parts, simple paths and later `UNION`, `GRAPH` and `VALUES` as splices of the text, with locked blocks for the rest, predicates from the schema and a layout that survives edits | implemented in part (Phases 1 and 2) |
| [U09](U09-path-finder.md) | Shortest, all shortest and k shortest paths between two entities, drawn and animated in Explore and on the query page's graphs, weighted by reifier properties, with excluded predicates and a hub degree limit added to F07's path search | implemented |
| [U10](U10-canvas-lenses.md) | Size, colour, badge and edge-style lenses on the graph views, inferred edges drawn dashed, PageRank, centrality and community lenses from view-only F14 runs cached on the server, and the derivation tree of an inferred edge from a search over the materialized closure | implemented (Phases 1, 2 and 3) |
| [U11](U11-linked-views.md) | Arcs between located entities on the map, facet histograms computed on the server whose brushes filter the canvas and add filters to the query, and an embedding map of a vector index whose lasso becomes a `VALUES` block, all sharing one selection | implemented in part (Phases 1, 2 and 3 with recorded omissions) |

### How the graph-interaction features are grouped

The user approved ten graph-interaction features on 2026-10-23. Three of them extend a
spec that already owns their data, so they are phases of that spec. The plan drawn on the
query's pattern is U04 Phase 4, because U04 owns the plan views and their redactions. The
validation overlay is U07 Phase 4, because U07 owns validation results and their access
rule. The time-lapse and the diff on the canvas are U01 Phase 4, because U01 owns the
slider, compare mode and their marks.

The other seven features form four new specs. The query builder is U08 rather than a phase
of U03, because U03 starts from data selected in Explore and has no way back from the
text, while U08's round trip through the text is a design of its own. The path finder is
U09, because it stands alone with its own server phase. The overlays that change how drawn
elements look, which are the algorithm lenses and the explanations of inferred edges, share
one lens model in U10, and U07 Phase 4 draws its badges with it. The views that sit beside
the canvas and share its selection, which are the map arcs, the facet histograms and the
embedding map, are U11.

The [provenance index](PROVENANCE.md) locates the records for these specs, the
allocator, the vendored spargebra and the development tools.
