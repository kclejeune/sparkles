# Sparkles

Sparkles is a fast RDF, SPARQL and OWL database written in Rust. It reimplements
[Apache Jena](https://jena.apache.org/) and Fuseki, with their protocols, semantics and
operational model, on the index and execution architecture of
[QLever](https://github.com/ad-freiburg/qlever). It ships as an embeddable library with
Rust, Python and JVM APIs, a Fuseki-compatible server and CLI, and a web UI.

> [!WARNING]
> Sparkles is experimental. There are no releases, and the on-disk format, HTTP API, CLI
> and library APIs can change in any commit without a migration path. Don't use it for
> data you can't regenerate.
>
> Much of the code, tests and documentation was written with AI models, directed and
> reviewed by the maintainer. The W3C conformance suites and differential tests are the
> main safeguard, but expect bugs. Issues and bug reports are welcome.

![The query editor with results](docs/images/query.png)

## Why Sparkles

* **Fuseki clients keep working.** Sparkles implements the SPARQL 1.1 Query, Update and
  Graph Store protocols, Fuseki's endpoints and `/$/` admin API, Jena's formats and
  dataset semantics, and TDB2's model of bulk loads, transactions, compaction and
  backups. It passes the W3C SPARQL suites in full, and Jena's own HTTP clients are
  tested against it.
* **QLever's speed, with updates.** Terms are 64-bit ids with numbers and dates inline,
  indexes are sorted and compressed permutations, and a cost-based planner drives
  column-at-a-time execution. Sparkles adds MVCC transactions, a write-ahead log and
  online compaction. It is the fastest of five engines on every benchmark query at 10.5M
  triples, and loads English DBpedia (1.24 billion triples) 2.8× faster than QLever.
* **History you can query and branch.** Every commit has a durable id. Queries can read
  any retained commit, time or named snapshot, and Sparkles diffs two commits, streams a
  change feed, answers SPARQL history queries, and keeps branches that merge three ways.
* **One server for the whole stack.** Reasoning, SHACL and ShEx validation, full-text,
  vector, path and spatial search, GraphQL, an MCP server for LLM agents, and access
  control down to single triples all run in the engine, on the same snapshots and
  budgets as SPARQL.

## Features

**Storage and history**
* MVCC snapshots with a single writer, a crash-safe WAL, and automatic background
  compaction that rewrites only the index blocks a change touches.
* Point-in-time reads (`?at=commit:N`, a time or a named snapshot), diffs as JSON or RDF
  Patch, a resumable change feed with server-sent events, and history queries that ask
  when a triple was added or which values a property took
  ([API](docs/API.md#point-in-time-reads-and-snapshots)).
* Branches that share their upstream's index until they compact, so creating one writes
  a few kilobytes, with fast-forward and three-way merges and conflict resolution
  ([API](docs/API.md#branches-and-merges)).
* RDF Patch applied as one commit, entity tags with `If-Match` writes, commit messages,
  and dry runs of any write that report its changes, validation and quota effect
  ([API](docs/API.md#write-previews)).
* Parallel bulk loading from every W3C syntax and Jena's TriX, RDF Thrift, RDF Protobuf
  and RDF/JSON, CSV imports through CSVW or Tarql-style templates, and incremental,
  deduplicated backups to a file system or S3 ([usage](docs/USAGE.md#backup-repositories)).

**Query**
* SPARQL 1.1 and SPARQL 1.2 / RDF 1.2: 482/482, 328/328, 157/157 and 269/269 on the W3C
  suites. Each result can come with its executed plan, and every query runs within
  memory, row and work budgets.
* Jena ARQ's language: `LATERAL`, path ranges, `LET`, `cdt:` lists and maps, its
  statistical aggregates, and its `fn:`, `afn:`, `math:` and property function
  libraries, checked against Jena's answers ([API](docs/API.md#arq-syntax-extensions)).
* Federated `SERVICE` under an outbound network policy, with Jena's `loop:`, `bulk:` and
  `cache:` options, and configurable DESCRIBE ([API](docs/API.md#describe)).

**Reasoning and validation**
* RDFS, OWL 2 RL and Jena rules, materialized and kept up to date incrementally, with
  staleness reports, inconsistency checks and Fuseki's RDFS on read
  ([API](docs/API.md#reasoning-status-and-diagnostics)).
* SHACL Core and SHACL-SPARQL (98/98 and 20/20 on the W3C suites), the SHACL compact
  syntax, and ShEx 2.1 with ShExC, ShExJ and ShExR ([API](docs/API.md#shacl-validation)).
* Write-time guards that validate each commit before it is written, re-checking only the
  focus nodes a write can affect, and shapes drafted from the data
  ([API](docs/API.md#write-time-validation)).
* Schema discovery with exact counts, class profiles and schema diffs between commits
  ([API](docs/API.md#class-profiles)).

**Search**
* Full-text search through Jena's `text:query`, ranked by BM25 with Tantivy, with
  stemming per language and rebuilds that don't block writes ([API](docs/API.md#full-text-search)).
* Vector similarity with an HNSW index, embeddings computed on write through any
  OpenAI-compatible endpoint, and hybrid text and vector ranking
  ([API](docs/API.md#vector-similarity)).
* Path search that returns the shortest, k shortest or all paths as solutions
  ([API](docs/API.md#path-search)).
* GeoSPARQL 1.1 with a spatial index, Jena's `spatial:` functions, spatial joins and
  nearest-neighbour search ([API](docs/API.md#geosparql)).

**Server and integrations**
* Fuseki's endpoints and admin API, plus endpoints for commits, schema, clones,
  reasoning and validation, all described by an OpenAPI 3.1 document
  ([API](docs/API.md#openapi-description)).
* Authentication with Basic, API tokens, OIDC, Cloudflare Access or trusted proxies, and
  access control per dataset, named graph, endpoint and triple, with rate limits
  ([API](docs/API.md#authentication-and-access-control)).
* Stored queries with typed parameters, a read-only GraphQL endpoint over a mapping
  schema, and an MCP server whose tools run as the caller
  ([MCP](docs/USAGE.md#mcp-server-llm-agents), [GraphQL](docs/API.md#graphql)).
* A web UI embedded in the binary, a Jena-style CLI that works on a database directory
  or a remote server, a formatter, linter and language server for SPARQL and RDF, and
  Prometheus metrics and OpenTelemetry traces.
* A Docker image and compose file, and a Nix package with a NixOS module
  ([Docker](docs/USAGE.md#docker), [NixOS](docs/USAGE.md#deploying-on-nixos)).

[docs/FEATURES.md](docs/FEATURES.md) lists every feature and what is not there yet.

## Comparison

| Engine | Where Sparkles stands |
|---|---|
| [Apache Jena / Fuseki](https://jena.apache.org/) | The same protocols, endpoints, admin API, CLI model and ARQ extensions, on sorted columnar indexes. Faster on every benchmark query at 10.5M triples, by a median of 60×. Reasoning is materialized, apart from RDFS on read, and there is no ontology API or JavaScript functions. |
| [QLever](https://github.com/ad-freiburg/qlever) | The same index and execution architecture, plus exact term identity, MVCC updates, the Graph Store Protocol, reasoning and validation. Faster on all 28 benchmark queries and all 20 WatDiv templates at 10.5M triples, using about 40% more server memory. Intermediate results are materialized, not streamed. |
| [Oxigraph](https://github.com/oxigraph/oxigraph) | Sparkles uses Oxigraph's parsers and SPARQL parser with its own storage and planner. Faster on every benchmark query at 10.5M triples, by a median of 87×. Sparkles fsyncs its writes and Oxigraph does not, and Oxigraph commits a stream of single-triple updates 2.7× faster. There is no WebAssembly build. |
| [Fluree](https://github.com/fluree/db) | Both keep history and branches. Sparkles adds full W3C SPARQL conformance and Fuseki compatibility, and has no policies stored in the data or clustering. Faster on every benchmark query Fluree completes at 10.5M triples, by a median of 7.7×. |

[docs/COMPARISON.md](docs/COMPARISON.md) lists the gaps per engine and where Sparkles
departs from Jena and QLever on purpose.

## Performance

All numbers come from one machine (an Intel i5-13500 with 15 GiB of RAM) on 2026-10-03,
with Sparkles at commit `98a75c1a`, each engine running alone with its result cache off.
At 10.5M triples:

| | Sparkles | Best of the others |
|---|---|---|
| Bulk load | **3.7 s** | QLever 9.7 s |
| Queries (28) | Fastest on all 28 | Fluree within 7% on three lookups and counts |
| WatDiv, 20 templates | **5.04 ms** geometric mean | Fluree 7.41 ms |
| Update latency (1 triple) | **4.17 ms**, fsynced | QLever 4.19 ms, in memory only |
| Throughput, 16 clients | **243 q/s** | QLever 92 q/s |
| Server memory after the run | 916 MiB, 379 MiB of it block cache | **QLever 653 MiB** |

Sparkles trades memory for speed by default, with a 1 GiB block cache per dataset. On
English DBpedia (1.24 billion triples) it loads in 596 s against QLever's 1,674 s and
Fluree's 2,919 s, and is faster than QLever on 28 of 29 warm queries and 26 of 31 cold
ones. [docs/BENCHMARKS.md](docs/BENCHMARKS.md) has every number, the memory tradeoff and
every query where Sparkles loses or ties.

## Getting started

Build from source (building the UI first embeds it in the binary), run it with Nix, or
use Docker:

```sh
pnpm -C ui install && pnpm -C ui build        # optional: the web UI
cargo install --path crates/sparkles-server   # installs the `sparkles` binary
nix run github:kclejeune/sparkles -- serve --data ./data
docker compose up --build -d                  # UI and server on 127.0.0.1:3030
```

Load a file and serve it, or create a dataset on a running server:

```sh
sparkles load --loc ./books books.ttl
sparkles serve --data ./data --loc books=./books

curl -X POST 'localhost:3030/$/datasets' -d 'dbName=films&dbType=persistent'
curl -X POST 'localhost:3030/films/data?default' -H 'Content-Type: text/turtle' --data-binary @films.ttl
```

Query and update:

```sh
curl localhost:3030/books/sparql -H 'Accept: text/csv' \
  --data-urlencode 'query=SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 10'
sparkles query --loc ./books 'SELECT (COUNT(*) AS ?n) { ?s ?p ?o }'
sparkles query --data books.ttl --query q.rq   # files in memory, no database
```

The UI is at <http://localhost:3030/ui/>. The CLI has Jena's `tdb2.*`, `arq` and `riot`
tools and Sparkles' own commands for backups, history, reasoning, validation, indexes and
more. `sparkles --help` lists them, and [docs/USAGE.md](docs/USAGE.md) covers the CLI and
running the server.

## Library usage

The `sparkles` crate is the embeddable library. With its default features it has no HTTP
server or async runtime, and the server and CLI are built on its public API.

```rust
use sparkles::Dataset;

let ds = Dataset::open("mydb")?;                 // or Dataset::memory()
ds.load_file("data.ttl.gz")?;
for row in &ds.select("SELECT ?s ?name { ?s <http://xmlns.com/foaf/0.1/name> ?name }")? {
    println!("{} {}", row.get("s").unwrap(), row.get("name").unwrap());
}
ds.update(r#"INSERT DATA { <http://ex/carol> <http://xmlns.com/foaf/0.1/name> "Carol" }"#)?;
```

`Dataset` also has a query builder, transactions, and handles for snapshots, history,
indexes, reasoning, validation and backups ([usage](docs/USAGE.md#embedding-the-library)).

The same engine is available from other languages:

* **Python.** `crates/sparkles-py` is a package with a pyoxigraph-style API and an
  rdflib store plugin ([usage](docs/USAGE.md#python)).
* **JVM.** `sparkles-jena` in `jvm/` is a Jena `DatasetGraph` backed by Sparkles. TDB2
  code runs on it after changing the line that opens the dataset, and queries run in
  Sparkles' engine ([usage](docs/USAGE.md#jvm-apache-jena)).
* **Remote.** `crates/sparkles-client` is a Rust client with Jena's `RDFConnection`
  operations, for Sparkles or any SPARQL endpoint ([usage](docs/USAGE.md#rust-client)).

```python
from sparkles import Dataset

with Dataset("mydb") as ds:
    ds.load(path="data.ttl.gz")
    for row in ds.query("SELECT ?s ?name WHERE { ?s <http://xmlns.com/foaf/0.1/name> ?name }"):
        print(row["s"], row["name"].value)
```

## Web UI

<table>
  <tr>
    <td><img src="docs/images/graph.png" alt="Results as a graph"></td>
    <td><img src="docs/images/explore.png" alt="The resource explorer"></td>
  </tr>
  <tr>
    <td>Results as a graph</td>
    <td>The resource explorer</td>
  </tr>
  <tr>
    <td><img src="docs/images/dataset.png" alt="The dataset page"></td>
    <td><img src="docs/images/schema.png" alt="The schema browser"></td>
  </tr>
  <tr>
    <td>The dataset page: indexes, reasoning, backups</td>
    <td>The schema browser</td>
  </tr>
  <tr>
    <td><img src="docs/images/validate.png" alt="The Validate panel"></td>
    <td><img src="docs/images/map.png" alt="A GeoSPARQL map"></td>
  </tr>
  <tr>
    <td>SHACL and ShEx validation</td>
    <td>GeoSPARQL results on a map</td>
  </tr>
</table>

## Documentation

| Document | Contents |
|---|---|
| [docs/FEATURES.md](docs/FEATURES.md) | Every feature with its status, and the known gaps. |
| [docs/USAGE.md](docs/USAGE.md) | The server and CLI, backups, MCP, the libraries and bindings, Docker and NixOS. |
| [docs/API.md](docs/API.md) | The HTTP API: Fuseki's endpoints and the `/$/` extensions. |
| [docs/openapi.json](docs/openapi.json) | The OpenAPI 3.1 description the server serves at `/$/openapi.json`. |
| [docs/COMPARISON.md](docs/COMPARISON.md) | Feature gaps against Jena/Fuseki, QLever, Fluree and Oxigraph, and departures from Jena and QLever. |
| [docs/BENCHMARKS.md](docs/BENCHMARKS.md) | Measured performance, against the other engines and on its own. |
| [docs/editors.md](docs/editors.md) | Formatter and language-server setups for editors. |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Building, mise tasks, tests, Nix and third-party licenses. |
| [Design specs](docs/specs/README.md) | Why each feature is built the way it is. |
| [docs/AUDIT.md](docs/AUDIT.md) | The Jena and QLever audits, and what Sparkles reuses from Oxigraph. |
| [ui/README.md](ui/README.md) | Developing the web UI. |

## Project layout

| Path | Role | Jena analogue |
|---|---|---|
| `crates/sparkles-core` | The engine: ids, vocabulary, indexes, bulk builder, MVCC store and WAL, SPARQL and RDF I/O | jena-core, jena-arq, jena-tdb2 |
| `crates/sparkles` | The library: the engine's modules, the `Dataset` API, its handles and the query builder | jena-querybuilder, in-process RDFConnection |
| `crates/sparkles-server` | The HTTP server and the `sparkles` CLI | jena-fuseki2, jena-cmds |
| `crates/sparkles-reasoner` | RDFS, OWL 2 RL and Jena rules by semi-naive forward chaining | jena-core `reasoner` |
| `crates/sparkles-shacl`, `crates/sparkles-shex` | SHACL and ShEx validation over store snapshots | jena-shacl, jena-shex |
| `crates/sparkles-backup` | Backup repositories on a file system or S3 | Fuseki `/$/backup` |
| `crates/sparkles-graphql` | The read-only GraphQL adapter | — |
| `crates/sparkles-fmt`, `crates/sparkles-fmt-wasm` | The formatter and linter, and their WebAssembly build | — |
| `crates/sparkles-client` | The Rust client of remote SPARQL endpoints | jena-rdfconnection (remote) |
| `crates/sparkles-py` | The Python package (PyO3, its own cargo workspace) | — |
| `crates/sparkles-ffi`, `jvm/` | The JVM bindings: the UniFFI native library and the Kotlin `sparkles-jena` library | jena-tdb2's `DatasetGraphTDB` |
| `vendor/spargebra` | Oxigraph's SPARQL parser, vendored with fixes ([PATCHED.md](vendor/spargebra/PATCHED.md)) | ARQ's grammar |
| `ui/` | The SvelteKit web UI | jena-fuseki-ui |

## Development

`mise run ci` runs the formatting checks, Clippy, every workspace test and the UI tests,
and `mise run test:w3c`, `test:shacl` and `test:shex` run the conformance suites from an
Apache Jena checkout. [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) covers the rest.

## License

Sparkles is licensed under the [Apache License 2.0](LICENSE). The vendored `spargebra`
keeps its MIT OR Apache-2.0 license. The licenses of the crates and npm packages that
ship in the binary are in [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) and
[THIRD_PARTY_LICENSES-UI.md](THIRD_PARTY_LICENSES-UI.md).
