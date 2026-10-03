# Sparkles

Sparkles is a fast RDF, SPARQL and OWL database written in Rust. It reimplements
[Apache Jena](https://jena.apache.org/) and Fuseki, with the same protocols, semantics and
operational model, on the index and execution architecture of
[QLever](https://github.com/ad-freiburg/qlever). It ships as an embeddable library, a
Fuseki-compatible server and CLI, and a web UI for managing databases, exploring graphs and
running queries.

> [!WARNING]
> Sparkles is experimental and not yet stable. There are no releases and no stability
> guarantees. The on-disk format, HTTP API, CLI and Rust API can change in any commit,
> without notice or a migration path. Sparkles is not recommended for production or for
> data you can't regenerate. Keep backups.
>
> Much of the code, tests and documentation was written with AI models, directed and
> reviewed by the maintainer. The W3C conformance suites and differential tests are the
> main safeguard, but expect bugs. Issues and bug reports are welcome.

![The query editor with results](docs/images/query.png)

## Philosophy

1. **Jena-compatible where users can see it.** Anything that talks to Fuseki should keep
   working. Sparkles implements the SPARQL 1.1 Query, Update and Graph Store protocols,
   Fuseki's endpoint names (`/{ds}/sparql|query|update|data|get|upload`) and the `/$/`
   admin API. It follows Jena for RDF and result formats and for dataset semantics: a
   default graph, named graphs and an optional union default graph. It keeps TDB2's
   operational model of bulk loads, transactions, compaction and backups.
2. **QLever-style internals where performance matters.** Terms are dictionary-encoded as
   64-bit tagged ids, and literals such as numbers and dates are stored inline in the id.
   Indexes are fully sorted, compressed permutation files. A cost-based DP planner drives
   column-at-a-time execution.
3. **Reuse the Rust RDF ecosystem.** Sparkles doesn't rewrite parsers that already exist.
   The term model, parsers, SPARQL algebra and XSD value space come from the Oxigraph
   project's crates: `oxrdf`, `oxttl`, `oxrdfxml`, `oxjsonld`, `spargebra`, `sparesults`
   and `oxsdatatypes`. Sparkles adds the storage, planner, executor, server, reasoner and
   UI.
4. **Library first.** `crates/sparkles` is an embeddable engine with no HTTP or async
   dependencies. It is the counterpart of Jena's `core`, `arq` and `tdb2`. The server
   (`sparkles-server`, the Fuseki equivalent) and the reasoner are separate crates built on
   its public API.

## Highlights

**Storage and engine**
* MVCC snapshots with a single writer and a crash-safe WAL. Compaction writes immutable
  generations of 7 sorted, compressed permutations ([features](docs/FEATURES.md#storage-tdb2-equivalent)).
* Automatic compaction in the background when a dataset's updates grow, with writes going
  on during the build. A delta that adds no terms rewrites only the index blocks it
  touches ([API](docs/API.md#automatic-compaction)).
* Durable commit ids, point-in-time reads by commit (`?at=commit:N`), time or named
  snapshot, and diffs between any two readable commits, as JSON or RDF Patch
  ([API](docs/API.md#point-in-time-reads-and-snapshots)).
* A change feed of commits and their changes, resumable from any readable commit, with
  long polling and server-sent events ([API](docs/API.md#change-feed)).
* History queries in SPARQL and over HTTP answer when a triple was added or removed,
  which commit last changed a subject, and which values a property took, with each
  commit's time, author and message. They read a change log that outlives compactions
  ([API](docs/API.md#history-queries)).
* RDF Patch applied as one commit through Fuseki's `patch` operation, in the text and the
  binary form, with `H prev` as an optimistic concurrency check
  ([API](docs/API.md#applying-rdf-patch)).
* Commit messages, optional change digests, and Graph Store entity tags with `If-Match`
  writes checked under the writer lock ([API](docs/API.md#entity-tags-and-conditional-requests)).
* Dry runs of updates, Graph Store writes and uploads. A dry run reports the commit, the
  changes per graph, the validation result and the quota effect, and writes nothing
  ([API](docs/API.md#write-previews)).
* A read-only integrity check ([usage](docs/USAGE.md#checking-a-database)).
* Parallel bulk loading with external sort, from the W3C RDF syntaxes and from Jena's TriX,
  RDF Thrift, RDF Protobuf and RDF/JSON.
* CSV and TSV imports with a default mapping, W3C CSVW metadata or Tarql-style CONSTRUCT
  templates ([usage](docs/USAGE.md#loading-csv-and-tsv)).
* N-Quads dumps, and incremental, deduplicated backups of persistent and in-memory
  datasets to a file system or S3 ([usage](docs/USAGE.md#backup-repositories)).

**SPARQL**
* SPARQL 1.1 Query and Update, and SPARQL 1.2 / RDF 1.2. Sparkles passes the W3C suites
  in full: SPARQL 1.0 482/482, 1.1 query 328/328, 1.1 update 157/157 and 1.2 269/269.
* A cost-based DP planner over columnar operators, a result cache, and memory, row and
  work budgets per query, which a request can lower. Each result comes with its executed
  plan ([optimizations](docs/COMPARISON.md#optimizations-adopted-from-qlever)).
* Jena ARQ's statistical aggregates (`MEDIAN`, `MODE`, `STDEV`, `VARIANCE` and their
  variants) and most of its `fn:`, `afn:` and `math:` functions, checked against Jena's
  answers ([API](docs/API.md#extension-functions-and-aggregates)).
* ARQ's `LATERAL`, property path ranges such as `p{1,3}` and CONSTRUCT templates with
  `GRAPH`, which Fuseki users write ([API](docs/API.md#arq-syntax-extensions)).
* DESCRIBE as Jena's concise bounded description by default, or the symmetric form or
  the resource's own triples, with labels and limits, per dataset or per request
  ([API](docs/API.md#describe)).
* A SPARQL 1.1 Service Description per dataset ([API](docs/API.md#service-description)).
* Federated `SERVICE` queries under an outbound network policy ([usage](docs/USAGE.md#outbound-requests-service-and-load)).

**Server and CLI**
* Fuseki's endpoints, Graph Store Protocol, upload and `/$/` admin API. Sparkles adds
  endpoints for commits, schema discovery, clones, reasoning and validation ([API](docs/API.md)).
* An OpenAPI 3.1 description of the whole API at `/$/openapi.json`, kept equal to the
  server's routes by a test, for client generators and API viewers
  ([API](docs/API.md#openapi-description)).
* Stored queries with typed parameters, which clients and MCP agents run by name. Values
  are bound as terms and never spliced into the text ([API](docs/API.md#stored-queries)).
* A read-only GraphQL endpoint per dataset over a reviewed mapping schema, drafted from
  SHACL shapes or the data. Each request runs as a fixed number of SPARQL queries with the
  caller's view and budgets ([API](docs/API.md#graphql)).
* Jena's own HTTP clients, including `RDFConnectionFuseki` and its RDF Thrift, are tested
  against the server ([usage](docs/USAGE.md#fuseki-and-jena-clients)).
* A Rust client, `sparkles-client`, for Sparkles and any SPARQL endpoint. It streams
  results as `oxrdf` terms, returns the commit of each write, and retries as the server's
  `Retry-After` directs ([usage](docs/USAGE.md#rust-client)).
* A Jena-style CLI with `tdb2.*` and `arq` equivalents. The commands work on a database
  directory or on a remote server ([usage](docs/USAGE.md#command-line-tools)). File tools
  match Jena's `riot`, `qparse`, `uparse`, `rdfdiff`, `iri`, `langtag`, `rsparql`,
  `rupdate`, `rset` and `rdfpatch` ([usage](docs/USAGE.md#file-tools)).

**Reasoning**
* RDFS, OWL 2 RL and Jena's rule syntax, materialized by semi-naive forward chaining.
* Sparkles reports when inferences are stale and, if asked, re-runs them automatically.
  A re-run updates the previous materialization incrementally, so a small change takes
  milliseconds. Sparkles also checks for OWL 2 RL inconsistencies
  ([API](docs/API.md#reasoning-status-and-diagnostics)).
* The reasoner reads the default graph or chosen data and ontology graphs, and follows
  `owl:imports` to graphs of the dataset or fetches them
  ([API](docs/API.md#input-graphs-and-imports)).
* RDFS on read answers queries over the RDFS closure without materializing it, with the
  same answers as Fuseki's `--rdfs` ([API](docs/API.md#rdfs-on-read)).

**Validation**
* SHACL Core and SHACL-SPARQL, with Fuseki's `/{ds}/shacl` endpoint. Both W3C suites
  pass (98/98 and 20/20) ([API](docs/API.md#shacl-validation)). SHACL 1.2's list
  constraints are checked too.
* Shapes can be written in the SHACL Compact Syntax (SHACLC) wherever they are accepted,
  and drafted shapes can be shown in it ([API](docs/API.md#shacl-compact-syntax-shaclc)).
* ShEx 2.1 with ShExC, ShExJ, ShExR and shape maps. It passes 99.9% of the shexTest
  validation tests ([API](docs/API.md#shex-validation)).
* Write-time guards validate each commit with SHACL or ShEx before it is written. A
  write re-validates only the focus nodes it can affect
  ([API](docs/API.md#write-time-validation)).
* Shapes drafted from the data give a guard a starting point. Each constraint has a
  support threshold and a count of the instances it would exclude
  ([API](docs/API.md#drafted-shapes)).
* The schema report lists a guard's SHACL constraints per class next to the observed
  counts, and says which ones a write cannot break
  ([API](docs/API.md#constraints-layer)).
* Class profiles list the predicates the instances of each class use, a schema diff
  shows what changed between two commits, and the server keeps its schema report up to
  date from each write's changes ([API](docs/API.md#class-profiles)).

**Search**
* Full-text search through Jena's `text:query`, ranked by BM25 with Tantivy and stemmed per
  language ([API](docs/API.md#full-text-search)).
* Hybrid search that fuses a full-text and a vector ranking by reciprocal rank fusion
  ([API](docs/API.md#hybrid-text-and-vector-search)).
* Vector similarity search over `spk:vector` literals, exact or through an HNSW index that
  sees every write at once ([API](docs/API.md#vector-similarity)).
* Embeddings computed on write. A vector index can embed selected literals through an
  OpenAI-compatible endpoint, such as OpenAI, Ollama or vLLM, in the background after
  each commit, and searches can pass text ([API](docs/API.md#embeddings-on-write)).
* Path search that returns paths as solutions: the shortest, all shortest or k shortest
  paths between nodes, or every path up to a length, with optional edge weights
  ([API](docs/API.md#path-search)).
* GeoSPARQL 1.1 with a spatial index per dataset, Jena's `spatial:` and `spatialF:`
  functions, spatial joins and nearest-neighbour search ([API](docs/API.md#geosparql)).

**Formatter**
* A formatter for SPARQL, Turtle/TriG, N-Triples/N-Quads and JSON-LD. It keeps comments
  and checks its own output. It runs as `sparkles fmt`, as `POST /$/format`, as the
  `sparkles lsp` language server ([editors](docs/editors.md)) and in the browser ([usage](docs/USAGE.md#formatting)).
* A linter for SPARQL, Turtle and TriG, with rules for prefixes, variables, cartesian
  products, FILTER scope, language tags and datatypes, and safe fixes. It runs as
  `sparkles lint`, in the language server and in the query editor ([usage](docs/USAGE.md#linting)).

**Operations**
* A SvelteKit web UI, embedded in the binary. It has a query editor, results as a table,
  graph, plan or map, a resource explorer, a schema browser, vector similarity search and
  index management, validation and backups ([screenshots](#web-ui)).
* Authentication with Basic, API tokens, OIDC (UI sign-in and the provider's access
  tokens), Cloudflare Access or trusted proxies, access control per dataset, named graph
  and endpoint, protections of triples by predicate, subject class or a
  pattern on the caller, and rate limiting ([API](docs/API.md#authentication-and-access-control)).
  The server can serve HTTPS itself ([TLS](docs/USAGE.md#tls)).
* Access logs, Prometheus metrics, a readiness endpoint and OpenTelemetry traces
  ([features](docs/FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)).
* Storage quotas per dataset, and a shutdown that lets requests in flight finish within
  a grace period ([API](docs/API.md#storage-quotas)).
* An MCP server for LLM agents, over stdio or at `/$/mcp` on the server. Each call runs as
  its caller, within query budgets, and the write tool is opt-in. Each stored query is a
  tool of its own ([usage](docs/USAGE.md#mcp-server-llm-agents)).
* A Docker image with a compose file, and a Nix package with a NixOS module
  ([Docker](docs/USAGE.md#docker), [NixOS](docs/USAGE.md#deploying-on-nixos)).

[docs/FEATURES.md](docs/FEATURES.md) lists every feature and what is not there yet.

## Comparison

| Engine | What it is | Where Sparkles stands |
|---|---|---|
| [Apache Jena / Fuseki](https://jena.apache.org/) | The reference Java stack. It has ARQ, TDB2 on B+trees, Fuseki, on-the-fly inference, jena-text and GeoSPARQL. | Sparkles has the same protocols, endpoints, admin API and CLI model, on sorted columnar indexes. It is faster on every benchmark query at 10.5M triples, by a median of 78×. Reasoning is materialized, apart from RDFS on read. Sparkles writes RDF Patch and applies it through Fuseki's `patch` operation. It has ARQ's query language, its function and property function libraries, and its `cdt:` lists and maps, but no JavaScript functions, and there is no ontology API. |
| [QLever](https://github.com/ad-freiburg/qlever) | A C++ engine for billions of triples, with lazy, streaming execution. | Sparkles uses the same index and execution architecture and adds exact term identity, MVCC updates, the Graph Store Protocol, reasoning and SHACL. It is faster on all 28 benchmark queries at 10.5M triples and on all 20 WatDiv templates, and its server uses about a third more memory at 10.5M. On English DBpedia (1.24 billion triples) it loads 2.9× faster and is faster warm on all 29 queries whose answers agree ([BENCHMARKS.md](docs/BENCHMARKS.md#dbpedia-at-124-billion-triples) has the cold runs). It materializes intermediate results. |
| [Oxigraph](https://github.com/oxigraph/oxigraph) | A Rust database and toolkit on RocksDB, with Python and WebAssembly packages. | Sparkles uses Oxigraph's parsers, SPARQL parser and datatypes, with its own storage and planner. It is faster on every benchmark query at 10.5M triples, by a median of 85×. It fsyncs its writes, so Oxigraph's single-triple updates are faster. It adds reasoning, validation, search, authentication and a UI. It has Rust and Python APIs and no WebAssembly build. |
| [Fluree](https://github.com/fluree/db) | A versioned, permissioned ledger with clustering, licensed under BUSL-1.1. JSON-LD is its main interface. | Sparkles passes the W3C SPARQL suites in full and is compatible with Fuseki. It has point-in-time reads, snapshots, diffs, history queries and protections of triples in its configuration, but no branches, policies stored in the data or clustering. It is faster on every benchmark query that Fluree completes at 10.5M triples, by a median of 9.8×, but only by 1–5% on a few counts and point lookups. |

[docs/COMPARISON.md](docs/COMPARISON.md) lists the feature gaps per engine, the places
where Sparkles departs from Jena and QLever on purpose, and the optimizations it adopted
from QLever.

## Performance

The benchmarks run hyperfine over HTTP against Jena/Fuseki, QLever, Fluree and Oxigraph.
Every result cache is off and each engine runs alone. Before timing, the benchmark checks
that all engines return the same answers. These numbers come from one machine, an Intel
i5-13500 with 15 GiB of usable RAM, on 2026-10-02, with Sparkles at commit `4995963`. At
10.5M triples:

| | Sparkles | Best of the others |
|---|---|---|
| Bulk load | **6.5 s** | QLever 9.3 s |
| Queries (28) | Fastest on all 28 | Fluree is within 5% on `count-all`, `distinct-obj` and `star-lookup`. |
| WatDiv, 20 templates (11M triples) | **4.95 ms** geometric mean, fastest on all 20 | Fluree 7.39 ms |
| Update latency (1 triple) | 4.9 ms, fsynced | **Oxigraph 3.7 ms**, not fsynced |
| Throughput, 16 clients | **242 q/s** | QLever 92 q/s |
| Server memory after the run | 892 MiB, of which 379 MiB is block cache | **QLever 676 MiB** |

Sparkles trades memory for speed by default. Each dataset gets a 1 GiB decoded-block
cache and a 512 MiB result cache, which was off in these runs, and every operator
materializes its result. With the block cache turned off, the server's RSS after the
10.5M run falls to 319 MiB, below QLever's, but throughput halves to 128 q/s and some
queries run up to 14× slower. A 256 MiB cache is as fast as the default on this data and
uses 161 MiB less ([the memory tradeoff](docs/BENCHMARKS.md#memory-and-the-speed-it-buys)).

On English DBpedia, 1.24 billion triples on the same machine and with a later build,
Sparkles loads the data in 584 s against QLever's 1,674 s, with a peak RSS of 6.6 GiB
against 11.1 GiB. Warm, it is faster on all 29 queries whose answers both engines agree
on. Its server's RSS after the queries was 5.6 GiB against QLever's 1.4 GiB. Much of
that is memory-mapped index pages the kernel can drop, plus the 1 GiB block cache.
Cold, QLever was faster on 17 of 31 queries, most of them point lookups and small
joins. Later changes to how a cold server reads its files made Sparkles faster on 7 of
the 8 cold lookups measured again
([DBpedia at 1.24 billion triples](docs/BENCHMARKS.md#dbpedia-at-124-billion-triples)).
[docs/BENCHMARKS.md](docs/BENCHMARKS.md) has every number for all three data sizes,
WatDiv, full-text search, cold starts and mixed read/write load, the memory tradeoff, and
every query where Sparkles loses or ties. The DBpedia harness runs at any scale from 10M
triples up with `mise run bench:billion [scale]`
([docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#benchmark-scripts)).

## Getting started

Build Sparkles from source or run it with Nix. To embed the web UI in the binary, build
the UI first:

```sh
pnpm -C ui install && pnpm -C ui build        # optional: the web UI
cargo install --path crates/sparkles-server   # installs the `sparkles` binary
# or: nix run github:kclejeune/sparkles -- serve --data ./data
```

Docker builds the UI and the binary in one step and serves them on the host's
`127.0.0.1:3030`, with the data in a named volume. [docs/USAGE.md](docs/USAGE.md#docker)
explains the image, authentication and backups.

```sh
docker compose up --build -d
```

Load a file into a new database and serve it on `127.0.0.1:3030`:

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

Query and update the data:

```sh
curl localhost:3030/books/sparql -H 'Accept: text/csv' \
  --data-urlencode 'query=SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 10'
sparkles query --server http://localhost:3030 --dataset books 'SELECT (COUNT(*) AS ?n) { ?s ?p ?o }'
sparkles query --data books.ttl --query q.rq   # files in memory, no server
curl localhost:3030/books/update \
  --data-urlencode 'update=INSERT DATA { <http://example.org/b1> <http://purl.org/dc/terms/title> "Dune" }'
```

The UI is at <http://localhost:3030/ui/>. `sparkles --help` and `sparkles help COMMAND`
describe every command and flag. [docs/USAGE.md](docs/USAGE.md) covers running and
operating the server, including network exposure, budgets, backups, Docker and NixOS.

## Command line

These commands work on a database directory (`--loc`) that no server has open:

```sh
sparkles load    --loc db data/*.ttl.gz       # parallel bulk load
sparkles query   --loc db 'SELECT ...'        # --results text|json|xml|csv|tsv, --explain, --time
sparkles dump    --loc db --out dump.nq.zst   # syntax and compression by extension
sparkles compact --loc db                     # merge updates into a new generation
sparkles backup  create --loc db --repo local # incremental backup to a repository
sparkles check   --loc db                     # read-only integrity check
sparkles fmt     --check queries/ shapes/     # SPARQL, Turtle, TriG, N-Triples, N-Quads, JSON-LD
```

| Command | What it does |
|---|---|
| `serve` | Run the SPARQL server with the web UI. |
| `load`, `query`, `update`, `patch`, `dump` | Bulk load, query, update and apply RDF Patch, locally or on a `--server`. `dump` exports N-Quads. |
| `csv` | Convert CSV and TSV tables to RDF, or print the CSVW metadata of the default mapping. `load` maps and loads them directly. |
| `compact`, `compaction`, `clone`, `stats`, `log`, `check`, `vocab-index` | Merge updates, set a dataset's automatic compaction, copy a dataset, show statistics or the commit history, verify a database, and add the sparse vocabulary index to a database built before it existed. |
| `quota`, `describe-settings` | Set a dataset's storage quota, and choose how DESCRIBE describes a resource. |
| `snapshot`, `diff`, `history` | Manage named snapshots, pin schedules, history retention and the commit catalog's horizon, show the quads added and removed between two commits, also as RDF Patch, and list the recorded changes of a subject, predicate or object across commits. |
| `backup`, `repo` | Write N-Quads dumps, and manage backup repositories on a file system or S3, restores and policies. |
| `infer` | Materialize RDFS, OWL 2 RL or Jena rules. Report staleness and check for inconsistencies. |
| `shacl`, `shex`, `validation` | Validate with SHACL or ShEx, and configure write-time guards. |
| `schema` | List classes and predicates with exact counts and their declarations, profile the classes, compare two commits' schemas, or draft shapes from the data. |
| `queries` | Store, list and run parameterized queries. |
| `graphql` | Run a GraphQL document, and print, install, delete or draft the mapping schema. |
| `text-index`, `vector`, `geo-index` | Manage the full-text, vector and spatial indexes. |
| `auth` | Hash passwords, manage API tokens and sign in for remote commands (`auth login`). |
| `mcp` | Run the MCP server for LLM agents over stdio. `serve --mcp` serves it over HTTP. |
| `fmt`, `lint`, `lsp` | Run the formatter, the linter or their language server. |
| `convert` (`riot`), `compare` (`rdfdiff`), `qparse`, `uparse`, `iri`, `langtag` | Convert, validate and count RDF files, compare them up to blank-node isomorphism, print a query's algebra or plan, and check IRIs and language tags. |
| `rsparql`, `rupdate`, `rset` | Query and update any SPARQL endpoint, and convert result sets. |
| `rdfpatch` | Print the rows of RDF Patch files and count them. |
| `fuseki-config` | Convert a Fuseki configuration (`config.ttl`, `shiro.ini`) into Sparkles settings, or check what converts. `serve --fuseki-config` starts from one directly. |
| `completions`, `man`, `openapi` | Print shell completions for bash, zsh, fish, elvish or PowerShell, write man pages, or print the OpenAPI description of the HTTP API. |

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

Besides `Dataset`, the library has a fluent query builder (`sparkles::querybuilder`),
term-level graph access and transactions. The reasoner and the SHACL and ShEx validators
are separate crates. [docs/USAGE.md](docs/USAGE.md#embedding-the-library) maps each of
them to its Jena equivalent.

The same engine is a Python package, built from `crates/sparkles-py` with
`mise run py:build`. Its API follows pyoxigraph's and accepts rdflib terms. It also
registers an rdflib store plugin, so `rdflib.Graph("Sparkles")` keeps its triples in
Sparkles and runs SPARQL in its engine. A GitHub Actions workflow builds and tests the
wheels for Linux, macOS and Windows.

```python
from sparkles import Dataset

with Dataset("mydb") as ds:                      # or Dataset() in memory
    ds.load(path="data.ttl.gz")
    for row in ds.query("SELECT ?s ?name WHERE { ?s <http://xmlns.com/foaf/0.1/name> ?name }"):
        print(row["s"], row["name"].value)
```

[docs/USAGE.md](docs/USAGE.md#python) covers the Python API.

A remote server is reached from Rust with `crates/sparkles-client`, which has Jena's
`RDFConnection` operations, async on tokio or blocking. It also works against Fuseki,
QLever, Oxigraph and Wikidata.

```rust
use sparkles_client::Client;

let ds = Client::new("http://localhost:3030")?.dataset("ds");
let receipt = ds.update("INSERT DATA { <urn:a> <urn:p> 1 }").await?;   // commit in receipt.commit_seq
let mut rows = ds.select("SELECT * { ?s ?p ?o }").await?;
while let Some(row) = rows.next().await {
    println!("{:?}", row?.get("s"));
}
```

[docs/USAGE.md](docs/USAGE.md#rust-client) covers the client.

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
| [docs/USAGE.md](docs/USAGE.md) | Running the server and CLI, with options, formatting, backups, outbound requests, integrity checks, MCP, embedding, the Python package, the Rust client, Docker and NixOS. |
| [docs/API.md](docs/API.md) | The HTTP API: Fuseki's endpoints and the `/$/` extensions. |
| [docs/openapi.json](docs/openapi.json) | The OpenAPI 3.1 description of the HTTP API, as the server serves it at `/$/openapi.json`. |
| [docs/COMPARISON.md](docs/COMPARISON.md) | How Sparkles compares with Jena/Fuseki, QLever, Fluree and Oxigraph, where it departs from Jena and QLever on purpose, and the optimizations it adopted from QLever. |
| [docs/BENCHMARKS.md](docs/BENCHMARKS.md) | Measured performance, against the other engines and on its own. |
| [docs/editors.md](docs/editors.md) | Formatter and language-server setups for editors. |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Building, mise tasks, tests, Nix and third-party licenses. |
| [Design specs](docs/specs/README.md) | Why each feature is built the way it is. |
| [docs/AUDIT.md](docs/AUDIT.md) | The Jena and QLever audits, what Sparkles reuses from Oxigraph, and why it is written in Rust. |
| [ui/README.md](ui/README.md) | Developing the web UI. |
| [vendor/spargebra/PATCHED.md](vendor/spargebra/PATCHED.md) | The fixes to the vendored SPARQL parser. |

## Project layout

| Path | Role | Jena analogue |
|---|---|---|
| `crates/sparkles` | Ids, vocabulary, permutation index, bulk builder, store (MVCC and WAL), SPARQL engine and RDF I/O | jena-core, jena-arq, jena-tdb2, jena-db, jena-querybuilder, jena-rdfconnection (in-process) |
| `crates/sparkles-reasoner` | RDFS, OWL 2 RL and Jena rules, materialized into `urn:x-sparkles:inferred` by semi-naive forward chaining | jena-core `reasoner` |
| `crates/sparkles-shacl` | SHACL Core and SHACL-SPARQL validation over store snapshots | jena-shacl |
| `crates/sparkles-shex` | ShEx 2.1 validation over store snapshots, with ShExC, ShExJ and ShExR schemas and shape maps | jena-shex |
| `crates/sparkles-fmt` | The comment-preserving formatter for SPARQL, Turtle, TriG, N-Triples, N-Quads and JSON-LD | — |
| `crates/sparkles-fmt-wasm` | The formatter, compiled to WebAssembly for the browser | — |
| `crates/sparkles-server` | The axum HTTP server and the `sparkles` CLI | jena-fuseki2, jena-cmds |
| `crates/sparkles-backup` | Backup repositories on a file system or S3, with incremental, deduplicated backups, restore and lifecycle policies | Fuseki `/$/backup` (N-Quads dumps only) |
| `crates/sparkles-client` | The Rust client of remote Sparkles servers and other SPARQL endpoints, async or blocking | jena-rdfconnection (remote), `RDFLinkHTTP` |
| `crates/sparkles-graphql` | The read-only GraphQL adapter, with the mapping schema, schema drafts and the compilation of requests to SPARQL algebra | — |
| `crates/sparkles-py` | The Python package, built with PyO3 and maturin in its own cargo workspace | — |
| `vendor/spargebra` | Oxigraph's SPARQL parser, vendored with fixes (`PATCHED.md`) | ARQ's JavaCC grammar |
| `ui/` | The SvelteKit UI for management, queries and graph exploration | jena-fuseki-ui |

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
