# Feature specs

These are the design specs for the features Sparkles added beyond its port of Jena,
Fuseki and QLever's engine. Each spec was written before the feature was built. The
sources were public standards, published papers, the Sparkles code, and the documentation
and permissively licensed code of other projects. [PROVENANCE.md](PROVENANCE.md) records
the sources of each spec and the dependencies it brought in. Fluree was not consulted for
any of them.

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
| `C` | Smaller server and engine capabilities: observability and budgets, schema discovery, cloning, inference freshness, access control, write-time validation and the MCP server. |
| `F` | Larger data features: full-text and vector search, backups to object storage, retained history and point-in-time reads. |
| `G` | Gaps against Apache Jena that were out of scope for the first version: GeoSPARQL and ShEx. |
| `P` | Bindings that embed the engine in other languages: Python. |
| `X` | Internal engineering that does not derive from any other database product: compression codecs and the formatter. |

Numbers are stable. Gaps in the numbering are roadmap items without a spec: a Cypher
frontend and property-graph view, a GraphQL adapter, tabular imports and a Datalog
frontend. G01 replaced the narrower geospatial item on that list.

## Specs

A spec's status is one of three values. *Implemented* means every phase shipped.
*Implemented in part* means the first phase shipped and some later phases or items were
not built; the status block says which. *Designed, not built* means there is no code yet.

| Spec | Summary | Status |
|---|---|---|
| [CI](CI-commit-identity.md) | Durable dataset ids, a gap-free commit sequence, commit receipts and headers, `/$/commits` and `sparkles log` | implemented in part (Phases 1–2) |
| [C01](C01-observability-and-budgets.md) | Request ids, access log, Prometheus metrics, readiness, OpenTelemetry, memory and result-size budgets, cancellation on disconnect | implemented in part |
| [C02](C02-schema-discovery.md) | `GET /$/schema/{ds}` with paginated class and predicate listings computed on the server, a VoID export, `sparkles schema`, and the UI's schema browser | implemented in part (Phase 1, VoID export) |
| [C06](C06-clone-to-sandbox.md) | Cloning a consistent snapshot of a dataset into a new, independent dataset | implemented in part (Phase 1) |
| [C08](C08-inference-freshness.md) | Whether materialized inferences are current, re-running them, and inconsistency diagnostics | implemented in part (Phases 1–2) |
| [C09](C09-dataset-access-control.md) | Authentication (Basic, API tokens, OIDC, trusted proxies, CLI logins) and per-dataset permissions | implemented in part (Phase 1, part of Phase 2) |
| [C10](C10-write-time-validation.md) | A per-dataset SHACL guard that validates the state after each write before the write commits | implemented in part (Phase 1) |
| [C11](C11-mcp-server.md) | `sparkles mcp`, a Model Context Protocol server with read-only query, schema, search, validation and formatting tools | implemented in part (Phase 1, part of Phase 2) |
| [F03](F03-full-text-search.md) | BM25 full-text search over literals with Tantivy, through Jena's `text:query` | implemented in part (Phase 1, part of Phase 2) |
| [F04](F04-vector-search.md) | `spk:vector` literals, similarity functions and exact top-k `spk:vectorSearch` | implemented in part (Phase 1) |
| [F05](F05-snapshot-repositories.md) | Incremental, deduplicated backups to a file system or S3, restore, verification, policies and GC | implemented in part (Phase 1, most of Phase 2) |
| [F06](F06-snapshots-and-point-in-time.md) | Point-in-time reads with `?at=`, named snapshots, a retention window and diffs between commits | implemented in part (Phases 1 and 2) |
| [G01](G01-geosparql.md) | GeoSPARQL 1.1 functions, Jena's spatial extensions, a spatial index, spatial joins and the UI's maps | implemented in part (Phases 1–2) |
| [G02](G02-shex.md) | ShEx 2.1 validation: ShExC, ShExJ and ShExR, shape maps, `POST /{ds}/shex` and write-time ShEx | implemented in part (Phases 1–2) |
| [P01](P01-python-bindings.md) | The `sparkles` Python package: datasets, SPARQL, terms, quads, transactions, dumps, reasoning and validation, built with PyO3 and maturin as abi3 wheels | implemented in part (Phase 1) |
| [X01](X01-compression-codecs.md) | zstd and brotli next to gzip and LZ4 for inputs, responses, dumps, backups and the UI's assets | implemented in part (Phase 1) |
| [X02](X02-formatter.md) | `sparkles fmt`, `POST /$/format`, `sparkles lsp` and the UI's Format button for SPARQL and RDF | implemented |

[PROVENANCE.md](PROVENANCE.md) is the clean-room provenance record for all of these
specs, the allocator, the vendored spargebra and the development tools.
