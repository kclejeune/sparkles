# Feature specs

These are the design specs of the features Sparkles added beyond its port of Jena,
Fuseki and QLever's engine. Each was written before its implementation, from public
standards, the documentation and permissively licensed code of other projects, published
papers and the Sparkles code; [PROVENANCE.md](PROVENANCE.md) records the sources of each
and the dependencies it brought in. Fluree was not consulted for any of them.

Each spec starts with a status block (status, phases shipped, links to the user docs) and
ends with an **Outcome** section: what was delivered, where the implementation departed
from the design and why, the decisions the maintainer made, the test and conformance
results at landing, the measured performance, and what was deferred or rejected. The text
between them is the design as it was written. Where it and the code disagree, the code,
[the API reference](../API.md) and [the feature list](../FEATURES.md) describe current
behaviour, and the Outcome says why.

## How to use them

* **As the design rationale.** A spec explains why a feature works the way it does: the
  goals and non-goals, the alternatives that were considered and rejected, the
  compatibility choices (usually with Jena or Fuseki), the failure modes and the
  acceptance examples the tests were written from. Read it before changing the feature.
* **Kept with the code.** When a change alters a feature's behaviour, update its spec in
  the same change: amend the Outcome (or add to it) rather than rewriting the original
  design, so the record of how the feature was arrived at stays intact. A new feature of
  similar size gets a new spec here, with a new ID and a PROVENANCE entry.
* **Cross-references.** Specs refer to each other by ID and section, such as
  [C10 §6.2](C10-write-time-validation.md); `§` numbers refer to the spec's own sections.
  The `A1`, `A2`, … labels inside a spec number its own acceptance examples.

## IDs

| Prefix | Group |
|---|---|
| `CI` | Durable commit identity: the commit sequence, dataset ids and receipts that most later features build on. |
| `C` | Smaller capabilities of the server and the engine: observability and budgets, schema discovery, cloning, inference freshness, access control, write-time validation, the MCP server. |
| `F` | Larger data features: full-text and vector search, backups to object storage, retained history and point-in-time reads. |
| `G` | Gaps against Apache Jena that were out of scope for the first version: GeoSPARQL and ShEx. |
| `X` | Internal engineering, not derived from any other database product: compression codecs and the formatter. |

Numbers are stable. Gaps in the numbering are roadmap items that have no spec (a Cypher
frontend and a property-graph view, a GraphQL adapter, tabular imports, a Datalog
frontend). G01 superseded the narrow geospatial item of that list.

## Specs

**Status:** *implemented*: every phase of the spec shipped; *implemented in part*: the
first phase shipped and later phases or items are not built (the status block says which);
*designed, not built*: no code yet.

| Spec | Summary | Status |
|---|---|---|
| [CI](CI-commit-identity.md) | Durable dataset ids, a gap-free commit sequence, commit receipts and headers, `/$/commits` and `sparkles log` | implemented in part (Phases 1–2) |
| [C01](C01-observability-and-budgets.md) | Request ids, access log, Prometheus metrics, readiness, OpenTelemetry, memory and result-size budgets, cancellation on disconnect | implemented in part |
| [C02](C02-schema-discovery.md) | `GET /$/schema/{ds}`: paginated class and predicate listings computed on the server, `sparkles schema`, the UI's schema browser | implemented in part (Phase 1) |
| [C06](C06-clone-to-sandbox.md) | Cloning a consistent snapshot of a dataset into a new, independent dataset | implemented in part (Phase 1) |
| [C08](C08-inference-freshness.md) | Whether materialized inferences are current, re-running them, and inconsistency diagnostics | implemented in part (Phase 1) |
| [C09](C09-dataset-access-control.md) | Authentication (Basic, API tokens, OIDC, trusted proxies, CLI logins) and per-dataset permissions | implemented in part (Phase 1, part of Phase 2) |
| [C10](C10-write-time-validation.md) | A per-dataset SHACL guard that validates each write's post-state before it commits | implemented in part (Phase 1) |
| [C11](C11-mcp-server.md) | `sparkles mcp`: a Model Context Protocol server with read-only query, schema, search and validation tools | implemented in part (Phase 1, part of Phase 2) |
| [F03](F03-full-text-search.md) | BM25 full-text search over literals with Tantivy, through Jena's `text:query` | implemented in part (Phase 1, part of Phase 2) |
| [F04](F04-vector-search.md) | `spk:vector` literals, similarity functions and exact top-k `spk:vectorSearch` | implemented in part (Phase 1) |
| [F05](F05-snapshot-repositories.md) | Incremental, deduplicated backups to a file system or S3, restore, verification, policies and GC | implemented in part (Phase 1, most of Phase 2) |
| [F06](F06-snapshots-and-point-in-time.md) | Point-in-time reads with `?at=`, named snapshots and a retention window | implemented in part (Phase 1) |
| [G01](G01-geosparql.md) | GeoSPARQL 1.1 functions, Jena's spatial extensions, a spatial index, spatial joins and the UI's maps | implemented in part (Phases 1–2) |
| [G02](G02-shex.md) | ShEx 2.1 validation: ShExC, ShExJ and ShExR, shape maps, `POST /{ds}/shex`, write-time ShEx | implemented in part (Phases 1–2) |
| [X01](X01-compression-codecs.md) | zstd and brotli next to gzip and LZ4 for inputs, responses, dumps, backups and the UI's assets | implemented in part (Phase 1) |
| [X02](X02-formatter.md) | `sparkles fmt`, `POST /$/format`, `sparkles lsp` and the UI's Format button for SPARQL and RDF | implemented |

[PROVENANCE.md](PROVENANCE.md) is the clean-room provenance record of all of them, of the
allocator, of the vendored spargebra and of the development tools.
