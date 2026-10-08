# Sparkles

Sparkles is a fast RDF, SPARQL and OWL database written in Rust. It reimplements
[Apache Jena](https://jena.apache.org/) and Fuseki, with their protocols, semantics and
operational model, on the index and execution architecture of
[QLever](https://github.com/ad-freiburg/qlever). It ships as an embeddable library with
Rust, Python, JVM and JavaScript APIs, a Fuseki-compatible server and CLI, and a web UI.

> [!WARNING]
> Sparkles is experimental. There are no releases, and the on-disk format, HTTP API, CLI
> and library APIs can change in any commit without a migration path. Don't use it for
> data you can't regenerate.
>
> Much of the code, tests and documentation was written with AI models, directed and
> reviewed by the maintainer. The W3C conformance suites and differential tests are the
> main safeguard, but expect bugs. Issues and bug reports are welcome.

![The query editor with results](docs/images/query.png)

## Philosophy

1. **Jena-compatible where users can see it.** Anything that talks to Fuseki should keep
   working. Sparkles implements the SPARQL 1.1 Query, Update and Graph Store protocols,
   Fuseki's endpoints and `/$/` admin API, Jena's formats and dataset semantics, and
   TDB2's model of bulk loads, transactions, compaction and backups.
2. **QLever-style internals where performance matters.** Terms are 64-bit ids with
   numbers and dates stored inline, indexes are sorted and compressed permutations, and
   a cost-based planner drives column-at-a-time execution. Sparkles adds MVCC
   transactions, a write-ahead log and online compaction.
3. **Reuse the Rust RDF ecosystem.** The term model, parsers, SPARQL algebra and XSD
   values come from the Oxigraph project's crates. Sparkles adds the storage, planner,
   executor, server, reasoner and UI.
4. **Library first.** The engine is a library with no HTTP server. The server, the CLI
   and the Python, JVM and JavaScript bindings are built on its public API, so an embedded program
   gets the same features as the server.

## Features

**Storage and history**
* MVCC snapshots with a single writer, a crash-safe WAL, and automatic background
  compaction that rewrites only the index blocks a change touches.
* Point-in-time reads (`?at=commit:N`, a time or a named snapshot), diffs as JSON or RDF
  Patch, a resumable change feed with server-sent events, and history queries that ask
  when a triple was added or which values a property took
  ([API](docs/API.md#point-in-time-reads-and-snapshots)).
* Branches that share their upstream's index until they compact, so creating one writes
  a few kilobytes, with fast-forward, squash, replayed and three-way merges, reverts and
  cherry-picks, renames, conflict resolution on the web UI's merge page, and a commit
  graph of every branch. In-memory datasets have isolated branches too
  ([API](docs/API.md#branches-and-merges)).
* RDF Patch applied as one commit, entity tags with `If-Match` writes, commit messages,
  and dry runs of any write that report its changes, validation and quota effect
  ([API](docs/API.md#write-previews)).
* Parallel bulk loading from every W3C syntax and Jena's TriX, RDF Thrift, RDF Protobuf
  and RDF/JSON, CSV imports through CSVW or Tarql-style templates, and incremental,
  deduplicated backups to a file system or S3. A selected branch can be backed up and
  restored as an independent dataset ([usage](docs/USAGE.md#backup-repositories)).

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

**Command line and tooling**
* A `sparkles` CLI with equivalents of Jena's `tdb2.*` and `arq` commands. They work on
  a database directory or on a remote server, and cover loads, dumps, backups, history,
  reasoning, validation and indexes ([usage](docs/USAGE.md#command-line-tools)).
* Jena's file tools: `convert` (`riot`) to validate and convert RDF, `compare`
  (`rdfdiff`), `qparse`, `uparse`, `rsparql`, `rupdate`, `rset`, `rdfpatch`, `iri` and
  `langtag` ([usage](docs/USAGE.md#file-tools)).
* A formatter for SPARQL, Turtle, TriG, N-Triples, N-Quads and JSON-LD that keeps
  comments and checks its own output, and a linter with safe fixes
  ([formatting](docs/USAGE.md#formatting), [linting](docs/USAGE.md#linting)).
* `sparkles lsp`, a language server that brings the formatter and linter to editors
  ([editors](docs/editors.md)), and shell completions and man pages.

**Libraries and bindings**
* A Rust library with the whole engine. Its `Dataset` API covers queries, updates,
  transactions and loads, and its handles cover snapshots, history, indexes, reasoning,
  validation and backups ([usage](docs/USAGE.md#embedding-the-library)).
* A Python package with a pyoxigraph-style API and an rdflib store plugin
  ([usage](docs/USAGE.md#python)).
* A JVM library, `sparkles-jena`, that gives Jena programs a `DatasetGraph` backed by
  Sparkles, so TDB2 code runs on it with a one-line change
  ([usage](docs/USAGE.md#jvm-apache-jena)).
* Node.js and TypeScript packages with an embedded engine, RDF/JS terms, asynchronous
  result streams and transactions, plus a remote client with generated API types
  ([usage](docs/USAGE.md#javascript-and-typescript)).
* A Rust client for Sparkles and any SPARQL endpoint ([usage](docs/USAGE.md#rust-client)).

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
* A web UI embedded in the binary, Prometheus metrics and OpenTelemetry traces.
* A Docker image and compose file, and a Nix package with a NixOS module
  ([Docker](docs/USAGE.md#docker), [NixOS](docs/USAGE.md#deploying-on-nixos)).

[docs/FEATURES.md](docs/FEATURES.md) lists every feature and what is not there yet.

## Comparison

| Engine | Where Sparkles stands |
|---|---|
| [Apache Jena / Fuseki](https://jena.apache.org/) | The same protocols, endpoints, admin API, CLI model and ARQ extensions, on sorted columnar indexes. Faster on every query Fuseki completes at 10.5M triples, by a median of 79×. Reasoning is materialized, apart from RDFS on read, and there is no ontology API or JavaScript functions. |
| [QLever](https://github.com/ad-freiburg/qlever) | The same index and execution architecture, plus exact term identity, MVCC updates, the Graph Store Protocol, reasoning and validation. Faster than QLever on all 28 benchmark queries and all 20 WatDiv templates at 10.5M triples, using about 29% more server memory. Eager execution is the default; optional streaming uses bounded batches and budgets growing operator state. |
| [Oxigraph](https://github.com/oxigraph/oxigraph) | Sparkles uses Oxigraph's parsers and SPARQL parser with its own storage and planner. Faster on every benchmark query at 10.5M triples, by a median of 85×. Sparkles synchronizes its writes; Oxigraph's default acknowledgments do not synchronize its WAL. [Write throughput](docs/BENCHMARKS.md#http-write-snapshot) depends on batch size, client count and vocabulary growth. There is no WebAssembly build. |
| [Fluree](https://github.com/fluree/db) | Both keep history and branches. Sparkles adds full W3C SPARQL conformance and Fuseki compatibility, and has no policies stored in the data or clustering. Faster on 26 of 27 benchmark queries Fluree completes at 10.5M triples, by a median of 9.9×; the remaining lookup is within 10%. |

[docs/COMPARISON.md](docs/COMPARISON.md) lists the gaps per engine and where Sparkles
departs from Jena and QLever on purpose.

## Performance

These measurements use one Intel i5-13500 with 15 GiB of RAM and result caches off.
Sparkles ran on 2026-10-08 with a release build at commit `ff7acde4`. The other engines'
values are from 2026-10-03, with the same data, queries and method, so they are dated
references rather than new interleaved runs. At 10.5M triples:

| | Sparkles | Best of the others |
|---|---|---|
| Bulk load | **3.9 s** | QLever 9.7 s |
| Queries (28) | Fastest on 27 of 28 | The remaining lookup difference is within 10% |
| WatDiv, 20 templates | **4.84 ms** geometric mean | Fluree 7.41 ms |
| Update latency (1 triple) | 4.88 ms, durable | **QLever 4.19 ms**, in memory only |
| Throughput, 16 clients | **241 q/s** | QLever 92 q/s |
| Server memory after the run | 840 MiB, 379 MiB of it block cache | **QLever 653 MiB** |

Sparkles trades memory for speed by default, with a 1 GiB block cache per dataset. On
English DBpedia (1.24 billion triples), the 2026-10-08 run loaded the data in 1,248 s,
against QLever's 1,674 s and Fluree's 2,919 s. Sparkles was faster than QLever on all 29
agreeing warm queries and on 18 of 31 cold ones.

Those DBpedia results regressed against the run of 2026-10-03 at commit `98a75c1a`,
which loaded the same data in 596 s and was faster than QLever on 26 of 31 cold queries.
The new load used about the same CPU time while keeping fewer cores busy. That is
consistent with I/O stalls, but the cause has not been established and is under
investigation ([details](docs/BENCHMARKS.md#changes-against-the-run-of-2026-10-03)).
[docs/BENCHMARKS.md](docs/BENCHMARKS.md) has every number, the memory tradeoff and every
query where Sparkles loses or ties.

On the 1.05M-triple HTTP write workload, default durable writes reach about
1,000 single-triple transactions/s or 54,000–59,000 effective changes/s in
100-change batches with one client. With four clients issuing 50,000 single-quad
requests, opt-in group commit reaches 529–543 acknowledged transactions/s versus
221–232 by default, while preserving durable acknowledgments.
[The write measurements](docs/BENCHMARKS.md#http-write-snapshot) include workload
details, repeat variability and a separate Oxigraph reference.

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

The UI is at <http://localhost:3030/ui/>. [docs/USAGE.md](docs/USAGE.md) covers running
and operating the server.

## Command line

These commands work on a database directory (`--loc`) that no server has open. Many also
take `--server URL` to work on a running server instead.

```sh
sparkles load    --loc db data/*.ttl.gz       # parallel bulk load
sparkles query   --loc db 'SELECT ...'        # --results text|json|csv|tsv, --explain, --time
sparkles dump    --loc db --out dump.nq.zst   # syntax and compression by extension
sparkles compact --loc db                     # merge updates into a new generation
sparkles backup  create --loc db --repo local # incremental backup to a repository
sparkles check   --loc db                     # read-only integrity check
sparkles fmt     --check queries/ shapes/     # format SPARQL and RDF files
```

| Commands | What they do |
|---|---|
| `serve` | Run the server with the web UI. |
| `load`, `query`, `update`, `patch`, `dump`, `csv` | Load, query, update, apply RDF Patch, export, and import CSV and TSV. |
| `compact`, `clone`, `stats`, `log`, `check` | Merge updates, copy a dataset, show statistics and the commit history, and verify a database. |
| `snapshot`, `diff`, `history`, `branch`, `merge`, `revert`, `cherry-pick` | Named snapshots and retention, diffs between commits, the changes of a term across commits, and branches. |
| `backup`, `repo` | Back up to a file system or S3, restore, and run backup policies. |
| `infer`, `shacl`, `shex`, `validation`, `schema` | Reason, validate, set write-time guards, and report or draft the schema. |
| `text-index`, `vector`, `geo-index`, `queries`, `graphql` | Manage the search indexes, stored queries and the GraphQL schema. |
| `convert`, `compare`, `qparse`, `uparse`, `rsparql`, `rupdate`, `rset` | Jena's file and remote tools: `riot`, `rdfdiff` and the rest. |
| `fmt`, `lint`, `lsp`, `mcp`, `auth`, `config` | Format and lint, the language server, the MCP server, sign-in and tokens, and Fuseki configuration import. |

`sparkles help COMMAND` describes each command and its flags, and
[docs/USAGE.md](docs/USAGE.md#command-line-tools) describes them in full.

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

The Python package ([usage](docs/USAGE.md#python)) and the JVM library
([usage](docs/USAGE.md#jvm-apache-jena)) run the same engine:

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
| `crates/sparkles-node`, `js/` | The Node-API addon, RDF/JS contracts and TypeScript engine and remote client packages | — |
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
