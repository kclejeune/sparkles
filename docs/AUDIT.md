# Audit: Apache Jena, QLever and Oxigraph → Sparkles

Source snapshots: Apache Jena `6.3.0-SNAPSHOT` (b1dcba53b5, 2026‑09‑28), QLever (b0c6d0cd,
2026‑09), Oxigraph (e0f286b0, 2026‑09‑23). Fluree was not audited (§2b).

## 1. Apache Jena — functional inventory

| Module | Java LOC (main/test) | Role | Sparkles status |
|---|---|---|---|
| jena-base | 19K / 8K | utilities, persistent maps (PMap/PSet) for TIM | replaced by Rust std + `imbl` |
| jena-iri3986, jena-langtag | 7K + 2K | RFC 3986 IRIs, BCP47 | `oxiri`, `oxilangtag` |
| jena-core | 141K / 84K | Node/Triple/Graph, Model API, datatypes, **rule reasoners** (RETE fwd, LP bwd), ARP RDF/XML, legacy OntModel | term model via `oxrdf`; reasoners → `sparkles-reasoner` |
| jena-arq | 309K / 82K | **RIOT** I/O + **SPARQL** (JavaCC parser → algebra → 21 optimizer passes → iterator engine), functions, update, SERVICE | `sparkles::{io,sparql}` |
| jena-db + jena-tdb2 | 25K + 19K | DBOE: CoW MVCC B+trees, journal, node table (MD5 → NodeId), inline NodeIds, 3 triple + 6 quad indexes, loaders, compaction | `sparkles::store` (QLever-style instead of B+trees) |
| jena-fuseki2 | ~36K | SPARQL server: query/update/GSP/upload/patch/shacl, `/$/` admin (datasets, stats, compact, backup, tasks, metrics) | `sparkles-server` |
| jena-ontapi | 35K | OWL2 object API (profiles DL/EL/QL/RL, no DL reasoner) | out of scope (see §5) |
| jena-shacl / jena-shex | 23K / 18K | SHACL Core + SPARQL; ShEx 2 | `sparkles-shacl`: SHACL Core + SHACL-SPARQL (W3C 98/98 + 20/20); `sparkles-shex`: ShEx 2.1 (ShExC, ShExJ, shape maps; shexTest validation 99.9%) |
| jena-text | 7.5K | Lucene text index | `text:query` subset over string literals (Tantivy, BM25; not jena-text's Lucene format or assembler) |
| jena-geosparql | 23K | GeoSPARQL 1.0/1.1 on JTS and Apache SIS: `geof:` and `spatialF:` functions, `spatial:` property functions over an STR-tree of feature envelopes, query rewrite, GML/KML/WKT/GeoJSON literals, EPSG CRSs | `sparkles::geo` (`geo` cargo feature) on the `geo`, `wkt`, `geojson`, `geo-index` and `geographiclib-rs` crates: the `geof:` functions over WKT and GeoJSON in built-in CRSs, a packed R-tree per generation with an overlay of commits, the `spatial:` property functions; Jena's behaviour where GeoSPARQL leaves room, with the divergences listed in the README. Not yet: `spatialF:`, query rewrite, GML/KML, other CRSs (§5) |
| jena-rdfpatch, rdfconnection, querybuilder, serviceenhancer, cmds | — | patch logs, client APIs, builders, CLI | CLI → `sparkles` binary; others n/a in Rust |
| jena-tdb1, commonsrdf | — | deprecated | skipped |

Key Jena behaviours to preserve:

* **Data model**: IRIs, blank nodes, literals (lang, datatype; RDF 1.2 triple terms and base direction), triples/quads, datasets with default + named graphs, union default graph option.
* **XSD value space**: numeric tower (integer ⊂ decimal ⊂ float ⊂ double + derived integer types), dateTime/date/time/durations, boolean, strings. Ordering by `ValueSpace` for ORDER BY.
* **RIOT**: Turtle, N-Triples, N-Quads, TriG, RDF/XML, JSON-LD 1.1 (+ RDF/JSON, Thrift, Protobuf, TriX — Jena-specific); streaming `StreamRDF` sinks; result formats JSON/XML/CSV/TSV.
* **ARQ**: full SPARQL 1.1 Query + Update, property paths, aggregates, subqueries, VALUES, SERVICE, EXISTS, function library (XPath `fn:`, `math:`, `afn:`), property functions; optimizer transforms (filter placement, filter equality substitution, TopN, implicit joins, …).
* **TDB2**: inline NodeIds (ints, decimals, doubles, dates, booleans) so FILTER/ORDER BY avoid the node table; MR+SW transactions with snapshot isolation; bulk loader pipeline; compaction into a new `Data-NNNN` generation; backups as `.nq.gz`.
* **Fuseki**: `/{ds}/sparql|query|update|data|get|upload`, `/$/ping|server|datasets|stats|compact|backup|tasks|metrics`.
* **Reasoning**: RDFS (full/default/simple), OWL Micro/Mini/Full rule sets, `GenericRuleReasoner` with Jena rule syntax `[name: (?a p ?b) builtin(?x) -> (?a q ?b)]`.

Conformance suites available in the Jena checkout (to be used by `sparkles` tests):
`jena-arq/testing/rdf-tests-cg/sparql/{sparql10,sparql11,sparql12}`, `jena-arq/testing/rdf-tests-cg/rdf/{rdf11,rdf12}`, `jena-arq/testing/ARQ`, `jena-shacl/src/test/files/std`, `jena-core/testing/wg`.

## 2. QLever — architecture and performance mechanisms

| Area | Mechanism | Adopted in Sparkles |
|---|---|---|
| Ids | 64-bit `ValueId`: 4-bit datatype + 60-bit payload; all-zero = UNDEF; Int/Double/Bool/Date/GeoPoint inline | ✅ (Int/Double/Bool inline; canonical-lexical-only so term identity is exact, like TDB2) |
| Vocabulary | IDs assigned in sort order → range/prefix filters on ids; FSST²-compressed, sparse in-RAM sample | ✅ sorted, front-coded, mmapped; prefix ranges on ids |
| Index build | parallel parse → per-batch partial vocabs → k-way merge → id remap → external sort per permutation | ✅ same pipeline (rayon) |
| Permutations | 6 perms (SPO SOP PSO POS OSP OPS) + graph column; ~31k-row blocks, per-column zstd; first/last triple of each block in RAM | ✅ 6 + GSPO, 32k-row blocks, per-column delta+varint+LZ4, block metadata in RAM for block skipping |
| Updates | immutable base + DeltaTriples located per block, snapshot per version, rebuild when delta grows | ✅ persistent (`imbl`) delta sets per permutation, WAL, MVCC snapshots, `compact` rebuild |
| Planner | DP over connected components with interesting sort orders, greedy fallback past budget; filters applied as soon as bound; cost = row counts; multiplicity-based join estimates | ✅ |
| Execution | column-major `IdTable`; operators materialize (some lazy); LocalVocab per result | ✅ column-major tables, per-query local vocab |
| Joins | zipper merge join w/ UNDEF, galloping join for skewed sizes, MultiColumnJoin, OptionalJoin, Minus, TransitivePath w/ bound side | ✅ merge + galloping + hash join; transitive path with bound-side BFS |
| GROUP BY | COUNT from metadata, sort-based grouping, special cases | ✅ COUNT fast paths + hash grouping |
| Cache | concurrent LRU keyed by subtree + delta version; pinning | ✅ LRU keyed by canonical plan + snapshot version |
| Limits | cancellation handle, memory-limited allocator, timeouts | ✅ cancellation (also on client disconnect) / timeouts; per-query budgets for estimated intermediate-result memory, response bytes and rows (estimates, not an allocator limit) |
| Server | streaming results, `qlever-json` with runtime-information tree, websockets for live plan | ✅ `x-sparkles+json` with executed plan tree (see API.md) |
| Patterns / text / spatial | `ql:has-predicate` patterns, text index, spatial joins | text: BM25 full-text search through `text:query` (Tantivy), no text/entity co-occurrence index; spatial: GeoSPARQL functions and a packed R-tree per dataset (ideas from QLever's geometry precomputation, not its code), no spatial joins yet; patterns ⏭ future work |

## 2a. Oxigraph — what Sparkles reuses

[Oxigraph](https://github.com/oxigraph/oxigraph) (MIT / Apache-2.0) is both a database
(RocksDB storage, lazy iterator-based SPARQL evaluation in `spareval`) and a set of RDF
libraries. Sparkles uses the libraries and replaces the database:

| Oxigraph crate | Role | In Sparkles |
|---|---|---|
| `oxrdf` 0.3, `oxiri`, `oxilangtag` | term model (IRIs, blank nodes, literals, RDF 1.2 triple terms), IRI and language-tag validation | ✅ the term model everywhere; ids and the vocabulary are Sparkles' own |
| `oxttl`, `oxrdfxml`, `oxjsonld`, `oxrdfio` 0.2 | parsers and serializers (Turtle, TriG, N-Triples, N-Quads, RDF/XML, JSON-LD) | ✅ all RDF I/O; the bulk loader drives `oxttl`'s parallel chunked parsing |
| `sparesults` 0.3 | SPARQL result formats (JSON, XML, CSV, TSV) | ✅ result parsing/serialization (plus our own `x-sparkles+json`) |
| `spargebra` 0.4.7 | SPARQL 1.1/1.2 parser and algebra | ✅ vendored with fixes for the W3C tests it failed and for left-associative arithmetic (`vendor/spargebra/PATCHED.md`) |
| `oxsdatatypes` 0.2 | XSD value space (decimal, dateTime, durations) | ✅ literal values and arithmetic |
| `sparopt`, `spareval` | algebra optimizer and evaluator | ✗ Sparkles has its own DP planner and columnar executor |
| `oxigraph` (store) | RocksDB storage, 9 index orders, in-place updates | ✗ Sparkles uses QLever-style sorted blocks (§2) |
| `spargeo` | GeoSPARQL functions on `geo` | ✗ not used: `sparkles::geo` has its own literal parsing (EPSG:4326 axes, CRS IRIs, byte offsets), geodesic measures and a spatial index; `spargeo` was a reference for the `geo` crate family |

Oxigraph is also one of the benchmark engines (`docs/BENCHMARKS.md`).

## 2b. Fluree — not audited

[Fluree DB](https://github.com/fluree/db) is licensed under BUSL-1.1. Sparkles neither
depends on it nor borrows from it: its source, tests and design documents were not read.
Features that other databases, Fluree among them, offer and Sparkles also provides
(durable commit ids, point-in-time reads and named snapshots, full-text and vector
search, dataset access control, an MCP server, backups to object storage) were
specified from the W3C and IETF standards, the documentation of permissively licensed
libraries, published papers and Sparkles' own code. Fluree appears only as a benchmark
engine, downloaded at benchmark time, and in the README comparison, which is based on
its public documentation.

## 3. Language decision: Rust

| Criterion | Rust | Go |
|---|---|---|
| SPARQL parser/algebra | `spargebra` (SPARQL 1.1 + 1.2, SSE output, 500K+ downloads, maintained with Oxigraph) | none maintained (`knakk/sparql` is an HTTP client template lib) — would need a hand-written parser (~50K lines of JavaCC-equivalent in Jena) |
| RDF formats | `oxttl` (Turtle/TriG/NT/NQ/N3, **parallel chunked parsing**), `oxrdfxml`, `oxjsonld`, `sparesults` (JSON/XML/CSV/TSV) | `knakk/rdf` (Turtle/NT), `json-gold`; no RDF/XML of note, no result-format libs |
| XSD datatypes | `oxsdatatypes` (decimal, dateTime, durations, arithmetic/comparison per XPath) | none |
| Reasoning/OWL | `horned-owl`, `reasonable` (OWL2 RL), `rudof` (SHACL/ShEx) available as references | none |
| Performance of QLever-style columnar engine | no GC; `u64` columns, predictable layout, LLVM autovectorization, `rayon` parallel sort, mmap via `memmap2` | GC (fine for pointer-free slices, but tail latencies on big materializations), weaker optimizer / bounds-check elimination, no SIMD autovectorization to speak of |
| Concurrency | `rayon`, `tokio`/`axum`, `arc-swap`, persistent collections (`imbl`) for MVCC | goroutines are excellent, but not the bottleneck here |
| Build/dev ergonomics | slower compile times | faster compiles, simpler code |

Rust wins decisively on ecosystem (the SPARQL/RDF stack alone saves months) and on the performance
envelope for a QLever-style engine. Go's advantages (compile speed, simplicity) don't offset having to
re-create the parser/algebra/datatype stack.

## 4. Sparkles architecture

```
ui/ (SvelteKit)  ──HTTP──▶  sparkles-server (axum; Fuseki protocol + /$/ admin; auth, rate
                                   │         limits, observability; CLI; MCP over stdio)
                                   ├─ sparkles-reasoner (RDFS / OWL-RL / Jena rules, semi-naive forward chaining)
                                   ├─ sparkles-shacl    (SHACL Core + SHACL-SPARQL, write-time validation)
                                   └─ sparkles-backup   (repositories on a file system or S3, backups, restore, policies)
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

JavaScript scripting functions, RDF Thrift/Protobuf, TriX, jena-ontapi's object mapping API,
jena-text's Lucene index format and assembler configuration (Sparkles implements
`text:query` itself), RDF Patch, backward-chaining (LP) rules, Shiro auth
(Sparkles has its own authentication). These are documented extension points rather than
hidden gaps.

GeoSPARQL is in, with these parts left out for now:

* **`spatialF:` functions** (Jena's filter functions: `convertLatLon`, `nearby`,
  `greatCircle`, `azimuth`, …) are deferred; the `geof:` functions cover the same ground.
* **Computed geometries are 2D.** Z and M values are read (`is3D`, `isMeasured`, `minZ`,
  `maxZ` report them) but not kept, so buffers, hulls, overlays and conversions return 2D
  geometries.
* **Unions of curves are not noded:** two lines that cross stay two lines, not split at the
  crossing point as JTS would (the result covers the same points).
* GML and KML literals, query rewrite of the topological properties, RDFS entailment of
  the geometry hierarchy, W3C Basic Geo points in the index, spatial joins, k-nearest
  ORDER BY, the map view in the UI, and CRSs beyond the built-in ones (no EPSG database is
  shipped).
