# Audit: Apache Jena, QLever and Oxigraph → Sparkles

> **Historical design record.** This audit captures the September 2026 source snapshots that
> guided Sparkles' initial architecture: Apache Jena `6.3.0-SNAPSHOT` (`b1dcba53b5`,
> 2026-09-28), QLever (`b0c6d0cd`, 2026-09) and Oxigraph (`e0f286b0`, 2026-09-23). It is
> kept as provenance for the original reuse and language decisions, not as a live feature
> specification.
>
> For current behavior, see [ARCHITECTURE.md](ARCHITECTURE.md) (system design),
> [FEATURES.md](FEATURES.md#known-gaps) (current capabilities and limits) and
> [COMPARISON.md](COMPARISON.md) (compatibility differences). Features added beyond these
> three projects have design specs in [`specs/`](specs/README.md) (§2b).

## 1. Apache Jena — functional inventory

| Module | Java LOC (main/test) | Role | Sparkles status |
|---|---|---|---|
| jena-base | 19K / 8K | Utilities and persistent maps (PMap/PSet) for TIM | Replaced by the Rust standard library and `imbl`. |
| jena-iri3986, jena-langtag | 7K + 2K | RFC 3986 IRIs and BCP47 language tags | Uses `oxiri` and `oxilangtag`, plus scheme and tag checks. |
| jena-core | 141K / 84K | Node/Triple/Graph, Model API, datatypes, rule reasoners (forward RETE, backward LP), ARP RDF/XML parser and legacy OntModel | Term model from `oxrdf`; forward reasoners in `sparkles-reasoner`. |
| jena-arq | 309K / 82K | RIOT I/O and SPARQL (JavaCC parser, algebra, 21 optimizer passes, iterator engine), functions, update and SERVICE | Implemented in `sparkles-core` (`io` and `sparql`) and re-exported by `sparkles`. |
| jena-db + jena-tdb2 | 25K + 19K | DBOE copy-on-write MVCC B+trees, journal, MD5 node table, inline NodeIds, 3 triple and 6 quad indexes, loaders and compaction | Replaced by QLever-style sorted blocks and WAL in `sparkles-core`. |
| jena-fuseki2 | ~36K | SPARQL server (query, update, GSP, upload, patch, shacl) and `/$/` admin API | Implemented in `sparkles-server`. |
| jena-ontapi | 35K | OWL 2 object API with DL, EL, QL and RL profiles and no DL reasoner | Out of v1 scope (§5); a separate Rust OWL 2 axiom crate (`sparkles-owl`) was added later. |
| jena-shacl / jena-shex | 23K / 18K | SHACL Core and SHACL-SPARQL; ShEx 2 | `sparkles-shacl` covers SHACL Core and SHACL-SPARQL (98/98 and 20/20 W3C). `sparkles-shex` covers ShEx with ShExC, ShExJ, ShExR and shape maps, and both support write-time guards ([C10](specs/C10-write-time-validation.md), [G02](specs/G02-shex/README.md)). |
| jena-text | 7.5K | Lucene text index | `text:query` subset over string literals on Tantivy with BM25; does not use Lucene's index format or assembler ([F03](specs/F03-full-text-search/README.md)). |
| jena-geosparql | 23K | GeoSPARQL 1.0/1.1 on JTS and Apache SIS (`geof:`, `spatialF:`, `spatial:` over STR-tree, query rewrite, WKT/GeoJSON/GML/KML, EPSG CRSs) | `sparkles-core` (`geo` feature) provides `geof:`, `spatial:`, `spatialF:`, a per-generation packed R-tree, spatial joins, query rewrite and EPSG-derived proj4 transforms without datum-shift grids ([G01](specs/G01-geosparql/README.md), [COMPARISON.md](COMPARISON.md#geosparql)). |
| jena-rdfpatch, rdfconnection, querybuilder, serviceenhancer, cmds | — | Patch logs, client APIs, builders, SERVICE enhancer and CLI | Covered by `sparkles` CLI, RDF Patch diffs/feeds/apply ([F10](specs/F10-replication.md)), `sparkles-client` ([P02](specs/P02-rust-client.md)), `sparkles::querybuilder` and engine SERVICE options ([G10](specs/G10-service-enhancer.md)). |
| jena-tdb1, commonsrdf | — | Deprecated | Skipped. |

Sparkles preserves these Jena behaviors from the audit baseline:

* **Data model.** IRIs, blank nodes, literals with a language tag or datatype, RDF 1.2
  triple terms and base direction, triples and quads, and datasets with a default graph,
  named graphs and the union default graph option.
* **XSD value space.** The numeric tower (integer ⊂ decimal ⊂ float ⊂ double, plus the
  derived integer types), dateTime, date, time, durations, booleans and strings. ORDER BY
  orders values as `ValueSpace` does.
* **RIOT.** Sparkles reads and writes Turtle, N-Triples, N-Quads, TriG, RDF/XML and
  JSON-LD 1.1. The server, `load` and `convert` also handle Jena's own RDF/JSON, Thrift,
  Protobuf and TriX, which the v1 scope left out (§5). Parsing streams into sinks as
  `StreamRDF` does, and results come in the JSON, XML, CSV and TSV formats.
* **ARQ.** Full SPARQL 1.1 Query and Update, property paths, aggregates, subqueries,
  VALUES, SERVICE, EXISTS, the function library (XPath `fn:`, `math:`, `afn:`) and property
  functions. The optimizer transforms, such as filter placement, filter equality
  substitution, TopN and implicit joins.
* **TDB2.** Inline NodeIds for ints, decimals, doubles, dates and booleans, so FILTER and
  ORDER BY avoid the node table. MR+SW transactions with snapshot isolation. The bulk
  loader pipeline. Compaction into a new `Data-NNNN` generation. Backups as `.nq.gz`.
* **Fuseki.** The endpoints `/{ds}/sparql|query|update|data|get|upload` and
  `/$/ping|server|datasets|stats|compact|backup|tasks|metrics`.
* **Reasoning.** RDFS (full, default and simple), the OWL Micro, Mini and Full rule sets,
  and `GenericRuleReasoner` with Jena rule syntax `[name: (?a p ?b) builtin(?x) -> (?a q ?b)]`.

The tests run these suites from the Jena checkout. The `sparkles-core` crate runs
`jena-arq/testing/rdf-tests-cg/sparql/{sparql10,sparql11,sparql12}` and ARQ's `LATERAL`
tests in `jena-arq/testing/ARQ`. The formatter's tests read the Turtle and TriG suites of
`jena-arq/testing/rdf-tests-cg/rdf/{rdf11,rdf12}`, `sparkles-shacl` runs
`jena-shacl/src/test/files/std`, and `sparkles-shex` runs the shexTest copy in
`jena-shex/src/test/files/spec`.

## 2. QLever — architecture and performance mechanisms

| Area | Mechanism | Adopted in Sparkles |
|---|---|---|
| Ids | A 64-bit `ValueId`: a 4-bit datatype and a 60-bit payload. All zeros is UNDEF. Int, Double, Bool, Date and GeoPoint are inline. | Adopted. Integer, decimal, double, boolean, dateTime and date are inline only in canonical lexical form, preserving exact term identity as in TDB2. |
| Vocabulary | Ids are assigned in sort order, so range and prefix filters work on ids. FSST²-compressed, with a sparse sample in RAM. | Adopted. Sorted, front-coded and memory-mapped, with prefix ranges on IDs and optional FSST suffixes. |
| Index build | Parallel parse → per-batch partial vocabularies → k-way merge → id remap → external sort per permutation | Adopted on Rayon. |
| Permutations | 6 permutations (SPO, SOP, PSO, POS, OSP, OPS) plus a graph column. Blocks of ~31k rows with per-column zstd. The first and last triple of each block stay in RAM. | Adopted. Six triple permutations plus GSPO, in 32k-row blocks with per-column delta, varint and LZ4; block metadata stays in RAM for skipping. |
| Updates | An immutable base plus DeltaTriples located per block, a snapshot per version, and a rebuild when the delta grows | Adopted. Persistent (`imbl`) delta sets per permutation, checksummed WAL, MVCC snapshots and online compaction. |
| Planner | DP over connected components with interesting sort orders, and a greedy fallback past a budget. Filters are applied as soon as their variables are bound. The cost is row counts, and join estimates use multiplicities. | Adopted. |
| Execution | A column-major `IdTable`. Operators materialize their results, though some are lazy. A LocalVocab per result. | Adopted. Column-major tables, snapshot-owning cursors and a local vocabulary per query. |
| Joins | Zipper merge join with UNDEF, galloping join for skewed sizes, MultiColumnJoin, OptionalJoin, Minus, and TransitivePath with a bound side | Adopted. Merge, galloping, hash and batched index joins; transitive paths use BFS from the bound side. |
| GROUP BY | COUNT from metadata, sort-based grouping and special cases | Adopted. Metadata and run-based COUNT fast paths plus hash grouping. |
| Cache | A concurrent LRU keyed by subtree and delta version, with pinning | Adopted. LRU keyed by canonical plan, snapshot version and access view. |
| Limits | A cancellation handle, a memory-limited allocator and timeouts | Adopted in part. Cancellation (including client disconnect), timeouts and per-query estimates for intermediate memory, response bytes and rows, rather than an allocator limit ([C01](specs/C01-observability-and-budgets.md)). |
| Server | Streaming results, `qlever-json` with a runtime-information tree, and websockets for the live plan | Adopted in part. `x-sparkles+json` carries the executed plan tree ([API.md](API.md)). |
| Patterns / text / spatial | `ql:has-predicate` patterns, a text index and spatial joins | Adapted. Tantivy BM25 powers `text:query` without a co-occurrence index; a packed R-tree drives GeoSPARQL spatial joins and nearest neighbors ([F03](specs/F03-full-text-search/README.md), [G01](specs/G01-geosparql/README.md)); predicate patterns answer `ql:has-predicate` and subject counts ([X11](specs/X11-index-compression-and-predicate-patterns.md#outcome)). |

## 2a. Oxigraph — what Sparkles reuses

[Oxigraph](https://github.com/oxigraph/oxigraph) (MIT / Apache-2.0) is both a database and
a set of RDF libraries. The database stores data in RocksDB and evaluates SPARQL lazily
with iterators, in `spareval`. Sparkles uses the libraries and replaces the database:

| Oxigraph crate | Role | In Sparkles |
|---|---|---|
| `oxrdf` 0.3, `oxiri`, `oxilangtag` | Term model (IRIs, blank nodes, literals, RDF 1.2 triple terms) and IRI/language-tag validation | Reused throughout for terms; Sparkles keeps its own IDs and vocabulary. |
| `oxttl`, `oxrdfxml`, `oxjsonld`, `oxrdfio` 0.2 | Parsers and serializers for Turtle, TriG, N-Triples, N-Quads, RDF/XML and JSON-LD | Reused for RDF I/O; the bulk loader drives `oxttl`'s parallel chunked parsing. |
| `sparesults` 0.3 | SPARQL result formats (JSON, XML, CSV, TSV) | Reused for result parsing and serialization; Sparkles adds `x-sparkles+json`. |
| `spargebra` 0.4.7 | SPARQL 1.1/1.2 parser and algebra | Vendored with W3C fixes and left-associative arithmetic ([`vendor/spargebra/PATCHED.md`](../vendor/spargebra/PATCHED.md)). |
| `oxsdatatypes` 0.2 | XSD value space (decimal, dateTime, durations) | Reused for literal values and arithmetic. |
| `sparopt`, `spareval` | Algebra optimizer and evaluator | Replaced by Sparkles' dynamic-programming planner and columnar executor. |
| `oxigraph` (store) | RocksDB storage with 9 index orders and in-place updates | Replaced by QLever-style sorted blocks (§2). |
| `spargeo` | GeoSPARQL functions on `geo` | Not used. `sparkles-core` parses geometries directly and implements geodesic measures and a spatial index. |

Oxigraph is one of the engines compared in [BENCHMARKS.md](BENCHMARKS.md).

## 2b. Features beyond the audited projects

[Fluree DB](https://github.com/fluree/db) is licensed under BUSL-1.1, and Sparkles does
not depend on it. Sparkles added several features that Fluree and other databases offer:
durable commit ids, point-in-time reads and named snapshots, full-text and vector search,
dataset access control, an MCP server, and backups to object storage. Their specs were
written from the W3C and IETF standards, the documentation of permissively licensed
libraries, published papers and Sparkles' existing code. The specs are in
[`specs/`](specs/README.md) ([CI](specs/CI-commit-identity.md),
[F06](specs/F06-snapshots-and-point-in-time.md), [F03](specs/F03-full-text-search/README.md),
[F04](specs/F04-vector-search.md), [C09](specs/C09-dataset-access-control.md),
[C11](specs/C11-mcp-server.md), [F05](specs/F05-snapshot-repositories/README.md)), and
[`specs/PROVENANCE.md`](specs/PROVENANCE.md) records the sources of each. Fluree appears
as a benchmark engine, downloaded at benchmark time, and in
[COMPARISON.md](COMPARISON.md#vs-fluree), which draws on its public documentation.

## 3. Language decision: Rust

| Criterion | Rust | Go |
|---|---|---|
| SPARQL parser/algebra | `spargebra`: SPARQL 1.1 and 1.2, SSE output, 500K+ downloads, maintained with Oxigraph | No maintained library. `knakk/sparql` is an HTTP client template library. Go would need a hand-written parser, where Jena has ~50K lines of JavaCC-equivalent code. |
| RDF formats | `oxttl` (Turtle, TriG, NT, NQ and N3, with parallel chunked parsing), `oxrdfxml`, `oxjsonld` and `sparesults` (JSON, XML, CSV, TSV) | `knakk/rdf` (Turtle, NT) and `json-gold`. No RDF/XML library of note and no result-format libraries. |
| XSD datatypes | `oxsdatatypes`: decimal, dateTime and durations, with arithmetic and comparison per XPath | None |
| Reasoning/OWL | `horned-owl`, `reasonable` (OWL 2 RL) and `rudof` (SHACL/ShEx) are available as references | None |
| Performance of QLever-style columnar engine | No GC. `u64` columns, a predictable layout, LLVM autovectorization, `rayon` parallel sort and mmap through `memmap2`. | A GC, which is fine for pointer-free slices but adds tail latency on big materializations. A weaker optimizer and bounds-check elimination, and little SIMD autovectorization. |
| Concurrency | `rayon`, `tokio`/`axum`, `arc-swap`, and persistent collections (`imbl`) for MVCC | Goroutines are excellent, but concurrency is not the bottleneck here. |
| Build/dev ergonomics | Slower compile times | Faster compiles and simpler code |

Rust wins on its ecosystem, since the SPARQL/RDF stack alone saves months, and on the
performance a QLever-style engine can reach. Go's faster compiles and simpler code do not
make up for re-creating the parser, algebra and datatype stack.

## 4. Sparkles architecture

[ARCHITECTURE.md](ARCHITECTURE.md) describes the current library boundaries, storage
and query execution, optimization mechanisms, and decisions across the implemented
specs, with Mermaid diagrams. This audit records the source projects and the rationale
for adopting their mechanisms.

## 5. Explicit non-goals for v1

The original v1 scope left out JavaScript scripting functions, jena-ontapi's object mapping API,
jena-text's Lucene index format and assembler configuration, backward-chaining (LP) rules
and Shiro authentication. Sparkles implements `text:query` itself and has its own
authentication ([C09](specs/C09-dataset-access-control.md)). RDF Thrift, RDF Protobuf,
TriX, RDF/JSON, RDF Patch and GeoSPARQL were also outside that scope, and Sparkles
implements them with limits.

The current [format and feature limits](FEATURES.md#known-gaps) and
[GeoSPARQL divergences](COMPARISON.md#geosparql) are maintained in the product guides.
The design specs record each original choice and its outcome.
