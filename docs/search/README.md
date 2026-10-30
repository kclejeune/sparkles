# Search and graph analysis capabilities

This page describes the available capabilities and their current limits. The
[capability matrix](../FEATURES.md#capability-status) records delivery status within
each stated scope.

## Search

Full-text, vector, path and graph-algorithm search run inside SPARQL and join with the
rest of a query.

### Full-text search

* Full-text search supports a subset of Jena's `text:query`, ranked by BM25 through
  Tantivy ([API.md](../API.md#full-text-search)). It needs the `text` cargo feature, which
  the server enables.
* Each quad is a document, kept current with each commit. A write stages its documents,
  and the next search or a 1 s tick commits them. A bulk commit updates only the documents
  it changes, and `POST /$/text/{ds}/rebuild` rebuilds the index while writes continue.
* A search returns the top k within a graph. The query string can be a variable that the
  rest of the group binds, and a join can pass a search the subjects it already has.
* The index stems the literals of configured languages with Tantivy's Snowball stemmers
  and stop words, and `lang:` searches use them, as with jena-text's multilingual support.
  Analyzers that reproduce Lucene's for English, French, Spanish, Italian, Portuguese,
  German, Dutch, Russian, Swedish and CJK text can be selected per language instead. With
  them, 1,576 of 1,590 stemmed searches in nine languages find the same literals as
  Fuseki, and the other 14 consist of stop words alone.
* The tokenizer of those analyzers follows the grammar of Lucene's `StandardTokenizer`
  over Unicode 12.1, emoji and South East Asian scripts included, and agrees with Lucene
  on every code point.
* Japanese, Chinese and Korean text can be segmented into words with a dictionary
  through the `ja-dict`, `zh-dict` and `ko-dict` analyzers. `ja-dict` and `ko-dict`
  produce the tokens of Lucene's Kuromoji and Nori analyzers on every line of their
  fixtures, and `zh-dict` segments by jieba's word frequencies. `sparkles text-dict`
  downloads the pinned IPADIC, mecab-ko-dic and jieba sources with verified hashes and
  compiles them.
* An index can be enabled when a dataset is created. A missing or damaged index recovers
  in the background at open while the RDF data stays usable, and text searches answer
  `503` until recovery succeeds. Reconfiguring an index waits for old writers and
  checkpoints before it reuses their paths.
* A background checkpoint synchronizes only the segment files written since the last
  one, from several threads, while documents continue to publish. Under steady writes it
  repeats its unlocked pass over the newest files and takes the writer lock only for a
  short last pass when those repetitions keep being raced.

### Vector search and embeddings

* Vectors are `spk:vector` literals or their compact base64 form `spk:vectorB64`, compared
  with `spk:cosine`, `spk:dot` and `spk:euclidean`.
* `spk:vectorSearch` returns the top k within the active graph. Its query can be a
  literal, an entity, a variable bound by the rest of the group, or a text for an index
  that embeds on write. It takes `ef:`, `exact:true`, `distinct:subject` and
  `candidates:join`. `ORDER BY DESC(spk:cosine(?v, C)) LIMIT k` over one pattern runs as
  an exact vector search.
* Vector indexes pack one predicate's vectors and build an HNSW graph over them in the
  background ([API.md](../API.md#vector-indexes)). `vector.json`, `/$/vector/{ds}/{name}` and
  `sparkles vector` manage them. The graph is a file of the generation that is mapped
  after a restart. New rows are inserted into it after each commit, and a compaction
  carries it over, so neither waits for a full rebuild. Searches overlay deleted and
  not-yet-inserted vectors exactly, past commits are searched exactly, and `M`,
  `efConstruction` and `efSearch` are set per index.
* An index can compute embeddings on write ([API.md](../API.md#embeddings-on-write)). It
  names the literals to embed, by predicate, language, class or a SELECT query, and an
  OpenAI-compatible endpoint such as OpenAI, Ollama or vLLM. A background worker embeds
  new and changed text after each commit, removes the vectors of deleted text, and catches
  up after restarts and bulk loads without resending unchanged text. Long texts can be
  split into overlapping chunks, and requests can be rate-limited by count and by
  estimated tokens. API keys come from operator-defined secrets, and Prometheus metrics
  report requests, failures, the backlog and the lag.
* The standard server includes the `embed-local` runtime and
  runs BERT, XLM-RoBERTa, NomicBERT and Qwen3 sentence-embedding models itself with Candle
  ([API.md](../API.md#model-providers), [F12](../specs/F12-local-embeddings.md)). Models run on
  a small thread pool at a lower priority than queries, load at first use and unload when
  idle. They come from a store of pinned, hash-verified snapshots that
  `sparkles models pull` fills from the Hugging Face Hub or a Nix build provides.
* `spk:hybridSearch` fuses a `text:query` ranking and a `spk:vectorSearch` ranking by
  reciprocal rank fusion and returns the fused score and both ranks
  ([API.md](../API.md#hybrid-text-and-vector-search)).

### Path search

* `SERVICE path:search` returns paths as solutions, one row per edge or per path
  ([API.md](../API.md#path-search), [U09](../specs/U09-path-finder.md#outcome)). It finds one
  shortest path per pair, every shortest path, the k shortest paths with Yen's algorithm,
  or every path up to a length.
* Edges come from a set of predicates, each followed forward, backward or both ways, from
  every triple, or from the solutions of a nested graph pattern. Each edge can be bound as
  a triple term. Sources and targets are constants or variables bound by the rest of the
  group.
* Weights on RDF 1.2 reifiers switch the search to Dijkstra's algorithm. A single pair
  runs from both ends, as a breadth-first search over the PSO and POS permutations
  without weights and as Dijkstra's algorithm with them. Yen's spur searches run from both
  ends too.
* `path:excludePredicate` skips predicates such as `rdf:type`, and `path:maxDegree` keeps
  paths from passing through hubs. Paths never use triples the caller cannot see.

### Graph algorithms

* `SERVICE algo:<name>`, `sparkles algo` and `POST /$/algo/{ds}/run` run graph
  algorithms ([API.md](../API.md#graph-algorithms), [F14](../specs/F14-graph-algorithms.md#outcome)).
  The MCP tool `graph_algorithm` and the Python and Node.js bindings expose them too.
* The algorithms are PageRank and personalized PageRank, betweenness, weakly and strongly
  connected components, label propagation, Louvain and Leiden with hierarchy levels, node
  similarity, triangle counts and clustering, k-core, breadth-first search, single-source
  shortest paths, and FastRP and node2vec embeddings.
* They run on a compressed sparse row projection built from chosen predicates,
  directions, node classes and reifier weights, or from a nested pattern. Results are
  rows that join with the rest of the query, and the projection holds only what the
  caller may read. Under `GRAPH ?g`, each graph's call uses only that graph's edges.
* A named projection is kept in `algo.json`, and built projections are cached for
  committed views.
* Weights follow each algorithm's domain. PageRank requires finite non-negative weights
  and treats nodes with no outgoing weight as dangling, and weights and sums must stay
  finite in f32. Graphalytics clustering defaults to forward edges.
* A run can write its results back in batched commits with a run record, as a
  cancellable task, and embeddings can go to a vector index the same way. Write-back
  replaces counters only within the destination graph.
* All calls share a thread pool, and each has a memory budget that charges every
  workspace before it grows. Long loops check cancellation and the deadline within a
  bounded amount of work.
* HTTP runs check the caller's Query or Update grants and rate limits before they read
  the dataset. A read run waits for the query memory pool as a query does, and a
  write-back run takes its grant when its task starts.


## GeoSPARQL

GeoSPARQL 1.1 and Jena's spatial functions run over a spatial index that is exact for
every snapshot. They need the `geo` cargo feature, which the server enables.

### Geometry literals and functions

* Literals can be `geo:wktLiteral`, `geo:geoJSONLiteral`, `geo:gmlLiteral` and
  `geo:kmlLiteral` ([API.md](../API.md#geosparql)). GML covers the Simple Features levels 0
  and 1 of GML 3.2 with linear curves and surfaces, and KML is version 2.2. Z and M
  layouts and EMPTY are supported, and parse errors give byte offsets. `asGML` and `asKML`
  write GML and KML.
* The built-in CRSs are CRS84, CRS84h, EPSG:4326 and EPSG:4979 with their latitude-first
  axes, and Web Mercator, with OGC, QUDT and EPSG units. The 120 UTM zones use Krüger's
  series. A `--geo-crs` file adds projected CRSs from proj4 definitions, and the server
  resolves 4,916 projected EPSG codes from an EPSG-derived table. Definitions that need
  grid files are refused, and queries warn when a CRS shifts its datum by approximate
  Helmert parameters.
* The functions cover the 24 topological relations and `relate` on DE-9IM, `convexHull`,
  `envelope`, `boundary`, `centroid`, the four overlay operations, the accessors,
  `boundingCircle`, `concaveHull`, `isSimple` and the six `geof:agg…` aggregates
  ([API.md](../API.md#hulls-aggregates-jena-filter-functions-utm-and-conversion)).
* `distance` is geodesic on WGS 84 by default, haversine if a dataset asks for it, and
  Euclidean for projected CRSs. `buffer` makes metric buffers through a local projection,
  and `area`, `length` and `perimeter` are geodesic.
* With `"wgs84": true` in `geo.json`, W3C Basic Geo `wgs84_pos:lat` and `long` pairs are
  indexed as points.

### Spatial index and queries

* Each dataset can have a spatial index, configured by `geo.json`, `/$/geo/{ds}`,
  `sparkles geo-index` or `serve --geo`. It is a packed Hilbert R-tree over a generation's
  geometry literals plus an overlay of committed writes. It stays within the `--geo-mb`
  memory budget and reports its status, rows, skipped literals and CRSs in
  `sparkles_geo_*` metrics.
* The index files in `gen-NNNN/geo/` are checksummed and read in place, so opening a
  database parses no literals. Damaged or foreign files are rebuilt, and compactions and
  bulk loads parse only new literals. The files are not part of backups or clones.
* Jena's `spatial:` property functions `nearby`, `withinCircle`, `withinBox`,
  `intersectBox`, the cardinal directions and their `…Geom` forms use the index, and so do
  spatial FILTERs. Arguments can be variables that the rest of the group binds. Jena's 15
  `spatialF:` filter functions and `spatial:equals` are supported.
* A GeoSPARQL relation, `relate` or a distance bound between two parts of a group runs as
  a spatial join instead of a cross product, and the index answers
  `ORDER BY geof:metricDistance(?w, C) LIMIT k`
  ([API.md](../API.md#spatial-joins-and-nearest-neighbours)). The nearest-neighbour
  search also handles independent join components, continuing through candidate
  batches that produce no rows after the joins and filters. It preserves local filter
  scope and filters connected candidates before crossing independent inputs.
  EXPLAIN shows `SpatialScan`, `SpatialPf`, `SpatialJoin` and `SpatialKnn`, and warns
  when a query misses a join or
  nearest-neighbour plan.
* Query rewrite of the 24 topological properties, over asserted triples and those derived
  through default geometries and serializations, is opt-in per dataset with
  `queryRewrite` ([API.md](../API.md#query-rewrite-spatialequals-and-rdfs-entailment)).
  `infer --vocab geosparql` adds RDFS entailment of the GeoSPARQL, Simple Features and GML
  vocabularies and types each geometry from its serializations, and
  `infer --geo-default-geometry` materializes default geometries.
* `GET /{ds}/geo?bbox=…` returns the indexed geometries in a box as simplified CRS84
  GeoJSON, and `POST /$/geo/convert` converts literals to CRS84 GeoJSON for maps.

## Known gaps

These limits name the roadmap items that address them. Current priorities and
dependencies are recorded in the [central roadmap](../ROADMAP.md).

### Search and graph algorithms

* Full-text search uses Tantivy's Snowball stemmers by default, and the analyzers that
  reproduce Lucene's are selected per language. Lucene's analyzers for other languages,
  such as Greek and Arabic, have no counterpart. Roadmap item 251.
* `zh-dict` segments Chinese by jieba's word frequencies, so its tokens differ from those
  of Lucene's SmartChineseAnalyzer. Kuromoji's normal and extended modes and the user
  dictionaries of Kuromoji and Nori have no counterpart. Where a run of more than 1,024
  UTF-16 units of unknown characters reaches that limit inside a surrogate pair, `ja-dict`
  and `ko-dict` cut it before the pair while Lucene cuts inside it, which can shift later
  positions. Roadmap item 250.
* Full-text documents have one field. Roadmap item 35.
* Enabling or reconfiguring a full-text index builds it while writes wait. A write
  transaction cannot search the full-text index after it changes data, because the index
  covers committed data only. Roadmap item 23.
* A vector index has no quantization, so its packed vectors take 4 bytes per dimension,
  roadmap item 34. Each pass that inserts new rows into a vector index's graph scans all
  rows inserted since the last compaction, item 82.
* A weighted shortest search with `path:maxLength` lists paths in order of cost until one
  is short enough, so a pair whose short paths are all dear runs until the visit limit.
  The UI's path finder always sends a length limit and stops at a million visits. Roadmap
  item 275.
* Weighted `path:allShortest` without `path:maxLength`, cycles through the source and
  weighted searches to several targets run Dijkstra's algorithm from the source alone. Every cheapest path of one pair
  takes 0.9 s on the 10.5M-triple benchmark data. Roadmap item 277.
* Graph algorithms match the Graphalytics reference outputs for BFS, WCC and PageRank on
  the two small official example graphs, through `scripts/graphalytics-reference.py`.
  There is no full Graphalytics driver, no run on the larger datasets, no reference check
  of CDLP, LCC or SSSP, and no certification. Roadmap item 8.
* The UI cannot complete `algo:` names, and openCypher procedures wait for an openCypher
  frontend. Write-back commits through
  the ordinary writer rather than a bulk writer. A write-back run has no deadline unless
  the request or `--update-timeout` gives one. Thread admission has no queue order, so a
  call that needs several threads can wait while one-thread calls take the free slots. A
  background view-only run reports progress by phase only. Roadmap item 8.
* Betweenness requires finite, strictly positive edge weights, while SSSP accepts zero
  weights and PageRank non-negative ones. Roadmap item 79.
* Weighted betweenness refuses a distance addition that cannot keep a strict increase at
  `f64` precision and refuses non-finite distances. Accepted distances and path counts are
  approximate, including rounded path ties, and sampling checks only the selected source
  searches. Roadmap item 80.


### GeoSPARQL

* EPSG codes come from a table of proj4 definitions rather than the EPSG database, and
  the library includes the table only with its `geo-epsg` feature. Geographic CRSs on
  datums other than WGS 84, grid-based datum shifts, and curved GML segments or solids
  are not supported, and Helmert datum shifts are approximate. Roadmap item 24.
* Geometry constructions return 2D results. Unions of crossing lines keep the covered
  points but do not split the lines at each crossing, unlike Jena's JTS engine. Roadmap
  item 70.

## Configuration and usage

The following guides describe configuration and supported UI, command-line, HTTP
or library workflows. Exact HTTP contracts remain in the API reference.

* Read [Full-text search](../API.md#full-text-search).
* Read [Vector indexes](../API.md#vector-indexes).
* Read [Embeddings on write](../API.md#embeddings-on-write).
* Read [Model providers](../API.md#model-providers).
* Read [Hybrid text and vector search](../API.md#hybrid-text-and-vector-search).
* Read [Path search](../API.md#path-search).
* Read [Graph algorithms](../API.md#graph-algorithms).
* Read [GeoSPARQL](../API.md#geosparql).
* Read [Hulls, aggregates, Jena filter functions, UTM and conversion](../API.md#hulls-aggregates-jena-filter-functions-utm-and-conversion).
* Read [Spatial joins and nearest neighbours](../API.md#spatial-joins-and-nearest-neighbours).

## Roadmap

The remaining work described above refers to [item 8](../ROADMAP.md), [item 23](../ROADMAP.md), [item 24](../ROADMAP.md), [item 34](../ROADMAP.md), [item 35](../ROADMAP.md), [item 70](../ROADMAP.md), [item 79](../ROADMAP.md), [item 80](../ROADMAP.md), [item 82](../ROADMAP.md), [item 250](../ROADMAP.md), [item 251](../ROADMAP.md), [item 275](../ROADMAP.md), [item 277](../ROADMAP.md).
The central roadmap owns priority and dependencies. Its
[completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs)
link partially complete specs to their remaining work and acceptance criteria.
