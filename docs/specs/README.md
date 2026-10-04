# Feature specs

These are the design specs for the features Sparkles added beyond its port of Jena,
Fuseki and QLever's engine. Each spec was written before the feature was built. The
sources were public standards, published papers, the Sparkles code, and the documentation
and permissively licensed code of other projects. [PROVENANCE.md](PROVENANCE.md) records
the sources of each spec and the dependencies it brought in.

Each spec opens with a status block that gives its status, the phases shipped and links
to the user docs. It closes with an **Outcome** section, which records what was delivered,
where the implementation departed from the design and why, and the decisions the
maintainer made. The Outcome also gives the test and conformance results at landing, the
measured performance, and what was deferred or rejected. The text in between is the
design as it was written. Where the design and the code disagree, the code,
[the API reference](../API.md) and [the feature list](../FEATURES.md) describe the current
behaviour, and the Outcome explains the difference.

## How to use them

* **As the design rationale.** A spec explains why a feature works the way it does. It
  covers the goals and non-goals, the alternatives that were rejected, the compatibility
  choices (usually with Jena or Fuseki), the failure modes, and the acceptance examples
  the tests were written from. Read the spec before you change the feature.
* **Kept with the code.** When a change alters a feature's behaviour, update its spec in
  the same change. Amend or extend the Outcome and leave the original design as it was,
  so the record of how the feature came about stays intact. A new feature of similar size
  gets a new spec here, with a new ID and a PROVENANCE entry.
* **Cross-references.** Specs refer to each other by ID and section, such as
  [C10 §6.2](C10-write-time-validation.md). A `§` number refers to a section of the spec
  it names. Inside a spec, the labels `A1`, `A2` and so on number its acceptance examples.

## IDs

| Prefix | Group |
|---|---|
| `CI` | Durable commit identity. The commit sequence, dataset ids and receipts that most later features build on. |
| `C` | Smaller server and engine capabilities: observability and budgets, schema discovery, a GraphQL adapter, CSV and TSV imports, cloning, inference freshness, access control, write-time validation, the MCP server, automatic compaction, write previews and stored queries. |
| `F` | Larger data features: a Cypher frontend, full-text and vector search, path search, embeddings computed on write, backups to object storage, retained history and point-in-time reads, branches and merges, replication, and encryption at rest. |
| `G` | Gaps against Apache Jena that were out of scope for the first version: GeoSPARQL, ShEx, the SHACL Compact Syntax, the command-line tools, the converter of Fuseki configurations and the SERVICE enhancer. |
| `P` | Bindings and clients for other programs. These are the Python package that embeds the engine, the Rust client of remote servers, the JVM library that embeds the engine behind Apache Jena's API, the Node.js packages for the embedded engine and a remote client, and the administration API that all of them share. |
| `X` | Internal engineering that does not derive from any other database product: compression codecs, the formatter, the linter, and the OpenAPI description with shell completions. |

Numbers are stable. Gaps in the numbering are roadmap items without a spec: writes and a
property-graph representation for the Cypher frontend, a Datalog frontend, and the
extension API for functions, property functions and aggregates (P03). G01
replaced the narrower geospatial item on that list.

## Specs

A spec's status is one of three values. *Implemented* means every phase shipped.
*Implemented in part* means the first phase shipped and some later phases or items were
not built; the status block says which. *Specified* means the design is written and
there is no code yet.

| Spec | Summary | Status |
|---|---|---|
| [CI](CI-commit-identity.md) | Durable dataset ids, a gap-free commit sequence, commit receipts and headers, `/$/commits` and `sparkles log`, commit messages, change digests, entity tags, the catalog horizon and the change feed | implemented (Phases 1–3) |
| [C01](C01-observability-and-budgets.md) | Request ids, access log, Prometheus metrics, readiness, OpenTelemetry, memory and result-size budgets, cancellation on disconnect | implemented in part |
| [C02](C02-schema-discovery.md) | `GET /$/schema/{ds}` with paginated class and predicate listings computed on the server, a VoID export, a SHACL constraints layer, subject classes per predicate, class profiles, schema diffs between commits, reports kept up to date from the changes, shapes drafted from the data, `sparkles schema`, and the UI's schema browser | implemented |
| [C03](C03-graphql.md) | A read-only GraphQL endpoint per dataset, with a schema mapped to classes and predicates by `@rdf` directives and drafted from SHACL shapes or the data, compiled to batched SPARQL algebra under the caller's view and budgets | implemented in part (Phase 1) |
| [C05](C05-tabular-imports.md) | CSV and TSV imports with a default mapping, W3C CSVW metadata or Tarql-style CONSTRUCT templates, in `sparkles load`, `sparkles csv` and uploads | implemented |
| [C06](C06-clone-to-sandbox.md) | Cloning a consistent snapshot of a dataset into a new, independent dataset | implemented in part (Phases 1 and 2, part of Phase 3) |
| [C08](C08-inference-freshness.md) | Whether materialized inferences are current, re-running them incrementally, inconsistency diagnostics, input graphs with `owl:imports`, and RDFS on read | implemented (Phases 1–4) |
| [C09](C09-dataset-access-control.md) | Authentication (Basic, API tokens, OIDC with access tokens and back-channel logout, trusted proxies, Cloudflare Access, CLI logins, native TLS) and per-dataset permissions | implemented (Phases 1 and 2, Phase 3 as C12) |
| [C10](C10-write-time-validation.md) | A per-dataset SHACL or ShEx guard that validates the state after each write before the write commits | implemented in part (Phases 1–3) |
| [C11](C11-mcp-server.md) | `sparkles mcp` and `/$/mcp`, a Model Context Protocol server over stdio and HTTP with query, schema, search, validation and formatting tools, an opt-in update tool, resources, prompts and stored queries as tools | implemented in part (Phases 1–2) |
| [C12](C12-graph-access-control.md) | Grants limited to some named graphs or endpoints of a dataset, enforced by a filtered dataset view in the engine | implemented |
| [C12b](C12b-triple-access-control.md) | C12 Phase 2: protections that hide triples by predicate, subject class or a SPARQL pattern with the caller bound, lifted by grants and enforced by a masked snapshot in the engine | implemented |
| [C13](C13-automatic-compaction.md) | Background compaction triggered by the delta's size, the log's size, age and idle time, with writes continuing during the build | implemented |
| [C15](C15-write-previews.md) | Dry runs of updates, Graph Store writes, uploads, RDF Patches and the MCP write tool, which report the commit, the changes, validation, quota and preconditions and roll back | implemented |
| [C16](C16-stored-queries.md) | Named queries per dataset with typed parameters bound as terms, versions, runs by name, MCP tools and the UI's saved queries | implemented |
| [F01](F01-cypher.md) | A read-only openCypher subset compiled to the SPARQL algebra over a property-graph view of RDF, with relationship identity and properties through RDF 1.2 reifiers, `/{ds}/cypher` and `sparkles cypher` | specified |
| [F03](F03-full-text-search.md) | BM25 full-text search over literals with Tantivy, through Jena's `text:query`, with Lucene's query syntax, highlighting and stemming per language | implemented in part (Phase 1, most of Phase 2, part of Phase 3) |
| [F04](F04-vector-search.md) | `spk:vector` literals, similarity functions, `spk:vectorSearch`, HNSW vector indexes per predicate, and hybrid ranking with full-text search | implemented in part (Phases 1, 1b and 2, part of Phase 3) |
| [F05](F05-snapshot-repositories.md) | Incremental, deduplicated backups to a file system or S3, restore, verification, policies and GC | implemented in part (Phase 1, most of Phase 2) |
| [F06](F06-snapshots-and-point-in-time.md) | Point-in-time reads with `?at=`, named snapshots, a retention window, diffs between commits as JSON or RDF Patch, a change feed, a change log and history queries | implemented in part (Phases 1–3; full-text search at pins and pin rebasing deferred) |
| [F07](F07-path-search.md) | Paths as solutions through `SERVICE path:search`: one, all or the k shortest paths, or every path up to a length, over chosen predicates and directions, with ends bound by the query and weights on reifiers | implemented in part (Phase 1, part of Phase 2) |
| [F08](F08-embeddings-on-write.md) | Vectors computed from selected literals by an OpenAI-compatible embeddings endpoint after each commit, with catch-up, re-embedding, status, metrics, chunking, rate limits and text queries | implemented (Phases 1 and 2) |
| [F09](F09-branches-and-merges.md) | Branches that share their parent's index until they compact, `?branch=` and `/{ds}@{branch}`, three-way merges of quad sets with cell conflicts and resolutions, protected branches, and clones of branches | implemented in part (Phase 1) |
| [F10](F10-replication.md) | Applying RDF Patch through Fuseki's `patch` operation, with `H prev` as a concurrency check, then read replicas that pull commits as RDF Patch, keep the primary's commit ids, bootstrap from a generation copy or a backup, and are promoted by hand | implemented in part (Phase 1; Phases 2 and 3 deferred) |
| [F11](F11-encryption-at-rest.md) | Client-side encrypted backup repositories with keyed blob ids, content-defined chunking, and AES-256-GCM encryption at rest for dataset files under KMS-wrapped per-dataset keys | specified |
| [G01](G01-geosparql.md) | GeoSPARQL 1.1 functions, Jena's spatial extensions, a spatial index, spatial joins and the UI's maps | implemented in part (Phases 1–2, part of Phase 3) |
| [G02](G02-shex.md) | ShEx 2.1 validation: ShExC, ShExJ and ShExR, shape maps, `POST /{ds}/shex` and write-time ShEx, with schemas in ShExR graphs of the dataset | implemented in part (Phases 1–2, Phase 3 except ShEx 2.2) |
| [G03](G03-shaclc.md) | The SHACL Compact Syntax (`text/shaclc`) read and written wherever shapes go in or come out, and the SHACL 1.2 list constraints | implemented in part (Phase 1) |
| [G05](G05-command-line-tools.md) | `convert` (`riot`), `qparse`, `uparse`, `compare` (`rdfdiff`), `iri`, `langtag`, `rsparql`, `rupdate`, `rset` and `rdfpatch`, and IRI and language-tag warnings in `convert --check`, `load --check` and `/$/validate/iri` | implemented |
| [G06](G06-arq-query-extensions.md) | Jena ARQ's syntax extensions: `LATERAL`, property path ranges `{n,m}` and CONSTRUCT templates with `GRAPH`, and DESCRIBE as a concise bounded description, its symmetric form or the resource's own triples, chosen per dataset or per request | implemented in part (Phases 1–2) |
| [G08](G08-fuseki-configuration.md) | `sparkles config import fuseki`, which turns a Fuseki configuration and its `shiro.ini` or password file into a `serve` script, a data migration script, an auth configuration and dataset settings with a report of each element, and `serve --fuseki-config` | implemented |
| [G10](G10-service-enhancer.md) | Jena's SERVICE enhancer: `loop`, `bulk` and `cache` options in front of a SERVICE IRI, correlated SERVICE on the `LATERAL` operator, bulk requests as VALUES blocks or Jena's UNION, a per-dataset cache of remote results kept apart per caller, and `urn:x-arq:self` | implemented |
| [P01](P01-python-bindings.md) | The `sparkles` Python package: datasets, SPARQL, terms, quads, transactions, dumps, reasoning and validation, built with PyO3 and maturin as abi3 wheels | implemented in part (Phase 1, most of Phase 2) |
| [P02](P02-rust-client.md) | `sparkles-client`, an async and blocking Rust client for Sparkles and any SPARQL endpoint, with streaming results as `oxrdf` terms, receipts, the Graph Store Protocol, retries that honour `Retry-After`, and a test that checks it against the OpenAPI description | implemented |
| [P04](P04-jvm-bindings.md) | `sparkles-jena`, a Kotlin library over UniFFI bindings that embeds the engine behind Jena's `DatasetGraph`, transactions and query and update engines, with a per-query fallback to ARQ, an assembler type for Fuseki, an in-process TDB2 import and native libraries in the jar | implemented in part (Phase 1) |
| [P05](P05-node-bindings.md) | Node.js and TypeScript packages: the engine as a napi-rs addon with prebuilt binaries, async iteration of results in batches off the JavaScript thread, transactions on the single writer with `AbortSignal`, RDF/JS terms and a Comunica source, a remote client typed from the OpenAPI description, and what a browser build would take | specified |
| [P06](P06-library-admin-api.md) | The embedding API's administration surface: `Dataset` handles for snapshots, history, backups, indexes, schema, stored queries, reasoning, validation, settings and GraphQL, a `Catalog` of datasets in one data directory with a directory lock, one `Control` for cancellation and progress, the server's handlers on that API, tests that map every OpenAPI operation to a library call and every library call to each binding, and the Python bindings' missing methods | implemented in part |
| [X01](X01-compression-codecs.md) | zstd and brotli next to gzip and LZ4 for inputs, responses, dumps, backups and the UI's assets | implemented in part (Phase 1) |
| [X02](X02-formatter.md) | `sparkles fmt`, `POST /$/format`, `sparkles lsp` and the UI's Format button for SPARQL and RDF | implemented |
| [X03](X03-openapi-and-completions.md) | An OpenAPI 3.1 description at `/$/openapi.json`, kept equal to the route table by a test and checked in, `sparkles openapi`, shell completions and man pages | implemented in part (Phase 1, part of Phase 2) |
| [X04](X04-linter.md) | `sparkles lint` for SPARQL, Turtle and TriG, with severities per rule, safe fixes, diagnostics and quick fixes in `sparkles lsp`, `POST /$/lint` and the query editor | implemented |

[PROVENANCE.md](PROVENANCE.md) is the provenance record for all of these
specs, the allocator, the vendored spargebra and the development tools.
