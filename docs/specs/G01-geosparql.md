# G01: GeoSPARQL (OGC GeoSPARQL 1.1, Jena spatial extensions, spatial index)

> **Status:** implemented in part (Phases 1–2, part of Phase 3)
>
> **Phases:** Phase 1 covers geometry literals, CRSs and units, the `geof:` functions, the
> spatial index with FILTER pushdown, Jena's `spatial:` property functions, and the server
> and CLI surfaces. Phase 2 covers spatial joins and k-NN, Query Rewrite, `spatial:equals`,
> RDFS entailment and default geometries, the aggregates, hulls and `isSimple`,
> `spatialF:`, UTM zones, persisted index files, W3C Basic Geo points, `GET /{ds}/geo`,
> `POST /$/geo/convert`, the UI maps and Oxigraph's GeoSPARQL tests. Phase 3 shipped GML
> and KML literals, geometry types from literals, variable `spatial:` arguments, a cell
> grid for spatial tests, projected CRSs from proj4 definitions and the opt-in `geo-epsg`
> build feature. The rest of Phase 3 is not built, as the [Outcome](#outcome) lists.
>
> **User docs:** [API: GeoSPARQL](../API.md#geosparql) ·
> [API: hulls, aggregates, `spatialF:`, UTM and conversion](../API.md#hulls-aggregates-jena-filter-functions-utm-and-conversion) ·
> [API: spatial joins and nearest neighbours](../API.md#spatial-joins-and-nearest-neighbours) ·
> [API: query rewrite and RDFS entailment](../API.md#query-rewrite-spatialequals-and-rdfs-entailment) ·
> [API: maps in the web UI](../API.md#maps-in-the-web-ui) ·
> [API: CRSs from proj4 definitions](../API.md#crss-from-proj4-definitions) ·
> [Features](../FEATURES.md#sparql-arq-equivalent) ·
> [Benchmarks: spatial index commit cost](../BENCHMARKS.md#spatial-index-commit-cost) ·
> [Benchmarks: GeoSPARQL Compliance Benchmark](../BENCHMARKS.md#geosparql-compliance-benchmark)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

The design depends on the durable `seq` from the commit-identity work
([CI](CI-commit-identity.md)), on the budgets of [C01](C01-observability-and-budgets.md),
and on the planner and executor as they stood at `e23a1f5`. It changes one README
decision. When Phase 1 lands, GeoSPARQL leaves the "Out of scope for v1" row in README
§Divergences, along with the `docs/AUDIT.md` rows for jena-geosparql and `spargeo`.

## 1. Summary, goals, non-goals

This spec adds GeoSPARQL to Sparkles. It covers:

* the geometry literals `geo:wktLiteral` and `geo:geoJSONLiteral`;
* the GeoSPARQL 1.1 `geof:` functions: the Simple Features, Egenhofer and RCC8 relations,
  `relate`, the non-topological and 1.1 measurement functions, and later the aggregates;
* Jena's `spatial:` property functions and `spatialF:` filter functions;
* the Query Rewrite and RDFS Entailment extensions;
* a spatial index for the common query shapes.

The index speeds up these shapes:

* a FILTER with a constant geometry (`sfWithin(?w, "POLYGON(…)")`, `distance(?w, C, u) < r`);
* a Jena property function (`?f spatial:nearby (51.5 -0.12 5 uom:kilometre 10)`);
* later, a spatial join between two variables (`FILTER(geof:sfContains(?region, ?point))`)
  and k-nearest-neighbour `ORDER BY geof:distance(…) LIMIT k`.

Literals are the source of truth. They are stored as ordinary vocabulary terms and never
rewritten. Parsed geometries and the R-tree are derived data. The index has the same
shape as the vector segment: a base structure per generation plus an overlay. The overlay,
however, is maintained in the commit path, the way [F03](F03-full-text-search.md) maintains
its documents, so every snapshot sees exactly its own geometries. That includes historical
`?at=` snapshots. The index is only an optimization. A query gives the same answer with
it, without it, and while it is building.

**Goals**

* Conformance to these GeoSPARQL 1.1 classes:
  * **Core**;
  * **Topology Vocabulary**, all three relation families;
  * **Geometry Extension**, WKT first and then GeoJSON;
  * **Geometry Topology Extension**, WKT and GeoJSON, with Simple Features, Egenhofer and
    RCC8;
  * **RDFS Entailment Extension**, WKT;
  * **Query Rewrite Extension**, WKT and GeoJSON, all three families.

  §1.1 shows which phase delivers each class. Sparkles states its conformance per class,
  as GeoSPARQL 1.1 Annex A requires.
* Jena compatibility in everything users see: the `geo:`, `geof:`, `spatial:` and
  `spatialF:` IRIs and argument forms. Existing Jena GeoSPARQL queries run unchanged. Their
  answers agree except where §11 lists a deliberate divergence. Each divergence fixes a
  Jena bug or a silent approximation.
* Correct geodesic distance, area, length and metric buffers on geographic CRSs, using
  the WGS 84 ellipsoid and Karney's algorithms.
* Spatial selection that uses the index and is consistent with the snapshot. There is no
  staleness window, and no `503` while an index builds.
* Pure Rust and permissive licenses only (MIT, Apache-2.0, BSD, CC0). The default build
  has no GEOS (LGPL) and no PROJ C library.
* Admin surfaces for the index in HTTP, the CLI and the UI; explain output; and a map view
  in the UI that needs no external tile server.

**Non-goals (all phases unless noted)**

* DGGS literals (`geo:dggsLiteral`, `geof:asDGGS`) and the Geometry Extension DGGS
  conformance class.
* 3D topology and M-aware computation. Sparkles parses, keeps and reports Z and M (`is3D`,
  `isMeasured`, `minZ`/`maxZ`), and every computation ignores them, as GeoSPARQL 1.1 §10.2
  prescribes.
* Arbitrary EPSG CRSs in Phases 1–2. Phase 3 adds an optional pure-Rust transform backend
  (§4.2.4).
* Raster data, routing, map tiles and a tile server.
* Jena's spatial index file format, its assembler vocabulary (`geosparql:GeosparqlDataset`)
  and the GeoSPARQL-Fuseki CLI options. Sparkles configures the index its own way (§2.8).
* QLever's `SERVICE spatialSearch:` syntax (§10; open question 14).

### 1.1 Conformance by phase

| Conformance class (1.1, `http://www.opengis.net/spec/geosparql/1.x/conf/…`) | Phase 1 | Phase 2 | Phase 3 |
|---|---|---|---|
| Core (`/conf/core`) | ✅ Vocabulary only. There is nothing to compute. | | |
| Topology Vocabulary (`/conf/topology-vocab-extension`), sf / eh / rcc8 | ✅ | | |
| Geometry Extension (`/conf/geometry-extension`), WKT | ✅ All functions except the aggregates, `boundingCircle`, `concaveHull`, `asGML` and `asKML`. | ✅ Complete. | |
| Geometry Extension, GeoJSON | ✅ Same as WKT. | ✅ | |
| Geometry Extension, GML / KML | | | ✅ GML 3.2 Simple Features profile and KML 2.2 geometry. |
| Geometry Extension DGGS | ❌ | ❌ | ❌ |
| Geometry Topology Extension, sf / eh / rcc8 × WKT / GeoJSON | ✅ | | GML, KML |
| RDFS Entailment Extension, WKT | | ✅ | GML hierarchy |
| Query Rewrite Extension, sf / eh / rcc8 | | ✅ | |

Phase 1 also delivers the spatial index, FILTER pushdown with constant geometries, and
Jena's `spatial:` property functions with constant arguments. Phase 2 adds spatial joins,
k-NN, the UI map and persisted index files (§6).

## 2. User-visible behavior

### 2.1 Namespaces and reserved terms

| Prefix | IRI | Used for |
|---|---|---|
| `geo:` | `http://www.opengis.net/ont/geosparql#` | Datatypes, properties and the 24 topological properties (Query Rewrite). |
| `geof:` | `http://www.opengis.net/def/function/geosparql/` | Functions. |
| `sf:` | `http://www.opengis.net/ont/sf#` | Geometry type IRIs, used by `geof:geometryType` and RDFS entailment. |
| `uom:` | `http://www.opengis.net/def/uom/OGC/1.0/` | Units. QUDT units also work (§4.3). |
| `spatial:` | `http://jena.apache.org/spatial#` | Jena property functions. |
| `spatialF:` | `http://jena.apache.org/function/spatial#` | Jena filter functions. |

Sparkles defines no GeoSPARQL IRIs of its own. Every term above belongs to OGC or Jena, so
queries are portable.

### 2.2 Data

```turtle
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
@prefix ex:  <http://example.org/> .
ex:paris a geo:Feature ;
    geo:hasDefaultGeometry ex:parisGeom .
ex:parisGeom a geo:Geometry ;
    geo:asWKT "POINT(2.3522 48.8566)"^^geo:wktLiteral .              # CRS84: longitude latitude
ex:louvre geo:hasGeometry [ geo:asWKT
    "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(48.8606 2.3376)"^^geo:wktLiteral ] .  # EPSG:4326: latitude longitude
ex:seine geo:hasGeometry [ geo:asGeoJSON
    "{\"type\":\"LineString\",\"coordinates\":[[2.25,48.84],[2.30,48.86],[2.36,48.85]]}"^^geo:geoJSONLiteral ] .
```

* Geometry literals load through every path (bulk load, GSP, `INSERT DATA`, upload) and
  are stored byte for byte. `"POINT(1 2)"` and `"Point (1.0 2.0)"` are different RDF terms
  with equal geometries. `=` and `sameTerm` still compare terms, and DISTINCT does not merge
  the two. Only the relation functions, such as `geof:sfEquals`, compare geometries.
* A literal whose lexical form is not a valid geometry of its datatype is stored anyway,
  because RDF 1.2 §3.4.2 accepts ill-typed literals. Functions raise a type error on it,
  the index skips it, and the index status counts it under `skipped.malformed`.
* With the index's `wgs84` option on, W3C Basic Geo pairs (`wgs84_pos:lat` and
  `wgs84_pos:long` on one subject) are indexed as points. This is Phase 2. Jena indexes
  these pairs by default when a graph has no GeoSPARQL literals.

### 2.3 Functions (`geof:`)

Every function works in FILTER, BIND, SELECT expressions, ORDER BY and HAVING, with or
without an index. Argument errors are SPARQL type errors, so they leave the variable
unbound in BIND and evaluate to false in FILTER (SPARQL 1.1 §17.2, §17.6; GeoSPARQL 1.1
§10.9.1). In the table, "geom" means a `geo:wktLiteral` or `geo:geoJSONLiteral`, and later
also a `gmlLiteral` or `kmlLiteral`. A geometry result takes the datatype and CRS of the
first geometry argument (GeoSPARQL 1.1 §10.9.1). A function with no geometry argument
returns WKT in CRS84.

Geometric results are new literals in the canonical form of §4.1.5.

| Function | Result | Phase | Semantics (§4 has the details) |
|---|---|---|---|
| `distance(g1, g2, unit)` | `xsd:double` | 1 | The shortest distance. On a geographic CRS it is geodesic on WGS 84 (§4.4.2). On a projected CRS it is Euclidean in CRS units, then converted. `0` when the geometries intersect. |
| `metricDistance(g1, g2)` | `xsd:double` | 1 | `distance(g1, g2, uom:metre)` |
| `buffer(g, radius, unit)` | geom | 1 | The radius is ≥ 0, or < 0 for areal geometries. A linear unit on a geographic CRS gives a metric buffer through a local projection (§4.4.3). An angular unit on a geographic CRS buffers in planar degrees, as Jena does. |
| `metricBuffer(g, radius)` | geom | 1 | `buffer(g, radius, uom:metre)` |
| `convexHull(g)` | geom | 1 | Planar, in the CRS of `g`. |
| `concaveHull(g, targetPercent?)` | geom | 2 | The `geo` concave hull. §4.4.6 documents the parameters. |
| `boundingCircle(g)` | geom | 2 | The smallest enclosing circle (Welzl), as a polygon with 32 segments per quadrant. |
| `envelope(g)` | geom | 1 | The axis-aligned bounding box as a `POLYGON`. A point gives the point, and a degenerate box gives a `LINESTRING`, as in Jena and JTS. |
| `boundary(g)` | geom | 1 | The OGC boundary. Points give an empty geometry, curves give their endpoints (mod-2 rule), and polygons give their rings. |
| `intersection` / `union` / `difference` / `symDifference(g1, g2)` | geom | 1 | Overlay (§4.4.5). `g2` is transformed into the CRS of `g1`. |
| `centroid(g)` | geom (`POINT`) | 1 | The planar centroid in the CRS of `g`, as in Jena (§4.4.4). |
| `getSRID(g)` | `xsd:anyURI` | 1 | The CRS IRI as written, or CRS84 when there is none. Jena returns `xsd:string` (§11 q3). |
| `transform(g, srs)` | geom | 1 | Converts between the built-in CRSs (§4.2). An unknown or unsupported target is a type error. |
| `asWKT(g)` / `asGeoJSON(g)` | `wktLiteral` / `geoJSONLiteral` | 1 | Converts the serialization. GeoJSON output is always CRS84 (RFC 7946; GeoSPARQL Req 26). |
| `asGML(g, profile)` / `asKML(g)` | | 3 | |
| `area(g, unit)` / `metricArea(g)` | `xsd:double` | 1 | Geodesic area on geographic CRSs (Karney), planar otherwise. `0` for non-areal geometries. |
| `length(g, unit)` / `metricLength(g)` | `xsd:double` | 1 | The geodesic or planar length of curves. Areas give their boundary length and points give 0 (§11 q11). |
| `perimeter(g, unit)` / `metricPerimeter(g)` | `xsd:double` | 1 | The boundary length of areal geometries, `0` otherwise. |
| `dimension(g)` | `xsd:integer` | 1 | The topological dimension: 0, 1 or 2. Empty geometries are covered below the table. |
| `coordinateDimension(g)` / `spatialDimension(g)` | `xsd:integer` | 1 | 2, 3 (XYZ or XYM) or 4 / 2 or 3 |
| `is3D(g)` / `isMeasured(g)` / `isEmpty(g)` | `xsd:boolean` | 1 | Read from the literal's declared layout. |
| `isSimple(g)` | `xsd:boolean` | 2 | OGC simplicity: no self-intersection except ring closure. Sparkles implements it itself (§4.4.7). |
| `geometryType(g)` | `xsd:anyURI` | 1 | `sf:Point`, `sf:LineString`, `sf:Polygon`, `sf:MultiPoint`, `sf:MultiLineString`, `sf:MultiPolygon` or `sf:GeometryCollection`. `sf:LinearRing`, `sf:Triangle`, `sf:TIN` and `sf:PolyhedralSurface` when the literal uses those types. |
| `numGeometries(g)` / `geometryN(g, n)` | `xsd:integer` / geom | 1 | Direct members only. An atomic geometry counts as 1, and `geometryN(g, 1)` is `g`. `n` is 1-based, and an out-of-range `n` is a type error. |
| `minX` `minY` `maxX` `maxY` (`g`) | `xsd:double` | 1 | In the literal's own axis order, as in Jena. For EPSG:4326, X is latitude. |
| `minZ` / `maxZ(g)` | `xsd:double` | 1 | A type error when `g` has no Z. |
| `relate(g1, g2, matrix)` | `xsd:boolean` | 1 | Matches the DE-9IM matrix against a pattern `[012TF*]{9}`. |
| 24 topological functions | `xsd:boolean` | 1 | §2.4 |
| `aggBoundingBox`, `aggBoundingCircle`, `aggCentroid`, `aggConvexHull`, `aggUnion`, `aggConcaveHull(g, pct)` | geom | 2 | Custom aggregates (§5.8). |

GeoSPARQL leaves `geof:dimension` of an empty geometry open, and Oxigraph returns `-1`.
Sparkles returns the declared dimension of the empty type (`POINT EMPTY` → 0,
`POLYGON EMPTY` → 2). It returns `-1` for `GEOMETRYCOLLECTION EMPTY` and the empty literal
`""`. It never raises a type error.

Phase 2 adds Jena's `spatialF:` filter functions, with the same argument forms as Jena:

* `convertLatLon(lat, lon)` and `convertLatLonBox(latMin, lonMin, latMax, lonMax)`, which
  both return EPSG:4326 WKT;
* `equals(g1, g2)`;
* `nearby(g1, g2, radius, unit)` and `withinCircle`, which test distance `<` radius;
* `distance(g1, g2, unit)`, `greatCircle(lat1, lon1, lat2, lon2, unit)` and
  `greatCircleGeom(g1, g2, unit)`;
* `angle`, `angleDeg`, `azimuth` and `azimuthDeg`;
* `transform(g, datatype, srs)`, `transformDatatype` and `transformSRS`.

As in Jena, a unit argument may be an IRI, an `xsd:anyURI` literal or a string.
`greatCircle*` follows the dataset's distance model (§4.4.2).

### 2.4 Topological relations

The relation functions take two geometries (`geof:sfIntersects(?a, ?b)`) and return
`xsd:boolean`. `g2` is transformed into the CRS of `g1`. When the two have no common
built-in CRS, the result is a type error. The DE-9IM matrix comes from `geo`'s `Relate`
and is planar in the CRS of `g1`. For a geographic CRS this means planar in longitude and
latitude. Jena, QLever and Oxigraph do the same, and it is the standard's own computation
model.

| Function | Holds when | Note |
|---|---|---|
| `sfEquals`, `ehEquals` | `T*F**FFF*` (topological equality) | The tables print `TFFFTFFFT`. That pattern is false for two equal points, because a point has an empty boundary. Sparkles uses the topological equality of JTS and `geo` (§11 q5). |
| `rcc8eq` | `TFFFTFFFT` | Areas only. |
| `sfDisjoint`, `ehDisjoint` | `FF*FF****` | 1.1 Table 2 misprints this as `FF**FF****`. |
| `rcc8dc` | `FFTFFTTTT` | Areas only. |
| `sfIntersects` | `T********` ∨ `*T*******` ∨ `***T*****` ∨ `****T****` | 1.1 Table 6 misprints the touches pattern. |
| `sfTouches`, `ehMeet` | `FT*******` ∨ `F**T*****` ∨ `F***T****` | False for two points. |
| `rcc8ec` | `FFTFTTTTT` | Areas only. |
| `sfWithin` | `T*F**F***` | |
| `sfContains` | `T*****FF*` | |
| `sfOverlaps` | `T*T***T**` for A/A and P/P; `1*T***T**` for L/L | False when the dimensions differ. |
| `ehOverlap` | `T*T***T**` | |
| `rcc8po` | `TTTTTTTTT` | Areas only. |
| `sfCrosses` | `T*T***T**` for P/L, P/A, L/A; `0********` for L/L | For L/L the vocabulary tables say `0********` and the function tables say `0*T***T**`. Sparkles follows the vocabulary tables and JTS. False for other dimension pairs. |
| `ehCovers` | `T*TFT*FF*` | |
| `ehCoveredBy` | `TFF*TFT**` | |
| `ehInside` | `TFF*FFT**` | |
| `ehContains` | `T*TFF*FF*` | |
| `rcc8tppi` / `rcc8tpp` / `rcc8ntpp` / `rcc8ntppi` | `TTTFTTFFT` / `TFFTTFTTT` / `TFFTFFTTT` / `TTTFFTFFT` | Areas only. |

* **Areas only.** RCC8 relations are defined for regions (A/A). With any non-areal
  argument the result is `false`, as in Jena and GeoSPARQL 1.1 Table 5.
* **Empty geometries.** DE-9IM applies, so an empty geometry is disjoint from everything.
  `sfDisjoint` and `ehDisjoint` are true. `rcc8dc` is false, because an empty geometry is
  not a region. Every other relation is false. Jena returns false for every relation,
  including disjoint (§11 q4).
* `relate(g1, g2, pattern)` matches the computed matrix against any 9-character pattern of
  `T F * 0 1 2`, ignoring case. Any other length or character is a type error. `relate`
  does not short-circuit on empty geometries. The matrix of an empty geometry is all `F`
  except `EE = 2`.

### 2.5 Jena property functions (`spatial:`)

A triple pattern whose predicate is one of these IRIs is a property function, not a data
match. Phase 1 supports constant arguments only, as with `text:query` and
`spk:vectorSearch`.

| Property function | Object list | Matches features whose geometry … |
|---|---|---|
| `spatial:nearby`, `spatial:withinCircle` | `(lat lon radius [unit [limit]])` | Is closer than `radius` to the EPSG:4326 point. The default unit is `uom:kilometre`. |
| `spatial:nearbyGeom`, `spatial:withinCircleGeom` | `(geom radius [unit [limit]])` | Is closer than `radius` to `geom`. |
| `spatial:withinBox` | `(latMin lonMin latMax lonMax [limit])` | Is within the box (`sfWithin`). |
| `spatial:withinBoxGeom` | `(geom [limit])` | Is within `geom`'s envelope. |
| `spatial:intersectBox` | `(latMin lonMin latMax lonMax [limit])` | Intersects the box. |
| `spatial:intersectBoxGeom` | `(geom [limit])` | Intersects `geom`'s envelope. |
| `spatial:north` `south` `east` `west` | `(lat lon [limit])` | Has an envelope that intersects the strip beyond the point. Sparkles uses Jena's definition. North runs from the point's latitude to 90°, across all longitudes. East and west extend up to 180° of longitude from the point and wrap at ±180°. |
| `spatial:northGeom` … `westGeom` | `(geom [limit])` | As above, measured from the edge of `geom`'s envelope. |
| `spatial:equals` | Subject and object are features or geometry literals. | `sfEquals`. Phase 2, with Query Rewrite. |

* **Subject.** The subject is the feature: a variable, an IRI or a blank-node label. `?f`
  binds each feature `F` that has a triple `F p G`, where `p` is a feature link and `G`
  has a matching indexed serialization. The feature links default to
  `geo:hasDefaultGeometry` and `geo:hasGeometry` and are configurable (§2.8). With `wgs84`
  on, the subjects of matching `lat`/`long` pairs also match. A constant subject restricts
  the answer to that feature. As in Jena, `spatial:` functions do not return stand-alone
  geometries that no feature links to. The `geof:` functions and Query Rewrite cover those.
* **Output.** The result has one solution per distinct matching feature (set semantics).
  Jena can repeat a feature that has two matching geometries. Under `GRAPH ?g { … }`, `?g`
  binds the graph of the serialization quad, and the feature link must be in the same
  graph.
* **Limit.** A `limit` above 0 keeps the `limit` matches nearest to the query geometry.
  For the box and cardinal functions, distance is measured to the centre of the query
  envelope. Ties are broken by subject id. Jena applies the limit in index order.
  Sparkles' order is deterministic and more useful. A `limit` of 0 or less, or no limit,
  returns all matches, subject to the row budget.
* **Exact refinement.** Every match is tested exactly against the stated relation. When
  the subject is unbound, Jena skips the exact test for `withinBox` and `intersectBox` and
  returns envelope hits. Those include false positives for non-rectangular geometries
  (§11 q9). The cardinal functions are defined on envelopes, so they need no exact test.
* **Without an index.** When the index is off, building or failed, the answer is the
  same. Sparkles computes it by enumerating the feature links (§5.6.3), within the query's
  time and row budgets. Jena raises "Dataset Context does not contain SpatialIndex".
* **Errors.** These return `400` with the message prefix `spatial:<name>: `:
  * an object that is not a list of the expected shape;
  * a non-numeric coordinate or radius;
  * a latitude outside ±90 or a longitude outside ±180;
  * an unknown unit;
  * a non-integer limit;
  * a variable argument. Phase 3 allows arguments bound from the left.

### 2.6 Query Rewrite Extension (Phase 2)

A triple pattern whose predicate is one of the 24 topological properties (`geo:sfWithin`,
`geo:ehMeet`, `geo:rcc8po`, …) matches two kinds of triple:

* the asserted triples, found by an ordinary scan;
* the derived triples of GeoSPARQL 1.1 §13 (rules `geor:sfWithin` and so on). The subject
  and object are spatial objects `so1` and `so2`. Each one resolves to geometry literals:
  * a **feature** resolves through `so geo:hasDefaultGeometry ?g . ?g asX ?lit`;
  * a **geometry** resolves through `so asX ?lit`;
  * a **literal** in subject or object position is itself. This is a Jena extension.

  `asX` ranges over the configured serialization predicates, by default `geo:asWKT`,
  `geo:asGeoJSON` and `geo:hasSerialization`. The derived triple `(so1, geo:R, so2)` holds
  when some pair of their literals satisfies `geof:R`.

The answer is the set union of the two, as the entailment regime defines it, so a triple
that is both asserted and derived appears once. A feature with several default geometries
relates to another object when any pair of geometries does. The 1.1 text allows several
default geometries, and then `:f1 geo:sfDisjoint :f1` can be true. The four rule cases
include geometry–geometry and feature–geometry pairs. So `?x geo:sfWithin ex:region`
returns both the features and the geometries inside the region, and a feature relates to
its own geometry. Jena behaves the same way. Add `?x a geo:Feature` to keep only features.

* Like the rules, rewrite uses `geo:hasDefaultGeometry`, not `geo:hasGeometry`. RDFS
  entailment does not help data that has only `hasGeometry`, because `hasDefaultGeometry`
  ⊑ `hasGeometry` and not the reverse. Such data needs
  `sparkles infer --geo-default-geometry` (Phase 2). It materializes `hasDefaultGeometry`
  for features with exactly one geometry, like Jena's `applyDefaultGeometry`.
* An unbound predicate (`ex:a ?p ex:b`) matches asserted triples only. 1.1 §13.5 leaves
  this case open.
* Rewrite is on by default, as in Jena. `queryRewrite: false` in `geo.json` turns it off
  for one dataset, and `serve --no-geo-rewrite` turns it off for the server. *(As built,
  the maintainer decided rewrite is off by default. `queryRewrite: true` turns it on per
  dataset. See the Outcome.)*
* §5.6.4 describes the evaluation strategies. With both ends constant, rewrite runs one
  test. With one end constant, it runs an index window query. With both ends variables, it
  runs a spatial self-join within the budgets. The disjoint relations (`sfDisjoint`,
  `ehDisjoint`, `rcc8dc`) cannot use the index, so they enumerate all pairs within the row
  budget.

### 2.7 RDFS Entailment Extension (Phase 2)

Sparkles reasons by materialization (a README decision). The extension is therefore the
GeoSPARQL ontology added to the existing RDFS profile:

* `sparkles-reasoner` embeds the GeoSPARQL 1.1 ontology (`geo`) and the Simple Features
  vocabulary (`sf`). OGC publishes both under the OGC Document License, which is
  permissive. The attribution is kept in the file header and in `THIRD_PARTY_LICENSES.md`.
* `sparkles infer --profile rdfs --vocab geosparql`, or `POST /$/reason/{ds}` with
  `{"profile":"rdfs","vocabularies":["geosparql"]}`, adds them to the TBox. The subclass and
  subproperty closure then lands in `urn:x-sparkles:inferred`:
  `sf:Polygon ⊑ sf:Surface ⊑ sf:Geometry ⊑ geo:Geometry`,
  `geo:asWKT ⊑ geo:hasSerialization`, `geo:hasDefaultGeometry ⊑ geo:hasGeometry`, and so
  on. Queries read the inferred graph
  with `reasoning=true`, as for every other profile. The vocabulary triples are not copied
  into user graphs.
* An optional rule, `geometryTypes` (Phase 3), materializes `?g a sf:Polygon` and similar
  triples from the type of `?g`'s serialization. The standard does not require it: Req 48
  asks for the hierarchy, not for typing from literals. Users still expect
  `?g a sf:Polygon` to work.

### 2.8 HTTP (`/$/` extensions, documented in `docs/API.md`)

| Method | Path | Phase | Description |
|---|---|---|---|
| GET | `/$/geo/{ds}` | 1 | Returns `GeoStatus`, or `{ "enabled": false }` when the index is off. `404` for an unknown dataset. |
| PUT | `/$/geo/{ds}` | 1 | Enables or reconfigures the index from a `GeoConfig` body. Returns `{ status, task }` with task kind `geo-index`. `400` for an invalid config, `403` for a read-only dataset. |
| DELETE | `/$/geo/{ds}` | 1 | Disables the index and removes `geo.json` and the derived files. Returns `204`. |
| POST | `/$/geo/{ds}/rebuild` | 1 | Rebuilds the current generation's base and returns a `Task`. `409` when a rebuild is already running. |
| POST | `/$/datasets` | 1 | Takes an optional `geo: true \| GeoConfig`. |
| GET | `/{ds}/geo?bbox=minLon,minLat,maxLon,maxLat&graph=&predicate=&limit=&tolerance=` | 2 | Returns the indexed geometries in a CRS84 box as a GeoJSON `FeatureCollection`, for the UI map. Each feature's `id` is the subject and its `properties` are `{ subject, feature?, graph, predicate }`. Geometries are simplified with Douglas–Peucker. `tolerance` is in degrees and defaults to the bbox size / 1024. The result is capped at `limit` (default 5,000, max 50,000) and has `truncated: true` when it hits the cap. |
| POST | `/$/geo/convert` | 2 | Converts `{ literals: [{ value, datatype }] }` to CRS84 GeoJSON geometries, with an error for each item that fails. The UI uses it to draw result columns. |

```ts
type GeoConfig = {
  predicates?: string[];          // serialization predicates indexed; default [geo:asWKT, geo:asGeoJSON, geo:hasSerialization]
  featureLinks?: string[];        // default [geo:hasDefaultGeometry, geo:hasGeometry]
  graphs?: { include?: "all" | string[]; exclude?: string[] };   // as text.json
  wgs84?: boolean;                // Phase 2: index wgs84_pos:lat/long pairs as points; default false
  queryRewrite?: boolean;         // Phase 2; default true
  distance?: "geodesic" | "haversine";   // default "geodesic" (§4.4.2)
  maxGeometryBytes?: number;      // default 16 MiB lexical form; larger literals are not indexed
  maxVertices?: number;           // default 1,000,000 per geometry
};
type GeoStatus = {
  enabled: boolean;
  state: "ready" | "building" | "failed" | "over-budget";
  progress?: number; message?: string;
  generation: string; commit: number;              // the base's generation; the view's commit (always the snapshot's)
  rows: { base: number; overlay: number; tail: number };
  literals: number;                                // distinct parsed geometries (geometry column)
  skipped: { malformed: number; unknownCrs: number; tooLarge: number; empty: number };
  crs: { [iri: string]: number };                  // literals per CRS
  memory: { treeBytes: number; geometryBytes: number; overlayBytes: number; budgetBytes: number };
  config: GeoConfig; formatVersion: 1;
  lastBuild?: { at: string; ms: number; rows: number };
};
```

`DatasetInfo` gets `geo: null | { state, rows }` and `DatasetStats` gets
`geo: GeoStatus | null`. The Prometheus metrics are `sparkles_geo_rows{dataset,part}`,
`sparkles_geo_build_seconds`, `sparkles_geo_candidates_total`,
`sparkles_geo_refined_total` (the number of exact tests run) and
`sparkles_geo_matches_total`.

### 2.9 CLI

```sh
sparkles geo-index --loc db                         # enable with defaults (if needed), build, print status
sparkles geo-index --loc db --predicate http://www.opengis.net/ont/geosparql#asWKT --exclude-graph urn:x-sparkles:inferred
sparkles geo-index --loc db --wgs84                 # Phase 2
sparkles geo-index --loc db --rebuild | --status | --disable
sparkles serve --loc ds=/db --geo ds[=geo.json]     # enable for a dataset (also --mem)
sparkles serve … --no-geo-rewrite                   # Phase 2
sparkles infer --loc db --profile rdfs --vocab geosparql [--geo-default-geometry]   # Phase 2
```

`load`, `update`, `query`, `infer`, `compact` and `clone` maintain or copy the index.
`geo.json` is a meta file like `text.json`: `clone` copies it and
[F05](F05-snapshot-repositories.md) backs it up. Derived files are never backed up.
`sparkles check` validates `geo.json`, and from Phase 2 also the headers and checksums of
the persisted files (§5.5). In a build without the feature, `geo-index` exits 2 with
`built without GeoSPARQL (cargo feature "geo")`.

### 2.10 Rust API (`sparkles`, feature `geo`)

```rust
pub mod geo {
    pub const WKT_LITERAL: &str = "http://www.opengis.net/ont/geosparql#wktLiteral";
    pub const GEOJSON_LITERAL: &str = "http://www.opengis.net/ont/geosparql#geoJSONLiteral";
    pub struct Geom { /* §5.2 */ }
    pub fn parse(lex: &str, datatype: &str) -> Result<Geom, GeomError>;   // with byte offset
    pub fn to_wkt(g: &Geom) -> String;              // canonical form, §4.1.5
    pub fn to_geojson(g: &Geom) -> String;
    pub fn literal(g: &Geom) -> oxrdf::Literal;
    pub struct GeoConfig { … }                      // serde = geo.json
}
impl Store / Dataset {
    pub fn enable_geo(&self, cfg: GeoConfig) -> Result<GeoStatus>;
    pub fn disable_geo(&self) -> Result<()>;
    pub fn rebuild_geo(&self) -> Result<GeoStatus>;
    pub fn geo_status(&self) -> Option<GeoStatus>;
}
```

SPARQL stays the query interface. The query builder needs nothing new.

### 2.11 UI (SvelteKit, `ui/`, Phase 2)

* **Map result view.** When a result column holds `wktLiteral` or `geoJSONLiteral`
  values, the results panel gets a "Map" tab next to Table, Graph and Plan. Each feature
  on the map has a popup that shows the row's other bindings (`TermView`). Clicking a row
  in the table highlights its geometry. The client converts EPSG:4326 (an axis swap) and
  EPSG:3857 literals itself. Other non-CRS84 literals go through `POST /$/geo/convert`.
  Literals that cannot be drawn are listed under the map.
* **Explorer.** A resource with a geometry, directly or through a feature link, shows a
  small map card. A "Nearby" action runs `spatial:nearbyGeom` with a radius picker and
  lists the results.
* **Dataset page.** A "Spatial index" panel, like the full-text panel, shows the state,
  the rows (base, overlay, tail), the skipped counts, a CRS histogram, memory use against
  the budget, the configured predicates and feature links, and the last build. Its
  buttons are Rebuild, Configure and Disable, and the tasks they start appear in
  `TaskList`.
* **Query editor.** The prefix completions include `geo:`, `geof:`, `uom:`, `spatial:` and
  `spatialF:`. `examples.ts` gets point-in-polygon, nearby and distance-ranking examples.
* **Library.** The map uses MapLibre GL JS 6 (BSD-3-Clause). It is loaded with a dynamic
  `import()` only when a map opens, so other pages do not pay for it. Leaflet 1.9
  (BSD-2-Clause) is the fallback if the bundle is too large (§11 q13).
* **Basemap.** The UI uses no external tile server by default. The OSM tile usage policy
  forbids offline and bulk use and requires a unique User-Agent, and the map must work on
  an air-gapped server. The UI ships Natural Earth 1:110m land, coastlines and country
  boundaries (public domain) as a small vector style, precompressed by
  `ui/scripts/precompress.mjs`. An operator can point `serve --map-style-url URL` at a
  MapLibre style JSON. The server's CSP then allows that origin for `connect-src` and
  `img-src`. Otherwise the UI contacts nothing external. When the style needs an
  attribution, the style supplies it.
* **`api.ts`** gets `geoStatus`, `geoConfigure`, `geoDisable`, `geoRebuild`, `geoBox` and
  `geoConvert`. `ui/mock/server.mjs` emulates `/$/geo/*` and `/{ds}/geo`.

## 3. Standards basis

* **OGC GeoSPARQL 1.1** (OGC 22-047r1, 2024; ISO 19186-1). The spec relies on:
  * the conformance classes (Annex A);
  * the vocabulary (§§7–9);
  * the literals (§10.2–10.8): Req 14–17 for WKT, 25–27 for GeoJSON, 20–22 for GML and
    30–32 for KML;
  * the functions (§10.9, Annex B) and the relation patterns (Tables 2, 5–8);
  * RDFS Entailment (§12, Req 47–49) and Query Rewrite (§13, rules `geor:*`).
* **GeoSPARQL 1.0** (OGC 11-052r4), where 1.1 changed something. In 1.0, for example, the
  WKT separator could only be spaces, and `hasDefaultGeometry` belonged to the Geometry
  Extension.
* **OGC/ISO Simple Features** (OGC 06-103r4 / ISO 19125-1) and **ISO 13249-3** for the WKT
  grammar, the geometry types, the boundary rule and DE-9IM. DE-9IM comes from Clementini,
  Di Felice and van Oosterom (1993) and Egenhofer and Franzosa (1991). RCC8 comes from
  Randell, Cui and Cohn (1992).
* **RFC 7946** (GeoJSON) for the geometry objects, CRS84, the removal of the `crs` member
  and the antimeridian guidance.
* **OGC CRS registry** (`http://www.opengis.net/def/crs/…`) and the EPSG axis orders:
  latitude, longitude for 4326 and longitude, latitude for CRS84. Also the OGC unit IRIs
  (`http://www.opengis.net/def/uom/OGC/1.0/…`) and the QUDT units that 1.1 §10.3
  recommends.
* **Karney 2013**, "Algorithms for geodesics", for geodesic distance and area on the
  ellipsoid, with the WGS 84 parameters `a = 6378137 m` and `f = 1/298.257223563`.
* **SPARQL 1.1 Query**: extension functions (§17.6), error semantics (§17.2), BGP and join
  semantics (§18), custom aggregate IRIs (§11), datasets and the active graph (§13), and
  collections for property-function arguments (§4.2.3). Also SPARQL 1.1 Entailment Regimes
  (RDFS), which 1.1 Req 47 names.
* **RDF 1.2 Concepts**: ill-typed literals (§3.4.2) and datatypes (§5).
* **R-trees**: Guttman 1984, STR packing (Leutenegger, Lopez and Edgington 1997), Hilbert
  packing (Kamel and Faloutsos 1993) and best-first k-NN (Hjaltason and Samet 1999).
* **Apache Jena GeoSPARQL** (the documentation and the `jena-geosparql` sources,
  Apache-2.0) for the `spatial:` and `spatialF:` vocabularies, their argument forms and
  defaults, and the behaviours that §11 compares.

## 4. Semantics

### 4.1 Literals

#### 4.1.1 `geo:wktLiteral`

```
wktLiteral := ws [ "<" IRI ">" lwsp ] geometry ws      | ws          (empty literal → empty geometry, Req 17)
lwsp       := 1*( %x20 / %x09 / %x0A / %x0D )            (1.1 allows any whitespace; 1.0 spaces)
geometry   := ISO 13249-3 / OGC 06-103r4 WKT, keywords case-insensitive,
              Z / M / ZM dimension markers, "EMPTY" at any level
```

* The IRI must be absolute. Sparkles normalizes aliases, looks the IRI up in the CRS
  table (§4.2), and keeps it verbatim for `getSRID`.
* Sparkles strips the `<IRI>` prefix itself and hands the rest to the `wkt` crate 0.14,
  which handles case-insensitive keywords, Z/M/ZM and `EMPTY`. Jena splits ordinates with
  hand-written code. In Sparkles any whitespace separates them, including tabs, newlines
  and repeated spaces.
* The accepted types are `POINT`, `LINESTRING`, `POLYGON`, `MULTIPOINT` (with or without
  inner parentheses), `MULTILINESTRING`, `MULTIPOLYGON` and `GEOMETRYCOLLECTION`.
  `LINEARRING`, `TRIANGLE`, `TIN` and `POLYHEDRALSURFACE` are also accepted. Computation
  treats them as polygons and multipolygons, but `geometryType` reports the written type.
  `CIRCULARSTRING`, `COMPOUNDCURVE`, `CURVEPOLYGON` and the other curved types are
  ill-typed (`unsupported geometry type`).
* Ordinates are decimal or scientific numbers (`-1.5e3`), parsed as `f64`. NaN, infinities
  and hex are ill-typed.
* A missing multi-geometry member (`MULTIPOINT((1 2),)`) and unbalanced parentheses make
  the literal ill-typed. So does nesting deeper than 32 `GEOMETRYCOLLECTION` levels.
* The parser checks only the structure each type needs. A `LINESTRING` needs at least 2
  points, or `EMPTY`. A polygon ring needs at least 4 points and must be closed (first =
  last). A literal that breaks these rules is ill-typed. Self-intersecting polygons are
  accepted, as in Jena. Overlay and relate on them give `geo`'s results, and `isSimple`
  reports them as not simple.
* A geometry that mixes coordinates with and without Z is ill-typed.

#### 4.1.2 `geo:geoJSONLiteral`

* The literal is an RFC 7946 Geometry object (`Point`, `LineString`, `Polygon`,
  `MultiPoint`, `MultiLineString`, `MultiPolygon` or `GeometryCollection`), parsed with
  `geojson` 1.0. `Feature` and `FeatureCollection` are ill-typed, because GeoSPARQL
  specifies "GeoJSON Geometry objects".
* The empty literal `""` and `null` are empty geometries (Req 27).
* The CRS is always CRS84 (Req 26). A `crs` member, from pre-RFC 7946 GeoJSON, makes the
  literal ill-typed. Sparkles does not silently ignore it.
* A position with 3 elements is XYZ. A position with 1 element, or with 4 or more, makes
  the literal ill-typed, since GeoJSON has no M.
* Ring orientation is not checked. RFC 7946 §3.1.6 says parsers should not reject a ring
  for its orientation.

#### 4.1.3 Other datatypes

Phase 3 adds `gmlLiteral` and `kmlLiteral`. `gmlLiteral` covers levels 0 and 1 of the GML
3.2 Simple Features profile (10-100r3), plus `gml:Curve` and `gml:Surface` with linear
segments only. `kmlLiteral` covers the KML 2.2 `Point`, `LineString`, `LinearRing`,
`Polygon` and `MultiGeometry`. Until then, values of these datatypes are ill-typed
geometry arguments, so functions raise a type error on them, and the index skips them.
The index reports them once in `GeoStatus.skipped` as `unsupportedDatatype`, a Phase 3
field.

#### 4.1.4 Empty geometries and dimensions

`""`, `POINT EMPTY`, `GEOMETRYCOLLECTION EMPTY` and so on are empty geometries, and
`isEmpty` is true for them. They occupy no space, so they are never indexed. The index
counts them in `skipped.empty`. Relations follow §2.4.

* `distance` involving an empty geometry is a type error, because there is no closest
  point.
* Measures are 0.
* Constructive functions return an empty geometry of the result type. The `intersection`
  of two disjoint polygons is `POLYGON EMPTY` in WKT, or
  `{"type":"GeometryCollection","geometries":[]}` in GeoJSON.
* `envelope`, `centroid`, `boundary` and `convexHull` of an empty geometry return
  `GEOMETRYCOLLECTION EMPTY`.
* `minX` and the other bounds of an empty geometry are a type error.

#### 4.1.5 Canonical output form

Sparkles never rewrites stored literals. The literals it creates as function results are
in canonical form:

* **WKT** is `[<IRI> ]TYPE[ Z| M| ZM](…)`. The type is uppercase, there is no space before
  `(`, `, ` separates points and one space separates ordinates. The CRS prefix is omitted
  for CRS84, as Jena does. Oxigraph always writes it. Numbers use Rust's shortest
  round-trip `f64` formatting, with these adjustments:
  * integral values have no `.0` (`POINT(2 48.8566)`);
  * `-0` is written as `0`;
  * values between 1e-6 and 1e21 have no exponent, and other values use exponent forms
    such as `1e-7`.

  Empty geometries are written `POINT EMPTY` and so on.
* **GeoJSON** is compact JSON with the members in the order `type`, then `coordinates` or
  `geometries`. It uses the same number formatting and has no `bbox`. Polygons keep the
  input's ring orientation. RFC 7946 §3.1.6 recommends right-hand rings for output.
  Sparkles keeps the computed orientation, and `asGeoJSON` forces exterior rings
  counter-clockwise, as Jena's writer does.
* Jena rounds transformed and constructed coordinates to 6 decimal places
  (`PRECISION_MODEL 1e6`). Sparkles does not, so results can differ from the 7th decimal
  on. A value-based comparison of results (`bench-answers.py`) must compare geometries
  with a tolerance.

### 4.2 Coordinate reference systems

#### 4.2.1 Built-in table (Phase 1)

| CRS IRI(s), after normalization | Kind | Axis order of the literal | Notes |
|---|---|---|---|
| `http://www.opengis.net/def/crs/OGC/1.3/CRS84` (the default) | geographic 2D | lon, lat | |
| `http://www.opengis.net/def/crs/OGC/0/CRS84h` | geographic 3D | lon, lat, h | Z is the ellipsoidal height. |
| `http://www.opengis.net/def/crs/EPSG/0/4326` | geographic 2D | **lat, lon** | Req 16 puts the axes in the CRS's order. |
| `http://www.opengis.net/def/crs/EPSG/0/4979` | geographic 3D | lat, lon, h | |
| `http://www.opengis.net/def/crs/EPSG/4326` (no `/0/`, from legacy GeoSPARQL 1.0 examples) | geographic 2D | lon, lat | Jena treats it as an alias of CRS84. Sparkles keeps that for compatibility. |
| `http://www.opengis.net/def/crs/EPSG/0/3857` (and `…/900913`) | projected (Web Mercator, spherical) | x, y (metres) | Closed-form forward and inverse projection. |
| `http://www.opengis.net/def/crs/EPSG/0/326NN`, `…/327NN` (UTM zones on WGS 84) | projected | E, N (metres) | Phase 2. Transverse Mercator with Krüger's 6th-order series (Karney 2011). |

Alias normalization maps these forms to the `http://www.opengis.net/def/crs/…` IRIs:

* `https://www.opengis.net/…` to `http://…`;
* `urn:ogc:def:crs:OGC:1.3:CRS84`, `urn:ogc:def:crs:EPSG::4326` and similar URNs;
* the short forms `CRS:84` and `EPSG:4326`.

Aliases affect only the CRS lookup. The stored term never changes.

#### 4.2.2 Axis order and internal coordinates

Internally every geometry is held as `(x, y)` = (east, north). That is (lon, lat) for
geographic CRSs and (E, N) for projected ones. EPSG:4326 and 4979 literals are swapped on
parse, and swapped back when a result in that CRS is written. All computation, the index
and GeoJSON use the internal order. `minX`, `maxX`, `minY` and `maxY` report the literal's
own axes (§2.3). Axis order is the most common GeoSPARQL mistake, so the documentation
calls it out.

#### 4.2.3 Mixed CRSs and unknown CRSs

* When a binary function gets arguments in two different built-in CRSs, it transforms
  `g2` into the CRS of `g1`. GeoSPARQL says calculations happen in the SRS of `geom1`.
  All the built-in geographic CRSs use WGS 84, so transforms between them are axis swaps.
  Transforms between geographic and projected CRSs use the projection formulas.
* A literal whose CRS IRI is not in the table is still a valid geometry. These functions
  work on it in its native coordinates: `getSRID`, `geometryType`, `dimension`, `isEmpty`,
  `asWKT`, `numGeometries`/`geometryN`, `min*`/`max*`, `envelope`, `convexHull`,
  `boundary` and `centroid`. Planar relations and overlay also work between two
  geometries in the same unknown CRS. Metric functions (`metricDistance`, `metricArea`,
  …), unit conversions and any mix with another CRS are type errors. The index skips such
  literals and counts them in `skipped.unknownCrs`, with their IRIs counted in
  `GeoStatus.crs`. Jena instead logs a warning and treats the coordinates as CRS84
  degrees, which silently gives wrong answers (§10).

#### 4.2.4 Additional CRSs (Phase 3)

An optional feature, `geo-proj4`, uses `proj4rs`. It is a pure-Rust port of proj4js
(MIT/Apache-2.0) and supports tmerc, lcc, laea, aea, stere, merc and other projections.
The CRS definitions come from a `crs.json` file that the operator supplies
(`{ "<CRS IRI>": { "proj4": "+proj=…", "axis": "en" | "ne" } }`), so Sparkles ships no
EPSG dataset. The EPSG terms of use forbid distribution for profit and require a notice
to recipients. Whether that is acceptable is a licensing question for the maintainer
(§11 q16). For that reason `crs-definitions` (CC0, but derived from EPSG) is not bundled
by default. `proj` (MIT bindings to PROJ 9, MIT) stays out of the default build
(§10). It could become another opt-in feature.

### 4.3 Units

A unit can be given as an IRI or as an `xsd:anyURI` literal. `spatialF:` functions also
accept a string. These units are accepted:

| Kind | OGC (`uom:` prefix) | QUDT (`http://qudt.org/vocab/unit/`) | EPSG URNs (`urn:ogc:def:uom:EPSG::`) |
|---|---|---|---|
| length | `metre`/`meter` (1), `kilometre`/`kilometer` (1000), `centimetre`/`centimeter`, `millimetre`/`millimeter`, `mile`/`statuteMile` (1609.344), `nauticalMile` (1852), `yard` (0.9144), `foot` (0.3048), `inch` (0.0254), `surveyFootUS` (1200/3937) | `M`, `KiloM`, `CentiM`, `MilliM`, `MI`, `MI_N`, `YD`, `FT`, `IN`, `FT_US` | 9001, 9036, 1033, 1025, 9093, 9030, 9096, 9002, 9003 |
| angle | `radian`, `microRadian`, `degree`, `minute`, `second`, `grad` | `RAD`, `MicroRAD`, `DEG`, `ARCMIN`, `ARCSEC`, `GON` | 9101, 9109, 9102, 9103, 9104, 9105 |
| area | `squareMetre`/`square_metre`/`square_meter`, `squareKilometre`/`square_kilometre`, `hectare`, `acre` (the OGC-namespace forms that Oxigraph accepts) | `M2`, `KiloM2`, `HA`, `AC`, `ARE`, `MI2`, `FT2`, `YD2` | — |

* An unknown unit IRI is a type error. Jena throws an `UnitsURIException`, which also
  becomes an expression error.
* **Linear units on a geographic CRS** work everywhere. Distance and length are geodesic
  metres converted to the unit, and buffer uses §4.4.3. Jena rejects metric buffers on
  geographic data and requires degrees.
* **Angular units on a geographic CRS.** `distance` returns the great-circle central angle
  between the closest points, computed with haversine on the geodetic coordinates.
  `buffer` buffers planarly in degrees, as Jena does.
* **Angular units on a projected CRS** are a type error.
* Area functions accept only area units. A length unit there is a type error.

### 4.4 Computation model

#### 4.4.1 Engine, precision, robustness

* The geometry algorithms come from `geo` 0.33 (MIT/Apache-2.0):
  * `Relate` (full DE-9IM), and `indexed::PreparedGeometry` for repeated relates against
    one geometry;
  * `BooleanOps` and `unary_union`, built on `i_overlay`;
  * `Buffer` (available since 0.31), `ConvexHull`, `ConcaveHull`, `Centroid`,
    `BoundingRect`, `Simplify` and `GeodesicArea`;
  * the metric-space `Distance` and `Length` API with `Euclidean` and `Geodesic`. The
    geodesic metric is Karney's, through `geographiclib-rs` (MIT).
* Coordinates are `f64`. Like `geo`, the predicates use `robust`'s adaptive-precision
  orientation tests, so relate results are exact for the input coordinates. Overlay and
  buffer compute their output coordinates in floating point, without snapping to a
  precision grid. Results can differ from JTS and GEOS in the last bits and in degenerate
  configurations. The acceptance tests (§7) therefore compare constructed geometries with
  `sfEquals` or with a coordinate tolerance, never as text.
* Relations on geographic CRSs are planar in (lon, lat). That is the standard's model, and
  every engine surveyed does the same. A geometry that crosses the antimeridian must use
  longitudes beyond ±180 or be split into a `MULTI*`, as RFC 7946 §3.1.9 recommends.
  Sparkles does not unwrap it.

#### 4.4.2 Distance

* **Projected CRS, or two geometries in the same unknown CRS.** The distance is Euclidean
  between the closest points (`geo` `Euclidean`), in CRS units, converted to the requested
  unit.
* **Geographic CRS with `distance: "geodesic"`, the default.**
  * Between two points, the distance is the WGS 84 geodesic (Karney, `geo::Geodesic`).
  * For other geometries, the distance is 0 if they intersect. Otherwise Sparkles maps
    both geometries into a local azimuthal equidistant projection (spherical AEQD, about
    60 lines of our own code). The projection is centred on the midpoint of the gap
    between their envelopes. Sparkles finds the closest points in that plane (`geo`
    `ClosestPoint`), maps them back and measures the geodesic between them.

    The result is always a true geodesic length between two points of the geometries, so
    it is never below the true distance. The target overestimate is under 0.1% for gaps
    up to 1,000 km and under 0.5% beyond. Tests verify it against GeographicLib on random
    pairs.
* **Geographic CRS with `distance: "haversine"`.** This is Jena's model, for
  compatibility. The closest pair is found planarly in degrees, with an antimeridian
  adjustment. The great-circle distance is then computed with the haversine formula on a
  sphere of radius 6,371,008.7714 m.
* The two models differ by up to 0.56%. One degree of longitude on the equator is
  111,319.491 m geodesic and 111,195.080 m haversine. Open question 2 records the default.
* **Lower bound for index pruning.** Take a sphere of radius `a(1 − e²)` = 6,335,439 m, the
  smallest meridional radius of curvature. The haversine distance on that sphere never
  exceeds the WGS 84 geodesic distance between the same geodetic coordinates. Both
  principal radii of curvature are at least that radius everywhere, so every curve is at
  least as long on the ellipsoid. The index uses this bound for radius windows and k-NN
  ordering (§5.7), so geodesic answers are exact.

#### 4.4.3 Buffer

* **Projected CRS, or an angular unit on a geographic CRS.** Sparkles runs the planar
  `geo` `Buffer` in CRS units, with round joins and caps and 8 segments per quarter circle
  (the JTS and Jena default). A negative radius shrinks an areal geometry, giving an empty
  geometry when it vanishes. For points and lines a negative radius is a type error.
* **Linear unit on a geographic CRS** (`metricBuffer`, `buffer(…, uom:metre)`). Sparkles:
  1. projects into a spherical AEQD centred on the geometry's envelope centre;
  2. buffers planarly in metres;
  3. projects back.

  For geometries whose extent plus radius is under 1,000 km, the target is a boundary
  within 0.5% of the radius from the true geodesic offset. Tests verify it against
  GeographicLib's direct solution. Larger inputs are a type error with the message
  `buffer: geometry too large for a metric buffer (extent + radius > 1000 km)`, so the
  user never gets a silently wrong shape. A buffer that would cover a pole is also a type
  error.

#### 4.4.4 Area, length, perimeter, centroid

* **`area` and `metricArea`.** On a geographic CRS, Sparkles uses
  `GeodesicArea::geodesic_area_unsigned` on the ellipsoid. Holes are subtracted and
  multipolygon members are summed. Overlapping members are merged with `unary_union`
  first, as QLever does. On a projected CRS it uses the planar `Area`. Non-areal
  geometries have area 0.
* **`length`.** A curve gives its geodesic or planar length. An areal geometry gives the
  length of all its rings, and a collection the sum of its members. A point gives 0.
  QLever uses only the exterior ring of a polygon (open question 11).
* **`perimeter`.** An areal geometry gives the length of all its rings. Other geometries
  give 0.
* **`centroid`.** The centroid is planar in the CRS of `g`, as in Jena and as the
  standard's "calculations in the SRS" implies. For geographic data that spans more than a
  few degrees, this is not the geodesic centroid. `aggCentroid` (Phase 2) behaves the same
  way.

#### 4.4.5 Overlay

`intersection`, `union`, `difference` and `symDifference` use `geo` `BooleanOps` when both
geometries are areal. The other dimension pairs work as follows:

* point with anything: point location (`CoordinatePosition`);
* line with area: `BooleanOps::clip`, keeping the inside or the outside;
* line with line: `line_intersection` over a segment R-tree finds the intersection points
  and segments, and union nodes both lines;
* collections: member by member, then `unary_union`.

The result dimension follows OGC, so `intersection` keeps the lowest-dimension parts that
exist. A result that `geo` cannot produce for a pair is a type error
(`intersection: unsupported for these geometry types`). The tests (§7) list the pairs
that are covered.

#### 4.4.6 Hulls and simplicity (Phase 2)

* **`concaveHull(g[, targetPercent])`** uses `geo` `ConcaveHull` (concaveman). GeoSPARQL
  requires the implementation to document its parameters. Sparkles documents one optional
  second argument, which maps `targetPercent ∈ (0, 100]` to the concavity. The default
  concavity is 2.0, `geo`'s default.
* **`isSimple`** is defined per geometry type:
  * a point is always simple;
  * a multipoint is simple when no two points are equal;
  * a curve is simple when no two segments intersect, except consecutive segments at
    their shared vertex and the closing vertex of a ring;
  * an areal geometry is simple when `Validation` reports no ring self-intersection;
  * a collection is simple when all its members are.

  `geo`'s `Validation` checks validity, not OGC simplicity, so the check is our own sweep
  over a segment R-tree.

### 4.5 Errors

Function errors are SPARQL type errors: the expression is in error. Planner-level errors
are `400` with `Error::Invalid`.

| Condition | Status | Message (prefix) |
|---|---|---|
| A malformed geometry constant in a query, such as a literal typed `geo:wktLiteral` that does not parse | 400 | `geo: malformed wktLiteral at offset N: …`. A constant that can never evaluate is reported instead of silently evaluating to false. Ill-typed *data* never raises an error. |
| `spatial:` argument errors | 400 | §2.5 |
| `spatial:` or rewrite with a variable argument (Phase 1) | 501 | `spatial:nearby: variable arguments are not supported yet` (`Error::Unsupported`) |
| Over budget (vertices, overlay, index memory) | 507 | §4.7 |
| `geof:` functions in a build without the `geo` feature | — | They are unknown extension functions, so they raise a type error like any unknown IRI does today. The plan also gets one warning per query: `geof:* needs cargo feature "geo"`. |
| `spatial:` property functions in a build without the `geo` feature | 501 | `built without GeoSPARQL (cargo feature "geo")` |
| Index admin while the index is disabled | 400 | `spatial index is not enabled` |
| A rebuild is already running | 409 | `spatial index build already running` |

### 4.6 Index semantics and consistency

* **What is indexed.** A **row** is a visible quad `(s, p, o, g)` where:
  * `p` is a configured serialization predicate;
  * `g` is in the graph scope. By default that is every graph, including
    `urn:x-sparkles:inferred`, and `reasoning=false` excludes the inferred graph through
    the graph filter, as for text;
  * `o` is a well-typed, non-empty geometry literal of a supported datatype in a built-in
    CRS, within `maxGeometryBytes` and `maxVertices`.

  Each row carries the envelope of `o` in CRS84 (internal lon/lat order), rounded outward
  to `f32`. W3C Basic Geo rows (Phase 2) have the form `(s, wgs84:lat, ·, g)`. Their point
  is built from the subject's `lat`/`long` pair in the same graph. When a subject has
  several pairs, Sparkles indexes the cross product, as Jena does.
* **MVCC.** A spatial operator on snapshot S sees exactly S's rows. Those are the
  generation's base rows minus `S.delta.del`, plus the overlay rows present in
  `S.delta.ins` (§5.3). There is no staleness window and no `503`. Historical snapshots
  (`?at=`, [F06](F06-snapshots-and-point-in-time.md)) get the same guarantee. Their
  generation's base is built on demand, within the budget.
* **Exactness.** The index only produces candidates. Every answer is refined with the
  exact predicate from §2.4 or §4.4, so results with and without the index are identical.
  The acceptance tests check this on random data (§7, A20).
* **Graph scope** works as in [F03](F03-full-text-search.md) and
  [F04](F04-vector-search.md). The active graph becomes a filter on `g`, applied before
  any top-`k`. Under a merged default graph without a graph variable, rows with equal
  `(s, o)` form one solution.

### 4.7 Limits and budgets

| Setting | Default | Where | On excess |
|---|---|---|---|
| `maxGeometryBytes` | 16 MiB | `geo.json` | Not indexed (`skipped.tooLarge`). Functions still evaluate it if `maxVertices` allows. |
| `maxVertices` (per geometry) | 1,000,000 | `geo.json` | Not indexed. Functions raise the type error `geometry too complex (N vertices)`. |
| `maxOpVertices` (sum of input vertices of one overlay, buffer, hull or relate) | 2,000,000 | `StoreOptions` / `--geo-op-vertices` | Type error `geometry operation too large`. |
| buffer output | ≤ 8 segments per quarter circle × input vertices | constant | |
| WKT/GeoJSON nesting depth | 32 | constant | Ill-typed. |
| geometry column + trees memory (`geo_budget_bytes`) | 4 GiB | `StoreOptions`, `--geo-mb` | The build is refused and the state becomes `over-budget`. Queries use the non-index plans. |
| per-query geometry memo | 64 MiB, LRU | `Ctx` | Evicts. |
| `limit` of `spatial:` | unbounded (row budget) | | `ctx.check_rows` → `507` |
| spatial join candidates (Phase 2) | `max_rows` | `Ctx` | `507` with `spatial join produced more than N candidate pairs` |

* `geo` calls cannot be interrupted. Every operator calls `ctx.check()` every 256 exact
  tests and every 4,096 candidate rows, and `maxOpVertices` bounds the cost of a single
  call. With `PreparedGeometry`, relate on two polygons is O((n + m) log(n + m)). Overlay
  and buffer are similar, with larger constants. A query that runs 1,000 relates against
  a 1M-vertex constant therefore still responds to timeouts.
* Constructed literals, such as buffers and unions, go to the query-local vocabulary.
  Their byte size is charged to `ctx`'s memory budget, so
  `SELECT (geof:buffer(?w, …) AS ?b)` over a large table fails with `507` instead of
  running out of memory.
* Index builds run on the rayon pool. They can be cancelled at store close, much like a
  `ctx`, and they check the budget before allocating, following F04's admission model.

## 5. Design

### 5.1 Modules and features

| Where | Change |
|---|---|
| `crates/sparkles/Cargo.toml` | Optional dependencies: `geo = "0.33"` with default features off (`earcutr` and `spade` are not needed), `wkt = "0.14"`, `geojson = "1"`, `geographiclib-rs = "0.2"` (already a `geo` dependency) and `geo-index = "0.4"`. The feature is `[features] geo = ["dep:geo", "dep:wkt", "dep:geojson", "dep:geo-index"]`. |
| `crates/sparkles-server/Cargo.toml` | `geo = ["sparkles/geo"]`, added to `default`. |
| `sparkles/src/geo/mod.rs` (always compiled) | IRIs, `GeoConfig` (serde), `GeoStatus`, the `not_built()` error, and the CRS and unit tables. These are pure data, so the planner can recognize the terms without the feature. |
| `geo/geom.rs` (`cfg(feature="geo")`) | `Geom`, parsing (a WKT front end over `wkt`, and GeoJSON), writing (§4.1.5) and axis handling. |
| `geo/crs.rs` | The CRS table, alias normalization, and transforms: axis swap, Web Mercator, AEQD and, in Phase 2, UTM. |
| `geo/ops.rs` | The functions of §2.3–2.4 over `Geom`: the DE-9IM table, the distance models, buffer, measures and overlay dispatch. |
| `geo/column.rs` | The geometry column, which maps an id to a parsed entry, per generation (§5.2). |
| `geo/index.rs` | `GeoBase` (a packed R-tree over the base rows), `Overlay`, `GeoView`, the build and the status. |
| `geo/search.rs` | Window, radius and k-NN search over base and overlay, with refinement. Join kernels in Phase 2. |
| `sparql/geopf.rs` (always compiled) | Recognizes `spatial:*` property functions and, in Phase 2, topological triple patterns, and decodes them into calls. Reuses `textpf::take_calls`. |
| `sparql/plan.rs` | `Kind::SpatialScan` and `Kind::SpatialPf`, plus `Kind::SpatialJoin`, `Kind::SpatialRelate` and `Kind::SpatialKnn` in Phase 2. Detection (§5.6) and `Optimizations::spatial_pushdown`. |
| `sparql/exec.rs` | Dispatches to `geo::search`. Without the feature it returns `Error::Unsupported`. |
| `sparql/expr.rs` | Adds `geof:` and `spatialF:` to `is_extension` and `extension()` through `geo::ops`, with id-aware geometry arguments (§5.8). |
| `sparql/exec.rs` aggregates (Phase 2) | `AggregateFunction::Custom` for the six `geof:agg*` IRIs. Today it returns unbound. |
| `sparql/cache.rs` | Spatial kinds are cacheable. The key adds the spec and the `GeoView`'s `(generation uid, epoch)`. The snapshot version already pins the overlay. |
| `store.rs` | `Generation.geo: geo::GenerationGeo`, `Snapshot.geo: Option<Arc<GeoView>>` and `Store.geo: ArcSwapOption<GeoIndex>`. Hooks in `open`, `publish_log` (`maintain_geo`) and `rebuild_locked` (§5.4–5.5). |
| `check.rs`, `clone`, F05 meta files | `geo.json`. |
| `sparkles-reasoner` (Phase 2) | The embedded `geo` and `sf` ontologies, `--vocab geosparql` and `--geo-default-geometry`. |
| server `http.rs`, `main.rs`, `state.rs`, `obs.rs` | The routes, CLI and metrics of §2.8–2.9. |
| `ui/` (Phase 2) | §2.11 |

### 5.2 Geometry representation and the geometry column

```rust
pub struct Geom {
    pub crs: CrsRef,                 // index into the CRS table, or Unknown(Arc<str>)
    pub declared: GeomType,          // the written type (LinearRing, Triangle, … kept for geometryType)
    pub layout: Layout,              // Xy | Xyz | Xym | Xyzm
    pub g: geo::Geometry<f64>,       // internal (east, north) coordinates
    pub z: Option<(f64, f64)>,       // minZ, maxZ when Z exists (Z values themselves are dropped)
    pub empty: bool,
}
```

The **geometry column** (`GenerationGeo.column`) maps a literal id to
`Arc<ColumnEntry { bbox84: [f64; 4], kind, flags, vertices: u32, geom: OnceLock<Arc<Geom>> }>`:

* **Base literals** (`Tag::Vocab`) enter the column when the base index is built (§5.3).
  The build takes the distinct object ids of the indexed predicates and parses them in
  parallel blocks, using `Vocab::get_sorted` key batches on rayon. Phase 1 keeps the parsed
  `Geom` in memory, so refinement never parses text again. Phase 2 persists the column
  (§5.5) as WKB-like records that are decoded on demand, so a restart parses no text.
* **Delta literals** (`Tag::Delta`) are parsed by the commit hook (§5.4) and inserted into
  the same generation's column, an `FxHashMap` behind a `RwLock`. Delta ids are stable
  within a generation, and these entries stay until the generation is dropped. The budget
  bounds their memory. When the overlay goes over budget, the status message asks for a
  compaction.
* **Other literals** go through the per-query memo
  `Ctx.geo_memo: FxHashMap<Id, Arc<Geom>>` (64 MiB, LRU). These are constants, computed
  values, objects of non-indexed predicates and local vocabulary. A BIND over many rows therefore parses each distinct
  literal once.

A point entry takes about 120 bytes of memory, counting the entry and its `Geom`. A
polygon takes about `16 · vertices + 160` bytes. 1M points come to ≈ 120 MB, and 1M
50-vertex polygons to ≈ 960 MB. Phase 2's persisted column moves this to mmapped pages,
at `16 · vertices + 48` bytes per entry on disk.

### 5.3 Index structures

```rust
pub struct GenerationGeo {                          // on Generation, dropped with it
    column: Column,                                 // §5.2
    base: OnceCell<Result<Arc<GeoBase>>>,           // built at enable/open/switch, or on demand
}
pub struct GeoBase {
    rows: Vec<Row>,                                 // base rows in PSO order per predicate
    tree: geo_index::rtree::RTree<f32>,             // packed Hilbert R-tree over rows' boxes (node size 16)
    config_hash: u64, built_ms: f64,
}
#[derive(Clone, Copy)] pub struct Row { s: u64, o: u64, g: u64, p: u16 /* predicate slot */, kind: u8, _pad: u8 }
pub struct GeoView {                                // on Snapshot (Arc), immutable
    generation: u64,                                // Generation.uid
    epoch: u64,                                     // +1 per enable/reconfigure/rebuild (cache keys)
    overlay: Arc<Overlay>,                          // packed tree over overlay rows (as of some earlier commit)
    tail: imbl::Vector<(Row, [f32; 4])>,            // rows inserted since the overlay tree was built
}
```

* The base is built from `Perm::Pso` with prefix `[p]` for each indexed predicate. It
  covers only the generation's base, as `vector::GenerationVectors::predicate` does, and
  keeps the rows whose object parses (§4.6). The boxes come from the column. The tree is a
  static packed R-tree in the Flatbush layout, from `geo-index`. It builds in O(n log n),
  has 16-entry nodes, and takes about `n · 1.07 · (16 + 4)` bytes plus `rows`.
* **Overlay** rows come only from transactions (§5.4). A row in `overlay` or `tail` is
  valid for snapshot S iff its quad is in `S.delta.ins`, which is an O(log n) `imbl` lookup
  on the PSO set. A base row is valid iff its quad is not in `S.delta.del`. `store::apply`
  keeps `ins` disjoint from the base and `del ⊆ base`, so
  `rows(S) = (base − del) ∪ (overlay ∪ tail) ∩ ins` exactly.
* A search queries `base.tree` and `overlay.tree`, and scans `tail` linearly. Tails stay
  short (§5.4), so the scan is cheap.

### 5.4 Commit path (`WriteTxn::publish_log` → `Store::maintain_geo`)

The hook runs after the WAL fsync and before the snapshot is published, at the same point
as `maintain_text`:

1. `touched` is the set of logged quads whose predicate is indexed and whose graph is in
   scope. A per-generation `FxHashMap<Id, bool>` predicate cache makes a commit without
   geometry cost one lookup per logged quad.
2. For each inserted quad, the hook looks up its object in the column, or parses it.
   Delta literals are parsed here, once. Valid rows are pushed to a clone of the previous
   view's `tail`. The tail is a persistent vector, so a push is O(1) amortized and shares
   structure with older snapshots. Deletes need no work, since validity is checked against
   the snapshot.
3. If `tail.len() > max(4096, overlay.rows / 8)`, the hook rebuilds the overlay tree from
   `overlay.rows ∪ tail`, drops rows that are not in the new snapshot's `delta.ins`, and
   starts an empty tail. This costs O(log n) per insert, amortized, and is bounded by the
   overlay size, which compaction resets.
4. The snapshot is published with `snap.geo = Some(new view)`.

A parse failure never fails a write. The ill-typed literal is counted and skipped. A panic
or error in the hook is logged and marks the index `failed`. The snapshot then carries
`geo: None`, and queries use the non-index plans until `rebuild` re-derives everything
from RDF. Those plans give correct answers, only slower. The commit latency target is no
measurable change for commits without geometry quads, and under 1 ms extra for a commit
that inserts 1,000 points (§9).

The writer mutex serializes commits. The column and the overlay therefore need no locking
beyond the column's `RwLock`. Readers only read entries that existed when their snapshot
was published.

### 5.5 Generation switches, open, rebuild, persistence

* **Compaction and bulk commits** (`rebuild_locked`). After the new generation is built,
  and while the writer lock is still held, Sparkles builds the generation's `GeoBase` and
  column in parallel from its PSO order. This happens before the snapshot is published,
  and the new snapshot starts with an empty overlay. Compaction takes longer by the build
  time, which §9 measures. Phase 2 reuses the previous generation's parsed geometries by
  literal key, since `vocab` keys are identical across generations. A compaction then
  parses only new literals.
* **Open.** When `geo.json` is present, the store opens with the base unbuilt and a
  background thread builds it. Until the `OnceCell` is filled, the planner treats the
  index as not ready and uses non-index plans. The answers are still correct, and explain
  says `spatial index building (37%)`. The overlay is rebuilt from the commits of the WAL
  replay, since the replayed log passes through `maintain_geo` like a live commit.
* **Rebuild and reconfigure.** Sparkles builds a new base for the current generation in
  the background. It then swaps the base in by republishing the current snapshot with a
  new `GeoView` (`epoch + 1`). In Phase 1 a rebuild holds the writer lock, as F03 Phase 1
  does. The overlay is rebuilt from the delta under the new configuration, by scanning
  `delta.ins` for the indexed predicates.
* **Persistence (Phase 2).** The build writes `column.spkg` and `rtree.spkg` to
  `gen-NNNN/geo/`. It follows the F04 segment pattern: write `*.tmp`, fsync, rename, then
  `sync_dir`.
  * The 64-byte header holds the magic `SPKGEO\0\x01`, `u32 format_version = 1`,
    `u32 kind`, `u64 rows`, `u64 config_hash` (FNV-1a of the canonical `geo.json`),
    `u64 base_seq`, `u64 meta.quads` and reserved bytes.
  * The footer holds `u64 rows`, `u64 xxh/fnv(header ‖ index section)` and the magic.
  * On open, a file is valid only if the magic, version, `config_hash`, `base_seq`, row
    count and footer all match. Otherwise Sparkles deletes it and rebuilds.

  The files are mmapped (`memmap2`), and `geo-index` trees work zero-copy from the bytes.
  A read-only server never writes them and builds in memory only. Removing the generation
  directory removes them. `sparkles check` verifies headers and footers, and with
  `--checksums` also the data sections.
* **In-memory stores** build in memory only.
* **Config** lives in `<root>/geo.json`: the `GeoConfig` plus `"formatVersion": 1`,
  written with `write_atomic`.

### 5.6 Planner

#### 5.6.1 FILTER pushdown with a constant geometry (Phase 1)

`scan_options(t)` receives the group's filters. Pushdown applies to a triple `?x <p> ?w`
when all of these hold:

* `p` is an indexed predicate;
* `?w` is a variable;
* the index is ready for the snapshot's generation;
* `ctx.opt.spatial_pushdown` is set.

Then each filter conjunct over `?w` with one of these shapes yields a **`SpatialScan`**
option:

| Conjunct (either argument order where symmetric) | Window | Exact test |
|---|---|---|
| `geof:sfIntersects` / `sfWithin` / `sfContains` / `sfOverlaps` / `sfCrosses` / `sfTouches` / `sfEquals`, `eh*` except `ehDisjoint`, `rcc8*` except `rcc8dc` (`?w`, C) | The envelope of C. | The relation. |
| `geof:relate(?w, C, "pattern")` where the pattern requires a non-empty intersection (one of II, IB, BI, BB is `T`/`0`/`1`/`2`) | The envelope of C. | `relate`. |
| `geof:distance(?w, C, u) < r`, `<= r`, `r > …`, `r >= …`; `geof:metricDistance`; `spatialF:nearby(?w, C, r, u)` / `withinCircle` | The envelope of C expanded by `r`. The expansion is in degrees on §4.4.2's lower-bound sphere and depends on latitude. The window is split at the antimeridian and covers the full longitude range near the poles. | The comparison. |
| `geof:sfIntersects(?w, geof:buffer(C, r, u))` and other relations whose constant argument is a constant expression | The constant is folded at plan time. | The relation. |

`C` is a constant geometry literal or a constant-folded expression. When several
conjuncts apply, their windows are intersected. The pushed conjuncts move into the
`SpatialSpec`, and the operator evaluates them against the column, with one
`PreparedGeometry` built for `C`. Other conjuncts stay ordinary filters.

The DP planner picks the plain scan or the spatial scan by cost, as `push_range` does for
numeric ranges. The cost model:

* `est_window` is the number of rows whose box intersects the window. It comes from the
  tree's upper levels: walk down to the first level with at least 256 nodes and sum the
  subtree sizes of the nodes that intersect the window. The result is an upper bound,
  accurate to within one node per boundary, and takes microseconds. The overlay rows are
  counted the same way, and the tail length is added.
* `est` is `est_window` times the selectivity of the exact test. For `sfWithin` or
  `sfContains` of points in a polygon, the selectivity is area(C) / area(box(C)), capped
  at 1. Otherwise it is 0.5.
* `cost` is `est_window` × (1 + `REFINE_COST(kind, vertices(C))`) + 4 × tree levels.
  `REFINE_COST` is 0.5 for a point-in-prepared-polygon test and grows with
  `log2(vertices)` for polygon–polygon relates. The plain scan costs
  `rows(p) × (1 + REFINE_COST)`, because the filter then runs on every row.

The `Node` description reads
`SpatialScan ?x ?w ← <asWKT> sfWithin POLYGON(5 pts) [window ≈ 1,240 of 1.0M rows; base+overlay]`.

#### 5.6.2 `spatial:` property functions (Phase 1)

`collect()` takes `spatial:*` triples and their argument lists out of the BGP with
`textpf::take_calls`, as for `spk:vectorSearch`. It pushes a `SpatialPf` leaf with
`SpatialPfSpec { func, query: Geom (or lat/lon converted with EPSG:4326), radius_m, limit,
subject: PathEnd, graph: GraphFilter, graph_var, dedup }`. The row estimate is
`min(limit, est_window × FEATURE_FANOUT)`. `FEATURE_FANOUT` is the number of distinct
subjects per object, taken from the predicate statistics of the feature links.

#### 5.6.3 Execution of `SpatialPf` and fallback

1. Search the window over base and overlay. When the index is not ready, scan each
   indexed predicate instead and test the window against the column, or against parsed
   literals when the column is not built either.
2. Refine each match exactly, as §2.5 describes.
3. Map geometry subjects to features. This takes one `POS` lookup with prefix
   `[link, geomSubject]` per distinct geometry subject and feature-link predicate. The
   lookup respects the graph filter, and under `GRAPH ?g` the same-graph rule.
4. Deduplicate the features. With a `limit`, rank them by exact distance to the query
   geometry and keep the first `limit`, breaking ties by subject id. For the box and
   cardinal functions, the distance is to the centre of the query envelope.

For `nearby` with a `limit` and a large radius, step 1 runs as a best-first k-NN traversal
(§5.7). The traversal stops once it has proven `limit` distinct features, so
`spatial:nearby (lat lon 20000 uom:kilometre 10)` does not refine the whole dataset.

#### 5.6.4 Phase 2 operators

* **Spatial join.** In `plan_group`, some conjuncts become join edges when `?a` and `?b`
  are bound by different join components:
  * `geof:R(?a, ?b)` for a relation R that is not a disjoint relation;
  * `geof:relate(?a, ?b, p)` with a pattern that requires an intersection;
  * `geof:distance(?a, ?b, u) <(=) r` with a constant `r`.

  `join_order` then connects the two components through a `SpatialJoin` node instead of a
  cross product plus a filter. A child can be an indexed scan, in which case the join
  probes the persistent index directly (an index nested-loop join). A child can also be
  any subplan. Its distinct geometry ids are then boxed from the column or parsed, and a
  packed tree is built over the smaller side.

  The join traverses the two trees together, or probes with each outer box. It then
  refines the pairs exactly with the relation, building a `PreparedGeometry` for an outer
  geometry when it has more than 8 candidates. The output rows are the row-id pairs,
  joined back to both children's tables. The cost is
  `(n + m) · log(min(n, m)) + pairs × REFINE_COST`, and the estimate is
  `est = n · m · overlap_ratio`. The overlap ratio is estimated from a 1,024-row sample of
  each side, using the two-level box count of §5.6.1.
* **Query Rewrite** (§2.6). `collect()` turns a topological-property triple into a
  `SpatialRelate` item. The item expands into `Union(Scan(asserted), Derived)` with set
  semantics, applying `Distinct` over the `(so1, so2)` pairs of the two branches.
  `Derived` resolves each spatial object to literals with a small UNION of scans, one per
  case: a feature through `hasDefaultGeometry` and then `asX`, a geometry through `asX`, or
  a constant literal. It then runs a spatial join when both sides are variables, a
  `SpatialScan` when one side is constant, or a single test when both are. Disjoint
  relations use a nested-loop join within the row budget.
* **k-NN.** `ORDER BY ASC(geof:distance(?w, C, u)) LIMIT k` becomes `SpatialKnn` when the
  only producer of `?w` in the group is an indexed scan, and the group's other leaves join
  on that scan's subject (a star). `metricDistance`, or a variable bound by `BIND` to
  either function, works the same way.

  The operator traverses the base and overlay trees best-first, ordered by the lower bound
  of §4.4.2. It emits candidates in increasing lower-bound order, in batches of `2k`, and
  joins each batch with the rest of the group. It stops once it has `k` results whose
  exact distance is ≤ the next lower bound.

  In ascending order, SPARQL sorts an expression error (unbound) first. The operator
  therefore first emits the scan's rows whose distance is an error: ill-typed, empty,
  unknown-CRS and unsupported literals. The column keeps these as `skipped` row lists, for
  base and overlay, for exactly this purpose. A `FILTER(BOUND(?d))` or a distance bound in
  the group removes them, and the documented query shape includes one. Other query shapes
  use the generic sort.

### 5.7 Search kernels (`geo/search.rs`)

* **Window.** The kernel runs `tree.search(box)` on base and overlay and scans the tail.
  It checks each row's validity against the snapshot (§5.3) and applies the graph filter.
  Under merged default graphs it deduplicates per `(s, o)`. The exact test runs on the
  column entry, box test first and then the relation. Like the vector search, the kernel
  runs in parallel over chunks of 4,096 candidates, with `ctx.check()` between chunks.
* **Radius.** The window is the box expanded by `r` on the lower-bound sphere. The exact
  test uses the configured distance model.
* **k-NN.** A priority queue over tree nodes, keyed by the lower-bound distance from the
  query to the node's box. It uses `geo-index`'s `neighbors_with_callbacks`, or our own
  traversal over its node layout if the callback API cannot take a custom metric. Leaf
  entries are refined in order. Ties are broken by the raw `(s, o, g)` ids.
* **Antimeridian.** A window that crosses ±180° is split into two boxes. Data rows are
  indexed as written (§4.4.1).

### 5.8 Functions in `expr.rs`

* `is_extension` accepts `geof:` and `spatialF:` when the feature is on.
* `extension()` dispatches to `geo::ops`. Geometry arguments go through
  `geom_arg(args, i, row, ctx)`:
  * When the argument expression is a variable, it takes the variable's id. A base or
    delta id is read from the generation column, with no parsing if the entry exists.
    Other ids go through `ctx.geo_memo`.
  * Otherwise it evaluates the expression to a `Value::Other { lex, dt }` and parses it
    through `ctx.geo_memo`, keyed by a 64-bit hash of `(dt, lex)`.
* Numeric results are `xsd:double`. They are inline when the low mantissa bits allow, and
  in the local vocabulary otherwise. Booleans are inline. Geometries are local-vocabulary
  literals.
* **Aggregates (Phase 2).** `AggregateFunction::Custom(iri)` covers
  `geof:aggBoundingBox`, `aggBoundingCircle`, `aggCentroid`, `aggConvexHull`, `aggUnion`
  and `aggConcaveHull`. `aggCentroid` is the planar centroid of the union of the inputs.
  `aggUnion` uses `unary_union`, limited by `maxOpVertices`. Each aggregate takes one
  geometry expression and honours `DISTINCT`. As with the built-in numeric aggregates, an
  ill-typed input makes the aggregate an error. Results are in the CRS of the first
  input, and inputs in other CRSs are transformed.
* **Constant folding.** A call whose arguments are all constants is evaluated once, at
  plan time, through the existing `Expr` constant path. The §5.6.1 windows need this.

### 5.9 Explain and profile

The plan nodes (`describe`) read:

* `SpatialScan` (Phase 1), as in §5.6.1;
* `SpatialPf ?f ← spatial:nearby POINT(-0.12 51.5) r=5 km limit 10 [features via hasDefaultGeometry|hasGeometry]`;
* `SpatialJoin ?a sfContains ?b [index nested loop on <asWKT>]` (Phase 2);
* `SpatialKnn ?w k=10 metricDistance POINT(…)` (Phase 2).

Profiles (`x-sparkles+json` and the UI's Plan tab) add per-operator counters:
`candidates`, `refined`, `matched`, `treeNodesVisited`, `index: ready | building (p%) |
off | failed`, and `fallback: true` when a non-index path ran. When a spatial filter was
not pushed down, explain adds a warning with the reason, such as the wrong shape, a
variable radius or the index being off. The MCP server's explain-with-warnings shows the
same warning.

## 6. Phasing

Estimates are in implementation days, on the same scale as F03 and F04.

### Phase 1 (about 7 days)

1. **Core** (2.5 days)
   * `geo` feature and dependencies;
   * `Geom`, WKT (with the CRS prefix) and GeoJSON parsing and writing;
   * CRS table (CRS84, CRS84h, 4326, 4979, legacy 4326, 3857), axis handling;
   * unit table.
2. **Functions** (1.5 days)
   * the 24 relations and `relate`;
   * `distance`, `metricDistance` (both distance models);
   * `buffer`, `metricBuffer` (planar and AEQD), `convexHull`, `envelope`, `boundary`,
     the four overlay functions (covered pairs per §4.4.5);
   * `centroid`, `getSRID`, `transform` (built-ins);
   * `asWKT`, `asGeoJSON`;
   * `area`/`metricArea`, `length`/`metricLength`, `perimeter`/`metricPerimeter`;
   * `dimension`, `coordinateDimension`, `spatialDimension`, `is3D`, `isMeasured`,
     `isEmpty`, `geometryType`, `numGeometries`, `geometryN`, `min*`/`max*`;
   * `geom_arg` with the memo.
3. **Index** (2 days)
   * `geo.json`, `GenerationGeo` (column + packed base tree, in memory);
   * `maintain_geo` with overlay and tail, compaction and bulk hooks, background build at
     open, WAL replay through the hook;
   * status and rebuild.
4. **Planner** (1 day)
   * `SpatialScan` pushdown with the cost model;
   * `spatial:` property functions (constant arguments), fallback paths;
   * explain counters and warnings.
5. **Surfaces and tests** (1 day)
   * `/$/geo` GET/PUT/DELETE/rebuild, `DatasetInfo.geo`, metrics;
   * `sparkles geo-index`, `serve --geo`, `check`, `clone`/backup meta file;
   * §7 A1–A23;
   * `docs/API.md`, README status and decision rows, `AUDIT.md` rows.

### Phase 2 (about 8 days)

* Spatial joins, distance-within joins and the k-NN rewrite (3 days).
* The Query Rewrite Extension, `--geo-default-geometry`, and the RDFS Entailment Extension
  with its ontology bundle and `--vocab geosparql` (1.5 days).
* Aggregates, which need custom aggregate support in `exec.rs`; `boundingCircle`,
  `concaveHull`, `isSimple`, the `spatialF:` functions, `spatial:equals`, UTM zones and W3C
  Basic Geo rows (1.5 days).
* The persisted column and trees, reuse of parsed geometries across compaction, and
  `check` coverage (1 day).
* The UI: the map tab, the explorer card and Nearby action, the dataset panel,
  `/{ds}/geo`, `/$/geo/convert` and the mock endpoints (1.5 days).
* Import of Oxigraph's test suite, Jena-derived test cases and benchmark scripts (§8,
  §9), plus a `docs/BENCHMARKS.md` section (0.5 day plus the run).

### Phase 3 (later, on demand)

* GML and KML literals, `asGML` and `asKML`, and the GML entailment hierarchy (Req 49).
* The `geo-proj4` feature with an operator-supplied CRS registry (§4.2.4).
* Variable arguments and arguments bound from the left for `spatial:`, like F04's
  `candidates:join`.
* k-NN with arbitrary joins, by generalizing batch-and-verify.
* Spatial joins with S2-cell or grid prefilters. QLever's cell grid is the model, and only
  the idea is borrowed.
* Simplified inner and outer polygon approximations for refinement (Bast et al. 2025).
* The `geometryTypes` materialization rule, and `geo:hasMetricArea` and similar
  properties computed by rewrite.
* Support for QLever's `SERVICE spatialSearch:` syntax, if users ask (§11 q14).
* Running the GeoSPARQL Compliance Benchmark in CI, if the maintainer accepts fetching
  GPL-2.0 code (§8).

## 7. Acceptance examples

The fixture is the dataset `ds` with the spatial index enabled with defaults. The
prefixes are `ex:`, `geo:`, `geof:`, `uom:`, `spatial:`, `spatialF:` and `sf:`. Literals
are CRS84 unless stated.

```trig
ex:A geo:hasDefaultGeometry ex:gA . ex:gA geo:asWKT "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"^^geo:wktLiteral .
ex:B geo:hasDefaultGeometry ex:gB . ex:gB geo:asWKT "POLYGON((5 5, 15 5, 15 15, 5 15, 5 5))"^^geo:wktLiteral .
ex:C geo:hasDefaultGeometry ex:gC . ex:gC geo:asWKT "POLYGON((10 0, 20 0, 20 10, 10 10, 10 0))"^^geo:wktLiteral .
ex:p1 geo:hasGeometry ex:g1 .  ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral .
ex:p2 geo:hasGeometry ex:g2 .  ex:g2 geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)"^^geo:wktLiteral .
ex:p3 geo:hasGeometry ex:g3 .  ex:g3 geo:asGeoJSON "{\"type\":\"Point\",\"coordinates\":[30,30]}"^^geo:geoJSONLiteral .
ex:bad geo:hasGeometry ex:gX . ex:gX geo:asWKT "POINT(1)"^^geo:wktLiteral .
ex:nil geo:hasGeometry ex:gE . ex:gE geo:asWKT ""^^geo:wktLiteral .
ex:mars geo:hasGeometry ex:gM . ex:gM geo:asWKT "<http://example.org/crs/mars> POINT(1 1)"^^geo:wktLiteral .
ex:G1 { ex:p4 geo:hasGeometry ex:g4 . ex:g4 geo:asWKT "POINT(3 3)"^^geo:wktLiteral . }
```

`gA` below abbreviates the literal of `ex:gA` in queries.

| # | Query / action | Expected |
|---|---|---|
| A1 | `SELECT ?g { ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, gA)) }` | `{ex:gA, ex:g1}` (a polygon is within itself under DE-9IM; corrected 2026-10-01). `g4` is in a named graph, `g2` is at lon 12 (outside A), `gX` is ill-typed (no error), `gE` is empty. Explain shows `SpatialScan … sfWithin`. With `?opt=-spatial_pushdown` the same answer and a `Filter` node |
| A2 | `ASK`s: `sfTouches(gA, gC)`, `sfIntersects(gA, gC)`, `sfOverlaps(gA, gB)`, `sfContains(gA, "POINT(2 2)")`, `rcc8ec(gA, gC)`, `rcc8po(gA, gB)`, `ehCovers(gA, "LINESTRING(0 1, 5 1)")`, `sfEquals("POINT(1 1)", "Point (1.0 1.0)")` | all `true`. `ehCovers(gA, "LINESTRING(1 1, 5 1)")` is `false` (no boundary contact). `sfOverlaps(gA, gC)`, `rcc8ec(gA, "POINT(10 5)")` (not areal) and `sfEquals("POINT(1 1)", "POINT(1 2)")` are `false` |
| A3 | `geof:sfWithin(?w, gC)` over all `asWKT` | `{ex:gC, ex:g2}` (gC within itself; corrected 2026-10-01): EPSG:4326 `POINT(2 12)` is lon 12, lat 2 |
| A4 | `geof:metricDistance("POINT(0 0)", "POINT(1 0)")` | `111319.49079327…e0` (|Δ| ≤ 1e-6 m). With `distance: "haversine"`: `111195.07973436…e0` |
| A5 | `geof:distance("POINT(0 0)", "POINT(1 0)", uom:kilometre)`, `…, <http://qudt.org/vocab/unit/KiloM>`, `…, "http://www.opengis.net/def/uom/OGC/1.0/kilometre"^^xsd:anyURI`, `…, uom:degree` | `111.3194907…` three times; `1.0` within 1e-9 (central angle) |
| A6 | `geof:distance(gA, "POINT(12 5)", uom:metre)` | the geodesic from (10 5) to (12 5): ≈ 221,800 m (asserted against GeographicLib's inverse solution to 0.1%) |
| A7 | `geof:getSRID(?w)` for g2 and g1; `geof:transform(g2, <…/OGC/1.3/CRS84>)`; `geof:asWKT(g2)` | `"http://www.opengis.net/def/crs/EPSG/0/4326"^^xsd:anyURI`, `"http://www.opengis.net/def/crs/OGC/1.3/CRS84"^^xsd:anyURI`; `"POINT(12 2)"^^geo:wktLiteral`; `"<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)"^^geo:wktLiteral` |
| A8 | `geof:minX(g2)`, `geof:maxY(gA)`, `geof:numGeometries("MULTIPOINT((1 1),(2 2))")`, `geof:geometryN(…, 2)`, `geof:geometryType(gA)`, `geof:dimension(gA)`, `geof:isEmpty(gE)`, `geof:dimension(gE)` | `2.0e0` (latitude), `10.0e0`, `2`, `"POINT(2 2)"`, `sf:Polygon` (anyURI), `2`, `true`, `-1` |
| A9 | `geof:intersection(gA, gB)`, `geof:union(gA, gC)`, `geof:envelope("LINESTRING(0 0, 2 1)")`, `geof:convexHull("MULTIPOINT((0 0),(2 0),(1 1),(1 0.5))")` | geometries `sfEquals` to `POLYGON((5 5,10 5,10 10,5 10,5 5))`, `POLYGON((0 0,20 0,20 10,0 10,0 0))`, `POLYGON((0 0,2 0,2 1,0 1,0 0))`, `POLYGON((0 0,2 0,1 1,0 0))` |
| A10 | `geof:metricArea("POLYGON((0 0, 1 0, 1 1, 0 1, 0 0))")`; `geof:area(…, <http://qudt.org/vocab/unit/KiloM2>)`; `geof:metricLength("LINESTRING(0 0, 1 0)")`; `geof:metricArea("POINT(0 0)")` | ≈ `1.2309e10` (Karney; the test asserts GeographicLib's reference value to 1e-6 relative); ≈ `12309`; `111319.49…`; `0.0e0` |
| A11 | `geof:metricBuffer("POINT(0 0)", 1000)` then `geof:metricArea` of it; `geof:buffer("POINT(0 0)", 1, uom:degree)` | area between −0.7% and +0.1% of π·10⁶ m² (a 32-gon has 0.64% less area than its circle); a planar circle of radius 1° |
| A12 | type errors: `BIND(geof:sfIntersects(?w, gA) AS ?x)` for `gX` and for `gM` (unknown CRS vs CRS84); `geof:distance(gA, gB, uom:parsec)`; `geof:area(gA, uom:metre)`; `geof:geometryN(gA, 2)`; `geof:minZ(gA)`; `geof:relate(gA, gB, "TT")` | unbound each. `geof:getSRID(gM)` → `<http://example.org/crs/mars>`; `geof:sfEquals(gM, gM)` → `true`; `geof:metricDistance(gM, gM)` → unbound |
| A13 | `SELECT ?f { ?f spatial:nearby (2 2 50 uom:kilometre) }`; then `(2 2 2000 uom:kilometre)` | `{ex:A, ex:p1}` (`gA` contains the point: distance 0); then `{ex:A, ex:B, ex:C, ex:p1, ex:p2}`, with closest points B (5 5) ≈ 471 km, C (10 2) ≈ 890 km, p2 (12 2) ≈ 1,113 km. `ex:p3` (30 30) is too far, and `ex:mars` (unknown CRS) and `ex:bad` never match |
| A14 | `SELECT ?f { ?f spatial:withinBox (0 0 5 5) }` (lat/lon order); `spatial:intersectBox (0 0 5 5)` | `{ex:p1}`; `{ex:A, ex:B, ex:p1}` |
| A15 | `SELECT ?f { ?f spatial:nearby (0 0 5000 uom:kilometre 2) }` | `ex:A` (0 m), then `ex:p1` (314 km): the 2 nearest, deterministic |
| A16 | `SELECT ?f ?g { GRAPH ?g { ?f spatial:nearby (3 3 10 uom:kilometre) } }` | `(ex:p4, ex:G1)` only |
| A17 | MVCC: `INSERT DATA { ex:p5 geo:hasGeometry ex:g5 . ex:g5 geo:asWKT "POINT(1 1)"^^geo:wktLiteral }` then A1; a reader holding the earlier snapshot runs A1; then `DELETE DATA { ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral }` and A1 | `{gA, g1, g5}`; the old reader still gets `{gA, g1}`; then `{gA, g5}`. `GET /$/geo/ds`: `rows.base` 7 (unchanged; g1's base row is now invalid), `rows.tail` 1 |
| A18 | `sparkles compact --loc db`, then A1 and A13 | same answers as before compaction; `rows.base` 7 (g5 in, g1 out), `rows.overlay = rows.tail = 0`; `epoch` unchanged |
| A19 | 10,000 point inserts in 100 commits (exceeding the tail threshold) and 1,000 deletes; then A1-shaped window queries on random boxes | answers equal the non-index plan (`spatial_pushdown` off) for 200 random boxes; status shows a rebuilt overlay |
| A20 | Property test (Phase 1, `cargo test --features geo`): 20,000 random points, lines and polygons (seeded), 500 random constant geometries × the 24 relations and distance windows, with and without the index, in the default graph, under `GRAPH ?g` and with a merged default graph | identical result multisets |
| A21 | Fallback: index enabled, `GenerationGeo.base` not built (test hook `pause_geo_build`); A1 and A13 | same answers; explain shows `index: building`, `fallback: true` |
| A22 | Errors: `?f spatial:nearby (91 0 1)`; `?f spatial:nearby (0 0 1 uom:parsec)`; `?f spatial:nearby (?lat 0 1)`; `FILTER(geof:sfWithin(?w, "POLYGON((0 0, 1 1"^^geo:wktLiteral))` | `400`, `400`, `501`, `400 geo: malformed wktLiteral at offset …` |
| A23 | `GET /$/geo/ds` after load | `{"enabled":true,"state":"ready","rows":{"base":7,"overlay":0,"tail":0},"literals":7,"skipped":{"malformed":1,"unknownCrs":1,"tooLarge":0,"empty":1},"crs":{"http://www.opengis.net/def/crs/OGC/1.3/CRS84":6,"http://www.opengis.net/def/crs/EPSG/0/4326":1,"http://example.org/crs/mars":1},…}`. The 7 rows are gA, gB, gC, g1, g2, g3 (GeoJSON) and g4 (graph `ex:G1`) |
| A24 (P2) | `SELECT ?a ?b { ?a geo:asWKT ?wa . ?b geo:asWKT ?wb FILTER(geof:sfContains(?wa, ?wb) && ?a != ?b) }` | `{(ex:gA, ex:g1), (ex:gC, ex:g2)}`; explain shows `SpatialJoin` |
| A25 (P2) | Query Rewrite, with the asserted triple `ex:A geo:sfContains ex:p1` added: (a) `SELECT ?x { ?x geo:sfContains ex:g1 }`; (b) `SELECT ?x { ?x geo:sfContains ex:p1 }`; (c) (b) after `sparkles infer --geo-default-geometry` | (a) `{ex:A, ex:gA, ex:g1}` (a point contains itself under DE-9IM); (b) `{ex:A}`: asserted only, since `ex:p1` has `hasGeometry` but no default geometry and no serialization of its own; (c) `{ex:A, ex:gA, ex:g1, ex:p1}`, with `ex:A` once although both asserted and derived |
| A26 (P2) | `SELECT ?g ?d { ?g geo:asWKT ?w BIND(geof:metricDistance(?w, "POINT(9 1)"^^geo:wktLiteral) AS ?d) FILTER(BOUND(?d)) } ORDER BY ?d LIMIT 2` | `(ex:gA, 0.0)`, `(ex:gC, ≈ 111,300)`; explain `SpatialKnn`. Without the FILTER, the error rows `gX`, `gE`, `gM` sort first (SPARQL order), and the operator emits them first as well |
| A27 (P2) | `SELECT (geof:aggUnion(?w) AS ?u) { VALUES ?w { gA gC } }` | `sfEquals` `POLYGON((0 0,20 0,20 10,0 10,0 0))` |
| A28 (P2) | RDFS: `infer --profile rdfs --vocab geosparql` with `ex:gA a sf:Polygon`; `SELECT ?g { ?g a geo:Geometry }` with `reasoning=true` | includes `ex:gA` |

The values in this table were computed by hand from the rules of §2–§4, not by an
implementation. Where a value disagrees with the rules, the rules win and the table gets
fixed. Approximate distances ("≈") are asserted against GeographicLib reference values,
not against the rounded numbers shown here.

## 8. Conformance testing

* **Sparkles' own tests** (§7) live in `crates/sparkles/src/sparql/tests.rs` and a new
  `geo/tests.rs`. They cover parser edge cases, CRS and axis handling, the DE-9IM table,
  the distance models against reference values, and budgets.
* **Oxigraph's GeoSPARQL test suite** (`testsuite/oxigraph-tests/geosparql/`) has 44 cases
  in W3C manifest form, under MIT OR Apache-2.0. It is vendored under
  `testsuite/geosparql/oxigraph/` with its license notice and runs in the existing
  W3C-manifest harness. Some cases are expected to fail, because Oxigraph rejects
  EPSG:4326, uses haversine, computes a planar centroid and so on. An
  `expected-failures.txt` lists each one with its reason.
* **Jena's `jena-geosparql` unit tests** (Apache-2.0, about 1,360 JUnit methods) are not
  ported as a harness. Where a test encodes observable behaviour, its expected values are
  rewritten as a Sparkles SPARQL test, with a comment that names the Jena test. This
  covers the parsers, the relations and the `spatial:` functions (`NearbyPFTest`,
  `WithinBoxPFTest`, the cardinal tests and the `SpatialIndexTestData` cities). If any test
  data is copied verbatim, Jena's `NOTICE` attribution goes into
  `THIRD_PARTY_LICENSES.md`. For the divergences in §11, the tests assert Sparkles'
  behaviour and give the Jena value in a comment.
* **The GeoSPARQL 1.1 specification examples** (Annex C, under the permissive OGC Document
  License) become a test fixture, both the example dataset and the queries.
* **The OGC Compliance Benchmark** (Jovanovik et al., github.com/OpenLinkSoftware/
  GeoSPARQLBenchmark) targets GeoSPARQL 1.0. It has 206 queries, 406 expected-result files
  and a dataset in RDF/XML, GML and GeoJSON. It is GPL-2.0-only, so Sparkles neither
  vendors nor links it.

  An opt-in developer script, `scripts/geosparql-benchmark.sh` (Phase 2), clones it at a
  pinned commit into `target/geosparql-benchmark/`. The script loads the dataset into a
  scratch server, runs the queries over HTTP and prints the score per requirement. Like
  `shellcheck` (README, PROVENANCE), it is a development tool and never ships. It needs
  the maintainer's approval (§11 q12).

  The target is to pass every WKT requirement, beating GeoSPARQL Fuseki 3.17's published
  177/206. The GML parts fail until Phase 3. The benchmark compares floating-point answers
  exactly, so mismatches caused only by the distance model or by 6-decimal rounding are
  reported separately.
* **OGC `ets-geosparql11`** (Apache-2.0) is a TEAM Engine skeleton with no tests as of
  2026-09. Revisit it when it has tests.

## 9. Performance targets and benchmark plan

The machine and method are those of `docs/BENCHMARKS.md`: each engine runs alone with a
warm page cache, timings come from hyperfine, and answers are fingerprinted before
timing. `bench-answers.py` gains a WKT-aware comparison that compares geometries by value
with a tolerance.

**Data.** `scripts/gen-geo.py N` generates a seeded dataset:

* `N` point features clustered around 500 "cities" with log-normal populations. They are
  in CRS84, with 1% in EPSG:4326 to exercise axis swapping.
* `N/10` line features, random walks with 2–200 vertices.
* `N/20` polygon features, star polygons with 4–500 vertices. 10% have holes and 5% are
  multipolygons.
* A three-level administrative hierarchy of polygons that tiles the world: 40 "countries",
  1,600 "states" and 64,000 "counties", with 20–2,000 vertices each. The borders are
  shared, so touches and within get exercised.
* Features linked with `hasDefaultGeometry`. Half of them also have labels and types, so
  joins with ordinary patterns are realistic. 10% are W3C Basic Geo points.

The sizes are N = 1M (≈ 9M triples) and N = 10M. A real-data check uses Natural Earth
admin boundaries (public domain) and a point set derived from GeoNames or OSM. The point
set is ODbL data, so it is built locally and never committed.

**Queries.** Each query also runs on Jena GeoSPARQL Fuseki with its spatial index, on
QLever where it has the feature, and on Oxigraph for the functions that need no index.

| Id | Query | Target (N = 1M, 16 threads) |
|---|---|---|
| Q1 | Points `sfWithin` a constant county polygon (≈ 1k results). | p50 ≤ 5 ms |
| Q2 | Points within 5 km of a constant point (`distance <`). | ≤ 3 ms |
| Q3 | `spatial:nearby (lat lon 50 uom:kilometre 10)` | ≤ 2 ms |
| Q4 | `spatial:withinBox` over 1% of the world. | ≤ 20 ms |
| Q5 (P2) | Count points per state, a 1M × 1,600 `sfContains` join. | ≤ 2 s |
| Q6 (P2) | County–county `sfTouches` self-join (64k). | ≤ 5 s |
| Q7 (P2) | The 10 nearest points to a constant (`ORDER BY metricDistance LIMIT 10`). | ≤ 3 ms |
| Q8 | `SUM(geof:metricArea(?w))` over 50k polygons. | ≤ 300 ms |
| Q9 | Q1 joined with labels and types (a 3-pattern star). | ≤ 10 ms |
| Q10 | Q1 with the index disabled (scan and filter). | Report the speedup only. |

**Writes and maintenance.**

* Index build when the index is enabled: ≤ 1.5 s for 1M points, ≤ 10 s for the 1M mixed
  features above.
* Peak build memory: ≤ 2× the final index memory.
* Compaction: at most 1.5× the build time added, and at most 0.3× in Phase 2 with
  parsed-geometry reuse.
* A single-triple `INSERT DATA` without geometry shows no regression beyond noise. A
  point insert costs ≤ +1 ms over a non-geo insert, and a 1,000-polygon insert ≤ +20 ms.
* Open time with persisted files (Phase 2): ≤ 200 ms extra for 1M rows.
* Memory in Phase 1, in memory: ≤ 150 bytes per point row and ≤ `16 · vertices + 200`
  bytes per polygon row. Report RSS next to Jena's.

Results go to a "GeoSPARQL" section of `docs/BENCHMARKS.md`. The "Where Sparkles loses"
list gets updated too, since QLever's libspatialjoin is likely faster on huge self-joins.

## 10. Rejected alternatives

* **GEOS through the `geos` crate.** `geos` is MIT, but libgeos is LGPL-2.1. Linking it
  statically (the `static` feature) or shipping it conflicts with the permissive-only
  policy. `geo` covers the algorithms Sparkles needs.
* **PROJ through `proj`/`proj-sys` as the default CRS engine.** The PROJ library is MIT,
  but the build needs CMake, a C++ toolchain and SQLite, plus libtiff with the network
  feature. PROJ also ships the EPSG dataset under its own terms of use. It could come
  later as an opt-in feature. `proj4rs` is the pure-Rust option (§4.2.4).
* **QLever-style inline `GeoPoint` ids.** QLever quantizes lat/lng to 30 bits each in the
  60-bit payload, in z-order or lat-major order. That is lossy (≈ 2 cm) and drops the
  lexical form, which breaks Sparkles' exact term identity (the README decision on
  canonical inlining). It would also need a fifth-from-last tag, and only 4 of 16 are left.
  In Sparkles a point literal costs one vocabulary entry plus a column entry.
* **A geo-split vocabulary**, like QLever's `.geometry` sub-vocabulary with `.geoinfo`
  records. It changes the vocabulary format and the id layout. The per-generation geometry
  column gives the same precomputation without touching the vocabulary.
* **Canonicalizing geometry literals on load**, for example always writing CRS84. That
  breaks term identity, and users' literals must round-trip.
* **A spatial index maintained like F03**, with one mutable structure, results filtered
  against the snapshot and removed entries kept until a seal. An R-tree has no cheap
  segment model like Tantivy's. Base, overlay and persistent tail give MVCC by
  construction.
* **F04's per-query overlay**, where the commit path does no work and each query parses and
  scans the delta. Every query after a write would parse the delta's geometries again.
  Parsing once in the commit path costs microseconds per inserted geometry.
* **`rstar` (a dynamic R*-tree) for the base.** It builds more slowly than a packed tree
  and cannot be used zero-copy from an mmapped file. Flatbush, JTS's `STRtree` and
  `geo-index` all use packed Hilbert or STR trees for static data.
* **Jena's feature-keyed, envelope-only index**, with one STRtree per graph whose items
  are feature nodes. Stand-alone geometries are invisible to it, it cannot report `?g`, it
  skips the exact test for boxes, and it is never updated incrementally. Sparkles keys the
  index by serialization quad and maps to features at query time.
* **Jena's behaviours that give wrong or surprising answers** that GeoSPARQL does not
  require: the silent CRS84 fallback for unknown CRSs, the planar-degree-only buffer on
  geographic data, envelope-only `withinBox` answers, and `false` for every relation on
  empty geometries. §11 lists each one as a divergence with a default.
* **Approximating distance in Web Mercator metres**, as QLever's `geof:distance` does
  through `webMercMeterDist`. The scale error grows with latitude and reaches ×2 at 60°.
* **S2 (`s2` 0.2, Apache-2.0).** It is a partial port without polygons ("lines and
  polygons aren't implemented yet"). Cell-based prefilters are a Phase 3 idea, not a
  dependency.
* **QLever's `SERVICE spatialSearch:` as the primary interface.** It overloads SERVICE,
  which F04 argued against for vector search. The standard FILTER form plus planner
  rewrites covers the same joins. A compatibility shim stays an open question.
* **Vendoring the GeoSPARQL Compliance Benchmark.** It is GPL-2.0-only.
* **OSM's tile servers as the default basemap.** The tile usage policy forbids offline and
  bulk use, requires a unique User-Agent and Referer, and blocks clients without notice.
  Air-gapped servers must still render maps.
* **A Sparkles-specific "geometry" literal datatype**, as F04 did for vectors. The OGC
  datatypes already exist, and real data uses them.

## 11. Open questions (defaults chosen here)

1. **Index opt-in.** The spatial index is enabled per dataset (`geo.json`), like text
   search. The functions always work. The alternative is to enable the index
   automatically when a load finds `geo:asWKT` or `geo:asGeoJSON` quads.
2. **Distance model.** The default is `geodesic` (WGS 84, Karney). `haversine`, with
   Jena's R = 6,371,008.7714 m, is a per-dataset option. Numbers identical to Jena's would
   need `haversine` as the default.
3. **`getSRID` datatype.** `xsd:anyURI`, as the standard says, rather than Jena's
   `xsd:string`.
4. **Empty geometries.** DE-9IM, where an empty geometry is disjoint from everything,
   rather than Jena's "always false".
5. **`sfEquals`/`ehEquals`.** Topological equality `T*F**FFF*`, so equal points are equal.
   The pattern printed in the tables, `TFFFTFFFT`, would make two equal points unequal.
6. **Legacy `…/EPSG/4326` (no `/0/`).** Treated as CRS84 lon/lat, as Jena's alias does,
   rather than as EPSG:4326 lat/lon.
7. **Unknown CRSs.** Such literals are parsed and usable with planar functions within the
   same CRS. They are not indexed, and metric functions raise an error. Jena falls back to
   CRS84 with a warning.
8. **Units.** Sparkles accepts OGC, QUDT and EPSG URN units. GeoSPARQL 1.1 recommends
   QUDT. Jena accepts OGC and EPSG, and QLever accepts QUDT.
9. **`withinBox` / `intersectBox`.** Sparkles always runs the exact test. Jena returns
   envelope hits when the subject is unbound.
10. **Feature links of `spatial:`.** `hasDefaultGeometry` and `hasGeometry`, as in Jena.
    Should 1.1's `hasCentroid` and `hasBoundingBox`, which are subproperties of
    `hasGeometry`, count too? The default is no, because they are not "the" geometry.
11. **`length` of a polygon.** The sum of all ring lengths, which equals the perimeter.
    QLever uses the exterior ring only. The standard says "the longest length from any one
    dimension", which is ambiguous.
12. **GeoSPARQL Compliance Benchmark (GPL-2.0).** Should the opt-in script that fetches it
    at test time be allowed, with the benchmark never vendored? The recommendation is yes.
13. **Map library and basemap size.** MapLibre GL JS, loaded lazily, with a bundled
    Natural Earth 1:110m basemap of a few hundred KB precompressed. The alternative is
    Leaflet, which is smaller and draws with canvas or SVG, and no basemap.
14. **QLever `SERVICE spatialSearch:` compatibility.** Not planned. Add a translation shim
    if QLever users ask.
15. **Strict writes.** Accept ill-typed geometry literals and count them, as RDF 1.2
    allows. An opt-in rejection like [C10](C10-write-time-validation.md) could come later.
16. **EPSG data.** For Phase 3, ship no EPSG-derived definitions, and let the operator
    supply `crs.json`. The exception would be bundled UTM-style definitions, if the
    maintainer accepts the EPSG terms or the CC0 label of `crs-definitions`. The built-in
    UTM zones are formulas with zone parameters, not EPSG data.
17. **Query Rewrite on by default.** This matches Jena. It changes the answers of existing
    queries that use `geo:sf*` as plain predicates on data with geometries. They get more
    rows, never fewer. A dataset can turn rewrite off.
18. **Index scope includes `urn:x-sparkles:inferred`.** As for text, and `reasoning=false`
    filters it out.
19. **Overlay tail threshold.** `max(4096, overlay/8)`. Measure it in Phase 1.
20. **Phase 1 commit latency.** Large polygons are parsed in the commit path, and a
    1M-vertex insert takes about 100 ms to parse. Is that acceptable, or should literals
    above 64 KiB be parsed at the first query instead?

## 12. Sources

* **Sparkles repository** (read only, at `e23a1f5`):
  * `README.md` (status, philosophy, decisions, "Out of scope for v1"), `docs/AUDIT.md`,
    `docs/BENCHMARKS.md`, the project's feature planning notes,
    [F03](F03-full-text-search.md), [F04](F04-vector-search.md),
    [PROVENANCE.md](PROVENANCE.md);
  * `crates/sparkles/src/`:
    * `text.rs`: deferred commits, views and slots, open, WAL catch-up, rebuild;
    * `vector.rs`: per-generation segments, delta overlay, budget, search kernel;
    * `store.rs`: `Generation`, `Delta`, `Snapshot`, `publish_log`, `rebuild_locked`,
      `maintain_text`, `StoreOptions`;
    * `id.rs`: tags and inline types;
    * `sparql/plan.rs`: `Kind`, `GraphFilter`, `collect`, `plan_group`, `join_order`,
      `place_filters`, `filter`/`push_range`, `vector_leaf`, `text_leaf`;
    * `sparql/textpf.rs` (`take_calls`), `sparql/expr.rs` (`is_extension`, `extension`),
      `sparql/exec.rs` (dispatch, custom aggregates), `sparql/cache.rs`, `sparql/ctx.rs`
      (`Ctx`, `Optimizations`), `sparql/keyfilter.rs`, `error.rs`;
  * `crates/sparkles-server/src/{main.rs,http.rs}`: `text-index` CLI and `/$/text` routes;
  * `crates/sparkles-reasoner/src/lib.rs` (profiles);
  * `ui/package.json`, `ui/src/routes`, `ui/src/lib`; `scripts/` (`bench.sh`, `gen-data.py`,
    `third-party-licenses.py`).
* **OGC GeoSPARQL 1.1**, OGC 22-047r1 (https://docs.ogc.org/is/22-047r1/22-047r1.html,
  2024-01-29), and its AsciiDoc source on the `geosparql-1.1` branch of
  github.com/opengeospatial/ogc-geosparql. The repository has no license file. The
  ontologies at http://www.opengis.net/ont/geosparql and …/ont/sf declare the permissive
  OGC Document License (https://www.ogc.org/license).
* **GeoSPARQL 1.0**, OGC 11-052r4 (https://docs.ogc.org/is/11-052r4/11-052r4.pdf).
* **OGC definitions server**, where the unit IRIs for metre, degree, radian and unity
  resolve, and the QUDT unit IRIs (http://qudt.org/vocab/unit/).
* **RFC 7946** (GeoJSON). Simple Features (OGC 06-103r4 / ISO 19125-1) and ISO 13249-3 are
  cited from the GeoSPARQL text and from general knowledge. Their rule on WKT keyword case
  was not re-checked.
* **Apache Jena** (Apache-2.0), github.com/apache/jena at
  `b1dcba53b5` (2026-09-28, 6.3.0-SNAPSHOT): `jena-geosparql` (`GeoSPARQLConfig`, `WKTReader`,
  `WKTWriter`, `GeometryWrapper`, `SRSInfo`, `UnitsOfMeasure`, `UnitsRegistry`,
  `GenericFilterFunction`, `GenericPropertyFunction`, `SpatialObjectGeometryLiteral`,
  `AccessGeoSPARQL`, `AccessWGS84`, `QueryRewriteIndex`, `GeometryLiteralIndex`,
  `spatial/property_functions/*`, `spatial/index/v2/*`, `SpatialIndexFindUtils`,
  `GeoSPARQLOperations`, `EgenhoferIntersectionPattern`, `GreatCircleDistance`, tests),
  `jena-fuseki2/jena-fuseki-mod-geosparql`, `jena-fuseki2/jena-fuseki-geosparql`
  (`ArgsConfig`), `jena-integration-tests` (`TestGeoAssembler`), `CHANGES.txt`, the
  Fuseki distribution `LICENSE`/`NOTICE`. Documentation:
  https://jena.apache.org/documentation/geosparql/, …/geosparql-fuseki,
  …/geosparql-assembler.html.
* **QLever** (Apache-2.0), github.com/ad-freiburg/qlever at
  `b0c6d0cd` (2026-09-30): `ValueId.h`, `GeoPoint.{h,cpp}`, `GeometryInfo*.{h,cpp}`,
  `GeoVocabulary.{h,cpp}`, `GeoCellGrid.{h,cpp}`, `SpatialJoin*.{h,cpp}`,
  `spatialJoinAlgorithms/*`, `SpatialQuery.{h,cpp}`, `QueryRewriteUtils.cpp`,
  `SparqlQleverVisitor.cpp`, `UnitOfMeasurement.h`, `RuntimeParameters.h`, `CMakeLists.txt`,
  tests. Documentation: https://docs.qlever.dev/geosparql/. Sparkles takes ideas from
  QLever but no code. QLever's geometry dependency, `ad-freiburg/spatialjoin`, is listed as
  Apache-2.0 on GitHub. The license of the `pb_util` library it pulls in could not be
  verified, and one survey recalled GPL-3.0. Nothing from either library is used.
* **Oxigraph `spargeo`** (MIT OR Apache-2.0), github.com/oxigraph/oxigraph at `e0f286b0` (2026-09-23):
  `lib/spargeo/src/{lib,parse,units,vocab}.rs`, `Cargo.toml`, `README.md`;
  `testsuite/oxigraph-tests/geosparql/`.
* **Rust crates** (crates.io API and docs.rs, consulted 2026-09-30; licenses from each
  crate's metadata): `geo` 0.33.1, `geo-types` 0.7.20, `wkt` 0.14.0, `geojson` 1.0.0,
  `geo-index` 0.4.0, `rstar` 0.13.0, `robust` 1.2.0, `i_overlay` 4.5.x (via `geo`; 9.0.0
  latest), `spade` 2.15.1 (all MIT OR Apache-2.0); `geographiclib-rs` 0.2.7 (MIT); `proj`
  0.31.0 / `proj-sys` 0.27.0 (MIT OR Apache-2.0; PROJ 9.9.0 itself MIT); `proj4rs` 0.2.0
  (MIT OR Apache-2.0); `crs-definitions` 0.6.0 (CC0-1.0, EPSG-derived); `s2` 0.2.0
  (Apache-2.0); `geos` 11.3.1 (MIT; binds LGPL-2.1 libgeos); `geozero` 0.15.1, `wkb` 0.9.2,
  `geo-traits` 0.3.0, `geoarrow` 0.9.0 (MIT OR Apache-2.0); `flatgeobuf` 6.0.1
  (BSD-2-Clause); `h3o` 0.11.0 (BSD-3-Clause); `static_aabb2d_index` 2.1.0 (MIT OR
  Apache-2.0). `geo`'s CHANGES.md gives these capability facts: `PreparedGeometry` arrived
  in 0.29 and moved to `geo::indexed` in 0.32, `Validation` in 0.30, `Buffer` in 0.31,
  `Covers` in 0.32 and `MakeValid` in 0.33. Geodesic and haversine distance work between
  points only.
* **EPSG Dataset Terms of Use** (https://epsg.org/terms-of-use.html).
* **Map libraries and data**: Leaflet (BSD-2-Clause, 1.9.4), MapLibre GL JS (BSD-3-Clause,
  6.11.2) and OpenLayers (BSD-2-Clause, 10.10.0); the Natural Earth terms of use (public
  domain); the OSM Foundation tile usage policy
  (https://operations.osmfoundation.org/policies/tiles/).
* **Test suites**: OGC `ets-geosparql11` (Apache-2.0), of which only a template exists.
  The GeoSPARQL Compliance Benchmark (github.com/OpenLinkSoftware/GeoSPARQLBenchmark,
  GPL-2.0-only), of which only the README, LICENSE and file layout were read.
* **Papers**:
  * Jovanovik, Homburg, Spasić, "A GeoSPARQL Compliance Benchmark", ISPRS IJGI 10(7):487,
    2021, doi:10.3390/ijgi10070487, for its results table.
  * Bast, Brosi, Kalmbach, "Efficient Spatial Joins on Large Geometry Sets", SIGSPATIAL
    2025, doi:10.1145/3748636.3762757. Also the 2024 SIGSPATIAL spatial-join paper, of
    which only the abstract was read, because the full text could not be fetched.
  * Karney, "Algorithms for geodesics", J. Geodesy 87:43–55, 2013,
    doi:10.1007/s00190-012-0578-z.
  * Karney, "Transverse Mercator with an accuracy of a few nanometers", J. Geodesy
    85:475–485, 2011, cited from general knowledge.
  * Leutenegger, Lopez, Edgington, "STR", ICDE 1997, doi:10.1109/ICDE.1997.582015.
  * Beckmann et al., "The R*-tree", SIGMOD 1990.
  * Guttman, "R-trees", SIGMOD 1984; Kamel and Faloutsos 1993 (Hilbert packing); and
    Hjaltason and Samet 1999 (distance browsing), all cited from general knowledge.
  * Egenhofer and Franzosa, IJGIS 5(2), 1991, doi:10.1080/02693799108927841.
  * Randell, Cui, Cohn, KR 1992.
  * Clementini, Di Felice, van Oosterom, SSD 1993, doi:10.1007/3-540-56869-7_16.
* **Not read**: no GPL or LGPL source code was read. The GeoSPARQL benchmark was looked
  at only for its license, size and layout, and GEOS and QLever's `pb_util` were not
  looked at at all.

## 13. Provenance entry (for `PROVENANCE.md`)

This is the entry as drafted with the spec, before implementation.
[PROVENANCE.md](PROVENANCE.md#geosparql) has the current entry, with the dependencies
that actually shipped.

```markdown
## GeoSPARQL

- **Spec:** `specs/G01-geosparql.md`, written on 2026-09-30 from the
  Sparkles code and specs (F03, F04, PROVENANCE); OGC GeoSPARQL 1.1 (22-047r1) and 1.0
  (11-052r4) and the GeoSPARQL ontologies (OGC Document License); RFC 7946; Apache Jena's
  `jena-geosparql` sources, tests and documentation (Apache-2.0; behaviour reference, as for
  all Jena ports); QLever's spatial sources and docs (Apache-2.0; ideas only); Oxigraph's
  `spargeo` and its GeoSPARQL tests (MIT OR Apache-2.0); crates.io and docs.rs pages for
  `geo`, `wkt`, `geojson`, `geo-index`, `rstar`, `geographiclib-rs`, `proj`, `proj4rs`,
  `crs-definitions`, `s2`, `geos` and others; the EPSG terms of use; the Leaflet, MapLibre,
  Natural Earth and OSM tile-policy pages; and papers (Jovanovik et al. 2021; Bast et al.
  2024/2025; Karney 2011/2013; STR, R*-tree, Egenhofer, RCC8, DE-9IM). The GeoSPARQL
  Compliance Benchmark (GPL-2.0-only) was looked at for its license, size and layout only.
  No GPL or LGPL source was read.
- **Implementation:** not started.
  - **Planned dependencies** (spec §5.1, all permissive), behind a `geo` feature of
    `sparkles` that the server enables by default: `geo` 0.33 (MIT OR Apache-2.0, with
    `geo-types`, `robust`, `rstar`, `i_overlay` and `geographiclib-rs` (MIT)), `wkt` 0.14,
    `geojson` 1, `geo-index` 0.4 (all MIT OR Apache-2.0). UI (Phase 2): MapLibre GL JS 6
    (BSD-3-Clause) and Natural Earth 1:110m data (public domain). Phase 3 optional:
    `proj4rs` 0.2 (MIT OR Apache-2.0). Test data (Phase 2): Oxigraph's GeoSPARQL test cases
    (MIT OR Apache-2.0), vendored with their notice.
- **Rejected** (spec §10):
  - GEOS (`geos` binds LGPL-2.1 libgeos) and PROJ via `proj` as the default (C/C++ build,
    EPSG terms);
  - QLever-style inline GeoPoint ids and a geo-split vocabulary;
  - canonicalizing geometry literals;
  - an F03-style mutable index and F04's per-query overlay;
  - `rstar` for the base index;
  - Jena's feature-keyed envelope-only index, silent CRS84 fallback and envelope-only box
    answers;
  - Web Mercator distance approximations;
  - `s2` as a dependency;
  - QLever's `SERVICE spatialSearch:` as the primary syntax;
  - vendoring the GPL-2.0 compliance benchmark;
  - OSM tile servers as the default basemap;
  - a Sparkles-specific geometry datatype.
```

## Outcome

**Delivered.** Phases 1 and 2 landed on 2026-10-01, and part of Phase 3 on 2026-10-02.
[PROVENANCE.md](PROVENANCE.md#geosparql) lists the dependencies. The Phase 3 work covers
these items.
* `--vocab geosparql` types geometries from their serializations (`sf:Polygon` from a WKT
  `POLYGON`, `gml:Polygon` from a GML root element) and adds the GML class hierarchy.
* `spatial:` arguments can be variables that the rest of the group binds, with one search
  per binding.
* A cell grid over each prepared region of 32 or more vertices decides most points and
  small regions in spatial joins and FILTERs without the exact test.
* `geo:gmlLiteral` and `geo:kmlLiteral` parse, `geof:asGML` and `geof:asKML` write them,
  and the index reads them. The UI's maps draw them too. The browser has no GML or KML
  reader, so it sends them to `POST /$/geo/convert`.
* `--geo-crs` registers projected CRSs from proj4 definitions, which transform through
  `proj4rs`.

The rest of Phase 3 is not built. That is k-NN with arbitrary joins, simplified polygon
approximations, `geo:hasMetricArea` and similar properties by rewrite, QLever's
`SERVICE spatialSearch:` syntax, and the Compliance Benchmark in CI.

**Decided by the maintainer.**
* Measures are geodesic on WGS 84 by default, with haversine as a per-dataset option
  (§11 q2).
* The index is opt-in per dataset (q1).
* No EPSG data ships (q16).
* The map uses MapLibre with a bundled Natural Earth basemap (q13).
* The GPL-2.0 Compliance Benchmark is only fetched at test time, behind
  `SPARKLES_ALLOW_GPL_BENCHMARK=1`, and never vendored (q12).
* Query Rewrite is off by default, against the default proposed in q17. A dataset turns it
  on with `"queryRewrite": true`, and `serve --no-geo-rewrite` remains a server-wide
  switch. Enabling an index therefore never changes what an existing query means.
* After Phase 1, two empty geometries are never `sfEquals`, and an empty geometry is only
  disjoint. This matches Jena, whose filter functions return false whenever a side is
  empty.

**Deviations.**
* Sparkles has its own WKT and GeoJSON readers and writers, and the `wkt` and `geojson`
  crates were dropped. Errors need byte offsets, and GeoSPARQL literals use `LINEARRING`,
  `TRIANGLE`, `TIN`, `POLYHEDRALSURFACE`, untagged 3D positions and a CRS IRI prefix.
  Constructed geometries are 2D.
* Metric buffers project with an ellipsoidal AEQD, not a spherical one (§4.4.3). A
  constructed literal over the memory budget is a type error, not `507`.
* Index files are keyed by a hash of the settings that change what is indexed, not of the
  whole `geo.json` (§5.5). Changing `distance` or `queryRewrite` therefore keeps them.
  `?at=` snapshots run without the index, by scanning, instead of building a base on
  demand (§4.6).
* Phase 3 departs from §4.1.3, §4.2.4 and §2.7 in these ways.
  * Geometry types come with `--vocab geosparql` rather than a separate
    `geometryTypes` rule. They are rules with two Sparkles builtins, `geoSfType` and
    `geoGmlType`, and run with the profile's other rules. The type is read from the WKT
    keyword, the GeoJSON `type` or the XML root element, without parsing coordinates.
  * GML elements are matched by local name in any namespace, since real data (the
    Compliance Benchmark's among it) uses outdated namespaces. `Envelope`,
    `PolyhedralSurface`, `Tin` and the GML 2 forms read too. Curved segments do not.
  * The CRS registry is per process and set by the global `--geo-crs` flag rather than
    a `crs.json` per dataset, because a CRS IRI means the same thing in every dataset.
    Only projected definitions are accepted. The registered definitions are part of the
    index files' identity, so changing them rebuilds the index.
  * No EPSG data ships, as decided (q16). The opt-in `geo-epsg` feature resolves other
    EPSG codes through `crs-definitions`, which is derived from EPSG.
  * A `spatial:` binding that does not make valid arguments matches nothing, while the
    same constant is a `400`. An unbound argument variable is a `400`, no longer `501`.
* Some choices were made during implementation and are open to revision:
  * `concaveHull`'s percentage maps linearly to the concavity.
  * `aggConcaveHull` takes one argument, because a SPARQL aggregate takes one expression.
  * `spatial:equals` is always available.
  * The `--vocab geosparql` axioms are written from the standard and land in the inferred
    graph.
  * `GET /{ds}/geo` scans when the index is not ready.
  * `POST /$/geo/convert` is open to any caller.

**Conformance.** The §7 examples and seeded comparisons of the index plan against the plain
plan run as tests, and so do the GML and KML examples printed in GeoSPARQL 1.1. The W3C
and SHACL results did not change. Sparkles passes 37 of the 44 cases in Oxigraph's
GeoSPARQL suite. The other 7 are listed with reasons in
`testsuite/geosparql/oxigraph/expected-failures.txt`. On the Phase 1 build, the
[Compliance Benchmark](../BENCHMARKS.md#geosparql-compliance-benchmark) scored 74 of 206.
After Phase 3 it scores 187 of 206, against the 177 GeoSPARQL Fuseki 3.17 published.
The extension requirements R25 to R30 ran against a database with RDFS entailment and
query rewrite, and the others against the plain data. Without the extensions the score is
166. The 19 misses are the empty-equality queries decided above, distances and a metre
buffer computed another way, and rewrite answers that DE-9IM does not give.

**Performance.** The index adds no measurable commit latency ([commit
cost](../BENCHMARKS.md#spatial-index-commit-cost)). The §9 query targets have not been
measured. The cell grid made 200,000 `sfContains` tests against a 1,024-vertex polygon
4.3 times faster (60 ms instead of 256 ms) and 5.4 times faster against a 16,384-vertex
one, for about 16 bytes per region vertex, up to 64 KiB per region.
