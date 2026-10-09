# Sparkles compared

This page compares Sparkles with Apache Jena/Fuseki, QLever, Fluree and Oxigraph. It lists
what each engine has that Sparkles lacks, the places where Sparkles departs from Jena and
QLever on purpose, and the QLever techniques it adopted. Benchmark results, including the
queries Sparkles loses, are in [BENCHMARKS.md](BENCHMARKS.md#where-sparkles-loses). The
full feature list is in [FEATURES.md](FEATURES.md).

* [Feature gaps by engine](#feature-gaps-by-engine)
  * [vs. Apache Jena / Fuseki](#vs-apache-jena--fuseki)
  * [vs. QLever](#vs-qlever)
  * [vs. Fluree](#vs-fluree)
  * [vs. Oxigraph](#vs-oxigraph)
* [Divergences from Jena / QLever (decisions)](#divergences-from-jena--qlever-decisions)
  * [General](#general)
  * [Server defaults](#server-defaults)
  * [GeoSPARQL](#geosparql)
  * [Out of scope for v1](#out-of-scope-for-v1)
* [Optimizations adopted from QLever](#optimizations-adopted-from-qlever)
* [Further scan and filter optimizations](#further-scan-and-filter-optimizations)

## Feature gaps by engine

### vs. Apache Jena / Fuseki

| Area | Jena / Fuseki | Sparkles |
|---|---|---|
| Full-text search | jena-text (Lucene), `text:query` | `text:query` with Lucene's query syntax, Jena's highlighting and stemming per language over string literals (Tantivy, BM25), updated on commit. No multi-field entity documents. |
| Spatial | GeoSPARQL 1.0/1.1: `geof:` and `spatialF:` functions, `spatial:` property functions over a spatial index, query rewrite of the topological properties, RDFS entailment of the geometry hierarchy, GML and KML literals, EPSG CRSs through Apache SIS | The GeoSPARQL 1.1 `geof:` functions over WKT, GeoJSON, GML and KML literals. Jena's `spatial:` property functions, with constant or variable arguments, and its `spatialF:` filter functions. A per-dataset spatial index serves FILTERs, property functions, spatial joins and nearest-neighbour ORDER BY. Query rewrite is off by default. `--vocab geosparql` adds RDFS entailment of the geometry hierarchy and types geometries from their literals. The CRSs are the built-in ones, the 120 UTM zones, projected CRSs that the operator defines with proj4 strings, and in the server the projected EPSG codes of an EPSG-derived proj4 table. There are no datum-shift grids ([AUDIT.md](AUDIT.md#5-explicit-non-goals-for-v1) §5). |
| Shape languages | ShEx (jena-shex), and SHACL Compact Syntax (SHACLC), read and written as an RDF syntax | SHACL, and ShEx 2.1 (ShExC, ShExJ, ShExR, SPARQL selectors). SHACLC is read wherever shapes are accepted and written for drafted shapes. It is a shapes syntax only, so data endpoints do not take it. No ShEx 2.2. |
| Inference | On-the-fly `InfModel`, backward and hybrid rules (LP engine), OWL Micro/Mini/Full, and RDFS on read for a dataset (Fuseki `--rdfs`, `ja:DatasetRDFS`). `OntModel` resolves `owl:imports`. | Forward materialization (RDFS, an OWL 2 RL subset, Jena forward rules) from the default graph or chosen data and ontology graphs, following `owl:imports` to graphs of the dataset or fetching them with Jena-style location mapping. RDFS on read per dataset gives the same answers as Jena's (`serve --rdfs`, `/$/rdfs/{ds}`). A re-run after writes, by hand or automatic, updates the inferences incrementally when the rules allow it, and stale inferences are reported. Inconsistency checks cover the OWL 2 RL `false` rules except `dt-not-type`, which is not full consistency checking. There are no backward rules and no ontology API. |
| Ontology API | jena-ontapi `OntModel` | ✗ (triples and SPARQL only) |
| Parameterized queries | `ParameterizedSparqlString` and query substitution in Java. Fuseki stores no queries. | Stored queries per dataset (`/$/queries/{ds}`, `sparkles queries`), run by name at `/{ds}/queries/{name}` with typed parameters bound as terms through initial bindings. They are versioned and offered as MCP tools. The query builder's `set_var` covers the Java use. |
| Linting | ✗ (`qparse` and `riot` report syntax errors only) | `sparkles lint` checks SPARQL, Turtle and TriG for undefined and unused prefixes, unused and unbound variables, cartesian products, FILTER scope, language tags and datatypes, and applies the safe fixes. The language server and the query editor show the same findings ([X04](specs/X04-linter.md)). |
| Shape drafting | ✗ | SHACL shapes and ShEx schemas drafted from the data, with a support threshold per constraint and the number of instances each constraint excludes (`/$/schema/{ds}/shapes`). |
| SPARQL extensions | Property functions (`list:member`, `apf:*`), `LET`, `LATERAL`, path ranges `{n,m}`, CONSTRUCT templates with `GRAPH`, custom aggregates (`MEDIAN`, `MODE`, `STDEV`, `VARIANCE`, `FOLD`), `cdt:` list/map literals, JavaScript functions, the full `afn:`/`fn:` library | ARQ's statistical aggregates (`MEDIAN`, `MODE`, `STDEV`, `STDEV_SAMP`, `STDEV_POP`, `VARIANCE`, `VAR_SAMP`, `VAR_POP`) as keywords, by IRI and through `AGG`, with ARQ's results. The `fn:`, `afn:` and `math:` library, with `afn:sprintf` and `fn:format-number` formatting as Java does ([API.md](API.md#extension-functions-and-aggregates)). The `list:` and `apf:` property functions, which read the variables bound before them as ARQ does ([API.md](API.md#arqs-property-functions)). `LATERAL`, `LET`, `SEMIJOIN`, `ANTIJOIN`, path ranges, `distinct(…)`, `multi(…)`, `:p^:q`, CONSTRUCT with `GRAPH`, and `cdt:` lists and maps with `FOLD` and `UNFOLD`, with ARQ's results ([API.md](API.md#arq-syntax-extensions)). Paths as solutions through `SERVICE path:search`, where Jena's property paths answer reachability only. `rdfs:member` and the container functions give the members of `rdf:Bag`, `rdf:Seq` and `rdf:Alt` as ARQ does. No JavaScript functions, `afn:context` or Leviathan functions. |
| Service description | Fuseki answers a GET of the query endpoint without a query with `404` | A SPARQL 1.1 Service Description of the query and update services, with the extension functions and aggregates, the entailment regime and the default dataset, filtered by the caller's grants ([API.md](API.md#service-description)). |
| Extension points | Registries for functions, property functions, aggregates, SERVICE executors and DESCRIBE handlers, algebra transforms and query engine factories | None are public. Extension functions are built in. DESCRIBE has three built-in modes in place of handlers: the concise bounded description, its symmetric form and the resource's own triples, with labels, reifiers and limits as options. A dataset setting or a request parameter picks one ([API.md](API.md#describe)). |
| SPARQL parser | JavaCC grammar | `spargebra` 0.4.7, vendored with fixes for the W3C tests it failed ([`vendor/spargebra/PATCHED.md`](../vendor/spargebra/PATCHED.md)) |
| RDF formats | RDF Thrift, RDF Protobuf, TriX, RDF/JSON | Turtle, N-Triples, N-Quads, TriG, RDF/XML and JSON-LD everywhere. The server, `sparkles load` and `sparkles convert` also read and write TriX, RDF Thrift, RDF Protobuf and RDF/JSON, and the server writes SELECT results in SPARQL Results Thrift, which is what `RDFConnectionFuseki` uses. The library's loader and the Python bindings take none of Jena's own formats. |
| Tabular data | The core distribution has no CSV reader. Tables are converted to RDF first with a separate tool, such as Tarql or a CSVW processor. | `sparkles load`, `sparkles csv` and uploads map CSV and TSV tables to triples with a default mapping, with W3C CSVW metadata (datatypes, null values, lists and URI templates in its minimal RDF mode), or with Tarql-style CONSTRUCT templates run by the engine. No RML or R2RML mappings, and no CSVW standard mode ([C05](specs/C05-tabular-imports.md)). |
| Change logs | RDF Patch (jena-rdfpatch): writing, applying, capturing a dataset's changes and logging them to rotating files. Fuseki's `patch` operation applies one. | RDF Patch output, as text and as RDF Thrift, for diffs between commits and for a change feed with long polling and server-sent events. Fuseki's `patch` operation at `/{ds}/patch` and `/{ds}` applies a patch in either form as one commit, where Fuseki takes the text form only. A `prev` header that names a commit of the dataset is a precondition. There are no patch logs or file rotation. |
| Fuseki operations | Shiro authentication, per-graph access control (fuseki-access), Prometheus `/$/metrics`, assembler (`config.ttl`) service definitions, `/$/validate/*`, prefix endpoints | Basic and Bearer tokens, OIDC sign-in for the UI, the OIDC provider's access tokens, Cloudflare Access assertions and trusted proxy headers, with per-dataset access levels. Native HTTPS through rustls with `--tls-cert`, where Fuseki configures Jetty with `--https`. Grants can be limited to some named graphs, for reading and writing, and to some endpoints, which covers fuseki-access and endpoint `allowedUsers`. Protections hide triples by predicate, by subject class or by a SPARQL pattern with the caller bound, which covers what the retired jena-permissions module's `SecurityEvaluator` decided per triple, as sets computed once per commit. Prometheus `/$/metrics` with Sparkles metric names, plus Fuseki's `fuseki_requests*` names with `--metrics-fuseki-names`, and no JVM metrics. Datasets are configured by CLI flags and the admin API. Prefixes through `/{ds}/prefixes`. |
| SERVICE | Bulk, correlated and cached SERVICE with `loop:`, `bulk:` and `cache:` options, and `urn:x-arq:self` (serviceenhancer). Its cache serves overlapping LIMIT and OFFSET ranges and learns each endpoint's result limit. | The same options with Jena's results ([API.md](API.md#service-options-loop-bulk-and-cache)). Bulk requests go out as one VALUES block when the pattern allows it and as Jena's UNION otherwise. A response cut short is asked for again one input at a time. The cache keeps each caller's entries apart and has no slice-aware pages or management functions ([G10](specs/G10-service-enhancer.md)). |
| Transactions over HTTP | — | — (as in Fuseki, one request is one transaction) |
| Backups | `/$/backup/{ds}`: a gzipped N-Quads dump of the whole dataset in the server's directory, restored by loading it into a new dataset | The same dumps, zstd by default (`?compression=gzip` gives Fuseki's `.nq.gz`; brotli and LZ4 also work). Also backup repositories on a file system or S3: incremental, deduplicated backups that restore to a ready database without a reload, with verification, schedules and retention. |
| Admin API | `/$/datasets` (also from an uploaded assembler, and `?state=offline`), `/$/server`, `/$/backup` with its alias `/$/backups`, `/$/backups-list`, `/$/compact` with `deleteOld`, `/$/tasks`, `/$/stats`, `/$/ping`, `/$/metrics` and `/$/validate/*` | The same routes, with Fuseki's JSON members in dataset, task and statistics responses. `POST /$/backups/{ds}` without a JSON body is Fuseki's dump, and with one it is a backup into a repository. Assembler bodies are read for the part that maps to a Sparkles dataset, and the rest is refused by name. `deleteOld=false` is refused, as compaction always removes the old generation, and the offline state is not persisted. `/$/validate/query` gives the algebra but not its quad and optimized forms. Sparkles adds routes for commits, snapshots, schema, reasoning, validation, indexes, quotas and clones. |
| Graph Store naming | Indirect (`?graph=`, with `default` and `union`) and direct naming, where the request URL is the graph, configured per service | Both, with `?graph=union` read-only as in Fuseki. Direct naming is on for every dataset or none, with `serve --gsp-direct-naming`. |
| Fuseki modules | Fuseki Main builds a server from modules (admin, UI, Shiro, Prometheus, graph access, GeoSPARQL index tasks), found through Java's `ServiceLoader` or `--modules` | The same functions are compiled in and chosen by cargo features and flags. There is no plugin interface. |
| Command-line tools | `riot` (parse, validate, convert), `arq`/`sparql`, `qparse`, `uparse`, `update`, `rsparql`, `rupdate`, `rset`, `rdfdiff`, `infer`, `shacl`, `shex`, `rdfpatch`, `iri`, `langtag`, `schemagen` and `tdb2.*` | `load`, `query`, `update`, `dump`, `compact`, `backup`, `stats`, `infer`, `shacl` and `shex` cover `tdb2.*`, `arq`, `shacl` and `shex`. `convert` (alias `riot`), `qparse`, `uparse`, `compare` (aliases `rdfdiff` and `rdfcompare`), `iri`, `langtag`, `rsparql`, `rupdate`, `rset` and `rdfpatch` cover the rest, and `patch` applies an RDF Patch to a database. `qparse` prints spargebra's algebra and the physical plan, where Jena prints its own algebra, quad form and optimized algebra. There is no `schemagen`. |
| Configuration | Assembler files (`config.ttl` and `run/configuration/*.ttl`) describe the services, datasets, text and spatial indexes, inference and access control. Users live in `shiro.ini` or the password file that `fuseki:passwd` names. | Server flags, settings files per dataset and a TOML auth configuration. `sparkles config import fuseki` translates a Fuseki configuration into them, with `tdb2.tdbdump` steps for the data and a report of each element it approximated or could not convert. `serve --fuseki-config` converts at each start. Assembler bodies of `POST /$/datasets` are read for the part that maps to one dataset ([G08](specs/G08-fuseki-configuration.md)). |
| Storage | TDB2 on copy-on-write B+trees, and TDB1, which is deprecated | Sorted, compressed permutation files with a delta and a WAL. Sparkles reads neither TDB format, so data moves between Jena and Sparkles as N-Quads or other RDF dumps. |
| RDFConnection / RDFLink | One interface for query, update, Graph Store operations and transactions, over a local dataset or a remote endpoint (`RDFConnectionRemote`, `RDFConnectionFuseki`) | `Dataset`, in Rust and Python, covers local databases. The Rust client `sparkles-client` covers remote servers with Jena's operations under Rust names (`query`, `select`, `update`, `get_graph`, `put_graph`, `load`), against Sparkles or any SPARQL endpoint, async or blocking. Its results stream as `oxrdf` terms and its writes return the commit they made. It has no transactions beyond one update request, and uses entity tags for optimistic concurrency, while Jena's remote transactions are a lock on the client. There is no remote client in Python. Jena's own remote clients work against the server: `RDFConnectionRemote`, `RDFConnectionFuseki`, `GSP`, `DSP`, `QueryExecHTTP` and `UpdateExecHTTP` are tested with every result format and RDF syntax (`mise run test:jena-clients`). |
| Query builder | jena-querybuilder (`SelectBuilder`, `ConstructBuilder`, `AskBuilder`, `DescribeBuilder`, `UpdateBuilder`, `WhereBuilder`, `ExprFactory`) | `sparkles::querybuilder`, with the same builders, typed terms, escaped literals and `set_var` |
| Commons RDF | jena-commonsrdf adapts Jena's terms to the Apache Commons RDF API | Terms are `oxrdf`'s, shared with the Rust crates built on it. Python terms convert to and from rdflib's. |
| IRIs and language tags | jena-iri3986 checks RFC 3986 and 3987 syntax and scheme rules (`http`, `urn`, `uuid`, `file`, `did`) with configurable severities. jena-langtag parses BCP 47 tags and writes them in canonical case. Parsers warn about problems, and `iri`, `langtag` and `/$/validate/*` report them. | `oxiri` and `oxilangtag` refuse malformed IRIs and language tags while parsing. Scheme rules for `http`, `https`, `urn` (with `uuid` and `oid`), `file` and `did`, and RFC 3986 normalization rules, give warnings in `iri`, `convert --check`, `load --check` and `/$/validate/iri`. `langtag` and `/$/validate/langtag` give the canonical case and subtags, and warn about grandfathered tags and extended language subtags. The checks are off by default and their severities are fixed. There is no subtag or scheme registry. |

### vs. QLever

| Area | QLever | Sparkles |
|---|---|---|
| Scale | Tested to tens of billions of triples (Wikidata, UniProt) | Measured up to 1.24 billion triples (English DBpedia, [BENCHMARKS.md](BENCHMARKS.md#dbpedia-at-124-billion-triples)). It loads them in 547 s, where QLever takes 1,674 s on the same machine. |
| Streaming execution | Lazy, block-wise scans, joins, filters and GROUP BY; results streamed to the client | Queries run eagerly by default, and each operator then materializes its result within row and memory budgets. Opt-in streaming execution runs scans, joins, DISTINCT and eligible aggregates batch by batch, and automatic selection uses it for large immutable scans and OPTIONAL counts. Responses over 1 MiB are streamed as they are serialized. Materialization and the 1 GiB block cache trade memory for speed. After the 10.5M benchmark, Sparkles' server holds 840 MiB to QLever's 653 MiB. Of that, 379 MiB is the block cache, and a server with the cache off ends at 304 MiB with about 46% less throughput ([BENCHMARKS.md](BENCHMARKS.md#memory-and-the-speed-it-buys)). |
| Block prefiltering | FILTER ranges and STRSTARTS checked against block min/max to skip blocks | Numeric range FILTERs on a scan's sort column read only the matching id ranges (inline integers and decimals). Non-canonical numerals are tested row by row. `STRSTARTS` and a `REGEX` anchored on a literal start read only the vocabulary ids of the keys with that start. |
| Pattern trick | `ql:has-predicate`, per-subject predicate patterns | ✗ (predicate counts come from index runs) |
| Text and spatial | `ql:contains-word`, BM25 scoring, spatial joins, a geo index | BM25 search through `text:query` (no text/entity co-occurrence index). GeoSPARQL functions, a spatial index, spatial joins and nearest-neighbour ORDER BY. |
| Path search | The `pathSearch` service returns every path between sources and targets, with edges from a nested subquery, edge properties, a cap on depth and on paths per target | `SERVICE path:search` returns one shortest path, every shortest path, the k shortest or every path up to a length, with weights through Dijkstra's algorithm. Edges come from predicates or from every triple, in either or both directions, not from a nested pattern ([F07](specs/F07-path-search.md)). |
| Vocabulary compression | FSST string compression, numeric IRIs encoded as ids | Front coding; no IRI encoding |
| Pinned results, materialized views | `pin-result-with-name`, materialized views | A result cache, without pinning |
| Live query monitoring | Runtime updates over a websocket | The executed plan, after completion |

In the other direction, Sparkles keeps Jena behaviour that QLever does not aim for: exact
term identity (no lossy inlining), the Graph Store Protocol, Fuseki's endpoints and admin
API, materialized reasoning, SHACL validation and an embeddable library.

### vs. Fluree

[Fluree DB](https://github.com/fluree/db) 4.x is the closest peer: a Rust RDF database with
a SPARQL 1.1 endpoint, a compressed columnar index and in-memory novelty over immutable
index files. It is built as a versioned, permissioned ledger with JSON-LD, SPARQL and
openCypher interfaces. Fluree is licensed under BUSL-1.1 (free except as a hosted
database service; each release becomes Apache-2.0 after four years). Sparkles does not
depend on it. Fluree appears here only as a benchmark target,
downloaded at benchmark time, and this comparison draws on its public documentation.

| Area | Fluree | Sparkles |
|---|---|---|
| History | An immutable, content-addressed commit chain, read at a transaction number, time or commit (`@t:`, `@time:`, `@commit:`). Commits can carry a writer-supplied event time, and `@recorded:` then reads by the time they were written. History queries return each assertion and retraction with its transaction, and branches rebase, merge and revert. | Durable, ordered commit ids and a commit catalog that can be pruned past a horizon. Point-in-time reads of every commit since the last compaction, and of older commits kept by named snapshots or a retention window. Diffs between any two readable commits, as JSON or RDF Patch, and a change feed that resumes from any commit the change log still holds. History queries return each addition and removal of a quad with its commit, time, author and message, in SPARQL or over HTTP, from a change log that outlives compactions. Branches of persistent datasets share their upstream's index until they compact, and merge with fast-forwards or three-way merges of quad sets, conflicts by cell or subject, and resolutions. There are no rebases or reverts. |
| Security | Access policies stored in the ledger, JWS / `did:key` signed requests and commits, OIDC, encryption at rest | Per-dataset access levels with Basic, API tokens, OIDC sign-in for the UI, the OIDC provider's access tokens and trusted proxy headers. Grants can be limited to some named graphs and endpoints. Protections in the server's configuration hide triples by predicate, by the class of their subject or by a SPARQL pattern with the caller bound, and grants lift them for reading or writing. No signed requests or encryption at rest. |
| Verifiability | Requests and transactions can be signed with JWS by a `did:key` identity, and a server can sign the commits it writes. `fluree verify` checks the commit chain and the objects it references. | An optional SHA-256 digest chains each commit's net changes (`--commit-digests`). `sparkles check` verifies WAL checksums and commit continuity. Nothing is signed. |
| Interfaces | SPARQL, a JSON-LD query and transaction language, openCypher with the Bolt protocol, and GraphQL with a schema derived from the data and its SHACL shapes. SELECT results can stream as NDJSON, and several queries can run on one snapshot in one request. | SPARQL with the Graph Store Protocol, the Rust and Python APIs, and an MCP server over stdio and HTTP. A read-only GraphQL endpoint maps a reviewed SDL schema to classes and predicates and runs each request as a fixed number of SPARQL queries. JSON-LD is an RDF format only. There is no JSON query language or Cypher, and GraphQL has no mutations. |
| Writes | Insert, upsert (replace the values of the given properties) and sync (make one graph equal to a payload and commit the difference), in JSON-LD, Turtle or TriG, besides SPARQL Update. A staged transaction can be previewed without committing it. | SPARQL Update, Graph Store writes, uploads and parallel bulk loads. Each commit records its net changes, and a Graph Store `PUT` through the write-ahead log records only the quads that change. `If-Match` makes a Graph Store write conditional on the commit it read. Any write can run as a dry run, which reports the commit, the changes, the validation result and the quota effect without writing anything. There is no upsert verb, because one SPARQL update expresses it. |
| RDF 1.2 | Annotations (`{\| \|}` and reifiers) describe edges, and Cypher relationship properties read the same data. Triple terms are accepted only as the object of `rdf:reifies`, without the triple-term functions, and a base direction is kept inside the language tag. | Triple terms in every position, annotations, reifiers, base-direction literals and the SPARQL 1.2 triple functions, in every RDF syntax. The SPARQL 1.2 suite passes 269/269. There is no property-graph view. |
| Ledgers and datasets | Each ledger has branches (`name:branch`), recorded in a nameservice on file, S3 or DynamoDB storage that can be listed and queried. A ledger keeps reserved graphs for its configuration and its commit metadata. | Fuseki datasets, created and listed through the admin API. Each has a UUID and a commit sequence, listed by `/$/commits` and `sparkles log`. Commit records cannot be queried with SPARQL. |
| Multi-source queries | One query can read several ledgers through `FROM` or a local `SERVICE`. Search indexes, Iceberg and Delta Lake tables, and SQL endpoints mapped with R2RML are queried as graphs. | A query reads one dataset. Other datasets and endpoints are reached through `SERVICE` over HTTP. There are no R2RML mappings or external tables. CSV and TSV tables can be imported with CSVW metadata or CONSTRUCT templates, but not queried in place. |
| Search | BM25 full-text as an index or an inline scoring function, and vector similarity functions with an HNSW index in builds that enable it. Points have an index for proximity, and other geometries an S2 cell index. `geof:distance` is the one GeoSPARQL function. | BM25 full-text through `text:query`, stemmed per language, vector search with an HNSW index or exactly, and hybrid ranking of both by reciprocal rank fusion. A vector index can compute its vectors from selected literals through an OpenAI-compatible endpoint after each commit, and searches can pass text. GeoSPARQL 1.1 has a spatial index, spatial joins and nearest neighbours. Vector indexes have no quantization. |
| Reasoning | Chosen per query or per ledger. RDFS and OWL 2 QL work by query rewriting, while OWL 2 RL and Datalog rules, in JSON or as SPARQL CONSTRUCT, are derived per query into a cached overlay within a fact and time budget. The ontology can come from other graphs or ledgers, follow `owl:imports`, or come with the query. | Materialized into a graph (RDFS, an OWL 2 RL subset, Jena rules) and updated incrementally after writes, with staleness reports and OWL 2 RL inconsistency checks. The reasoner reads the default graph or chosen graphs and follows `owl:imports`, fetching missing ones on request. RDFS on read is the only query-time reasoning. |
| Validation | SHACL at transaction time and through a validate endpoint, and uniqueness constraints on chosen properties. | SHACL Core and SHACL-SPARQL, which pass both W3C suites, and ShEx 2.1, on request or as write-time guards. Guards validate incrementally and reject, warn or block only new violations. Uniqueness can be written as a SHACL-SPARQL constraint. |
| Configuration | Policy, SHACL, reasoning, rule and uniqueness settings are triples in each ledger's configuration graph, so they are versioned with the data and can differ per graph. | Server flags and per-dataset settings files (`validation.json`, `reasoning.json`, `text.json`, `geo.json`, `vector.json`, `queries.json`), set through the admin API and CLI. They are not part of the commit history. Stored queries keep a version history of their own. |
| Deployment | Memory, file, S3 with DynamoDB, or IPFS storage. Raft clusters replicate writes, read-only query peers follow an event stream, and ledgers are cloned, pushed and pulled between servers. | A single node on local disk, run from the binary, the repository's Docker image or the NixOS module. Backups go incrementally to a file system or S3, and the change feed streams every commit as RDF Patch, but no replica applies it. |
| AI agents | The server's MCP endpoint has a schema tool and a SPARQL tool with byte-budgeted results. The CLI adds stdio tools for its documentation and for a memory store for coding assistants, and turns PDF, Office and Markdown files into a graph of chunks, embeddings and entity mentions. | An MCP server over stdio and HTTP, with tools for datasets, schema, bounded SPARQL, explain, describe, commits, text and vector search, validation and formatting, an opt-in update tool, resources and prompts. Each call runs as its caller within the query budgets. Sparkles computes embeddings of selected literals through an OpenAI-compatible endpoint, but it does not ingest documents or split text into chunks. |
| Observability | OpenTelemetry traces, structured logs, health and stats endpoints, and per-query tracking of time and "fuel" with limits. Prometheus metrics are planned. | An access log, Prometheus metrics, readiness, and OpenTelemetry traces, metrics and logs. Queries run within memory, row and work budgets. |
| Embedding | An async Rust API on Tokio, built from a builder over memory, file or S3 storage and split into many crates behind feature flags. | A synchronous Rust library with no HTTP or async dependencies, a Python package, a JVM library that gives Jena programs a `DatasetGraph`, and Node.js and TypeScript packages. The reasoner, validators, formatter and server are separate crates on its public API. |

Sparkles is ahead on:
* **Conformance.** Sparkles passes the W3C SPARQL 1.0, 1.1 and 1.2 suites in full and
  the SHACL Core and SHACL-SPARQL suites (98/98, 20/20). Fluree's documentation reports
  that it runs the SPARQL 1.1 suite with some tests registered as not supported, such as
  those that keep a time zone offset, and that it does not yet run the SPARQL 1.2 suite.
* **Jena/Fuseki compatibility.** Fuseki's endpoints and `/$/` admin API, the Jena-style
  CLI and rules, and federation through `SERVICE` to any endpoint (Fluree federates only
  between its own ledgers and has no remote `LOAD`).
* **GeoSPARQL and validation.** The GeoSPARQL 1.1 functions with a spatial index and
  spatial joins, and ShEx next to SHACL.
* **Index design.** Fluree keeps 4 index orders, Sparkles 7. Fluree's planner is greedy;
  Sparkles uses dynamic programming. Both index in the background once uncommitted
  changes pass a threshold. Sparkles keeps updates in an in-memory delta and compacts it
  into a new generation when it passes a share of the base index, a size or an age
  ([C13](specs/C13-automatic-compaction.md)).

[BENCHMARKS.md](BENCHMARKS.md) has the head-to-head numbers.

### vs. Oxigraph

[Oxigraph](https://github.com/oxigraph/oxigraph) (MIT / Apache-2.0) is a Rust RDF database
and toolkit on RocksDB. Sparkles uses Oxigraph's libraries for its term model, parsers,
serializers, SPARQL parser and XSD datatypes (`oxrdf`, `oxttl`, `oxrdfio`, `spargebra`,
`sparesults`, `oxsdatatypes`). The storage engine, query planner and executor are
Sparkles' own.

| Area | Oxigraph | Sparkles |
|---|---|---|
| Embedding | A Rust library, Python (`pyoxigraph`) and JavaScript/WebAssembly packages, an in-memory store | A Rust library, a Python package (`sparkles`, abi3 wheels for Linux and Apple silicon macOS built by a release workflow, not on PyPI), a JVM library (`sparkles-jena`, a Jena `DatasetGraph`) and Node.js packages with a native addon, persistent or in-memory. The JVM and Node packages are not on Maven Central or npm either. The Python API follows pyoxigraph's names for terms, `query`, `load`, `dump` and `quads_for_pattern`, and adds transactions as context managers, reasoning, validation, history and an rdflib store plugin whose SPARQL runs in Sparkles. No WebAssembly build. |
| Storage | RocksDB (a C++ LSM tree) with 9 index orders (6 for named graphs, 3 for the default graph) and a string dictionary; updates in place; online backups as RocksDB checkpoints (a complete copy in a new local directory, hard-linked on the same file system) | Immutable sorted blocks in 7 orders, plus an in-memory delta logged to a WAL and merged by compaction. Online backups to repositories on a file system or S3, incremental and deduplicated across backups and datasets, with restore, verification, schedules and retention. |
| Spatial | GeoSPARQL functions (`spargeo`, on by default in the CLI); no spatial index | GeoSPARQL 1.1 functions (geodesic measures, EPSG:4326 axis order, metric buffers) and a per-dataset spatial index |
| Write durability | One RocksDB transaction per request, written to RocksDB's WAL without an fsync (RocksDB's default) | The WAL is fsynced before a write is acknowledged. A commit waits for one `fdatasync`. When it adds terms the dataset has not seen before, they go to a separate file, which is synced at the same time as the WAL. A commit message or change digest adds one more. |

Oxigraph describes its query evaluation as "not optimized yet". It evaluates lazily, one
iterator per RocksDB scan. In the benchmarks at 10.5M triples, it answers point lookups
and single paths about as fast as Sparkles, within 1.1–1.4×. Joins, grouping, sorting and
counting run about 20–1,900× slower, and one EXISTS join about 8,400× slower. It serves 1.7
concurrent star-join queries per second to Sparkles' 241. Its load, including
`optimize`, takes 14.1 s to Sparkles' 3.9 s. A single-triple update takes 4.3 ms to
Sparkles' 4.9 ms, although Oxigraph does not fsync it, and Oxigraph commits a stream of
them 1.7× faster.
Oxigraph has no reasoning, SHACL, full-text or vector search, path search, CSV imports,
point-in-time reads, authentication or per-dataset permissions, Fuseki admin API, GraphQL
endpoint, query budgets, result cache or web UI.

## Divergences from Jena / QLever (decisions)

### General

| Decision | Rationale |
|---|---|
| Rust instead of Java or C++ | [AUDIT.md](AUDIT.md#3-language-decision-rust) §3: `spargebra`, `oxttl` and related crates provide the parser and format stack, there is no garbage collector, and performance is predictable. |
| Sorted-block permutations instead of TDB2's B+trees | Scan-heavy analytics run much faster and the files are smaller. Updates go to a delta that compaction merges, not into the index in place. |
| Values are inlined only when their lexical form is canonical | QLever inlines lossily: doubles lose 4 bits and the lexical form is dropped. Sparkles keeps exact RDF term identity (`"01"^^xsd:integer` ≠ `"1"^^xsd:integer`), as Jena does. Doubles whose low mantissa bits are set go to the vocabulary. |
| Literals with different lexical forms are different terms, even when they have the same value | QLever folds `xsd:float` and `xsd:double` literals whose lexical forms parse to the same number into one term. RDF 1.1 identifies a literal by its lexical form and datatype, so Sparkles keeps them apart, as Jena does. On English DBpedia, QLever therefore counts 1,014,683,273 triples and Sparkles 1,014,683,275, because two pairs of triples (in `geo:lat` and `dbo:orbitalPeriod`) differ only in the lexical form of such a literal. |
| The graph is a 4th key column in every permutation, plus a GSPO permutation | Matches QLever's graph column. GSPO gives TDB2-style graph-scoped access (dumps, enumerating `GRAPH ?g {}`). |
| Deltas are persistent ordered sets (`imbl`), logged to the WAL | Snapshots publish in O(1) for MVCC. QLever locates delta triples per block instead; Sparkles may adopt that later. |
| Blank nodes are stored ids and serialize as `_:b<hex>` | A stored node keeps its label across requests, and the dataset APIs and query bindings accept it back. Blank nodes that a query makes are `_:q<hex>` and belong to their result. |
| LZ4 instead of zstd for index blocks; front coding instead of FSST for the vocabulary | Fast decoding on the query path. zstd is used where ratio matters more than decode speed: backups, dumps, HTTP and the full-text document store. zstd blocks and FSST remain possible upgrades. |
| Canonical decimal output follows XSD 1.1 (`"4"^^xsd:decimal`); Jena writes `"4.0"` | Inherited from `oxsdatatypes`. The values are equal, so value-based result comparison is unaffected. |
| SPARQL parsing and algebra through `spargebra`, not a port of ARQ's JavaCC grammar | The algebra matches SPARQL 1.1 §18. Of ARQ's syntax extensions, the vendored parser has the statistical aggregate keywords, `AGG <iri>(…)`, `LATERAL`, path ranges and CONSTRUCT templates with `GRAPH`, all accepted by default as Fuseki accepts them. `LET`, `FOLD` and the `apf:` property functions are not supported. Sparkles reads `p{0,}` as `p{*}`, where Jena 6.2.0 leaves out the zero-length match. |
| `agg:median` and `agg:mode` are aggregates, like the keywords `MEDIAN` and `MODE` | ARQ has the keywords only and reads the IRIs as calls of unknown functions. Sparkles needs the IRIs to write a parsed query back as text, and writes the keywords when it does. |
| `MODE` of a group where no value repeats is the first value in row order | Under ARQ's own rule every value then reaches the highest count, one, and the first gets there first. ARQ fails the whole query with a `NumberFormatException` instead, which also happens for every `MODE(DISTINCT …)`. |
| `fn:years-from-duration` and the other duration accessors normalize the duration and give each component its sign, as F&O 3.1 §8.2 specifies (`PT90M` has 1 hour and 30 minutes) | ARQ returns the fields as written and without the sign, so `fn:minutes-from-duration("PT90M")` is 90 and `fn:hours-from-duration("-PT90M")` is 0. |
| Casts of an `xsd:dateTime` or `xsd:date` to `xsd:gYear`, `gMonthDay` and the other Gregorian types keep the timezone, as F&O 3.1 §19.1.5 specifies | ARQ drops it, so it casts `"2024-05-06T10:00:00Z"` to `"--05-06"` where Sparkles gives `"--05-06Z"`. |
| `xsd:unsignedByte(…)` is a cast | ARQ registers casts to the other derived integer types but not to `unsignedByte`, so it calls an unknown function. |
| Filter placement and equality substitution happen in the planner, not as ARQ-style algebra transforms | Same effect as `TransformFilterPlacement` and `TransformFilterEquality`, with one fewer pass over the algebra. |
| `REDUCED` is a no-op | The spec allows it. |
| DESCRIBE's default includes the descriptions of reifiers, as the W3C concise bounded description does | Jena's `DescribeBNodeClosure` leaves them out, so an annotated triple loses its annotation in the description. On data without reifiers the two answers are the same, and `"reifiers": false` gives Jena's answer on any data. |
| A DESCRIBE without a dataset in the query reads the store's default graph, even when queries see the union of the named graphs | This is what Jena's handler reads in a TDB2 dataset with `unionDefaultGraph`. Each named graph in which the resource appears is read on its own as well. |
| `GRAPH ?g { P }` binds `?g` as a scan column when `P` is a plain join group; otherwise `P` is evaluated per named graph and joined with `?g`, like Jena's `OpGraph` | The fast path covers the common case; the fallback keeps SPARQL scoping exact (for example OPTIONAL or MINUS inside GRAPH). |
| `GROUP_CONCAT` always returns a simple literal | Spec behaviour. Jena keeps a common language tag. |
| Triple terms are vocabulary entries with a canonical nested key (blank nodes inside keep their store identity); patterns with variables inside `<<( … )>>` bind a hidden variable that a `TripleTerm` operator decomposes | The 64-bit id model and the permutations stay unchanged. Key order puts triple terms between literals and IRIs, so term-kind checks stay O(1). |
| The effective boolean value of an ill-typed boolean or numeric literal is an error | SPARQL 1.2 §17.2.2. SPARQL 1.1 said `false`. |
| Reasoning is materialized (forward chaining into `urn:x-sparkles:inferred`, queried as default ∪ inferred), not computed on the fly like Jena's `InfGraph` | Queries run at the speed of the plain index. The cost is re-running `/$/reason` after updates. The reasoning status records the commit it ran at, so stale inferences are reported, and `serve --auto-reason` re-runs them automatically. Backward (LP) rules are not supported. |
| An `AS ?v` target already in scope is rejected (SPARQL §18.2.1) | `spargebra` does not check this, so Sparkles does, matching Jena and QLever. |
| Stemming uses Tantivy's Snowball stemmers and stop word lists, not Lucene's language analyzers | Lucene uses the same Snowball stemmers for Danish, Dutch, Finnish, Hungarian, Norwegian, Romanian, Russian, Swedish and Turkish. Its English analyzer uses the original Porter stemmer, French, German, Spanish, Italian and Portuguese use light stemmers, and Greek and Arabic use stemmers of their own. A stemmed search can therefore find different literals than in Jena. On a test corpus in nine languages, 1,235 of 1,590 stemmed queries matched the same literals in both. The Snowball languages agreed on 662 of 671 queries, and English on 172 of 191. |
| A literal's language is its primary subtag: `en-GB` literals are stemmed as English, and `lang:en` finds them | Jena matches the language tag exactly, so `lang:en` leaves `en-GB` literals out. |
| The full-text index commits lazily: a write stages its documents, and the next text query that needs them, or a tick about once a second, commits them | A Tantivy commit flushes a segment and costs more than the indexing, so a burst of writes shares one commit. Each snapshot still searches exactly its own documents: later ones are filtered out, and removed ones are kept until their batch commits. After a crash the WAL restores what was only staged. Jena's text index commits with each transaction. |
| N-Quads backups (`/$/backup/{ds}`, `sparkles backup`) are zstd (level 3, `.nq.zst`) by default; Fuseki writes gzip (`.nq.gz`). `?compression=gzip` or `--compress gzip` writes Fuseki's format | At 10.5M triples, zstd took 8.2 s for 81.5 MB and gzip (level 6) 41 s for 74.9 MB: five times faster for a file 9% larger. `sparkles load` and uploads read both. |

### Server defaults

| Decision | Rationale |
|---|---|
| `serve` listens on `127.0.0.1` by default. Without `--auth-config` it refuses a non-loopback address unless `--allow-open-network` (or `SPARKLES_ALLOW_OPEN_NETWORK=1`) is given. Fuseki listens on all interfaces. | Without authentication every caller can read, write and administer everything, so exposing that must be explicit. The override logs a warning, as does a network listener without rate limits. |
| Without `--auth-config`, `serve` sends no CORS headers unless `--cors-origin` names an origin, refuses cross-site writes (`Origin`, `Sec-Fetch-Site`), and accepts only IP addresses, `localhost`, `--host` and `--public-host` names in `Host`. Fuseki answers CORS from any origin. | Every caller of an open server is its administrator. Without these checks, any web page the operator opens could read, write and `LOAD` local files through the browser, directly or by rebinding its DNS name. |
| `--max-export-mb` defaults to `0` (unlimited), while query responses are capped at 1 GiB (`--max-result-mb`) | A Graph Store GET of a graph or dataset is the export path, streamed from one snapshot, and a finite default would cut off legitimate dumps. The cost is that any reader can make the server stream the whole dataset (CPU and bandwidth, not memory). Deployments that expose reads to untrusted clients should set `--max-export-mb` and rate-limit the `query` class. |
| Compaction runs on its own (`--no-auto-compact` turns it off). A dataset is compacted when its delta reaches 10,000 quads plus 5% of the base index, a million quads, 512 MiB or a day's age, or after five quiet minutes with at least 10,000 quads in the delta. Writes go on during the build. TDB2 compacts only on request and blocks writers meanwhile. | Statistics, characteristic sets and block skipping are exact only on the base index, and a large delta slows scans and costs memory, so a long-running server needs compaction without an operator. Writes wait only for the final switch, which takes milliseconds. The cost is a full rebuild per compaction, CPU and I/O that the build limits to a quarter of the cores at a lower priority, and a full upload at the next incremental backup. |
| A client's `timeout=` is capped at `--max-timeout` (default 1800 s, `0` for no cap) for queries, updates and Graph Store writes. The default query timeout is 60 s. Writes have no default deadline (`--update-timeout 0`) but are cancelled when their client disconnects. | A request may ask for more than the default but cannot hold a worker forever. A long load is not cut off by a default it did not ask for, and a disconnected load stops (its rate-limit concurrency slot stays taken until it has). |

### GeoSPARQL

| Decision | Rationale |
|---|---|
| Distances, lengths and areas on geographic CRSs are geodesic on the WGS 84 ellipsoid (Karney). `geo.json` `"distance": "haversine"` gives Jena's sphere (R = 6,371,008.7714 m). | Up to 0.5% more accurate than the sphere, at a small cost per call. Jena computes great-circle distances on a sphere. |
| Literals are read in their CRS's own axis order (EPSG:4326 is latitude first). The legacy `…/def/crs/EPSG/4326` (without `/0/`) is CRS84, as in Jena. | GeoSPARQL Req 16. `minX`…`maxY` report the literal's own axes, as in Jena. |
| A literal in a CRS this build does not know is still a valid geometry: same-CRS planar relations, accessors and constructions work; metric functions and mixes with other CRSs are type errors; the index leaves it out and counts it in its status | Jena logs a warning and treats the coordinates as CRS84 degrees, which gives wrong answers silently. |
| Relations follow DE-9IM: an empty geometry is disjoint from everything (`sfDisjoint` is true, every other relation false), equal points are `sfEquals`, `sfCrosses` of two curves is `0********`, and RCC8 relations hold between regions only | Jena returns false for every relation on an empty geometry and compares `sfEquals` with the tables' `TFFFTFFFT` pattern, under which two equal points are not equal. |
| Query rewrite is off by default (`geo.json` `"queryRewrite": true` per dataset) | Jena rewrites by default. Each rewritten pattern costs a spatial search or join, and enabling a spatial index should not change what an existing query over asserted triples means. |
| `geof:getSRID` returns an `xsd:anyURI`; `geof:dimension` of an empty geometry is its type's dimension (`-1` for an empty collection) | The GeoSPARQL 1.1 signature (Jena returns `xsd:string`), and never a type error. |
| `geof:concaveHull(g, targetPercent)` sets concaveman's concavity to `targetPercent / 25` (50 gives the default 2.0, 100 the convex hull); `geof:aggConcaveHull` uses the default; `spatialF:angle` follows Jena's documented meaning (clockwise from the y axis) in every quadrant | GeoSPARQL leaves the hull parameter to the implementation, and a SPARQL aggregate takes one expression. Jena's `angle` is a quarter turn off south-east and north-west of the first point. |
| CRSs beyond the built-in ones come from proj4 definitions in a `--geo-crs` file, transformed by `proj4rs`. The server resolves other projected EPSG codes through an EPSG-derived proj4 table, and the library does so only with its `geo-epsg` feature. Sparkles has no datum-shift grids, so definitions that need one are refused, and Helmert shifts are reported as approximate in the plan's warnings. | Jena uses Apache SIS with an EPSG database. The server carries the EPSG terms of use in its license notices. The library leaves the table out so that an embedder opts in to those terms, as Apache SIS users do. |
| `infer --vocab geosparql` types each geometry from its serialization literals (`sf:Polygon` from a WKT `POLYGON`, `gml:Polygon` from a GML root element), alongside the vocabulary's class hierarchy. | GeoSPARQL requires the hierarchy, and users expect `?g a sf:Polygon` to match data that only has a WKT literal. The type comes from the literal's keyword without parsing its coordinates. |
| The spatial index is opt-in per dataset (`geo.json`). The `geof:` functions work without it, and every index hit is refined with the exact test, so answers are the same with or without it. | Jena builds its index for the whole dataset at start-up, and its `spatial:withinBox` / `intersectBox` return envelope hits for an unbound subject. |

### Out of scope for v1

JavaScript functions, Jena's own RDF formats in the library's loader and the Python bindings, jena-ontapi
object mapping, jena-text's Lucene index format (Sparkles implements `text:query`
itself), SHACL-AF rules (also absent from Jena), backward-chaining (LP) rules, and Shiro
as an authentication framework. `sparkles config import fuseki` moves the users and URL
rules of a `shiro.ini` into Sparkles' auth configuration.

## Optimizations adopted from QLever

* **Ids with inline values.** The top 4 bits hold a tag and the low 60 bits a payload.
  UNDEF is 0, so it sorts first. Numbers and booleans never touch the dictionary.
* **Sorted vocabulary.** Id order equals term order, so prefix and range restrictions
  become id ranges (`Vocab::prefix_range`).
* **Permutation files.** Blocks store each column separately (delta, zig-zag varint,
  LZ4). Each block's first and last key stay in RAM, which allows block skipping on
  bound prefixes and exact counts with at most two block decodes.
* **Bulk build pipeline.** Parallel chunked parsing produces per-batch vocabularies. They
  are merged into one in parallel, over ranges of keys that sampled keys delimit. The
  batches are then read once, in chunks that are remapped to global ids in parallel and
  sorted in place by SPO, OSP and PSO, each order written as a compressed sorted run.
  The runs of each order are merged in a thread of their own. As in QLever, the
  permutations are built in pairs: SOP, OPS and POS come from the merged SPO, OSP and PSO
  streams by sorting each run of the shared first column. Data that fits into the sort
  budget is sorted in memory the same way. The temporary files are compressed too. The
  batches' quads use the column blocks of the runs, the partial vocabularies are
  front-coded LZ4 blocks, and the maps from batch ids to global ids are delta-coded.
* **Immutable base plus delta.** Updates are layered on the immutable index, in the style
  of QLever's `DeltaTriples`. Snapshots are versioned, and caches are keyed by snapshot
  version. A scan merges the delta into the blocks it changes. It finds the base rows
  between two delta keys by binary search, and only those blocks have every column
  decoded.
* **Rebuilds in the background.** Like QLever's index rebuild, a compaction builds the
  new index from a snapshot while updates continue, then carries the updates made since
  the snapshot into the new index with their ids remapped. Sparkles carries each commit
  into the new generation's log, so the commits made during the build stay readable at
  `?at=`. The automatic trigger takes QLever's `min`, `max` and `fraction` form
  (`--rebuild-index-strategy automatic:min:max:fraction`), adds the size of the log, age
  and idle time, and waits for bulk loads, backups and the retention window.
* **Columnar execution and planning.** Execution is column-major. The planner orders joins
  with QLever's dynamic program, which keeps the cheapest plan per subset of patterns and
  sort order, and merge joins run on sorted scans. A hash join's output keeps the order of
  the input it probes, so a merge join above it reads it as sorted. Groups too large for
  the program are planned in rounds or greedily, as described under join ordering on cost
  summaries below.
* **Decoded-block cache.** A shared cache of decoded blocks, weighted by bytes.
* **Result cache.** Executed subtrees are cached under a canonical plan key and the
  snapshot version, so updates invalidate entries without extra work. Results with
  query-local terms or non-deterministic functions are not cached. Each operator reports
  `cached` in the plan.
* **GROUP BY + COUNT from index runs.** When the group key is a scan's sort column, counts
  come from runs in the blocks; the scan is never materialized.
* **Planner details.** Filters are placed as soon as their variables are bound. Scan sizes
  are exact from block metadata (at most two block decodes). Joins are estimated from
  characteristic sets or probed values where those apply (see below), and otherwise from
  per-predicate distinct subject and object counts with QLever's 0.7 correction factor. A
  pattern with a single free subject, predicate or object has a distinct value of it per
  row. Costs are counted in rows read by a scan. A hash join costs 8 of them for each row
  of its smaller input, which goes into the hash table at 3 to 15 ns a row, and one for
  each row of the larger input that probes it. A scan reads a row in 1.3 to 2.2 ns. With
  the flat table switched off (`flat_hash_join`), a row of the smaller input costs 48,
  since the lists per key take 18 to 100 ns a row. Merge joins gallop through skewed
  inputs. `COUNT(*)` over one pattern comes from index
  metadata. Transitive paths traverse from the bound side, with index lookups per
  frontier node, instead of materializing the closure.
* **Executed-plan feedback.** Every query returns a runtime-information tree (estimated
  and actual rows, time per operator), like `qlever-json`. The UI renders it.

## Further scan and filter optimizations

Each of these can be switched off per query (`QueryOptions::optimizations`) or per process
(`SPARKLES_DISABLE_OPTIMIZATIONS=range_pushdown,…`). EXPLAIN shows which ones ran.

* **`COUNT(DISTINCT ?v)` from index runs.** Over one triple pattern, the scan switches to a
  permutation sorted on `?v` and counts runs of equal ids (`CountDistinctFromIndex`). No
  rows are materialized or hashed. When the pattern is `?s ?p ?o`, or binds only its
  predicate, the count comes from the index statistics instead, corrected like the counts
  from statistics below (`CountDistinctFromMetadata`, part of `metadata_counts`).
* **Filters on vocabulary keys.** `CONTAINS`, `STRSTARTS`, `STRENDS` and `REGEX` over `?v`
  or `STR(?v)`, and `LANGMATCHES(LANG(?v), …)`, are tested on the stored key bytes
  (`"lexical 0xFF @lang`, `<iri`). Each front-coded block is read once, in parallel,
  without allocating a string per term. A `CONTAINS` needle is searched with a substring
  finder built once, and a key is checked to be UTF-8 only when it matches, since a byte
  match of UTF-8 text is a match of the strings. Each parallel task tests with its own
  copy of a regular expression, because threads that share one wait for its match caches.
  Terms added by updates are tested on their delta keys. Inline values (numbers, dates)
  go through the general evaluator.
* **Filtered counts from index runs** (`count_filter_runs`). `COUNT(*)`, `COUNT(?x)` and
  `COUNT(DISTINCT ?v)` over a FILTER that reads only `?v` of a single triple pattern read
  the pattern from the permutation sorted on `?v`. The filter is tested once per run of
  equal ids, and the count is the sum of the lengths of the runs that pass, or the number
  of such runs for `COUNT(DISTINCT ?v)` (`CountFilterFromRuns`). No rows are
  materialized. When the snapshot's delta has no key in the pattern's range, the index
  blocks are read in parallel and each block's vocabulary ids are tested on their keys as
  the block is read, in pieces of 8192 rows so that a short scan still uses every thread.
  Otherwise the runs are read in order with the delta merged in. At 10.5M triples,
  counting the `foaf:name` values that contain "Ada" tests 1.02M names and takes about
  4 ms in the server, against 14 ms when the name column was materialized first.
* **Filters on the runs of a scan** (`filter_scan_runs`). A FILTER directly over a scan
  sorted on a variable it tests evaluates the conjuncts that read only that variable once
  per value, then copies out of the index blocks only the rows of the values that pass,
  in parallel. The other conjuncts are applied to those rows. EXPLAIN notes
  `[runs of ?v: N values tested on vocabulary keys, M passed]`. Union-graph dedup, which
  compares neighbouring rows, and a delta with keys in the scan's range keep the generic
  scan and filter.
* **Filter selectivity from samples** (`sampled_filters`). Without other knowledge the
  planner assumes that a FILTER conjunct keeps 30% of its input. A conjunct whose
  variables are all bound by one triple pattern is instead tested on a sample of that
  pattern's rows, and the share of the sample it keeps becomes its estimate. The sample
  comes from the sorted index. The first and last keys of the pattern's blocks are held in
  memory by the block metadata and cost nothing to read. When they are too few, one or
  two blocks are decoded as well, preferably blocks the cache already holds, and give 128
  rows each. A pattern of at most a few thousand rows is read whole, with the delta. The
  pattern is read sorted on another variable when it has one, so that a decoded block
  holds values from all over the filtered variable's range. Selectivities are kept with
  the snapshot, and later queries at the same commit reuse them. A conjunct over several
  patterns keeps the fixed estimate. EXPLAIN notes `[selectivity 0.0650 of 187 sampled
  rows]` on the filter. At 10.5M triples, `?p foaf:name ?n ; foaf:age ?a
  FILTER(CONTAINS(?n, "Ada"))` keeps 5% of the names. With an estimate of 6.5% instead
  of 30%, the planner tests each distinct name once in a scan sorted on the name, sorts
  the 50,000 rows that pass and merges them with the ages. The query runs in 12 ms
  instead of 20 ms. Sampling adds 0.02 to 0.08 ms to planning a filter when the blocks are
  cached, and 0.3 to 0.6 ms when one has to be decoded.
* **Star estimates from characteristic sets** (`characteristic_sets`). A join on a
  variable estimated from distinct values assumes that the values of the side with fewer
  are all among the other side's. For patterns on one subject this holds only when every
  subject with the rarer predicate has the others. In WatDiv, where users have most
  predicates independently of each other, it overestimated a star of six patterns
  tenfold. A bulk load or compaction now counts the characteristic sets of the subjects,
  after Neumann and Moerkotte: each set of predicates some subject has exactly, with its
  subjects and the triples of each predicate. The classes a subject has by `rdf:type`
  count as predicates of their own, since a class usually decides which predicates its
  instances have. The statistics keep the 10,000 sets with the most subjects. A join on a
  subject variable of patterns `?s p ?o` with a constant predicate, or `?s a <class>`, is
  then estimated from the sets that hold the predicates of both sides, including the
  number of triples per subject of each predicate within those sets. Whatever else
  restricts an input is taken to be independent of the predicates. Patterns with another
  constant object, predicates that the kept sets cover poorly, and stores loaded before
  this change keep the estimate from distinct values. The sets holding a predicate are
  found once per index generation, and the sums for a query's predicates are kept for
  later queries. On WatDiv at 1.1M triples, the joins of the C3 star are estimated
  exactly, and C3 runs in 0.65 ms instead of 1.48 ms. C2 runs in 0.03 ms instead of
  0.21 ms.
* **Join estimates from probed values** (`probed_keys`). When one input of a group is
  small, a VALUES table or a pattern of at most 1,024 rows, the planner reads up to 32 of
  its values, spread over them in order, and counts the rows each has in every other
  pattern of the group on the same variable, as an index join would look them up. The mean
  count and the share of values that have rows then estimate a join of the small input
  with that pattern. They replace the assumption that every value is on the other side
  with the pattern's average number of rows, less 30%. This is index-based join sampling
  as Leis et al. describe it, from one small input. When the small input is itself a
  pattern the characteristic sets count, their estimate comes first, since the sets know
  how a star's predicates occur together. The counts read each block of a pattern once,
  and at most two blocks per group that the block cache does not hold. A small pattern
  whose blocks are not cached is left to a later query, after this one has read them. The
  counts are kept with the snapshot. At 10.5M triples, a star of three patterns over
  30,000 VALUES keys is estimated at its 30,000 rows instead of 10,290.

  Joins that neither the characteristic sets nor probed values estimate keep QLever's
  0.7 correction. Measured over the two-input joins of every plan of `scripts/bench.sh`,
  the queries for these estimates and WatDiv at 1.1M triples, with the new estimates
  switched off, the factor makes the median join of the generated data 30% low, since its
  joins follow references that exist. The median WatDiv join is still 3.4 times too high
  with it. No single factor fits both. With the new estimates, the median join of both
  datasets is estimated exactly, and the mean error falls from a factor of 1.37 to 1.04
  on the generated data and from 4.2 to 1.6 on WatDiv.
* **Key ranges for a fixed start** (`filter_key_ranges`). Under a `STRSTARTS`, or a
  `REGEX` anchored on a literal start (`^abc` with no flag other than `s` and no
  alternation), the two operators above read only the base-vocabulary ids of the keys
  with that start: literals for `?v`, literals and IRIs for `STR(?v)`. Ids outside the
  base vocabulary (inline values and terms added by updates) are still read and tested.
* **Id ranges for a fixed start** (`filter_id_ranges`). The same starts restrict the other
  places that test such a filter on vocabulary keys. A FILTER over the rows of a join
  fails the base-vocabulary ids outside the ranges without reading their keys, and reads
  only the keys of the ids inside them, which lie together in the vocabulary. The planner
  costs a filtered scan sorted on the tested variable by the rows of its ranges, counted
  exactly, instead of by every row of the scan. On English DBpedia, `?s a dbo:Band ;
  rdfs:label ?l FILTER(REGEX(?l, "^The B"))` then reads the 23,812 labels in the range
  and merges them with the bands, instead of looking up the label of each of the 36,745
  bands. It went from 18.8 to 3.8 ms warm and from 1,040 to 90 ms cold.
* **Pure expressions per distinct value** (`expr_cache`). A FILTER conjunct, BIND, ORDER BY
  key or aggregate argument that reads one variable and gives the same result for the same
  term is evaluated once per distinct id of that variable. Rows look up the result, errors
  included. RAND, UUID, STRUUID, BNODE and EXISTS run per row; NOW and the base IRI are
  fixed for the query. On a column sorted by the variable, the distinct ids are its runs.
  Otherwise a sample estimates how often values repeat, and when fewer than half the rows
  repeat a value the operator runs row by row. EXPLAIN notes `[expr cache: …]` with the
  distinct count or the reason it ran row by row, and reports `exprCacheHits`,
  `exprCacheMisses` and `exprCacheSkipped`. A constant regular expression is compiled once
  per thread and reuses its match cache. The planner counts the cost of sorting an
  unsorted input column, so a single pattern under such a FILTER is read from the
  permutation sorted on the filtered variable.
* **Numeric range scans.** A FILTER that compares a scan's sort column with numeric
  constants reads only the id ranges that can match. Inline integers, and inline decimals
  of one scale, sort by value within their id segment, so each segment's matches form one
  range, found by binary search with the ordinary comparison. Doubles and vocabulary
  literals are read and tested; booleans, dates and blank nodes are skipped. The planner
  costs the scan from an exact count of the rows in those ranges (`IndexRangeScan`).
* **Top-k heap** (`topk_heap`). `ORDER BY … LIMIT k` (with or without OFFSET) keeps the
  best k rows in a bounded heap. Each key is classified once, so most comparisons are
  of exact decimals, doubles or strings, and the remaining pairs go to the ordinary
  comparator. The later keys are evaluated only for rows whose first key can still
  enter. Streaming execution reads its input batch by batch into the same heap.
  EXPLAIN notes `[top-k heap kept N of M rows]`. With the heap off, the two shortcuts
  below apply.
* **Numeric top-k.** `ORDER BY ?v LIMIT k` over numbers ranks cheap rounded keys first and
  computes exact values only for rows that can still reach the first k.
* **First-key top-k** (`topk_first_key`). `ORDER BY k1 k2 … LIMIT k` finds the k-th row by
  the first key alone and drops every row whose first key is worse, since at least k rows
  come before it. The later keys, often IRIs or strings that must be decoded, are
  evaluated only for the rows that remain. EXPLAIN notes `[first-key prefilter kept N
  rows]`.
* **Ordered-scan top-k.** `ORDER BY ?v LIMIT k` (with or without OFFSET) over one triple
  pattern and its FILTERs reads the pattern in `?v` order and stops once the first k rows
  are proven (`IndexTopK`). The scan switches to a permutation sorted on `?v`. Inline
  integers, decimals of one scale and doubles of one sign sort by value within their id
  segment, and each segment is read from its best end, a few rows at a time, until the
  k-th candidate beats the best unread value. Vocabulary literals (non-canonical numerals,
  other numeric types, strings), IRIs and blank nodes are not in value order by id, so
  they are read whole. Candidates are ranked by the ordinary ORDER BY in the plain scan's
  row order, so ties come out the same. NaN, dates and durations have no total order and
  use the plain sort. The planner picks this operator, from exact row counts per segment,
  when it reads at most half of the pattern's rows.
* **Incremental GROUP BY.** With one group key and COUNT, SUM, AVG, MIN, MAX or SAMPLE over
  variables, each group keeps a running state in a hash map keyed by id. Sums stay exact
  64-bit integers until a value is not an inline integer.
* **Count joins from key runs.** `COUNT(*)` over two scans joined on one variable reads both
  sides as (key, run length) pairs from indexes sorted on that variable and sums the
  products (`CountJoinFromRuns`).
* **Counts from statistics.** `GROUP BY ?class` with a count over `?s a ?class`, and
  `GROUP BY ?p` with a count over `?s ?p ?o`, read their counts from the index statistics
  (`GroupCountFromMetadata`, part of `metadata_counts`). The statistics describe the base
  index as the last compaction or bulk load wrote it, so the updates since then are
  applied at query time. Each inserted or deleted quad changes a quad count by one. It
  changes a distinct count only when an index probe finds no other quad that holds the
  value before or after the change. Quads of graphs the query does not read are taken out
  in the same way. The quad counts per predicate are used only when the query reads a
  single graph, because a union of graphs counts a triple once however many graphs hold
  it. The planner uses the statistics when one probe per changed value costs less than
  reading the scan. The corrected counts are kept with the snapshot, so later queries at
  the same commit reuse them. EXPLAIN notes `[from statistics, corrected for N delta
  quads]` or `[from statistics, without N quads of graphs not read]`. The
  `delta_statistics` switch limits the statistics to a store without a delta.
* **Batched path frontiers.** `p*` and `p+` traversals expand a large BFS level with one
  merged pass over the predicate's index rows instead of a seek per node.
* **Selective column decoding.** The block cache holds decoded columns, and scans decode
  only the key columns they read (variables, graph, repeated variables), which also leaves
  more room in the cache.
* **Decorrelated EXISTS.** `FILTER EXISTS { P }` and `FILTER NOT EXISTS { P }`, where `P`
  holds triple patterns, paths without `*` or `?`, `GRAPH` and deterministic FILTERs,
  evaluate `P` once and keep the distinct values of the variables the outer rows bind.
  Each outer row probes that set instead of evaluating the substituted pattern. A row that
  leaves some of them unbound (after OPTIONAL) probes the set of the bound ones; a row
  that binds a variable used only by a FILTER inside `P` is still evaluated by
  substitution. The key set is built once per query, only when `P` costs less than
  evaluating it per distinct outer key, and EXISTS stays per row when the set does not fit
  in the memory budget. EXPLAIN shows `[EXISTS decorrelated on ?y: …]` or the reason it
  was not, with `exists*` counters.
* **Anti-join MINUS** (`anti_join`). When the two sides of a MINUS share one variable that
  every row of both binds, the left rows whose value appears on the right are removed. Both
  sides sorted on the variable give a merge; otherwise the right side's values go into a
  set of ids that the left rows probe. EXPLAIN notes `[anti-join on ?p by merge]`. Other
  MINUS shapes keep the generic compatibility test.
* **Merge left joins** (`merge_left_join`). An OPTIONAL whose two sides share one
  variable, are both sorted on it and both always bind it runs as a merge. A branch-free
  zipper like the anti-join's finds where each left row's key starts on the right, and
  each left row is then written with the right rows of its key, or alone. The output keeps
  the left side's order, and the planner treats it as sorted that way. A FILTER inside
  the OPTIONAL is tested on the matched rows, and a left row none of whose matches pass
  is kept alone. At 10.5M triples, `optional-count` runs in 6 ms instead of 15 ms on a
  warm store. EXPLAIN notes `[merge on ?p]`. Other OPTIONALs keep the hash join, which
  puts the unmatched left rows last.
* **Flat hash joins** (`flat_hash_join`). A hash join groups the rows of its smaller input
  by key in flat arrays instead of a list per key. It counts each key's rows and then
  places every row, so the rows of a key are one span of an array, in row order. An input
  of 64K rows or more is first split into partitions of about 8K rows by bits of a hash
  of the key, so that each partition's table stays in the CPU caches while it is built,
  and the partitions are built in parallel. The larger input probes the table in pieces
  of 32K rows in parallel, and the pieces' pairs are joined in its order, so the output
  keeps the probing side's order. Joining a million rows with a million takes 16 to 21
  ms instead of 62 to 69 ms, and joins of inputs that fit in the caches take as long as
  before. EXPLAIN notes `[flat hash table: N keys in M parts]`.
* **Batched index joins.** When a join's input always binds a variable that a triple
  pattern can be read sorted on, and has few distinct values of it for the pattern's size,
  the pattern is read only for those values (`IndexJoin`). The sorted distinct keys become
  key ranges, and ranges in adjacent blocks are read in one scan: scattered keys cost a
  seek per region, dense keys one sweep. Each input row then joins its key's rows, so the
  input's order and duplicates are kept. The planner offers this next to merge and hash
  joins. The keys are read with one cursor over the permutation (`gallop_index_join`).
  Each key finds its first block by galloping over the blocks' first and last keys from
  the block of the key before. It finds its rows by galloping in the block the cursor
  holds, from where the key before ended, and the spans of the keys in the rows read are
  found the same way. A range with changes from updates in it is read by a scan that
  merges them. A key then takes 35 to 90 ns when the keys lie close together and 50 to
  630 ns when they are spread over the pattern, where binary searches and a scan per
  cluster of keys took 120 to 490 ns and 250 to 1250 ns. A scan reads a row in 2.2 ns on
  the same machine. The planner counts 15 scanned rows per key, plus 3.5 for each
  doubling of the pattern's rows per key, as fitted on keys spread at random. Each block
  the keys touch costs 256 and each row read 8. These figures were measured on stores of
  1.05M and 10.5M triples by the ignored tests in `sparql/costcal_tests.rs`. Probing then
  wins when the input has up to about 5% as many keys as the pattern has rows, against a
  merge join, and more against a hash join with a large input to build. With the cursor
  switched off, a key costs 140 rows plus 6 per doubling, and probing wins up to about 1
  or 2%. Probing is not offered when it would cost more than twice the scan of the
  pattern. EXPLAIN counts the keys, seeks, blocks and rows read (`batched_join`).
* **Fused stars.** Index joins on one subject over constant predicates (`?p ex:worksFor
  ex:org7 ; foaf:name ?n ; foaf:age ?a`) run as one operator (`StarJoin`). It walks each
  subject's SPO run once and picks out the star's predicates, or probes each pattern's own
  permutation, whichever touches fewer blocks, and builds the output once instead of
  through intermediate tables (`star_fusion`). The planner costs a star as its separate
  index joins, since a fused star still spends about as long per key and pattern. A fused
  star looks up every pattern for every key of its input, so each of those index joins is
  costed for the keys of the star's input, not for those left after the patterns below
  it (`fused_star_costs`).
* **Whole-block scans under graph filters.** A block slice is copied column-wise whenever
  every row passes the graph filter (one pass over the graph column), so default-graph
  queries avoid row-by-row filtering.
* **Join ordering on cost summaries** (`pruned_join_order`). The dynamic program runs on
  small summaries of plans, which hold the cost, the estimated rows, the sort variable and
  the distinct-value estimates that later joins read. The plan tree is built once, for the
  chosen joins only. A greedy plan is made first, and a partial plan that costs more than
  it is dropped, because it cannot be part of a cheaper plan. A plan sorted on a variable
  that no later join reads is dropped when another plan of the same patterns has the same
  estimates at no more cost. Only subsets whose patterns share variables are planned. Up to
  ten patterns, the result is the exhaustive program's plan or one of equal cost, except in
  rare cases where a dropped plan is the one that a later filter would have favored. Every
  WatDiv and `scripts/bench.sh` query plans at the same cost as before, and a star of nine
  patterns (WatDiv S1) plans in about a millisecond instead of 400 ms. A group whose
  subsets have too many splits to enumerate in about a millisecond is planned in rounds.
  Each round plans the subsets up to the size that fits, and the cheapest plan of that
  size becomes one input of the next round. That plan never costs more than the greedy
  one. Groups of more than 16 patterns keep the greedy plan, which costs each pair of
  plans once. Switched off, the program builds every candidate plan tree for every split
  of up to 12 patterns, and larger groups are planned greedily.
