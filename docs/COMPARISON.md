# Sparkles compared

This page compares Sparkles with Apache Jena/Fuseki, QLever, Fluree and Oxigraph. It lists
what each engine has that Sparkles lacks and the places where Sparkles departs from Jena
and QLever on purpose. [ARCHITECTURE.md](ARCHITECTURE.md) explains the engine's design,
and the [query optimization reference](internals/OPTIMIZATIONS.md) records its detailed
fast paths. Benchmark results, including the
queries Sparkles loses, are in [BENCHMARKS.md](BENCHMARKS.md#where-sparkles-loses). The
full feature list is in [FEATURES.md](FEATURES.md).

Sparkles is an unreleased development build. Comparisons describe the scope documented
here, rather than guaranteeing compatibility with every version or configuration of
another engine. The [benchmark reference setup](benchmarks/reference/README.md) names
the versions used for measured comparisons.

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
| Full-text search | `jena-text` (Lucene), `text:query` | `text:query` with Lucene query syntax, Jena highlighting and per-language stemming via Tantivy (BM25), updated on commit. Multi-field entity documents are not supported. |
| Spatial | GeoSPARQL functions, property functions, a spatial index and CRS conversion through Apache SIS. | GeoSPARQL 1.1 functions, a spatial index and supported Jena spatial functions, with narrower CRS and geometry coverage. See [GeoSPARQL](search/README.md#geosparql). |
| Shape languages | ShEx (`jena-shex`) and SHACL Compact Syntax (SHACLC) as an RDF syntax. | SHACL (including SHACL 1.2 node expressions) and ShEx 2.2 (`EXTENDS`, `ABSTRACT`, SPARQL selectors). SHACLC is accepted for shapes and drafted output, not data endpoints. |
| Inference | On-the-fly inference, forward and backward rules, OWL profiles and RDFS on read. | Incremental materialization (RDFS, OWL 2 RL, Jena, Datalog, SHACL-AF, SPIN), RDFS on read, demand derivation, `reasoning=auto`, OWL 2 QL and `@hybrid jena`. Derived layers and per-pattern `auto` selection remain open ([reasoning](reasoning/README.md#reasoning)). |
| Ontology API | `jena-ontapi` `OntModel` | OWL 2 axiom APIs across bindings, HTTP, MCP and CLI, with functional and Manchester syntax plus EL, Horn and ALCHI classification. JVM `SparklesOnt.model` bridges to supplied `jena-ontapi`, with no native `OntModel` ([OWL 2 ontologies](reasoning/README.md#owl-2-ontologies)). |
| Parameterized queries | `ParameterizedSparqlString` and Java substitution. Fuseki stores no queries. | Versioned stored queries per dataset (`/$/queries/{ds}`, `sparkles queries`) run at `/{ds}/queries/{name}` with typed term parameters and MCP exposure. `querybuilder::set_var` covers Java substitution. |
| Linting | ✗ (`qparse` and `riot` report syntax errors only) | `sparkles lint` checks SPARQL, RDF and shapes syntaxes for common issues, applies safe fixes and checks supported SHACL query literals ([limits](querying/README.md#sparql-and-formats)). |
| Shape drafting | ✗ | Drafts SHACL shapes and ShEx schemas from data at `/$/schema/{ds}/shapes`, reporting support thresholds and excluded instance counts. |
| SPARQL extensions | ARQ property functions, syntax extensions, aggregates and function libraries. | Selected ARQ syntax, functions and property functions, including `LATERAL` and composite datatypes. JavaScript functions and some ARQ extensions remain unsupported ([ARQ extensions](querying/README.md#jena-arq-extensions)). |
| Service description | Fuseki returns `404` for a GET of the query endpoint without a query. | SPARQL 1.1 Service Description covering query/update endpoints, extension functions, aggregates, entailment and default dataset, filtered by caller grants ([API.md](API.md#service-description)). |
| Extension points | Registries for functions, property functions, aggregates, SERVICE executors, DESCRIBE handlers, algebra transforms and query engine factories. | Per-query Rust registries for scalar functions, aggregates and property functions with budgeted callbacks ([P03](specs/P03-query-extensions.md)), plus three built-in DESCRIBE modes ([API.md](API.md#describe)). No general server plugin interface. |
| SPARQL parser | JavaCC grammar | Vendored `spargebra` 0.4.7 with patches for W3C conformance failures ([`vendor/spargebra/PATCHED.md`](../vendor/spargebra/PATCHED.md)). |
| RDF formats | W3C syntaxes plus RDF Thrift, RDF Protobuf, TriX and RDF/JSON. | W3C syntaxes plus TriX, RDF Thrift, RDF Protobuf and RDF/JSON streamed directly across server, CLI, `LOAD` and bindings. Server also writes SPARQL Results Thrift ([FEATURES.md](FEATURES.md#known-gaps)). |
| Tabular data | No built-in CSV reader; tables convert through external tools such as Tarql or CSVW processors. | `load`, `csv` and uploads map CSV/TSV via default, W3C CSVW (with key checks) or Tarql templates ([C05](specs/C05-tabular-imports.md)). `sparkles map` runs R2RML, RML and YARRRML over CSV, TSV, JSON, Parquet and SQLite ([F19](specs/F19-mappings-and-virtual-graphs/README.md)). |
| Change logs | RDF Patch (`jena-rdfpatch`) writes, applies and logs changes to rotating files; Fuseki `patch` applies text patches. | Text and RDF Thrift patches for commit diffs, long-poll/SSE change feeds and `/{ds}/patch` commits with `prev` preconditions. Rotating patch log files are not supported ([RDF Patch](storage/README.md#rdf-patch)). |
| Fuseki operations | Shiro auth, `fuseki-access` graph ACLs, Prometheus `/$/metrics`, assembler `config.ttl`, `/$/validate/*` and prefix endpoints. | Basic, Bearer, OIDC, Cloudflare Access and proxy auth with rustls HTTPS (`--tls-cert`), plus graph, endpoint and triple protections ([access control](server/README.md#access-control-and-security)). Prometheus `/$/metrics` (optional `--metrics-fuseki-names`, no JVM metrics) and `/{ds}/prefixes` are included. |
| SERVICE | Correlated, bulk and cached `SERVICE` (`loop:`, `bulk:`, `cache:`, `urn:x-arq:self` via `serviceenhancer`), with slice-aware caching and endpoint limit learning. | Same options and results ([API.md](API.md#service-options-loop-bulk-and-cache)), sending bulk VALUES or UNION batches with single-input fallback on truncation. Cache is caller-isolated without slice-aware pages or management functions ([G10](specs/G10-service-enhancer.md)). |
| Transactions over HTTP | — | — (as in Fuseki, one request is one transaction) |
| Backups | `/$/backup/{ds}` writes a gzipped N-Quads dump locally, restored by reloading into a new dataset. | Same dumps (zstd default; gzip, brotli and LZ4 optional), plus incremental deduplicated filesystem or S3 repositories with direct restore, verification, schedules and retention ([backups](operations/README.md#backups)). |
| Admin API | `/$/datasets` (including assemblers and `?state=offline`), `/$/server`, `/$/backup`, `/$/backups-list`, `/$/compact` (`deleteOld`), `/$/tasks`, `/$/stats`, `/$/ping`, `/$/metrics` and `/$/validate/*`. | Same routes and Fuseki JSON shapes, plus commits, snapshots, schema, reasoning, validation, indexes, quotas and clones ([API.md](API.md#datasets-admin)). Refuses unsupported assembler parts and `deleteOld=false`; offline state is unpersisted and `/$/validate/query` omits optimized algebra. |
| Graph Store naming | Indirect (`?graph=`, `default`, `union`) and per-service direct naming where the request URL names the graph. | Both modes, with `?graph=union` read-only as in Fuseki and per-dataset direct naming via `serve --gsp-direct-naming`. |
| Fuseki modules | Fuseki Main loads modules (admin, UI, Shiro, Prometheus, graph access, GeoSPARQL) via `ServiceLoader` or `--modules`. | Equivalent capabilities are compiled in and enabled via cargo features and CLI flags, without a plugin interface. |
| Command-line tools | `riot`, `arq`/`sparql`, `qparse`, `uparse`, `update`, `rsparql`, `rupdate`, `rset`, `rdfdiff`, `infer`, `shacl`, `shex`, `rdfpatch`, `iri`, `langtag`, `schemagen` and `tdb2.*`. | Subcommands cover `tdb2.*`, `arq`, `riot`, `qparse`, `uparse`, `rdfdiff`, `infer`, `shacl`, `shex`, `iri`, `langtag`, `rsparql`, `rupdate`, `rset`, `rdfpatch` and `patch` ([command line](querying/README.md#command-line)). `qparse` prints `spargebra` algebra and physical plans; `schemagen` is absent. |
| Configuration | Assembler files (`config.ttl`, `run/configuration/*.ttl`) define services, datasets, indexes, inference and access control; users live in `shiro.ini` or password files. | Server flags, per-dataset settings and TOML auth config. `sparkles config import fuseki`, `serve --fuseki-config` and `POST /$/datasets` translate supported Fuseki assemblers and report unmapped elements ([G08](specs/G08-fuseki-configuration.md)). |
| Storage | TDB2 on copy-on-write B+trees (TDB1 is deprecated). | Sorted compressed permutation files with a delta and WAL. Neither TDB format is read directly; data moves via N-Quads or RDF dumps. |
| RDFConnection / RDFLink | Unified query, update, Graph Store and transaction API over local or remote datasets (`RDFConnectionRemote`, `RDFConnectionFuseki`). | Local `Dataset` (Rust, Python) and streaming Rust `sparkles-client` (ETag concurrency, single-request updates; no Python remote client). Jena's own remote HTTP clients pass `mise run test:jena-clients` against the server ([libraries](integrations/README.md#libraries-and-bindings)). |
| Query builder | `jena-querybuilder` (`SelectBuilder`, `ConstructBuilder`, `AskBuilder`, `DescribeBuilder`, `UpdateBuilder`, `WhereBuilder`, `ExprFactory`) | `sparkles::querybuilder` provides the same builders, typed terms, escaped literals and `set_var`. |
| Commons RDF | `jena-commonsrdf` adapts Jena terms to Apache Commons RDF. | Terms use `oxrdf` in Rust and convert to and from `rdflib` terms in Python. |
| IRIs and language tags | `jena-iri3986` checks RFC 3986/3987 and scheme rules with configurable severities; `jena-langtag` canonicalizes BCP 47 tags. Parsers warn and `/$/validate/*` reports them. | `oxiri` and `oxilangtag` reject malformed syntax on parse; opt-in scheme (`http`, `https`, `urn`, `file`, `did`), normalization and BCP 47 checks run in CLI tools and `/$/validate/*`. Severities are fixed, with no registry. |

### vs. QLever

| Area | QLever | Sparkles |
|---|---|---|
| Scale | Tested to tens of billions of triples (Wikidata, UniProt). | Measured up to 1.24B input triples on English DBpedia; see [DBpedia results](benchmarks/dbpedia/RESULTS.md#dbpedia-at-124-billion-triples) for timings, index size and measurement limits. |
| Streaming execution | Lazy block-wise scans, joins, filters and GROUP BY, streamed to clients. | Automatic mode streams large scans (≥1,000 rows), large groupings and memory-heavy plans while running small plans eagerly, with default disk spill and optional encryption ([ARCHITECTURE.md](ARCHITECTURE.md#eager-execution-and-cursors-have-different-memory-costs), [memory report](benchmarks/memory/RESULTS.md#memory-and-the-speed-it-buys)). |
| Block prefiltering | FILTER ranges and `STRSTARTS` checked against block min/max to skip blocks. | Inline numeric range scans, vocabulary prefix/id ranges for `STRSTARTS` and anchored `REGEX`, selective column decoding and a numeric value column ([optimization reference](internals/OPTIMIZATIONS.md)). |
| Pattern trick | `ql:has-predicate` and per-subject predicate patterns. | Per-subject predicate patterns for default and union graphs, adjusted for pending writes, serving `spk:hasPredicate`, `ql:has-predicate`, subject counts and `SELECT DISTINCT ?p` ([X11](specs/X11-index-compression-and-predicate-patterns.md#outcome)). |
| Text and spatial | `ql:contains-word`, BM25 scoring, spatial joins and a geo index. | BM25 search via `text:query` (no text/entity co-occurrence index), plus GeoSPARQL functions, a spatial index, spatial joins and nearest-neighbour `ORDER BY`. |
| Path search | `pathSearch` service returning all paths with nested subquery edges, edge properties and depth or per-target caps. | `SERVICE path:search` returns one shortest, all shortest, k-shortest or length-bounded paths with Dijkstra weights over predicates, all triples or nested patterns ([F07](specs/F07-path-search.md)). |
| Vocabulary compression | FSST string compression and numeric IRIs encoded as ids. | Front coding by default, with optional FSST suffixes saving about 30% of vocabulary size; IRIs are not encoded. |
| Pinned results, materialized views | `pin-result-with-name` and materialized views. | Result cache and incremental table views via `SERVICE view:read`, plus SSE live queries. Named pins, graph views and commit-time maintenance are unavailable. |
| Live query monitoring | Runtime updates over a websocket. | Executed plan returned after completion. |

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
| History | Immutable content-addressed commit chain queried by transaction, time, commit or writer event time, with history queries and branch rebase, merge and revert. | Durable commits, point-in-time reads while retained, snapshots, diffs, change feeds and history queries. Memory and persistent branches support three-way or replayed merges, reverts and cherry-picks, without a separate rebase verb ([USAGE.md](USAGE.md#branches-and-merges)). |
| Security | In-ledger access policies, JWS/`did:key` signed requests and commits, OIDC and encryption at rest. | Dataset, graph, triple, branch and endpoint grants with Basic, token, OIDC and proxy auth. Live files are unencrypted and unsigned; backup repositories support encryption ([FEATURES.md](FEATURES.md#known-gaps)). |
| Verifiability | JWS/`did:key` signed requests, transactions and commits, checked by `fluree verify`. | Optional SHA-256 commit change digests (`--commit-digests`) and WAL/continuity verification via `sparkles check`; nothing is signed. |
| Interfaces | SPARQL, JSON-LD query/transaction language, openCypher (Bolt) and schema-derived GraphQL, with NDJSON streaming and multi-query snapshots. | SPARQL, Graph Store, Rust/Python/JVM/Node.js APIs, MCP (stdio/HTTP) and read-only GraphQL over a reviewed SDL mapping. JSON-LD is an RDF syntax only; no JSON query language or Cypher. |
| Writes | SPARQL Update plus JSON-LD, Turtle and TriG insert, upsert, sync and staged transaction previews. | SPARQL Update, Graph Store writes (`PUT` records net quad changes; `If-Match` preconditions), uploads, bulk loads and dry-run previews reporting changes, validation and quota effects. No upsert verb, as SPARQL Update expresses it. |
| RDF 1.2 | Annotations (`{\| \|}`) and reifiers bridged to Cypher properties; triple terms only as `rdf:reifies` objects without triple functions, and base direction stored in language tags. | Full RDF/SPARQL 1.2 support across all syntaxes (269/269 W3C tests), including triple terms in any position, annotations, reifiers, base direction and triple functions. No property-graph view. |
| Ledgers and datasets | Branched ledgers (`name:branch`) in a queryable file, S3 or DynamoDB nameservice, with reserved graphs for config and commit metadata. | Fuseki-style named datasets with durable UUIDs and commit sequences via `/$/datasets`, `/$/commits`, catalog APIs and CLI. `SERVICE hist:changes` queries commit changes in SPARQL ([API.md](API.md#history-queries)), without a general commit catalog view. |
| Multi-source queries | Multi-ledger `FROM` or local `SERVICE`, plus search indexes, Iceberg/Delta Lake tables and R2RML SQL graphs queried in place. | Queries read one dataset and federate via HTTP `SERVICE`. R2RML, RML, YARRRML and CSVW import CSV, TSV, JSON, Parquet and SQLite at load time rather than querying virtual graphs in place. |
| Search | Indexed or inline BM25 full-text, optional HNSW vector similarity, point/S2 spatial indexes and `geof:distance`. | Stemmed BM25 (`text:query`), exact and HNSW vector search with commit-time embeddings, reciprocal rank fusion and indexed GeoSPARQL 1.1 with spatial joins and nearest neighbours. Vector indexes lack quantization ([search](search/README.md#search)). |
| Reasoning | Per-query or per-ledger RDFS and OWL 2 QL rewriting, plus budgeted query-time OWL 2 RL and Datalog overlays with `owl:imports`. | Incremental materialization (RDFS, OWL 2 RL, Jena, Datalog, SHACL-AF, SPIN), RDFS on read, OWL 2 QL, demand derivation, `reasoning=auto`, `owl:imports` and EL/Horn/ALCHI classification. Per-pattern `auto` selection and complete OWL 2 DL remain open ([reasoning](reasoning/README.md#reasoning)). |
| Validation | Transaction-time and endpoint SHACL validation plus property uniqueness constraints. | W3C-passing SHACL Core and SHACL-SPARQL plus ShEx 2.2 on demand or as incremental write guards (reject, warn or block new violations). SHACL-SPARQL expresses uniqueness constraints. |
| Configuration | Policy, SHACL, reasoning, rule and uniqueness settings live as versioned triples in each ledger's config graph. | Server flags and per-dataset JSON settings files managed via admin API and CLI, outside dataset commit history. Stored queries keep their own version history. |
| Deployment | Memory, file, S3/DynamoDB or IPFS storage with Raft write clusters, event-stream query peers and ledger push/pull. | Single node on local disk via binary, Docker or NixOS, with incremental filesystem/S3 backups. The change feed streams commits as RDF Patch, without replication apply. |
| AI agents | MCP schema and byte-budgeted SPARQL tools, CLI docs and coding-assistant memory tools, plus PDF, Office and Markdown chunking with embeddings. | Scoped MCP profiles, cited memory, Ask pipeline, GraphRAG reports and Markdown, HTML, text, PDF (optional OCR) and Office ingestion with branch or claim review. Images and audio are not ingested ([AI and agents](ai/README.md#ai-and-agents)). |
| Observability | OpenTelemetry traces, structured logs, health/stats endpoints and per-query time and fuel limits; Prometheus planned. | Access log, Prometheus metrics, readiness checks, OpenTelemetry traces, metrics and logs, plus per-query memory, row and work budgets. |
| Embedding | Async Tokio Rust API over memory, file or S3 storage across feature-flagged crates. | Synchronous Rust library (no HTTP/async deps), Python package, Jena-compatible JVM `DatasetGraph` and Node.js/TypeScript packages, with reasoner, validators, formatter and server as separate crates. |

Sparkles is ahead on:
* **Conformance.** Sparkles passes the W3C SPARQL 1.0, 1.1 and 1.2 suites in full and
  the SHACL Core and SHACL-SPARQL suites (98/98, 20/20). Fluree's documentation reports
  that it runs the SPARQL 1.1 suite with some tests registered as not supported, such as
  those that keep a time zone offset, and that it does not run the SPARQL 1.2 suite.
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
| Embedding | Rust library, Python (`pyoxigraph`), JavaScript/WebAssembly packages and an in-memory store. | Persistent or in-memory Rust, Python (`pyoxigraph`-style API plus transactions, reasoning, validation, history and `rdflib` plugin), JVM (`sparkles-jena`) and Node.js packages, unpublished to registries and without WebAssembly ([libraries](integrations/README.md#libraries-and-bindings)). |
| Storage | RocksDB LSM tree with 9 index orders (6 named, 3 default), a string dictionary, in-place updates and local hard-linked checkpoint backups. | Immutable sorted blocks in 7 orders with an in-memory WAL-backed delta merged by compaction, plus incremental deduplicated filesystem or S3 backup repositories with verification, schedules and retention. |
| Spatial | GeoSPARQL functions (`spargeo`, enabled in CLI) without a spatial index. | GeoSPARQL 1.1 functions (geodesic measures, EPSG:4326 axis order, metric buffers) and a per-dataset spatial index. |
| Write durability | One RocksDB transaction per request written to RocksDB's WAL without `fsync` (default). | Commits `fdatasync` the WAL before acknowledgement, syncing new-term files in parallel and adding one sync when commit messages or change digests are enabled. |

The [query measurements](benchmarks/synthetic/RESULTS.md#synthetic-suite-at-105m-triples),
[write measurements](benchmarks/writes/RESULTS.md#writes-and-updates) and
[binding comparisons](benchmarks/bindings/RESULTS.md#bindings) record the workload-specific differences.

Oxigraph has no reasoning, SHACL, full-text or vector search, path search, CSV imports,
point-in-time reads, authentication or per-dataset permissions, Fuseki admin API, GraphQL
endpoint, query budgets, result cache or web UI.

## Divergences from Jena / QLever (decisions)

### General

| Decision | Rationale |
|---|---|
| Rust instead of Java or C++ | [AUDIT.md](AUDIT.md#3-language-decision-rust) §3: `spargebra` and `oxttl` supply the RDF/SPARQL parser stack without garbage collection, giving predictable performance. |
| Sorted-block permutations instead of TDB2's B+trees | Scan-heavy analytics run faster with smaller files; updates go to an in-memory delta merged by compaction rather than updating indexes in place. |
| Inline only canonical lexical forms | QLever inlines lossily, dropping 4 mantissa bits and lexical forms. Sparkles preserves exact RDF term identity (`"01"^^xsd:integer` ≠ `"1"^^xsd:integer`) like Jena, storing non-canonical or high-precision doubles in the vocabulary. |
| Distinct lexical forms remain distinct terms even with equal numeric values | RDF 1.1 identifies literals by lexical form and datatype, matching Jena rather than QLever's float/double folding. On English DBpedia Sparkles counts 1,014,683,275 triples vs. QLever's 1,014,683,273 due to two such pairs. |
| Graph as 4th key column in every permutation plus GSPO | Matches QLever's graph column while GSPO provides TDB2-style graph-scoped scans for dumps and `GRAPH ?g {}`. |
| Deltas are persistent B+ tree sets logged to the WAL | Snapshots publish in O(1) for MVCC; per-block delta indexing like QLever's remains a future option. |
| Blank nodes are stored ids serialized as `_:b<hex>` | Stored blank nodes retain stable labels across requests and query bindings, while query-created blank nodes serialize as `_:q<hex>` per result. |
| LZ4 index blocks and front-coded vocabulary instead of zstd and FSST | Prioritizes query decode speed while using zstd for backups, dumps, HTTP and full-text storage. Optional `--vocab-codec fsst` saves ~30% of vocabulary; zstd blocks would save 12%–19% of the index. |
| Canonical decimal output follows XSD 1.1 (`"4"^^xsd:decimal`) rather than Jena's `"4.0"` | Inherited from `oxsdatatypes`; the numeric values are equal, so value-based result comparisons are unaffected. |
| SPARQL parsing through vendored `spargebra`, not ARQ's JavaCC grammar | Supplies SPARQL 1.1/1.2 algebra and ARQ syntax (`LATERAL`, `LET`, `FOLD`, `UNFOLD`, aggregates, path ranges, `CONSTRUCT` with `GRAPH`) beside `apf:` functions ([API.md](API.md#arq-syntax-extensions)). `p{0,}` matches zero length like `p{*}`, unlike Jena 6.2.0. |
| `agg:median` and `agg:mode` act as aggregates beside `MEDIAN` and `MODE` | ARQ recognizes only the keywords and treats the IRIs as unknown functions. Sparkles accepts the IRIs so parsed queries round-trip back to keyword syntax. |
| `MODE` with no repeated values returns the first value in row order | Every value ties at count 1 and the first wins under ARQ's own rule, whereas ARQ throws `NumberFormatException` (and fails all `MODE(DISTINCT …)` queries). |
| Duration accessors (`fn:years-from-duration`, etc.) normalize components and preserve sign | Follows F&O 3.1 §8.2 (`PT90M` yields 1 hour, 30 minutes). ARQ returns raw unsigned fields, giving 90 minutes for `PT90M` and 0 hours for `-PT90M`. |
| Casts from `xsd:dateTime`/`xsd:date` to Gregorian types keep the timezone | Follows F&O 3.1 §19.1.5 (`"2024-05-06T10:00:00Z"` casts to `"--05-06Z"`), whereas ARQ drops the timezone (`"--05-06"`). |
| `xsd:unsignedByte(…)` is a supported cast | ARQ omits `unsignedByte` from its derived integer casts and rejects it as an unknown function. |
| Filter placement and equality substitution run in the planner, not as algebra transforms | Matches ARQ's `TransformFilterPlacement` and `TransformFilterEquality` with one fewer pass over the algebra. |
| `REDUCED` is a no-op | Permitted by the SPARQL specification. |
| `DESCRIBE` includes reifier descriptions by default (W3C CBD) | Jena's `DescribeBNodeClosure` omits reifiers, dropping triple annotations. Results match without reifiers, and `"reifiers": false` reproduces Jena on any dataset. |
| `DESCRIBE` without a dataset clause reads the default graph even under `unionDefaultGraph` | Matches Jena's TDB2 handler behavior, reading the default graph and each named graph containing the resource individually. |
| `GRAPH ?g { P }` binds `?g` as a scan column for plain join groups | Optimizes the common case while falling back to per-graph evaluation (like Jena's `OpGraph`) to preserve exact scoping for `OPTIONAL` and `MINUS`. |
| `GROUP_CONCAT` always returns a simple literal | Follows the SPARQL specification; Jena preserves a shared language tag. |
| Triple terms live in the vocabulary with canonical nested keys and hidden-variable decomposition | Keeps 64-bit ids and permutations unchanged; ordering triple terms between literals and IRIs keeps term-kind checks O(1) while preserving blank-node identity. |
| Effective boolean value of an ill-typed boolean or numeric literal is an error | Follows SPARQL 1.2 §17.2.2; SPARQL 1.1 returned `false`. |
| Materialized forward chaining into `urn:x-sparkles:inferred` is the default reasoning mode | Materialized queries run at index speed with commit-freshness tracking and `--auto-reason` updates, while opt-in RDFS on read, OWL 2 QL, demand derivation (`@hybrid jena`) and `reasoning=auto` cover on-the-fly inference within budgets. |
| Rejects `AS ?v` targets already in scope (SPARQL §18.2.1) | `spargebra` omits this check, so Sparkles enforces it to match Jena and QLever. |
| Default stemming uses Tantivy Snowball stemmers instead of Lucene language analyzers | Lucene-compatible analyzers for nine languages plus CJK are opt-in (auto-selected on Fuseki import), matching Jena on 1,576/1,590 stemmed queries vs. 1,302 by default; the remaining 14 stop-word-only queries return `400` instead of all literals. |
| Literal language matching uses the primary subtag (`en-GB` matches `lang:en`) | Jena matches language tags verbatim, excluding `en-GB` from `lang:en`. |
| Full-text index commits lazily on text reads or a 1 s tick | Amortizes expensive Tantivy segment flushes across write bursts while preserving exact snapshot visibility and WAL crash recovery; Jena commits per transaction. |
| N-Quads backups default to zstd level 3 (`.nq.zst`) instead of Fuseki's gzip (`.nq.gz`) | At 10.5M triples, zstd runs 5× faster (8.2 s, 81.5 MB) than gzip level 6 (41 s, 74.9 MB). `?compression=gzip` / `--compress gzip` selects Fuseki's format, and loaders read both. |

### Server defaults

| Decision | Rationale |
|---|---|
| `serve` binds `127.0.0.1` by default and requires `--allow-open-network` (or `SPARKLES_ALLOW_OPEN_NETWORK=1`) for non-loopback binds without `--auth-config` | Unauthenticated servers grant full admin access, so network exposure must be explicit (unlike Fuseki's all-interface bind) and logs warnings when open or unthrottled. |
| Unauthenticated servers disable CORS unless `--cors-origin` is set, block cross-site writes and validate `Host` headers | Prevents arbitrary web pages or DNS rebinding from reading, writing or `LOAD`ing local files on an open admin server, unlike Fuseki's open CORS default. |
| `--max-export-mb` defaults to `0` (unlimited), while query responses cap at 1 GiB (`--max-result-mb`) | Graph Store `GET` streams dataset exports from one snapshot without truncating legitimate dumps. Exposed servers should set `--max-export-mb` and rate-limit the `query` class. |
| Automatic background compaction runs by default (`--no-auto-compact` disables it), unlike TDB2's blocking manual compaction | Keeps statistics and scans fast as deltas grow ([compaction](storage/README.md#compaction-clones-and-integrity)), pausing writes only milliseconds at switch while trading throttled rebuild I/O and a full next incremental backup. |
| Client `timeout=` is capped at `--max-timeout` (default 1800 s); queries default to 60 s and writes have no default deadline (`--update-timeout 0`) | Prevents runaway requests from holding workers indefinitely while letting long loads finish unless the client disconnects, which cancels the write. |

### GeoSPARQL

| Decision | Rationale |
|---|---|
| Geographic distances, lengths and areas use WGS 84 geodesic math (Karney); `geo.json` `"distance": "haversine"` selects Jena's sphere | Up to 0.5% more accurate than Jena's spherical great-circle formula (R = 6,371,008.7714 m) at a small per-call cost. |
| Literals follow CRS axis order (EPSG:4326 is lat/lon); legacy `…/def/crs/EPSG/4326` maps to CRS84 as in Jena | Follows GeoSPARQL Req 16, with `minX`…`maxY` reporting the literal's own axes as in Jena. |
| Unknown CRSs remain valid geometries for same-CRS planar operations, return type errors for metric or mixed-CRS calls and are skipped by the index | Avoids Jena's silent fallback of treating unknown-CRS coordinates as CRS84 degrees. |
| Relations follow DE-9IM: empty geometries are disjoint (`sfDisjoint` true), equal points satisfy `sfEquals`, curve `sfCrosses` is `0********` and RCC8 applies to regions only | Jena returns false for all relations on empty geometries and uses `TFFFTFFFT` for `sfEquals`, making equal points unequal. |
| Spatial query rewrite is opt-in per dataset (`geo.json` `"queryRewrite": true`) | Unlike Jena's enabled default, enabling a spatial index does not silently alter asserted-triple queries or add unexpected spatial join costs. |
| `geof:getSRID` returns `xsd:anyURI`; `geof:dimension` on empty geometries returns the type's dimension (`-1` for empty collections) | Follows the GeoSPARQL 1.1 signature (Jena returns `xsd:string`) without raising type errors on empty geometries. |
| `geof:concaveHull` maps `targetPercent / 25` to concaveman concavity; `geof:aggConcaveHull` uses the default; `spatialF:angle` is clockwise from the y axis in all quadrants | GeoSPARQL leaves hull concavity implementation-defined, SPARQL aggregates take one expression, and Jena's `angle` is off by 90° in two quadrants. |
| Extra CRSs load from `--geo-crs` proj4 files via `proj4rs` or the server's EPSG table (`geo-epsg` feature in library); datum-shift grids are refused and Helmert shifts warn | Replaces Jena's Apache SIS EPSG database while keeping EPSG license terms opt-in for library embedders. |
| `infer --vocab geosparql` derives geometry types (`sf:Polygon`, `gml:Polygon`) from literal keywords beside the vocabulary hierarchy | Allows `?g a sf:Polygon` to match WKT/GML literals directly without parsing coordinates. |
| Spatial indexes are opt-in per dataset (`geo.json`); `geof:` works without one and refines every index hit exactly | Preserves identical answers with or without an index, unlike Jena's startup-wide index and envelope-only `spatial:withinBox` / `intersectBox` hits. |

### Out of scope for v1

The original v1 exclusions are recorded in [AUDIT.md](AUDIT.md#5-explicit-non-goals-for-v1),
and Sparkles implements several of them. The remaining [known gaps](FEATURES.md#known-gaps)
include JavaScript functions, a native Jena `OntModel` implementation without
`jena-ontapi` (the JVM binding bridges to `jena-ontapi`'s `OntModel` through
`SparklesOnt.model`), Lucene index-file compatibility and Shiro as an authentication
framework. Demand derivation supports Jena backward and hybrid rules within the
[documented limits](reasoning/README.md#reasoning). `sparkles config import fuseki`
translates supported Shiro users and URL rules into Sparkles' auth configuration.

## Optimizations adopted from QLever

The adopted storage and execution mechanisms are explained in
[ARCHITECTURE.md](ARCHITECTURE.md#storage-immutable-indexes-with-transactional-overlays),
including tagged ids, sorted vocabularies, permutation blocks, MVCC overlays,
columnar execution and join planning.

## Further scan and filter optimizations

The [query optimization reference](internals/OPTIMIZATIONS.md) records the detailed
operator fast paths, applicability rules, switches and implementation measurements.
[ARCHITECTURE.md](ARCHITECTURE.md#query-execution-and-where-optimizations-live) shows
where they sit in the query pipeline, and [BENCHMARKS.md](BENCHMARKS.md) owns the
performance comparisons.
