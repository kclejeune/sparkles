# Search and graph analysis

Sparkles runs BM25 full-text search, HNSW vector search, hybrid reciprocal rank fusion, typo-tolerant ontology entity search, weighted path search, graph algorithms, and GeoSPARQL 1.1 queries inside SPARQL or dedicated HTTP endpoints. Every search and algorithm respects MVCC snapshots and caller access grants. The [capability matrix](../FEATURES.md#capability-status) records delivery status for each area.

| Section | What it covers |
|---|---|
| [Search](#search) | Full-text BM25, ontology typeahead search, vector indexes, embeddings on write, path search, and graph algorithms. |
| [GeoSPARQL](#geosparql) | WKT/GeoJSON/GML/KML literals, CRSs, topological and metric functions, R-tree indexes, spatial joins, and nearest-neighbour queries. |
| [Configuration and usage](#configuration-and-usage) | Links to HTTP API endpoints and index settings. |
| [Known gaps](#known-gaps) | Analyzer, quantization, path, Graphalytics, and CRS/geometry limits. |

## Search

Full-text, vector, hybrid, path, and graph-algorithm operators execute inside SPARQL queries and join with surrounding patterns. Entity search powers fast typeahead for ontology browsers and editors.

### Full-text search

Full-text search implements a BM25 subset of Jena's `text:query` backed by Tantivy ([API.md](../API.md#full-text-search)); it requires the `text` cargo feature, which is enabled in standard server builds.

* **Indexing and lazy commits.** Each quad is indexed as one document. Writes stage documents immediately and commit them on the next search or a 1-second timer so write bursts share Tantivy flushes, while each snapshot still sees exact committed results. Bulk commits update only changed documents, and `POST /$/text/{ds}/rebuild` rebuilds online while writes continue. Background checkpoints synchronize only new segment files across multiple threads and take the writer lock only for a short final pass if steady writes keep racing them.
* **Querying and joins.** Searches return the top `k` hits within a graph. Query strings can come from bound SPARQL variables, and surrounding joins can push candidate subjects into the search.
* **Language analyzers and CJK segmentation.** Configured languages use Tantivy's Snowball stemmers and stop-word lists by default, matching `lang:` queries. Optional per-language Lucene-compatible analyzers cover English, French, Spanish, Italian, Portuguese, German, Dutch, Russian, Swedish, and CJK using Lucene's Unicode 12.1 `StandardTokenizer` grammar; across 1,590 stemmed queries in nine languages, 1,576 match Fuseki exactly (the remaining 14 consist solely of stop words). Dictionary-based `ja-dict`, `zh-dict`, and `ko-dict` analyzers segment Japanese, Chinese, and Korean text (matching Lucene Kuromoji and Nori on fixtures and jieba for Chinese), with pinned dictionaries fetched and compiled via `sparkles text-dict`.
* **Online configuration.** Enable or reconfigure a live dataset with `PUT /$/text/{ds}`. Writes continue during the build, and an existing index serves searches under its active configuration until the replacement is ready. A first build returns `503` for text queries while it runs. Persistent datasets resume interrupted configurations on open. An active index stays configured if its replacement fails before publication. Writes use solo commits during a configuration build. The write-latency comparison is tracked in [roadmap item 1345](../ROADMAP.md#performance-follow-ups).
* **Transaction and historical searches.** A search sees earlier changes in the same write transaction. Historical queries can search a retained commit in the live generation within the adjustment budget. A named snapshot created with `text: true` keeps its own index across compaction, restarts and live index changes. See [Named snapshots and retention](../API.md#named-snapshots-and-retention).

### Entity search

`GET /$/ontology/{ds}/search` and the Python, Node.js, and JVM ontology handles provide typo-tolerant entity search over OWL/RDFS classes, properties, individuals, datatypes, and SKOS concepts, schemes, and collections ([API.md](../API.md#ontology)).

* **Matching and fuzzy tolerance.** Search indexes labels, synonyms (SKOS, SKOS-XL, `oboInOwl`), short forms (`oboInOwl:id`, `skos:notation`, prefixed and local names), and definitions across all languages with case, diacritics, and punctuation folded. Queries match exact strings, prefixes, and word prefixes, tolerating 1 edit (including transpositions) from 3 characters and 2 edits from 6 (4 and 8 for partial words being typed) using Levenshtein automata that prune distant prefix runs.
* **Deterministic ranking.** Hits rank in tiers: exact labels, label prefixes, synonyms and short forms, label/synonym words, fuzzy matches, and definition words. Ties break deterministically by fewer edits, non-deprecated status, caller language preference, length, text, and IRI.
* **Per-branch memory index and access control.** Each branch builds its index lazily on first search and updates incrementally from subsequent commits (rebuilding only on large jumps, head rollbacks, missing change records, or restart). On ChEBI lite (238,190 entities), the index uses ~233 MiB (~292 MiB RSS), builds in 1.3 s, and answers typeahead queries within 8.3 ms at p99 ([entity search benchmark](../benchmarks/entity-search/README.md)). Results respect graph and triple grants by checking each matched value against the caller's view.

### Vector search and embeddings

* **Literals and similarity functions.** Vectors are stored as `spk:vector` or compact base64 `spk:vectorB64` literals and compared with `spk:cosine`, `spk:dot`, and `spk:euclidean`.
* **SPARQL search.** `spk:vectorSearch` returns the top `k` matches in the active graph from a vector literal, entity, bound variable, or query text (when the index embeds on write), supporting `ef:`, `exact:true`, `distinct:subject`, and `candidates:join`. Single-pattern `ORDER BY DESC(spk:cosine(?v, C)) LIMIT k` executes as an exact vector search.
* **HNSW indexes.** Configured via `vector.json`, `/$/vector/{ds}/{name}`, or `sparkles vector`, each index packs one predicate's vectors and maintains an HNSW graph (`M`, `efConstruction`, `efSearch`) stored as a memory-mapped generation file ([API.md](../API.md#vector-indexes)). New vectors insert incrementally after commits and carry across compactions without full rebuilds; queries overlay pending/deleted vectors exactly, and historical commits run exact search.
* **Embeddings on write.** Indexes can compute embeddings automatically for literals selected by predicate, language, class, or `SELECT` query through OpenAI-compatible endpoints (OpenAI, Ollama, vLLM) or the built-in `embed-local` Candle runtime supporting BERT, XLM-RoBERTa, NomicBERT, and Qwen3 models ([API.md](../API.md#embeddings-on-write), [F12](../specs/F12-local-embeddings.md)). Background workers embed changed text, drop vectors for deleted literals, split long text into overlapping chunks, enforce request and token rate limits, and catch up after restarts or bulk loads without re-sending unchanged text. Local models load lazily from hash-verified snapshots (`sparkles models pull` or Nix) on a low-priority pool and unload when idle; remote API keys come from operator secrets, and Prometheus metrics track requests, errors, backlog, and lag.
* **Hybrid search.** `spk:hybridSearch` combines `text:query` and `spk:vectorSearch` using reciprocal rank fusion and returns fused scores alongside both ranks ([API.md](../API.md#hybrid-text-and-vector-search)). MCP memory tools reuse the same `sparkles::rank::fuse_rrf` helper and `text:query` execution path.

### Path search

`SERVICE path:search` returns one shortest path per pair, all shortest paths, `k` shortest paths via Yen's algorithm, or all paths up to a maximum length, emitting one solution row per edge or per path ([API.md](../API.md#path-search), [U09](../specs/U09-path-finder.md#outcome)).

* **Edge sources and bindings.** Edges come from selected predicates (forward, backward, or undirected), all triples, or a nested graph pattern, and each edge can bind as an RDF 1.2 triple term. Sources and targets may be constants or bound join variables.
* **Bidirectional and weighted algorithms.** Unweighted single-pair searches run bidirectional BFS over PSO and POS, while unweighted searches with multiple or unbound ends run one-ended BFS. With weights on RDF 1.2 reifiers, single-pair shortest paths, single-pair all-shortest paths, shortest cycles, and Yen's spur searches between distinct nodes run bidirectional Dijkstra, while searches with multiple or unbound ends run one-ended Dijkstra. With `path:maxLength`, weighted single-pair `path:shortest` and Yen's weighted spurs run a bidirectional Pareto-pruned search over `(node, length)` pairs that discards labels dominated in both cost and length instead of enumerating longer paths, falling back to a source-order bounded search when wide weights or overflow could affect ranking. Unbound ends in weighted `path:shortest` with `path:maxLength` run a one-ended Pareto-pruned search, while weighted `path:allShortest` with `path:maxLength` requires both ends bound because it enumerates tied short paths through Yen's algorithm. Ties between weighted paths compare exact sums while edge weights stay within a factor of 2²⁸.
* **Pruning and visibility.** `path:excludePredicate` omits predicates such as `rdf:type`, `path:maxDegree` avoids traversing high-degree hubs, and all traversals exclude triples hidden from the caller.

### Graph algorithms

Graph algorithms run via `SERVICE algo:<name>`, `sparkles algo`, `POST /$/algo/{ds}/run`, MCP `graph_algorithm`, and Python/Node.js bindings ([API.md](../API.md#graph-algorithms), [F14](../specs/F14-graph-algorithms.md#outcome)).

* **Supported algorithms.** PageRank and personalized PageRank, betweenness centrality, weakly and strongly connected components, label propagation, Louvain and Leiden community detection (with hierarchy levels), node similarity, triangle counting and local clustering coefficient, k-core, breadth-first search, single-source shortest paths (SSSP), and FastRP and node2vec embeddings.
* **Projections and visibility.** Algorithms execute over a compressed sparse row (CSR) projection built from selected predicates, directions, node classes, and reifier weights, or from a nested pattern. Named projections live in `algo.json` and cache per committed view. Under `GRAPH ?g`, each graph runs on its own edges, and projections include only caller-visible edges. PageRank requires finite non-negative weights (treating zero-out-weight nodes as dangling, finite in `f32`), and Graphalytics clustering defaults to forward edges.
* **Write-back and resource controls.** Runs can write results or embeddings back in batched commits with a run record as a cancellable task (replacing counters only in the target graph). All runs share a thread pool, reserve workspace memory before growing, poll cancellation and deadlines in long loops, and enforce HTTP Query/Update grants, rate limits, and query memory-pool admission.

## GeoSPARQL

GeoSPARQL 1.1 and Apache Jena spatial functions run over an exact per-dataset spatial index. Spatial support requires the `geo` cargo feature, enabled in standard server builds.

### Geometry literals and functions

* **Serializations.** Supported datatypes are `geo:wktLiteral`, `geo:geoJSONLiteral`, `geo:gmlLiteral` (GML 3.2 Simple Features levels 0 and 1 with linear curves and surfaces), and `geo:kmlLiteral` (KML 2.2), including Z/M dimensions and `EMPTY` geometries with byte-offset parse errors ([API.md](../API.md#geosparql)). `asGML` and `asKML` serialize geometries to GML and KML.
* **Coordinate reference systems.** Built-in CRSs include CRS84, CRS84h, EPSG:4326 and EPSG:4979 (latitude-first axes), Web Mercator, and all 120 UTM zones via Krüger's series, with OGC, QUDT, and EPSG units. `--geo-crs` loads additional projected CRSs from proj4 definitions, and the server resolves 4,916 projected EPSG codes from an EPSG-derived table. Definitions requiring grid files are refused, and approximate Helmert datum shifts emit plan warnings.
* **Functions and aggregates.** Supports all 24 DE-9IM topological relations plus `relate`, `convexHull`, `envelope`, `boundary`, `centroid`, overlay operations, accessors, `boundingCircle`, `concaveHull`, `isSimple`, and all six `geof:agg…` aggregates ([API.md](../API.md#hulls-aggregates-jena-filter-functions-utm-and-conversion)). `distance` computes WGS 84 geodesic distance by default (or spherical haversine when configured in `geo.json`, and Euclidean on projected CRSs), `buffer` creates metric buffers through a local projection, and `area`, `length`, and `perimeter` are geodesic. Enabling `"wgs84": true` in `geo.json` also indexes W3C Basic Geo `wgs84_pos:lat`/`long` pairs as points.

### Spatial index and queries

* **Packed Hilbert R-tree.** Configured via `geo.json`, `/$/geo/{ds}`, `sparkles geo-index`, or `serve --geo`, the spatial index stores a packed Hilbert R-tree in checksummed `gen-NNNN/geo/` files plus an in-memory overlay of committed writes, bounded by `--geo-mb` and reported in `sparkles_geo_*` metrics. Rows that commits add after the overlay was built are kept in a persistent append-only vector that snapshots share, and the budget counts those rows but not the vector's chunk buffers and branches ([X16](../specs/X16-storage-consolidation/SPEC.md#item-1152-outcome)). Files map directly on open without re-parsing literals, rebuild automatically if damaged, and parse only new literals across compactions and bulk loads; index files are excluded from backups and clones.
* **Indexed functions, joins, and nearest neighbours.** Jena's `spatial:` property functions (`nearby`, `withinCircle`, `withinBox`, `intersectBox`, cardinal directions, and `…Geom` forms), 15 `spatialF:` filter functions, `spatial:equals`, and spatial `FILTER` expressions all use the index and accept join-bound variables. Spatial relations, `relate`, and distance bounds between graph patterns execute as spatial joins rather than cross products, and `ORDER BY geof:metricDistance(?w, C) LIMIT k` runs as an indexed nearest-neighbour search, continuing across empty candidate batches even when combined with independent join components ([API.md](../API.md#spatial-joins-and-nearest-neighbours)). `EXPLAIN` shows `SpatialScan`, `SpatialPf`, `SpatialJoin`, and `SpatialKnn` and warns when a query misses a spatial plan.
* **Query rewrite, RDFS typing, and GeoJSON routes.** Opt-in `queryRewrite` rewrites the 24 topological properties over asserted and default-geometry triples ([API.md](../API.md#query-rewrite-spatialequals-and-rdfs-entailment)). `infer --vocab geosparql` derives GeoSPARQL/SF/GML class hierarchies and types geometries from their serialization literals, and `infer --geo-default-geometry` materializes default geometries. `GET /{ds}/geo?bbox=…` returns simplified CRS84 GeoJSON for map tiles, and `POST /$/geo/convert` converts geometry literals to CRS84 GeoJSON.

## Configuration and usage

| Topic | Reference |
|---|---|
| Full-text and ontology entity search | [Full-text search](../API.md#full-text-search), [Ontology](../API.md#ontology) |
| Vector indexes, embeddings on write, and hybrid search | [Vector indexes](../API.md#vector-indexes), [Embeddings on write](../API.md#embeddings-on-write), [Model providers](../API.md#model-providers), [Hybrid search](../API.md#hybrid-text-and-vector-search) |
| Path search and graph algorithms | [Path search](../API.md#path-search), [Graph algorithms](../API.md#graph-algorithms) |
| GeoSPARQL functions, joins, and query rewrite | [GeoSPARQL](../API.md#geosparql), [Hulls and aggregates](../API.md#hulls-aggregates-jena-filter-functions-utm-and-conversion), [Spatial joins and k-NN](../API.md#spatial-joins-and-nearest-neighbours), [Query rewrite](../API.md#query-rewrite-spatialequals-and-rdfs-entailment) |

## Known gaps

Each limit below cites the [central roadmap](../ROADMAP.md) item tracking it.

### Search and graph algorithms

* **Entity search limits.** The entity index lives in memory only and builds lazily on the first search per branch after startup (roadmap [item 657](../ROADMAP.md)). It queries branch heads only, refusing `at` and `inferences` (roadmap [item 658](../ROADMAP.md)). Tokenization splits on spaces, punctuation, camel case, and digits, so unspaced scripts match only as whole texts or prefixes (roadmap [item 659](../ROADMAP.md)).
* **Full-text limits.** Lucene-compatible analyzers do not cover languages outside the ten listed above, such as Greek or Arabic (roadmap [item 251](../ROADMAP.md)). `zh-dict` uses jieba rather than Lucene's SmartChineseAnalyzer, Kuromoji/Nori user dictionaries and Kuromoji normal/extended modes are absent, and 1,024-UTF-16-unit unknown runs split before rather than inside surrogate pairs (roadmap [item 250](../ROADMAP.md)). Documents have a single field (roadmap [item 35](../ROADMAP.md)).
* **Vector index limits.** Vector indexes lack quantization and store 4 bytes per dimension (roadmap [item 34](../ROADMAP.md)). Incremental HNSW insertion passes scan all rows inserted since the last compaction (roadmap [item 82](../ROADMAP.md)).
* **Path search limits.** A single source with multiple bound targets runs a unidirectional search instead of bidirectional pairs for unweighted searches or when `path:maxLength` is unset (roadmap [item 491](../ROADMAP.md)). Yen's unbounded weighted spur searches that close cycles through the source run unidirectional Dijkstra (roadmap [item 492](../ROADMAP.md)). Double-double weight sums remain exact only within a factor of 2²⁸ between edge weights (roadmap [item 493](../ROADMAP.md)).
* **Graph algorithm limits.** Graphalytics verification covers BFS, WCC, and PageRank on the two small official example graphs via `scripts/graphalytics-reference.py`, without a full driver, large-graph runs, CDLP/LCC/SSSP checks, or certification. The UI does not autocomplete `algo:` names, write-back uses the ordinary writer without a default deadline, thread admission has no queue ordering, and background view-only runs report phase-level progress only (roadmap [item 8](../ROADMAP.md)). openCypher procedures await the openCypher frontend (roadmap [item 20](../ROADMAP.md)). Weighted betweenness requires strictly positive weights (roadmap [item 79](../ROADMAP.md)) and refuses non-strict `f64` distance increments or non-finite values, with approximate path counts on rounded ties (roadmap [item 80](../ROADMAP.md)).

### GeoSPARQL

* **CRSs and curves.** Projected EPSG lookup uses a proj4 table (opt-in via the `geo-epsg` library feature) rather than the full EPSG database. Non-WGS 84 geographic datums, grid-based datum shifts, and curved GML segments are not supported, and Helmert shifts are approximate; exact Helmert shifts, additional datums, opt-in grid files, and densified curves are planned (roadmap [item 24](../ROADMAP.md)).
* **Query forms.** Nearest-neighbour searches require a constant query geometry rather than a per-row binding, and QLever `spatialSearch:` syntax, metric-property rewrites, and polygon approximations beyond the cell grid are not implemented (roadmap [item 24](../ROADMAP.md)).
* **Geometry boundaries.** GML solids, 3D constructions, and exact curve arithmetic are outside scope; constructions produce 2D results. Line unions do not split crossing lines at intersections, and Sparkles has no Jena-compatible behavior mode ([COMPARISON.md](../COMPARISON.md#geosparql)).

## Roadmap

Core design specifications for this area are [F03](../specs/F03-full-text-search/SPEC.md), [F04](../specs/F04-vector-search.md), [F07](../specs/F07-path-search.md), [F12](../specs/F12-local-embeddings.md), [F14](../specs/F14-graph-algorithms.md), [G01](../specs/G01-geosparql/SPEC.md), and [U09](../specs/U09-path-finder.md). Entity search measurements live in [docs/benchmarks/entity-search/README.md](../benchmarks/entity-search/README.md).

The central [roadmap](../ROADMAP.md) owns priorities and dependencies, and its [completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs) cover items 8, 20, 23, 24, 34, 35, 79, 80, 82, 250, 251, 491–493, and 657–659.
