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
| Full-text search | jena-text (Lucene), `text:query` | A `text:query` subset over string literals (Tantivy, BM25), updated on commit. No highlighting, per-language stemming or multi-field entity documents. |
| Spatial | GeoSPARQL 1.0/1.1: `geof:` and `spatialF:` functions, `spatial:` property functions over a spatial index, query rewrite of the topological properties, RDFS entailment of the geometry hierarchy, GML and KML literals, EPSG CRSs through Apache SIS | The GeoSPARQL 1.1 `geof:` functions over WKT and GeoJSON literals in the built-in CRSs and the 120 UTM zones; Jena's `spatial:` property functions and `spatialF:` filter functions; a per-dataset spatial index used by FILTERs, property functions, spatial joins and nearest-neighbour ORDER BY; query rewrite (off by default) and RDFS entailment of the geometry hierarchy (`--vocab geosparql`). No geometry-type entailment, GML/KML literals or EPSG database ([AUDIT.md](AUDIT.md#5-explicit-non-goals-for-v1) §5). |
| Shape languages | ShEx (jena-shex) | SHACL, and ShEx 2.1 (ShExC, ShExJ, ShExR, SPARQL selectors). No ShEx 2.2. |
| Inference | On-the-fly `InfModel`, backward and hybrid rules (LP engine), OWL Micro/Mini/Full | Forward materialization only (RDFS, an OWL 2 RL subset, Jena forward rules). Not incremental: after an update the inferences are marked stale and recomputed in full, on request or automatically with `--auto-reason`. Inconsistency checks cover a fixed subset of the OWL 2 RL `false` rules (`owl:Nothing`, `disjointWith`, `AllDisjointClasses`, sameAs/differentFrom, functional literals), not full consistency. |
| Ontology API | jena-ontapi `OntModel` | ✗ (triples and SPARQL only) |
| SPARQL extensions | Property functions (`list:member`, `apf:*`), `LET`, custom aggregates (`MEDIAN`, `MODE`, `FOLD`), `cdt:` list/map literals, JavaScript functions, the full `afn:`/`fn:` library | ✗ (the common `fn:`, `afn:` and `math:` functions only) |
| SPARQL parser | JavaCC grammar | `spargebra` 0.4.7, vendored with fixes for the W3C tests it failed ([`vendor/spargebra/PATCHED.md`](../vendor/spargebra/PATCHED.md)) |
| RDF formats | RDF Thrift, RDF Protobuf, TriX, RDF/JSON | ✗ (Turtle, N-Triples, N-Quads, TriG, RDF/XML and JSON-LD only) |
| Change logs | RDF Patch (jena-rdfpatch), Fuseki `/patch` | ✗ |
| Fuseki operations | Shiro authentication, per-graph access control (fuseki-access), Prometheus `/$/metrics`, assembler (`config.ttl`) service definitions, `/$/validate/*`, prefix endpoints | Basic and Bearer tokens, OIDC sign-in for the UI and trusted proxy headers, with per-dataset access levels; no per-graph ACLs. Prometheus `/$/metrics` with Sparkles metric names (not `fuseki_requests_*`) and no JVM metrics. Datasets are configured by CLI flags and the admin API. Prefixes through `/{ds}/prefixes`. |
| SERVICE | Bulk, batched and cached SERVICE (serviceenhancer) | Plain SERVICE only |
| Transactions over HTTP | — | — (as in Fuseki, one request is one transaction) |
| Backups | `/$/backup/{ds}`: a gzipped N-Quads dump of the whole dataset in the server's directory, restored by loading it into a new dataset | The same dumps, zstd by default (`?compression=gzip` gives Fuseki's `.nq.gz`; brotli and LZ4 also work). Also backup repositories on a file system or S3: incremental, deduplicated backups that restore to a ready database without a reload, with verification, schedules and retention. |

### vs. QLever

| Area | QLever | Sparkles |
|---|---|---|
| Scale | Tested to tens of billions of triples (Wikidata, UniProt) | Tested to 10.5M. The external-sort path has tests but no measurements at 100M+. |
| Streaming execution | Lazy, block-wise scans, joins, filters and GROUP BY; results streamed to the client | Every operator materializes its result, within row and memory budgets. Responses over 1 MiB are streamed as they are serialized. |
| Block prefiltering | FILTER ranges and STRSTARTS checked against block min/max to skip blocks | Numeric range FILTERs on a scan's sort column read only the matching id ranges (inline integers and decimals). Non-canonical numerals are tested row by row. |
| Pattern trick | `ql:has-predicate`, per-subject predicate patterns | ✗ (predicate counts come from index runs) |
| Text and spatial | `ql:contains-word`, BM25 scoring, spatial joins, a geo index | BM25 search through `text:query` (no text/entity co-occurrence index). GeoSPARQL functions, a spatial index, spatial joins and nearest-neighbour ORDER BY. |
| Vocabulary compression | FSST string compression, numeric IRIs encoded as ids | Front coding; no IRI encoding |
| Pinned results, materialized views | `pin-result-with-name`, materialized views | A result cache, without pinning |
| Live query monitoring | Runtime updates over a websocket | The executed plan, after completion |

In the other direction, Sparkles keeps Jena behaviour that QLever does not aim for: exact
term identity (no lossy inlining), the Graph Store Protocol, Fuseki's endpoints and admin
API, materialized reasoning, SHACL validation and an embeddable library.

### vs. Fluree

[Fluree DB](https://github.com/fluree/db) 4.x is the closest peer: a Rust RDF database with
a SPARQL 1.1 endpoint, a compressed columnar index and in-memory novelty over immutable
index files. It is built as a versioned, permissioned ledger with JSON-LD as its main
interface. Fluree is licensed under BUSL-1.1 (free except as a hosted database service;
each release becomes Apache-2.0 after four years). Sparkles neither depends on it nor
borrows from it. Fluree appears here only as a benchmark target, downloaded at benchmark
time, and this comparison draws on its public documentation.

| Area | Fluree | Sparkles |
|---|---|---|
| History | An immutable, content-addressed commit chain; time travel (`@t:`, `@iso:`, `@commit:`); history queries; branches, merge and revert | Durable, ordered commit ids and a commit catalog. Point-in-time reads of every commit since the last compaction, and of older commits kept by named snapshots or a retention window. No cross-commit history queries, diffs, branches or merges. |
| Security | Access policies stored in the ledger, JWS / `did:key` signed requests and commits, OIDC, encryption at rest | Per-dataset access levels with Basic, API tokens, OIDC sign-in for the UI and trusted proxy headers. No policy language, signed requests or encryption at rest. |
| Interfaces | JSON-LD transactions and queries (FQL), openCypher with Bolt, GraphQL, SQL / R2RML / Iceberg graph sources, an MCP server | SPARQL, the Rust API and a read-only MCP server over stdio. JSON-LD is an RDF format only. |
| Search | BM25 full-text, vector (HNSW), geospatial | BM25 full-text (`text:query`), exact vector search (`spk:vectorSearch`) and GeoSPARQL with a spatial index. No approximate (HNSW) vector index. |
| Deployment | S3, DynamoDB or IPFS storage; Raft clustering; read replicas ("query peers") | A single node on local disk, with incremental, deduplicated backups to a file system or S3 |
| Reasoning | At query time (RDFS / OWL 2 QL rewriting; OWL 2 RL / Datalog with a fact budget) | Materialized (RDFS, OWL 2 RL, Jena rules) |

Sparkles is ahead on:
* **Conformance.** Sparkles passes the W3C SPARQL 1.1 suites in full and the SHACL Core
  and SHACL-SPARQL suites (98/98, 20/20). The suites have not been run against Fluree,
  and its TSV output is not W3C-formatted.
* **Jena/Fuseki compatibility.** Fuseki's endpoints and `/$/` admin API, the Jena-style
  CLI and rules, and federation through `SERVICE` to any endpoint (Fluree federates only
  between its own ledgers).
* **Index design.** Fluree keeps 4 index orders, Sparkles 7. Fluree's planner is greedy;
  Sparkles uses dynamic programming. Fluree indexes in the background once uncommitted
  changes pass a threshold; Sparkles keeps updates in an in-memory delta and compacts on
  request.

[BENCHMARKS.md](BENCHMARKS.md) has the head-to-head numbers.

### vs. Oxigraph

[Oxigraph](https://github.com/oxigraph/oxigraph) (MIT / Apache-2.0) is a Rust RDF database
and toolkit on RocksDB. Sparkles uses Oxigraph's libraries for its term model, parsers,
serializers, SPARQL parser and XSD datatypes (`oxrdf`, `oxttl`, `oxrdfio`, `spargebra`,
`sparesults`, `oxsdatatypes`). The storage engine, query planner and executor are
Sparkles' own.

| Area | Oxigraph | Sparkles |
|---|---|---|
| Embedding | A Rust library, Python (`pyoxigraph`) and JavaScript/WebAssembly packages, an in-memory store | A Rust library, persistent or in-memory. No Python or WebAssembly bindings. |
| Storage | RocksDB (a C++ LSM tree) with 9 index orders (6 for named graphs, 3 for the default graph) and a string dictionary; updates in place; online backups as RocksDB checkpoints (a complete copy in a new local directory, hard-linked on the same file system) | Immutable sorted blocks in 7 orders, plus an in-memory delta logged to a WAL and merged by compaction. Online backups to repositories on a file system or S3, incremental and deduplicated across backups and datasets, with restore, verification, schedules and retention. |
| Spatial | GeoSPARQL functions (`spargeo`, on by default in the CLI); no spatial index | GeoSPARQL 1.1 functions (geodesic measures, EPSG:4326 axis order, metric buffers) and a per-dataset spatial index |
| Write durability | One RocksDB transaction per request, written to RocksDB's WAL without an fsync (RocksDB's default) | The WAL is fsynced before a write is acknowledged |

Oxigraph describes its query evaluation as "not optimized yet". It evaluates lazily, one
iterator per RocksDB scan. In the benchmarks it loads data second fastest, after Sparkles,
but joins, grouping, sorting and counting run 10–400× slower than Sparkles at 10.5M
triples, and it serves 2 concurrent star-join queries per second to Sparkles' 191.
Oxigraph has no reasoning, SHACL, full-text or vector search, point-in-time reads,
authentication or per-dataset permissions, Fuseki admin API, query budgets, result cache
or web UI.

## Divergences from Jena / QLever (decisions)

### General

| Decision | Rationale |
|---|---|
| Rust instead of Java or C++ | [AUDIT.md](AUDIT.md#3-language-decision-rust) §3: `spargebra`, `oxttl` and related crates provide the parser and format stack, there is no garbage collector, and performance is predictable. |
| Sorted-block permutations instead of TDB2's B+trees | Scan-heavy analytics run much faster and the files are smaller. Updates go to a delta that compaction merges, not into the index in place. |
| Values are inlined only when their lexical form is canonical | QLever inlines lossily: doubles lose 4 bits and the lexical form is dropped. Sparkles keeps exact RDF term identity (`"01"^^xsd:integer` ≠ `"1"^^xsd:integer`), as Jena does. Doubles whose low mantissa bits are set go to the vocabulary. |
| The graph is a 4th key column in every permutation, plus a GSPO permutation | Matches QLever's graph column. GSPO gives TDB2-style graph-scoped access (dumps, enumerating `GRAPH ?g {}`). |
| Deltas are persistent ordered sets (`imbl`), logged to the WAL | Snapshots publish in O(1) for MVCC. QLever locates delta triples per block instead; Sparkles may adopt that later. |
| Blank nodes are stored ids and serialize as `_:b<hex>` | Labels round-trip through the protocol, as with Jena's `<_:…>` handling. |
| LZ4 instead of zstd for index blocks; front coding instead of FSST for the vocabulary | Fast decoding on the query path. zstd is used where ratio matters more than decode speed: backups, dumps, HTTP and the full-text document store. zstd blocks and FSST remain possible upgrades. |
| Canonical decimal output follows XSD 1.1 (`"4"^^xsd:decimal`); Jena writes `"4.0"` | Inherited from `oxsdatatypes`. The values are equal, so value-based result comparison is unaffected. |
| SPARQL parsing and algebra through `spargebra`, not a port of ARQ's JavaCC grammar | The algebra matches SPARQL 1.1 §18. ARQ's syntax extensions (LET, `apf:` property functions, custom aggregates) are not supported. |
| Filter placement and equality substitution happen in the planner, not as ARQ-style algebra transforms | Same effect as `TransformFilterPlacement` and `TransformFilterEquality`, with one fewer pass over the algebra. |
| `REDUCED` is a no-op | The spec allows it. |
| `GRAPH ?g { P }` binds `?g` as a scan column when `P` is a plain join group; otherwise `P` is evaluated per named graph and joined with `?g`, like Jena's `OpGraph` | The fast path covers the common case; the fallback keeps SPARQL scoping exact (for example OPTIONAL or MINUS inside GRAPH). |
| `GROUP_CONCAT` always returns a simple literal | Spec behaviour. Jena keeps a common language tag. |
| Triple terms are vocabulary entries with a canonical nested key (blank nodes inside keep their store identity); patterns with variables inside `<<( … )>>` bind a hidden variable that a `TripleTerm` operator decomposes | The 64-bit id model and the permutations stay unchanged. Key order puts triple terms between literals and IRIs, so term-kind checks stay O(1). |
| The effective boolean value of an ill-typed boolean or numeric literal is an error | SPARQL 1.2 §17.2.2. SPARQL 1.1 said `false`. |
| Reasoning is materialized (forward chaining into `urn:x-sparkles:inferred`, queried as default ∪ inferred), not computed on the fly like Jena's `InfGraph` | Queries run at the speed of the plain index. The cost is re-running `/$/reason` after updates. The reasoning status records the commit it ran at, so stale inferences are reported, and `serve --auto-reason` re-runs them automatically. Backward (LP) rules are not supported. |
| An `AS ?v` target already in scope is rejected (SPARQL §18.2.1) | `spargebra` does not check this, so Sparkles does, matching Jena and QLever. |
| The full-text index commits lazily: a write stages its documents, and the next text query that needs them, or a tick about once a second, commits them | A Tantivy commit flushes a segment and costs more than the indexing, so a burst of writes shares one commit. Each snapshot still searches exactly its own documents: later ones are filtered out, and removed ones are kept until their batch commits. After a crash the WAL restores what was only staged. Jena's text index commits with each transaction. |
| N-Quads backups (`/$/backup/{ds}`, `sparkles backup`) are zstd (level 3, `.nq.zst`) by default; Fuseki writes gzip (`.nq.gz`). `?compression=gzip` or `--compress gzip` writes Fuseki's format | At 10.5M triples, zstd took 8.2 s for 81.5 MB and gzip (level 6) 41 s for 74.9 MB: five times faster for a file 9% larger. `sparkles load` and uploads read both. |

### Server defaults

| Decision | Rationale |
|---|---|
| `serve` listens on `127.0.0.1` by default. Without `--auth-config` it refuses a non-loopback address unless `--allow-open-network` (or `SPARKLES_ALLOW_OPEN_NETWORK=1`) is given. Fuseki listens on all interfaces. | Without authentication every caller can read, write and administer everything, so exposing that must be explicit. The override logs a warning, as does a network listener without rate limits. |
| Without `--auth-config`, `serve` sends no CORS headers unless `--cors-origin` names an origin, refuses cross-site writes (`Origin`, `Sec-Fetch-Site`), and accepts only IP addresses, `localhost`, `--host` and `--public-host` names in `Host`. Fuseki answers CORS from any origin. | Every caller of an open server is its administrator. Without these checks, any web page the operator opens could read, write and `LOAD` local files through the browser, directly or by rebinding its DNS name. |
| `--max-export-mb` defaults to `0` (unlimited), while query responses are capped at 1 GiB (`--max-result-mb`) | A Graph Store GET of a graph or dataset is the export path, streamed from one snapshot, and a finite default would cut off legitimate dumps. The cost is that any reader can make the server stream the whole dataset (CPU and bandwidth, not memory). Deployments that expose reads to untrusted clients should set `--max-export-mb` and rate-limit the `query` class. |
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
| The spatial index is opt-in per dataset (`geo.json`). The `geof:` functions work without it, and every index hit is refined with the exact test, so answers are the same with or without it. | Jena builds its index for the whole dataset at start-up, and its `spatial:withinBox` / `intersectBox` return envelope hits for an unbound subject. |

### Out of scope for v1

JavaScript functions, RDF Thrift/Protobuf/TriX, jena-ontapi object mapping, jena-text's
Lucene index format and assembler configuration (Sparkles implements `text:query` itself),
SHACL-AF rules (also absent from Jena), RDF Patch, backward-chaining (LP) rules and Shiro
authentication.

## Optimizations adopted from QLever

* **Ids with inline values.** The top 4 bits hold a tag and the low 60 bits a payload.
  UNDEF is 0, so it sorts first. Numbers and booleans never touch the dictionary.
* **Sorted vocabulary.** Id order equals term order, so prefix and range restrictions
  become id ranges (`Vocab::prefix_range`).
* **Permutation files.** Blocks store each column separately (delta, zig-zag varint,
  LZ4). Each block's first and last key stay in RAM, which allows block skipping on
  bound prefixes and exact counts with at most two block decodes.
* **Bulk build pipeline.** Parallel chunked parsing produces per-batch vocabularies, which
  are k-way merged into one. Ids are remapped in parallel, and each permutation is built
  with a parallel sort, or with sorted runs and a k-way merge when the data exceeds the
  memory budget.
* **Immutable base plus delta.** Updates are layered on the immutable index, in the style
  of QLever's `DeltaTriples`. Snapshots are versioned, and caches are keyed by snapshot
  version. A scan merges the delta into the blocks it changes. It finds the base rows
  between two delta keys by binary search, and only those blocks have every column
  decoded.
* **Columnar execution and planning.** Execution is column-major. The planner is a DP over
  interesting sort orders with a greedy fallback, and merge joins run on sorted scans.
* **Decoded-block cache.** A shared cache of decoded blocks, weighted by bytes.
* **Result cache.** Executed subtrees are cached under a canonical plan key and the
  snapshot version, so updates invalidate entries without extra work. Results with
  query-local terms or non-deterministic functions are not cached. Each operator reports
  `cached` in the plan.
* **GROUP BY + COUNT from index runs.** When the group key is a scan's sort column, counts
  come from runs in the blocks; the scan is never materialized.
* **Planner details.** Filters are placed as soon as their variables are bound. Scan sizes
  are exact from block metadata (at most two block decodes). Join estimates use
  per-predicate distinct subject and object counts with QLever's 0.7 correction factor.
  Merge joins gallop through skewed inputs. `COUNT(*)` over one pattern comes from index
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
  without allocating a string per term. Terms added by updates are tested on their delta
  keys. Inline values (numbers, dates) go through the general evaluator.
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
* **Batched index joins.** When a join's input always binds a variable that a triple
  pattern can be read sorted on, and has few distinct values of it for the pattern's size,
  the pattern is read only for those values (`IndexJoin`). The sorted distinct keys become
  key ranges, and ranges in adjacent blocks are read in one scan: scattered keys cost a
  seek per region, dense keys one sweep. Each input row then joins its key's rows, so the
  input's order and duplicates are kept. The planner offers this next to merge and hash
  joins when probing (seeks, blocks touched, rows) is estimated at under half the cost of
  scanning the pattern. EXPLAIN counts the keys, seeks, blocks and rows read
  (`batched_join`).
* **Fused stars.** Index joins on one subject over constant predicates (`?p ex:worksFor
  ex:org7 ; foaf:name ?n ; foaf:age ?a`) run as one operator (`StarJoin`). It walks each
  subject's SPO run once and picks out the star's predicates, or probes each pattern's own
  permutation, whichever touches fewer blocks, and builds the output once instead of
  through intermediate tables (`star_fusion`).
* **Whole-block scans under graph filters.** A block slice is copied column-wise whenever
  every row passes the graph filter (one pass over the graph column), so default-graph
  queries avoid row-by-row filtering.
