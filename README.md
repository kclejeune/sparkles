# Sparkles

Sparkles is a fast RDF, SPARQL and OWL database written in Rust. It reimplements
[Apache Jena](https://jena.apache.org/) and Fuseki (same protocols, semantics and
operational model) on the index and execution architecture of
[QLever](https://github.com/ad-freiburg/qlever). It ships as an embeddable library, a
Fuseki-compatible server and CLI, and a web UI for managing databases, exploring graphs and
running queries.

> [!WARNING]
> Sparkles is experimental and not yet stable. There are no releases and no stability
> guarantees: the on-disk format, HTTP API, CLI and Rust API can change in any commit,
> without notice or a migration path. It is not recommended for production or for data you
> can't regenerate. Keep backups.
>
> Much of the code, tests and documentation was written with AI models, directed and
> reviewed by the maintainer. The W3C conformance suites and differential tests are the
> main safeguard, but expect bugs. Issues and bug reports are welcome.

![The query editor with results](docs/images/query.png)

## Philosophy

1. **Jena-compatible where users can see it.** Anything that talks to Fuseki should keep
   working. That covers the SPARQL 1.1 Query, Update and Graph Store protocols, Fuseki's
   endpoint names (`/{ds}/sparql|query|update|data|get|upload`), the `/$/` admin API, RDF
   and result formats, dataset semantics (default and named graphs, optional union default
   graph) and TDB2's operational model (bulk load, transactions, compaction, backups).
2. **QLever-style internals where performance matters.** Terms are dictionary-encoded into
   64-bit tagged ids with inline literals. Indexes are fully sorted, compressed
   permutation files. A cost-based DP planner drives column-at-a-time execution.
3. **Reuse the Rust RDF ecosystem.** Sparkles doesn't rewrite parsers that already exist.
   `oxrdf`, `oxttl`, `oxrdfxml`, `oxjsonld`, `spargebra`, `sparesults` and `oxsdatatypes`
   from the Oxigraph project supply the term model, parsers, SPARQL algebra and XSD value
   space. Sparkles adds the storage, planner, executor, server, reasoner and UI.
4. **Library first.** `crates/sparkles` is an embeddable engine with no HTTP or async
   dependencies, the counterpart of Jena's `core`, `arq` and `tdb2`. The server
   (`sparkles-server`, the Fuseki equivalent) and the reasoner are separate crates built on
   its public API.

## Highlights

**Storage and engine**
* MVCC snapshots with a single writer, a crash-safe WAL, and compaction into immutable
  generations of 7 sorted, compressed permutations ([features](docs/FEATURES.md#storage-tdb2-equivalent)).
* Durable commit ids, point-in-time reads (`?at=commit:N`, time, named snapshots) and a
  read-only integrity check ([API](docs/API.md#point-in-time-reads-and-snapshots)).
* Parallel bulk loading with external sort.
* N-Quads dumps, and incremental, deduplicated backups to a file system or S3
  ([usage](docs/USAGE.md#backup-repositories)).

**SPARQL**
* SPARQL 1.1 Query and Update, and SPARQL 1.2 / RDF 1.2. It passes the W3C suites in full:
  1.0 482/482, 1.1 query 328/328, 1.1 update 157/157, 1.2 269/269.
* A cost-based DP planner over columnar operators, a result cache, per-query memory and
  row budgets, and the executed plan returned with each result ([optimizations](docs/COMPARISON.md#optimizations-adopted-from-qlever)).
* Federated `SERVICE` under an outbound network policy ([usage](docs/USAGE.md#outbound-requests-service-and-load)).

**Server and CLI**
* Fuseki's endpoints, Graph Store Protocol, upload and `/$/` admin API, plus extensions
  for commits, schema discovery, clones, reasoning and validation ([API](docs/API.md)).
* A Jena-style CLI (`tdb2.*` and `arq` equivalents) that works on database directories or
  on a remote server ([usage](docs/USAGE.md#command-line-tools)).

**Reasoning**
* RDFS, OWL 2 RL and Jena rule syntax, materialized by semi-naive forward chaining.
* Staleness tracking, opt-in automatic re-runs and OWL 2 RL inconsistency checks
  ([API](docs/API.md#reasoning-status-and-diagnostics)).

**Validation**
* SHACL Core and SHACL-SPARQL (W3C 98/98 and 20/20), with Fuseki's `/{ds}/shacl` endpoint
  ([API](docs/API.md#shacl-validation)).
* ShEx 2.1: ShExC, ShExJ, ShExR and shape maps; passes 99.9% of the shexTest validation
  tests ([API](docs/API.md#shex-validation)).
* Write-time guards that validate each commit with SHACL or ShEx before it is written
  ([API](docs/API.md#write-time-validation)).

**Search**
* Full-text search through Jena's `text:query`, ranked by BM25 (Tantivy) ([API](docs/API.md#full-text-search)).
* Exact vector similarity search over `spk:vector` literals ([API](docs/API.md#vector-similarity)).
* GeoSPARQL 1.1 with a spatial index per dataset, Jena's `spatial:` and `spatialF:`
  functions, spatial joins and nearest neighbours ([API](docs/API.md#geosparql)).

**Formatter**
* A comment-preserving, self-checking formatter for SPARQL, Turtle/TriG, N-Triples/N-Quads
  and JSON-LD. It runs as `sparkles fmt`, as `POST /$/format`, as the `sparkles lsp`
  language server ([editors](docs/editors.md)) and in the browser ([usage](docs/USAGE.md#formatting)).

**Operations**
* A SvelteKit web UI embedded in the binary: query editor, results as a table, graph, plan
  or map, explorer, schema browser, validation and backups ([screenshots](#web-ui)).
* Authentication (Basic, API tokens, OIDC, trusted proxies) with per-dataset access
  control, and rate limiting ([API](docs/API.md#authentication-and-access-control)).
* Observability: access logs, Prometheus metrics, a readiness endpoint and OpenTelemetry
  traces ([features](docs/FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)).
* An MCP server with read-only, budgeted tools for LLM agents ([usage](docs/USAGE.md#mcp-server-llm-agents)).

The full list, with what is not there yet, is in [docs/FEATURES.md](docs/FEATURES.md).

## Comparison

| Engine | What it is | Where Sparkles stands |
|---|---|---|
| [Apache Jena / Fuseki](https://jena.apache.org/) | The Java reference stack: ARQ, TDB2 B+trees, Fuseki, on-the-fly inference, jena-text, GeoSPARQL | Same protocols, endpoints, admin API and CLI model on sorted columnar indexes; 1.8–580× faster at 10.5M triples. Reasoning is materialized only; no ARQ extensions, RDF Patch or ontology API |
| [QLever](https://github.com/ad-freiburg/qlever) | C++ engine for billions of triples with lazy, streaming execution | Same index and execution architecture, plus exact term identity, MVCC updates, the Graph Store Protocol, reasoning and SHACL; wins 19 of 20 queries at 10.5M (one tie), but is measured only to 10.5M and materializes intermediate results |
| [Oxigraph](https://github.com/oxigraph/oxigraph) | Rust database and toolkit on RocksDB, with Python and WebAssembly packages | Shares its parsers, SPARQL parser and datatypes; its own storage and planner, 1.5–465× faster at 10.5M, fsynced writes, plus reasoning, validation, search, auth and a UI; Rust bindings only |
| [Fluree](https://github.com/fluree/db) | A versioned, permissioned ledger (BUSL-1.1), JSON-LD first, with clustering | Full W3C SPARQL conformance and Fuseki compatibility; point-in-time reads and snapshots but no branches, history queries, policy language or clustering; faster on most queries, slower on a few single-pattern scans |

[docs/COMPARISON.md](docs/COMPARISON.md) has the feature gaps per engine, the deliberate
divergences from Jena and QLever, and the optimizations adopted from QLever.

## Performance

Measured with hyperfine over HTTP against Jena/Fuseki, QLever, Fluree and Oxigraph, with
every result cache off and each engine running alone, after checking that all engines
return the same answers. At 10.5M triples:

| | Sparkles | Next best |
|---|---|---|
| Bulk load | **4.7 s** | Oxigraph 9.0 s |
| Queries (20) | fastest on 17 | Fluree on `distinct-obj`, `contains`; QLever ties `minus` |
| Update latency (1 triple) | **5.4 ms** | Fluree 6.8 ms |
| Throughput, 16 clients | **193 q/s** | QLever 57 q/s |
| Server memory after the run | 921 MiB | **QLever 362 MiB** |

Datasets above 10.5M triples, cold caches and standard benchmarks (LUBM, BSBM, WatDiv)
have not been measured yet. [docs/BENCHMARKS.md](docs/BENCHMARKS.md) has every number,
both data sizes, and where Sparkles loses.

## Getting started

Build from source, or run it with Nix. The build embeds the web UI if it has been built
first:

```sh
pnpm -C ui install && pnpm -C ui build        # optional: the web UI
cargo install --path crates/sparkles-server   # installs the `sparkles` binary
# or: nix run github:kclejeune/sparkles -- serve --data ./data
```

Load a file into a new database and serve it (the server listens on `127.0.0.1:3030`):

```sh
sparkles load --loc ./books books.ttl
sparkles serve --data ./data --loc books=./books
```

Or create a dataset on a running server and load data over HTTP:

```sh
curl -X POST 'localhost:3030/$/datasets' -d 'dbName=films&dbType=persistent'
curl -X POST 'localhost:3030/films/data?default' -H 'Content-Type: text/turtle' --data-binary @films.ttl
sparkles load --server http://localhost:3030 --dataset films more-films.ttl.gz
```

Query and update:

```sh
curl localhost:3030/books/sparql -H 'Accept: text/csv' \
  --data-urlencode 'query=SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 10'
sparkles query --server http://localhost:3030 --dataset books 'SELECT (COUNT(*) AS ?n) { ?s ?p ?o }'
sparkles query --data books.ttl --query q.rq   # files in memory, no server
curl localhost:3030/books/update \
  --data-urlencode 'update=INSERT DATA { <http://example.org/b1> <http://purl.org/dc/terms/title> "Dune" }'
```

Then open the UI at <http://localhost:3030/ui/>. `sparkles --help` and
`sparkles help COMMAND` describe every command and flag. [docs/USAGE.md](docs/USAGE.md)
covers running and operating the server (network exposure, budgets, backups, NixOS).

## Command line

Common commands on a database directory (`--loc`) that no server holds open:

```sh
sparkles load    --loc db data/*.ttl.gz       # parallel bulk load
sparkles query   --loc db 'SELECT ...'        # --results text|json|xml|csv|tsv, --explain, --time
sparkles dump    --loc db --out dump.nq.zst   # N-Quads, compressed by extension
sparkles compact --loc db                     # merge updates into a new generation
sparkles backup  create --loc db --repo local # incremental backup to a repository
sparkles check   --loc db                     # read-only integrity check
sparkles fmt     --check queries/ shapes/     # SPARQL, Turtle, TriG, N-Triples, N-Quads, JSON-LD
```

| Command | Does |
|---|---|
| `serve` | the SPARQL server with the web UI |
| `load`, `query`, `update`, `dump` | bulk load, query and update (locally or on a `--server`), N-Quads export |
| `compact`, `clone`, `stats`, `log`, `check` | maintenance: merge updates, copy, statistics, commit history, verify |
| `snapshot` | named snapshots and history retention for point-in-time reads |
| `backup`, `repo` | N-Quads dumps, backup repositories (file system or S3), restore, policies |
| `infer` | materialize RDFS / OWL 2 RL / Jena rules, staleness, inconsistency checks |
| `shacl`, `shex`, `validation` | SHACL and ShEx validation, write-time guards |
| `schema` | classes and predicates with exact counts and declarations |
| `text-index`, `geo-index` | full-text and spatial indexes |
| `auth` | password hashes, API tokens, `auth login` for remote commands |
| `mcp` | the MCP server for LLM agents (stdio) |
| `fmt`, `lsp` | the formatter and its language server |

[docs/USAGE.md](docs/USAGE.md#command-line-tools) describes each one.

## Library usage

`crates/sparkles` is an embeddable engine with no HTTP or async dependencies. The CLI and
server are built on its public API.

```rust
use sparkles::Dataset;

let ds = Dataset::open("mydb")?;                 // or Dataset::memory()
ds.load_file("data.ttl.gz")?;                    // parallel bulk path for large inputs
let q = "PREFIX foaf: <http://xmlns.com/foaf/0.1/>
         SELECT ?s ?name WHERE { ?s foaf:name ?name } LIMIT 10";
for row in &ds.select(q)? {
    println!("{} {}", row.get("s").unwrap(), row.get("name").unwrap());
}
ds.update(r#"INSERT DATA { <http://ex/carol> <http://xmlns.com/foaf/0.1/name> "Carol" }"#)?;
```

Beyond `Dataset`, embedders get a fluent query builder (`sparkles::querybuilder`),
term-level graph access and transactions, and the reasoner, SHACL and ShEx crates.
[docs/USAGE.md](docs/USAGE.md#embedding-the-library) maps them to their Jena equivalents.

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
| [docs/FEATURES.md](docs/FEATURES.md) | every feature with its status, and the known gaps |
| [docs/USAGE.md](docs/USAGE.md) | running the server and CLI: options, formatting, backups, outbound requests, checks, MCP, embedding, NixOS |
| [docs/API.md](docs/API.md) | the HTTP API (Fuseki-compatible, plus `/$/` extensions) |
| [docs/COMPARISON.md](docs/COMPARISON.md) | Jena/Fuseki, QLever, Fluree and Oxigraph compared; design decisions; optimizations |
| [docs/BENCHMARKS.md](docs/BENCHMARKS.md) | measured performance against the other engines, and on its own |
| [docs/editors.md](docs/editors.md) | formatter and language-server setups for editors |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | building, mise tasks, tests, Nix, third-party licenses |
| [Design specs](docs/specs/README.md) | why each feature is built the way it is |
| [docs/AUDIT.md](docs/AUDIT.md) | the Jena and QLever audits, what is reused from Oxigraph, the language decision |
| [ui/README.md](ui/README.md) | developing the web UI |
| [vendor/spargebra/PATCHED.md](vendor/spargebra/PATCHED.md) | the fixes to the vendored SPARQL parser |

## Project layout

| Path | Role | Jena analogue |
|---|---|---|
| `crates/sparkles` | ids, vocabulary, permutation index, bulk builder, store (MVCC + WAL), SPARQL engine, RDF I/O | jena-core, jena-arq, jena-tdb2, jena-db, jena-querybuilder, jena-rdfconnection (in-process) |
| `crates/sparkles-reasoner` | RDFS / OWL 2 RL / Jena rule syntax, semi-naive forward chaining into `urn:x-sparkles:inferred` | jena-core `reasoner` |
| `crates/sparkles-shacl` | SHACL Core + SHACL-SPARQL validation over store snapshots | jena-shacl |
| `crates/sparkles-shex` | ShEx 2.1 validation (ShExC, ShExJ, ShExR, shape maps) over store snapshots | jena-shex |
| `crates/sparkles-fmt` | comment-preserving formatter for SPARQL, Turtle, TriG, N-Triples, N-Quads and JSON-LD | — |
| `crates/sparkles-fmt-wasm` | the formatter for the browser (WebAssembly) | — |
| `crates/sparkles-server` | axum HTTP server + `sparkles` CLI | jena-fuseki2, jena-cmds |
| `crates/sparkles-backup` | backup repositories (file system or S3): incremental, deduplicated backups, restore, lifecycle policies | Fuseki `/$/backup` (N-Quads dumps only) |
| `vendor/spargebra` | Oxigraph's SPARQL parser, vendored with fixes (`PATCHED.md`) | ARQ's JavaCC grammar |
| `ui/` | SvelteKit management / query / graph-exploration UI | jena-fuseki-ui |

## Development

`mise run ci` runs formatting checks, Clippy, every workspace test and the UI tests.
`mise run test:w3c`, `test:shacl` and `test:shex` run the conformance suites from an
Apache Jena checkout. [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) covers the toolchain,
the tasks, the suites and their environment variables, the end-to-end tests and the Nix
flake.

## License

Sparkles is licensed under the [Apache License 2.0](LICENSE). The vendored `spargebra`
keeps its MIT OR Apache-2.0 license. The licenses and notices of the crates and npm
packages that ship in the binary are in
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) and
[THIRD_PARTY_LICENSES-UI.md](THIRD_PARTY_LICENSES-UI.md).
