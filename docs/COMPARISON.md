# Sparkles compared

How Sparkles relates to Apache Jena/Fuseki, QLever, Fluree and Oxigraph: what each of them
has that Sparkles does not (yet), where Sparkles departs from Jena and QLever on purpose,
and which of QLever's techniques it adopted. The measured side (which queries and datasets
Sparkles loses on, and what the benchmarks do not cover) is in
[BENCHMARKS.md](BENCHMARKS.md#where-sparkles-loses); the full feature list is in
[FEATURES.md](FEATURES.md).

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

| Area | Jena / Fuseki has | Sparkles |
|---|---|---|
| Full-text search | jena-text (Lucene), `text:query` | `text:query` subset over string literals (Tantivy, BM25), updated in the commit path; no highlighting, per-language stemming or entity-style multi-field documents yet |
| Spatial | GeoSPARQL 1.0/1.1: `geof:` and `spatialF:` functions, `spatial:` property functions with a spatial index, query rewrite of the topological properties, RDFS entailment of the geometry hierarchy, GML and KML literals, EPSG CRSs through Apache SIS | the GeoSPARQL 1.1 `geof:` functions over WKT and GeoJSON literals in the built-in CRSs (and the 120 UTM zones), Jena's `spatial:` property functions and `spatialF:` filter functions, and a spatial index per dataset that FILTERs, property functions, spatial joins and nearest-neighbour ORDER BY use; query rewrite of the topological properties (off by default) and RDFS entailment of the geometry hierarchy (`--vocab geosparql`); no geometry-type entailment, GML/KML literals or EPSG database yet (see [AUDIT.md](AUDIT.md#5-explicit-non-goals-for-v1) §5) |
| Shape languages | ShEx (jena-shex) | SHACL and ShEx 2.1 (ShExC, ShExJ, ShExR, SPARQL selectors); no ShEx 2.2 |
| Inference | on-the-fly `InfModel`, backward / hybrid rules (LP engine), OWL Micro/Mini/Full | forward materialization only (RDFS, OWL 2 RL subset, Jena forward rules); not maintained incrementally: after updates the inferences are reported stale and re-run on request or, opt-in, automatically (a full recomputation); inconsistency detection covers a fixed subset of the OWL 2 RL `false` rules (`owl:Nothing`, `disjointWith`, `AllDisjointClasses`, sameAs/differentFrom, functional literals), not full consistency checking |
| Ontology API | jena-ontapi `OntModel` object API | ✗ none (triples / SPARQL only) |
| SPARQL extensions | property functions (`list:member`, `apf:*`), `LET`, custom aggregates (`MEDIAN`, `MODE`, `FOLD`), `cdt:` list/map literals, JavaScript functions, full `afn:`/`fn:` library | ✗ none of the extensions; common `fn:`/`afn:`/`math:` functions only |
| SPARQL parser | JavaCC grammar | `spargebra` 0.4.7, vendored with fixes for the W3C syntax/evaluation tests it failed (see [`vendor/spargebra/PATCHED.md`](../vendor/spargebra/PATCHED.md)) |
| RDF formats | RDF Thrift, RDF Protobuf, TriX, RDF/JSON | ✗ (Turtle, N-Triples, N-Quads, TriG, RDF/XML, JSON-LD only) |
| Change logs | RDF Patch (jena-rdfpatch), Fuseki `/patch` endpoint | ✗ none |
| Fuseki operations | Shiro authentication, per-graph access control (fuseki-access), Prometheus `/$/metrics`, assembler (`config.ttl`) service definitions, `/$/validate/*`, prefix read/write endpoints | Basic, Bearer tokens, OIDC (UI) and trusted proxy headers, with per-dataset levels; no graph-level ACLs yet; Prometheus `/$/metrics` with Sparkles metric names (not Fuseki's `fuseki_requests_*`), no JVM metrics; datasets are configured by CLI flags / admin API only; prefixes via `/{ds}/prefixes` |
| SERVICE | bulk / batched / cached SERVICE (serviceenhancer) | plain SERVICE only |
| Transactions over HTTP | — | — (same as Fuseki: one request = one transaction) |
| Backups | `/$/backup/{ds}`: a gzipped N-Quads dump of the whole dataset per backup, in the server's directory; restored by loading it into a new dataset | the same dumps (`/$/backup/{ds}`), zstd by default (`?compression=gzip` for Fuseki's `.nq.gz`; also brotli or LZ4), plus backup repositories on a file system or S3: incremental, deduplicated backups that restore to a ready database without a reload, with verification, schedules and retention |

### vs. QLever

| Area | QLever has | Sparkles |
|---|---|---|
| Scale | tested to tens of billions of triples (Wikidata, UniProt) | tested to 10.5M; the external-sort path is covered by tests but not measured at 100M+ |
| Streaming execution | lazy, block-wise evaluation of scans, joins, filters and GROUP BY; results streamed to the client | every operator materializes its full result (bounded by row and memory budgets); responses over 1 MiB are streamed to the client as they are serialized |
| Block prefiltering | FILTER ranges / STRSTARTS evaluated against block min/max to skip blocks | numeric range FILTERs on a scan's sort column read only the matching id ranges (inline integers and decimals); non-canonical numerals are still tested row by row |
| Pattern trick | `ql:has-predicate`, per-subject predicate patterns | ✗ (predicate counts use index runs instead) |
| Text / spatial | `ql:contains-word`, BM25 scoring, spatial joins, geo index | BM25 full-text search via `text:query` (no text/entity co-occurrence index); GeoSPARQL functions, a spatial index, spatial joins on GeoSPARQL FILTERs and nearest-neighbour ORDER BY |
| Vocabulary compression | FSST string compression, IRI-as-id encoding for numeric IRIs | front coding, no IRI encoding |
| Named / pinned results, materialized views | `pin-result-with-name`, materialized views | result cache only (no pinning) |
| Live query monitoring | websocket runtime-information updates | executed plan returned after completion only |

Conversely, Sparkles has Jena behaviour that QLever does not aim for: exact term
identity (no lossy inlining), the Graph Store Protocol, Fuseki endpoints and admin API,
materialized reasoning, SHACL validation, and an embedded library API.

### vs. Fluree

[Fluree DB](https://github.com/fluree/db) 4.x is the closest peer: a Rust RDF database
with a SPARQL 1.1 endpoint, a compressed columnar index and in-memory novelty over
immutable index files. Its product focus differs: it is a versioned, permissioned ledger
with JSON-LD as its primary interface. It is licensed under BUSL-1.1, free to use except
as a hosted database service and converting to Apache-2.0 four years after each release,
so Sparkles does not depend on it or borrow from it. It appears here only as a benchmark
comparison (downloaded at benchmark time), and this comparison is based on its public
documentation.

| Area | Fluree has | Sparkles |
|---|---|---|
| History | immutable commit chain (content-addressed), time travel (`@t:`, `@iso:`, `@commit:`), history queries, branches / merge / revert | durable, ordered commit ids and a commit catalog; point-in-time reads of every commit since the last compaction, and of older ones kept by named snapshots or a retention window; no history queries across commits, diffs, branches or merges yet |
| Security | ledger-stored access policies, JWS / `did:key` signed requests and commits, OIDC, encryption at rest | per-dataset access levels with Basic, API tokens, OIDC sign-in for the UI and trusted proxy headers; no policy language, signed requests or encryption at rest |
| Interfaces | JSON-LD transactions and queries (FQL), openCypher + Bolt, GraphQL, SQL / R2RML / Iceberg graph sources, MCP server | SPARQL, the Rust API and an MCP server (stdio, read-only tools); JSON-LD as an RDF format only |
| Search | BM25 full-text, vector (HNSW), geospatial | BM25 full-text (`text:query`), exact vector search (`spk:vectorSearch`) and GeoSPARQL with a spatial index; no approximate (HNSW) vector index yet |
| Deployment | S3 / DynamoDB / IPFS storage, Raft clustering, read replicas ("query peers") | single node, local disk, plus incremental, deduplicated backups to a file system or S3 |
| Reasoning | at query time (RDFS / OWL 2 QL rewriting, OWL 2 RL / Datalog with a fact budget) | materialized (RDFS, OWL 2 RL, Jena rules) |

Where Sparkles is ahead:
* **Conformance.** Sparkles passes the W3C SPARQL 1.1 suites in full and the SHACL
  Core and SHACL-SPARQL suites (98/98, 20/20). The suites have not been run against
  Fluree. Its TSV output is not W3C-formatted.
* **Jena/Fuseki compatibility.** Fuseki endpoints and `/$/` admin, the Jena-style CLI
  and rules, and external `SERVICE` federation (Fluree federates only between its own
  ledgers).
* **Index design.**
  * Fluree keeps 4 index orders against Sparkles' 7.
  * Its planner is greedy; Sparkles' is dynamic-programming.
  * It indexes in the background once uncommitted changes pass a threshold. Sparkles
    folds updates into an in-memory delta and compacts on request.

See [BENCHMARKS.md](BENCHMARKS.md) for the head-to-head numbers.

### vs. Oxigraph

[Oxigraph](https://github.com/oxigraph/oxigraph) (MIT / Apache-2.0) is a Rust RDF
database and toolkit on RocksDB. Sparkles is built on Oxigraph's libraries: `oxrdf`,
`oxttl`/`oxrdfio`, `spargebra`, `sparesults` and `oxsdatatypes` supply its term model,
parsers, serializers, SPARQL parser and XSD datatypes. The storage engine, query planner
and executor are Sparkles' own, so the two share a front end and differ in how queries
run.

| Area | Oxigraph has | Sparkles |
|---|---|---|
| Embedding | Rust library, Python (`pyoxigraph`) and JavaScript/WebAssembly packages, an in-memory store | Rust library (persistent or in-memory); no Python or WebAssembly bindings |
| Storage | RocksDB (an LSM tree; C++), 9 index orders (6 for named graphs, 3 for the default graph) plus a string dictionary; updates in place; online backups via RocksDB checkpoints (each a complete database in a new local directory, hard-linked when on the same file system) | immutable sorted blocks in 7 orders plus a WAL-logged in-memory delta, merged by compaction; online backups into backup repositories on a file system or S3 (incremental and deduplicated across backups and datasets), with restore, verification, schedules and retention |
| Spatial | GeoSPARQL functions (`spargeo`, on by default in the CLI; no spatial index) | GeoSPARQL 1.1 functions (geodesic measures, EPSG:4326 axis order, metric buffers) and a spatial index per dataset |
| Write durability | a RocksDB transaction per request, written to RocksDB's WAL without an fsync (RocksDB's default write options) | the WAL is fsynced before a write is acknowledged |

Oxigraph describes its query evaluation as "not optimized yet": it evaluates lazily,
iterator by iterator over RocksDB scans. In the benchmarks it loads quickly (second to
Sparkles) but joins, grouping, sorting and counting are 10–400× slower at 10.5M
triples, and it serves 2 concurrent star-join queries/s against Sparkles' 191. Sparkles
adds what Oxigraph leaves out: reasoning, SHACL, full-text and vector search, point-in-time
reads, authentication and per-dataset permissions, Fuseki's admin API, budgets and a
result cache, and the web UI.

## Divergences from Jena / QLever (decisions)

### General

| Decision | Rationale |
|---|---|
| Rust instead of Java/C++ | See [AUDIT.md](AUDIT.md#3-language-decision-rust) §3: `spargebra`, `oxttl` and related crates provide the parser and format stack, there is no GC, and performance is predictable. |
| Sorted-block permutations instead of TDB2's B+trees | Scan-heavy analytics are much faster and the files are smaller. Updates go into a delta that is periodically compacted, rather than being done in place. |
| Values are inlined only when the lexical form is canonical | QLever inlines lossily (doubles lose 4 bits, and the lexical form is dropped). Sparkles keeps exact RDF term identity (`"01"^^xsd:integer` ≠ `"1"^^xsd:integer`), as Jena does. Doubles whose low mantissa bits are nonzero go to the vocabulary. |
| Graph stored as a 4th key column in every permutation, plus a GSPO permutation | This matches QLever's graph column. GSPO gives TDB2-style graph-scoped access (dumps, `GRAPH ?g {}` enumeration). |
| Deltas held as persistent ordered sets (`imbl`) and WAL-logged | Gives O(1) snapshot publication for MVCC. QLever locates delta triples per block instead; we may adopt that later. |
| Blank nodes are stored ids and serialize as `_:b<hex>` | Labels round-trip through the protocol, like Jena's `<_:…>` handling. |
| LZ4 instead of zstd for index blocks; front coding instead of FSST for the vocabulary | Fast decoding on the query path. zstd is used where ratio matters more than decode speed (backups, dumps, HTTP, the full-text document store); zstd blocks and FSST remain possible upgrades. |
| Canonical decimal output follows XSD 1.1 (`"4"^^xsd:decimal`), whereas Jena writes `"4.0"` | Comes from `oxsdatatypes`; the values are equal, so value-based result comparison is unaffected. |
| SPARQL parsing and algebra via `spargebra` instead of a port of ARQ's JavaCC grammar | The algebra matches SPARQL 1.1 §18. ARQ syntax extensions (LET, `apf:` property functions, custom aggregates) are not supported. |
| Filter placement and equality substitution happen in the planner rather than as ARQ-style algebra transforms | Same effect as `TransformFilterPlacement` / `TransformFilterEquality`, with one less pass over the algebra. |
| `REDUCED` is a no-op | Allowed by the spec. |
| `GRAPH ?g { P }` binds `?g` as a scan column when `P` is a plain join group; otherwise `P` is evaluated per named graph and joined with `?g`, like Jena's `OpGraph` | The fast path covers the common case, and the fallback keeps SPARQL scoping exact (e.g. OPTIONAL or MINUS inside GRAPH). |
| `GROUP_CONCAT` always returns a simple literal | Spec behaviour; Jena keeps a common language tag. |
| Triple terms are vocabulary entries with a canonical nested key (blank nodes inside them keep store identity); patterns with variables inside `<<( … )>>` bind a hidden variable and are decomposed by a `TripleTerm` operator | Keeps the 64-bit id model and all permutations unchanged; key order puts triple terms between literals and IRIs, so term-kind checks stay O(1). |
| Effective boolean value of ill-typed boolean/numeric literals is an error | SPARQL 1.2 §17.2.2 (SPARQL 1.1 said `false`). |
| Reasoning is materialized (forward chaining into the `urn:x-sparkles:inferred` graph, queried as default ∪ inferred) instead of Jena's on-the-fly `InfGraph` | Query speed stays that of the plain index. The trade-off is re-running `/$/reason` after updates: the reasoning status records the commit it was made at, so stale inferences are reported (and can be re-run automatically with `serve --auto-reason`). Backward (LP) rules are not supported. |
| `AS ?v` targets that are already in scope are rejected (SPARQL §18.2.1) | `spargebra` does not check this, so Sparkles validates it itself, matching Jena and QLever. |
| The full-text index is committed lazily: a write stages its documents, and the next text query that needs them (or a tick about once a second) commits them | A Tantivy commit flushes a segment and costs more than the indexing itself, so a burst of writes shares one. Each snapshot still searches exactly its own documents (later ones are filtered out against it, removed ones are kept until their batch is committed), and after a crash the WAL restores what was only staged. Jena's text index commits with each transaction. |
| N-Quads backups (`/$/backup/{ds}`, `sparkles backup`) are zstd (level 3, `.nq.zst`) by default, where Fuseki writes gzip (`.nq.gz`); `?compression=gzip` or `--compress gzip` writes Fuseki's format | At 10.5M triples zstd took 8.2 s for 81.5 MB and gzip (level 6) 41 s for 74.9 MB: five times faster for a file 9% larger. `sparkles load` and uploads read both. |

### Server defaults

| Decision | Rationale |
|---|---|
| `serve` listens on `127.0.0.1` by default and refuses a non-loopback address without `--auth-config` unless `--allow-open-network` (or `SPARKLES_ALLOW_OPEN_NETWORK=1`) is given (Fuseki listens on all interfaces) | Without authentication every caller may read, write and administer everything, so exposing that is an explicit choice; the override still logs a warning, as does a network listener without rate limits. |
| Without `--auth-config`, `serve` sends no CORS headers unless `--cors-origin` names an origin, refuses cross-site writes (`Origin`, `Sec-Fetch-Site`) and answers only IP addresses, `localhost`, `--host` and `--public-host` names in `Host` (Fuseki answers CORS from any origin) | Every caller of an open server is its administrator, so any web page the operator opens could otherwise read, write and `LOAD` local files through the browser, directly or by rebinding its DNS name. |
| `--max-export-mb` stays `0` (unlimited) by default, while query responses are capped at 1 GiB (`--max-result-mb`) | A Graph Store GET of a graph or a whole dataset is the export path, streamed from one snapshot, and a finite default would cut off legitimate dumps. The cost: any reader can make the server stream its whole dataset (CPU and bandwidth, not memory). Deployments that expose reads to untrusted clients should set `--max-export-mb` and rate-limit the `query` class. |
| A client's `timeout=` is capped at `--max-timeout` (default 1800 s, `0`: no cap) for queries, updates and Graph Store writes alike; the default query timeout stays 60 s, and writes have no default deadline (`--update-timeout 0`) but are cancelled when their client disconnects | A request may ask for a longer timeout than the default, but not hold a worker indefinitely; a long load is not cut off by a default it did not ask for, and a disconnected one stops (its rate-limit concurrency slot stays taken until it has). |

### GeoSPARQL

| Decision | Rationale |
|---|---|
| GeoSPARQL distances, lengths and areas on geographic CRSs are geodesic on the WGS 84 ellipsoid (Karney); `geo.json` `"distance": "haversine"` gives Jena's sphere (R = 6,371,008.7714 m) | Up to 0.5% more accurate than the sphere, at a small cost per call. Jena computes great-circle distances on a sphere. |
| GeoSPARQL literals are read with their CRS's own axis order (EPSG:4326 is latitude first); the legacy `…/def/crs/EPSG/4326` (without `/0/`) is CRS84, as in Jena | GeoSPARQL Req 16. `minX`…`maxY` report the literal's own axes, as in Jena. |
| GeoSPARQL literals in a CRS this build does not know are valid geometries: same-CRS planar relations, accessors and constructions work, metric functions and mixes with other CRSs are type errors, and the index leaves them out (counted in its status) | Jena logs a warning and treats the coordinates as CRS84 degrees, which gives wrong answers silently. |
| GeoSPARQL relations follow DE-9IM: an empty geometry is disjoint from everything (`sfDisjoint` true, every other relation false), equal points are `sfEquals`, `sfCrosses` of two curves is `0********`, RCC8 relations hold between regions only | Jena returns false for every relation on an empty geometry and compares `sfEquals` with the tables' `TFFFTFFFT` pattern, under which two equal points are not equal. |
| GeoSPARQL query rewrite is off by default (`geo.json` `"queryRewrite": true` per dataset) | Jena rewrites by default. Each rewritten pattern costs a spatial search or join, and enabling a spatial index should not change what an existing query over asserted triples means. |
| `geof:getSRID` returns an `xsd:anyURI`; `geof:dimension` of an empty geometry is its type's dimension (`-1` for an empty collection) | The GeoSPARQL 1.1 signature (Jena returns `xsd:string`); never a type error. |
| `geof:concaveHull(g, targetPercent)` sets concaveman's concavity to `targetPercent / 25` (50 = the default 2.0, 100 = the convex hull); `geof:aggConcaveHull` uses the default; `spatialF:angle` follows Jena's documented meaning (clockwise from the y axis) in every quadrant | GeoSPARQL leaves the hull parameter to the implementation, and a SPARQL aggregate takes one expression. Jena's `angle` is a quarter turn off south-east and north-west of the first point. |
| The spatial index is opt-in per dataset (`geo.json`); the `geof:` functions work without it, and every answer from the index is refined with the exact test, so answers are the same with or without it | Jena's index is built for the whole dataset at start-up and its `spatial:withinBox`/`intersectBox` return envelope hits for an unbound subject. |

### Out of scope for v1

JS scripting functions, RDF Thrift/Protobuf/TriX, jena-ontapi object mapping, jena-text's
Lucene index format and assembler configuration (Sparkles implements `text:query` itself),
SHACL-AF rules (also absent in Jena), RDF Patch, backward-chaining (LP) rules, Shiro auth.

## Optimizations adopted from QLever

* **Ids with inline values.** The top 4 bits hold a tag and the low 60 bits a payload.
  UNDEF is 0, so it sorts first. Numbers and booleans never touch the dictionary.
* **Sorted vocabulary.** Id order equals term order, so prefix and range restrictions
  become id ranges (`Vocab::prefix_range`).
* **Permutation files.** Blocks store columns separately (delta + zig-zag varint +
  LZ4). Each block's first and last key stays in RAM, for block skipping on bound
  prefixes and exact counts with at most two block decodes.
* **Bulk build pipeline.** Parallel chunked parsing feeds per-batch partial
  vocabularies. These are k-way merged into a global vocabulary, ids are remapped in
  parallel, and each permutation is built with a parallel sort (or sorted runs plus a
  k-way merge when the data exceeds the memory budget).
* **Immutable base plus delta.** Updates are layered on the immutable index, in the
  style of QLever's `DeltaTriples`. Snapshots are versioned, and caches are keyed
  by snapshot version.
* **Columnar execution and planning.** Execution is column-major; the planner is a DP
  over interesting sort orders with a greedy fallback. Merge joins run on sorted
  scans.
* **Decoded-block cache.** A shared cache of decoded blocks, weighted by bytes.
* **Result cache.** Executed subtrees are cached under a canonical plan key plus the
  snapshot version, so updates invalidate naturally. Results with query-local terms or
  non-deterministic functions are skipped. Each operator reports `cached` in the plan.
* **GROUP BY + COUNT from index runs.** When the group key is a scan's sort column,
  counts come from runs in the blocks without materializing the scan.
* **Planner details.** Filters are placed as soon as their variables are bound. Scan
  sizes are exact from block metadata (at most two block decodes). Join estimates use
  per-predicate distinct subject/object statistics with QLever's 0.7 correction factor.
  Merge joins use galloping for skewed inputs. `COUNT(*)` over a single pattern is
  answered from index metadata. Transitive paths traverse from the bound side (index
  lookups per frontier node) instead of materializing the closure.
* **Executed-plan feedback.** Every query returns a runtime-information tree
  (estimated vs. actual rows, time per operator), like `qlever-json`. The UI renders it.

## Further scan and filter optimizations

Each of these can be switched off per query (`QueryOptions::optimizations`) or per
process (`SPARKLES_DISABLE_OPTIMIZATIONS=range_pushdown,…`), and EXPLAIN shows which one
ran.

* **`COUNT(DISTINCT ?v)` from index runs.** Over a single triple pattern, the scan is
  re-targeted to a permutation sorted on `?v` and the distinct values are counted as
  runs of equal ids (`CountDistinctFromIndex`). No rows are materialized or hashed.
* **Filters on vocabulary keys.** `CONTAINS` / `STRSTARTS` / `STRENDS` / `REGEX` over
  `?v` or `STR(?v)`, and `LANGMATCHES(LANG(?v), …)`, are tested directly on the stored
  key bytes (`"lexical 0xFF @lang`, `<iri`). Each front-coded block is read once, in
  parallel, with no per-term string allocation. Terms added by updates are tested on
  their delta keys; inline values (numbers, dates) fall back to the general evaluator.
* **Pure expressions per distinct value** (`expr_cache`). A FILTER conjunct, BIND,
  ORDER BY key or aggregate argument that reads one variable, and gives the same result
  for the same term, is evaluated once per distinct id of that variable. Rows look up
  the result, errors included. RAND, UUID, STRUUID, BNODE and EXISTS are evaluated per
  row; NOW and the base IRI are fixed for the query. When the column is sorted on the
  variable, the distinct ids are its runs. Otherwise a sample estimates how often values
  repeat, and inputs where fewer than half of the rows repeat a value are evaluated row
  by row. EXPLAIN notes `[expr cache: …]` with the distinct count, or why the operator
  ran row by row, and reports `exprCacheHits` / `exprCacheMisses` / `exprCacheSkipped`.
  A constant regular expression is compiled once per thread and reused with its match
  cache.
* **Numeric range scans.** A FILTER comparing a scan's sort column with numeric
  constants reads only the id ranges that can match. Inline integers, and inline
  decimals of one scale, sort by value within their id segment. So each segment's
  matching ids form one range, found by binary search with the ordinary comparison.
  Doubles and literals from the vocabulary are read and tested; booleans, dates and
  blank nodes are skipped. The planner costs the scan from an exact count of the rows
  in those ranges (`IndexRangeScan` in EXPLAIN).
* **Numeric top-k.** `ORDER BY ?v LIMIT k` over numbers ranks cheap rounded keys first.
  Exact values are computed only for rows that can still reach the first k.
* **Ordered-scan top-k.** `ORDER BY ?v LIMIT k` (and OFFSET) over a single triple
  pattern, with FILTERs over it, reads the pattern in the order of `?v` and stops once
  the first k rows are proven (`IndexTopK` in EXPLAIN). The scan is re-targeted to a
  permutation sorted on `?v`. Inline integers, decimals of one scale and doubles of
  one sign sort by value within their id segment. Each segment is read from its best
  end, a few rows at a time, until the k-th candidate is strictly better than its best
  unread value. Vocabulary literals (non-canonical numerals, other numeric types,
  strings), IRIs and blank nodes are not in value order by id, so they are read whole.
  The candidates are ranked by the ordinary ORDER BY in the plain scan's row order, so
  ties come out the same. NaN, dates and durations have no total order, and fall back
  to the plain sort. The planner picks it from exact row counts per segment when it
  reads at most half of the pattern's rows.
* **Incremental GROUP BY.** With one group key and COUNT / SUM / AVG / MIN / MAX /
  SAMPLE over variables, each group keeps a running state in a hash map on the key id.
  Sums stay exact 64-bit integers until a value is not an inline integer.
* **Count joins from key runs.** `COUNT(*)` over two scans joined on one variable reads
  both sides as (key, run length) pairs from indexes sorted on that variable and sums
  the products (`CountJoinFromRuns`).
* **Class counts from statistics.** `GROUP BY ?class` with a count over `?s a ?class`
  uses the per-class counts in the index statistics when they are exact: no delta, and
  all data in the default graph (`GroupCountFromMetadata`).
* **Batched path frontiers.** `p*` / `p+` traversals expand a large BFS level with one
  merged pass over the predicate's index rows, instead of one seek per node.
* **Selective column decoding.** The block cache holds decoded columns. Scans decode
  only the key columns they read (variables, graph, repeated variables), which also
  leaves room for more of the cache.
* **Decorrelated EXISTS.** `FILTER EXISTS { P }` / `FILTER NOT EXISTS { P }`, where `P`
  is made of triple patterns, paths without `*` or `?`, `GRAPH` and deterministic
  FILTERs, evaluates `P` once and keeps the distinct values of the variables the outer
  rows bind. Each outer row then probes that set instead of evaluating the substituted
  pattern. A row that leaves some of them unbound (after OPTIONAL) probes the set of
  its bound ones. A row that binds a variable only a FILTER inside `P` uses is still
  evaluated by substitution. The key set is built once per query, only when `P` costs
  less than evaluating it per distinct outer key, and the EXISTS stays per row when
  the set does not fit in the memory budget (`[EXISTS decorrelated on ?y: …]` in
  EXPLAIN, or the reason it was not, with `exists*` counters).
* **Batched index joins.** When the input of a join always binds a variable that a
  triple pattern can be read sorted on, and has few distinct values of it for the
  pattern's size, the pattern is read only for those values (`IndexJoin` in EXPLAIN).
  The distinct keys, sorted, become key ranges, and ranges whose blocks are adjacent are
  read by one scan: scattered keys cost a seek per region, dense keys one sweep. Each
  input row then joins its key's rows, so the input's order and duplicates are kept.
  The planner offers it next to the merge and hash joins when probing (seeks, touched
  blocks, rows) is estimated at under half the cost of scanning the pattern. EXPLAIN
  counts the keys, seeks, blocks and rows read (`batched_join`).
* **Fused stars.** Index joins on one subject over constant predicates (`?p ex:worksFor
  ex:org7 ; foaf:name ?n ; foaf:age ?a`) run as one operator (`StarJoin`). It either
  walks each subject's SPO run once, picking out the star's predicates, or probes each
  pattern's own permutation, whichever touches fewer blocks, and forms the output once
  instead of through the chain's intermediate tables (`star_fusion`).
* **Whole-block scans under graph filters.** A block slice is copied column-wise
  whenever every row passes the graph filter (one pass over the graph column), so
  default-graph queries no longer fall back to row-by-row filtering.
