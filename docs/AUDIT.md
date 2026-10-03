# Audit: Apache Jena, QLever and Oxigraph → Sparkles

This audit covers these source snapshots: Apache Jena `6.3.0-SNAPSHOT` (b1dcba53b5,
2026‑09‑28), QLever (b0c6d0cd, 2026‑09) and Oxigraph (e0f286b0, 2026‑09‑23). Fluree was not
audited (§2b). Each feature Sparkles added beyond this audit has a design spec in
[`specs/`](specs/README.md), which also records how it was implemented.

## 1. Apache Jena — functional inventory

| Module | Java LOC (main/test) | Role | Sparkles status |
|---|---|---|---|
| jena-base | 19K / 8K | Utilities, and the persistent maps (PMap/PSet) for TIM | Replaced by the Rust standard library and `imbl` |
| jena-iri3986, jena-langtag | 7K + 2K | RFC 3986 IRIs and BCP47 language tags | `oxiri`, `oxilangtag` |
| jena-core | 141K / 84K | Node/Triple/Graph, the Model API, datatypes, the rule reasoners (forward RETE, backward LP), the ARP RDF/XML parser and the legacy OntModel | The term model comes from `oxrdf`. The reasoners became `sparkles-reasoner`. |
| jena-arq | 309K / 82K | RIOT I/O and SPARQL (JavaCC parser → algebra → 21 optimizer passes → iterator engine), with functions, update and SERVICE | `sparkles::{io,sparql}` |
| jena-db + jena-tdb2 | 25K + 19K | DBOE: copy-on-write MVCC B+trees, a journal, the node table (MD5 → NodeId), inline NodeIds, 3 triple and 6 quad indexes, loaders and compaction | `sparkles::store`, QLever-style instead of B+trees |
| jena-fuseki2 | ~36K | The SPARQL server: query, update, GSP, upload, patch and shacl endpoints, and the `/$/` admin API (datasets, stats, compact, backup, tasks, metrics) | `sparkles-server` |
| jena-ontapi | 35K | The OWL 2 object API, with DL, EL, QL and RL profiles and no DL reasoner | Out of scope (§5) |
| jena-shacl / jena-shex | 23K / 18K | SHACL Core and SHACL-SPARQL; ShEx 2 | `sparkles-shacl` implements SHACL Core and SHACL-SPARQL and passes the W3C suites (98/98 and 20/20). `sparkles-shex` implements ShEx 2.1 with ShExC, ShExJ, ShExR and shape maps, and passes 99.9% of the shexTest validation tests. Either can validate writes ([C10](specs/C10-write-time-validation.md), [G02](specs/G02-shex.md)). |
| jena-text | 7.5K | A Lucene text index | A `text:query` subset over string literals, on Tantivy with BM25. It does not use jena-text's Lucene format or assembler ([F03](specs/F03-full-text-search.md)). |
| jena-geosparql | 23K | GeoSPARQL 1.0/1.1 on JTS and Apache SIS. It has the `geof:` and `spatialF:` functions, `spatial:` property functions over an STR-tree of feature envelopes, query rewrite, GML, KML, WKT and GeoJSON literals, and EPSG CRSs. | `sparkles::geo` (the `geo` cargo feature), built on the `geo`, `geo-index` and `geographiclib-rs` crates with its own WKT and GeoJSON readers. It has the `geof:` functions over WKT, GeoJSON, GML and KML, a packed R-tree per generation with an overlay of commits, the `spatial:` property functions, Jena's `spatialF:` filter functions, spatial joins, query rewrite, RDFS entailment of the GeoSPARQL vocabulary with geometry types from literals, and projected CRSs from proj4 definitions through `proj4rs`. It follows Jena where GeoSPARQL leaves room, and [COMPARISON.md](COMPARISON.md#geosparql) lists the divergences. EPSG codes need a proj4 definition from the operator, since no EPSG database ships (§5). The design is in [G01](specs/G01-geosparql.md). |
| jena-rdfpatch, rdfconnection, querybuilder, serviceenhancer, cmds | — | Patch logs, client APIs, builders and the CLI | The CLI became the `sparkles` binary. The others do not apply in Rust. |
| jena-tdb1, commonsrdf | — | Deprecated | Skipped |

Sparkles preserves these Jena behaviours:

* **Data model.** IRIs, blank nodes, literals with a language tag or datatype, RDF 1.2
  triple terms and base direction, triples and quads, and datasets with a default graph,
  named graphs and the union default graph option.
* **XSD value space.** The numeric tower (integer ⊂ decimal ⊂ float ⊂ double, plus the
  derived integer types), dateTime, date, time, durations, booleans and strings. ORDER BY
  orders values as `ValueSpace` does.
* **RIOT.** Turtle, N-Triples, N-Quads, TriG, RDF/XML and JSON-LD 1.1. Jena's own
  RDF/JSON, Thrift, Protobuf and TriX are out of scope (§5). Streaming `StreamRDF` sinks. The JSON,
  XML, CSV and TSV result formats.
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

The `sparkles` tests run these conformance suites from the Jena checkout:
`jena-arq/testing/rdf-tests-cg/sparql/{sparql10,sparql11,sparql12}`, `jena-arq/testing/rdf-tests-cg/rdf/{rdf11,rdf12}`, `jena-arq/testing/ARQ`, `jena-shacl/src/test/files/std`, `jena-core/testing/wg`.

## 2. QLever — architecture and performance mechanisms

| Area | Mechanism | Adopted in Sparkles |
|---|---|---|
| Ids | A 64-bit `ValueId`: a 4-bit datatype and a 60-bit payload. All zeros is UNDEF. Int, Double, Bool, Date and GeoPoint are inline. | ✅ Integer, decimal, double, boolean, dateTime and date are inline, but only in canonical lexical form, so term identity is exact, as in TDB2. |
| Vocabulary | Ids are assigned in sort order, so range and prefix filters work on ids. FSST²-compressed, with a sparse sample in RAM. | ✅ Sorted, front-coded and memory-mapped, with prefix ranges on ids. |
| Index build | Parallel parse → per-batch partial vocabularies → k-way merge → id remap → external sort per permutation | ✅ The same pipeline, on rayon. |
| Permutations | 6 permutations (SPO, SOP, PSO, POS, OSP, OPS) plus a graph column. Blocks of ~31k rows with per-column zstd. The first and last triple of each block stay in RAM. | ✅ 6 permutations plus GSPO, in 32k-row blocks with per-column delta, varint and LZ4. Block metadata stays in RAM for block skipping. |
| Updates | An immutable base plus DeltaTriples located per block, a snapshot per version, and a rebuild when the delta grows | ✅ Persistent (`imbl`) delta sets per permutation, a WAL, MVCC snapshots, and `compact` to rebuild. |
| Planner | DP over connected components with interesting sort orders, and a greedy fallback past a budget. Filters are applied as soon as their variables are bound. The cost is row counts, and join estimates use multiplicities. | ✅ |
| Execution | A column-major `IdTable`. Operators materialize their results, though some are lazy. A LocalVocab per result. | ✅ Column-major tables and a local vocabulary per query. |
| Joins | Zipper merge join with UNDEF, galloping join for skewed sizes, MultiColumnJoin, OptionalJoin, Minus, and TransitivePath with a bound side | ✅ Merge, galloping and hash joins. Transitive paths use BFS from the bound side. |
| GROUP BY | COUNT from metadata, sort-based grouping and special cases | ✅ COUNT fast paths and hash grouping. |
| Cache | A concurrent LRU keyed by subtree and delta version, with pinning | ✅ An LRU keyed by the canonical plan and the snapshot version. |
| Limits | A cancellation handle, a memory-limited allocator and timeouts | ✅ Cancellation, including on client disconnect, and timeouts. Per-query budgets cover estimated intermediate-result memory, response bytes and rows. They are estimates, not an allocator limit ([C01](specs/C01-observability-and-budgets.md)). |
| Server | Streaming results, `qlever-json` with a runtime-information tree, and websockets for the live plan | ✅ `x-sparkles+json` with the executed plan tree (see API.md). |
| Patterns / text / spatial | `ql:has-predicate` patterns, a text index and spatial joins | Full-text search uses BM25 through `text:query` (Tantivy), with no text/entity co-occurrence index. GeoSPARQL functions and a packed R-tree per dataset also drive spatial joins and nearest neighbours. The R-tree borrows ideas from QLever's geometry precomputation, not its code ([F03](specs/F03-full-text-search.md), [G01](specs/G01-geosparql.md)). Patterns are future work. |

## 2a. Oxigraph — what Sparkles reuses

[Oxigraph](https://github.com/oxigraph/oxigraph) (MIT / Apache-2.0) is both a database and
a set of RDF libraries. The database stores data in RocksDB and evaluates SPARQL lazily
with iterators, in `spareval`. Sparkles uses the libraries and replaces the database:

| Oxigraph crate | Role | In Sparkles |
|---|---|---|
| `oxrdf` 0.3, `oxiri`, `oxilangtag` | The term model (IRIs, blank nodes, literals, RDF 1.2 triple terms), and IRI and language-tag validation | ✅ The term model, everywhere. Sparkles has its own ids and vocabulary. |
| `oxttl`, `oxrdfxml`, `oxjsonld`, `oxrdfio` 0.2 | Parsers and serializers for Turtle, TriG, N-Triples, N-Quads, RDF/XML and JSON-LD | ✅ All RDF I/O. The bulk loader drives `oxttl`'s parallel chunked parsing. |
| `sparesults` 0.3 | SPARQL result formats (JSON, XML, CSV, TSV) | ✅ Result parsing and serialization. Sparkles adds its own `x-sparkles+json`. |
| `spargebra` 0.4.7 | The SPARQL 1.1/1.2 parser and algebra | ✅ Vendored, with fixes for the W3C tests it failed and for left-associative arithmetic (`vendor/spargebra/PATCHED.md`). |
| `oxsdatatypes` 0.2 | The XSD value space (decimal, dateTime, durations) | ✅ Literal values and arithmetic. |
| `sparopt`, `spareval` | The algebra optimizer and evaluator | ✗ Sparkles has its own DP planner and columnar executor. |
| `oxigraph` (store) | RocksDB storage with 9 index orders and in-place updates | ✗ Sparkles uses QLever-style sorted blocks (§2). |
| `spargeo` | GeoSPARQL functions on `geo` | ✗ Not used. `sparkles::geo` has its own literal parsing (EPSG:4326 axes, CRS IRIs, byte offsets), geodesic measures and spatial index. `spargeo` served as a reference for the `geo` crate family. |

Oxigraph is one of the engines in the benchmarks (`docs/BENCHMARKS.md`).

## 2b. Fluree — not audited

[Fluree DB](https://github.com/fluree/db) is licensed under BUSL-1.1. Sparkles neither
depends on it nor borrows from it, and its source, tests and design documents were not
read. Some features that other databases offer, Fluree among them, were added to Sparkles:
durable commit ids, point-in-time reads and named snapshots, full-text and vector search,
dataset access control, an MCP server, and backups to object storage. Their specs were
written from the W3C and IETF standards, the documentation of permissively licensed
libraries, published papers and Sparkles' existing code. The specs are in
[`specs/`](specs/README.md) ([CI](specs/CI-commit-identity.md),
[F06](specs/F06-snapshots-and-point-in-time.md), [F03](specs/F03-full-text-search.md),
[F04](specs/F04-vector-search.md), [C09](specs/C09-dataset-access-control.md),
[C11](specs/C11-mcp-server.md), [F05](specs/F05-snapshot-repositories.md)), and
[`specs/PROVENANCE.md`](specs/PROVENANCE.md) records the sources of each. Fluree appears
only as a benchmark engine, downloaded at benchmark time, and in
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

```
ui/ (SvelteKit)  ──HTTP──▶  sparkles-server (axum; Fuseki protocol + /$/ admin; auth, rate
                                   │         limits, observability; CLI; MCP over stdio)
                                   ├─ sparkles-reasoner (RDFS / OWL-RL / Jena rules, semi-naive forward chaining)
                                   ├─ sparkles-shacl    (SHACL Core + SHACL-SPARQL, write-time validation)
                                   ├─ sparkles-shex     (ShEx 2.1, write-time validation)
                                   ├─ sparkles-fmt      (formatter; also built for the browser as sparkles-fmt-wasm)
                                   ├─ sparkles-backup   (repositories on a file system or S3, backups, restore, policies)
                                   └─ sparkles-client   (the Rust client; the CLI shares its credentials file code)
                                   │
sparkles (library)
 ├─ id        64-bit tagged ids, inline literals
 ├─ vocab     sorted front-coded base vocab (mmap) + delta vocab + local vocab
 ├─ index     permutation files: 32k-row blocks, per-column compression, in-RAM block metadata
 ├─ builder   parallel bulk loader (partial vocabs → merge → remap → external sort)
 ├─ store     generations, WAL, MVCC snapshots (base ⊕ delta), commit catalog, history,
 │            compaction, backup capture
 ├─ sparql    spargebra → planner (DP + interesting orders) → columnar operators → results
 ├─ text      full-text index (Tantivy)
 ├─ geo       GeoSPARQL: literals, CRSs, units, the geof: functions, the spatial index (geo, geo-index)
 ├─ vector    exact vector similarity
 ├─ codec     gzip / zstd / brotli / LZ4
 └─ io        RDF & result-format parsing/serialization (Oxigraph crates)
```

## 5. Explicit non-goals for v1

Sparkles v1 leaves out JavaScript scripting functions, RDF Thrift and Protobuf, TriX,
jena-ontapi's object mapping API, jena-text's Lucene index format and assembler
configuration, RDF Patch, backward-chaining (LP) rules and Shiro authentication. Sparkles
implements `text:query` itself and has its own authentication
([C09](specs/C09-dataset-access-control.md)). These are documented extension points, not
hidden gaps.

GeoSPARQL is supported ([G01](specs/G01-geosparql.md)), except for these parts:

* **Computed geometries are 2D.** Z and M values are read, and `is3D`, `isMeasured`,
  `minZ` and `maxZ` report them, but they are not kept. Buffers, hulls, overlays and
  conversions return 2D geometries.
* **Unions of curves are not noded.** Two lines that cross stay two lines. JTS would split
  them at the crossing point. The result covers the same points either way.
* **CRSs.** Sparkles ships no EPSG database. CRSs beyond the built-in ones and the 120
  UTM zones need a proj4 definition from the operator (`--geo-crs`), or a build with the
  `geo-epsg` feature. Geographic CRSs on other datums and grid-based datum shifts are
  not supported.
* **GML.** Curved segments (arcs, circles, splines) and solids are not read.
