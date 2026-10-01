# G01: GeoSPARQL (OGC GeoSPARQL 1.1, Jena spatial extensions, spatial index)

> **Status:** implemented in part (Phases 1 and 2; Phase 3 not built)
>
> **Phases:** Phase 1 (geometry literals, CRSs and units, the `geof:` functions, the
> spatial index with FILTER pushdown, Jena's `spatial:` property functions, server and CLI
> surfaces); Phase 2 (spatial joins and k-NN, Query Rewrite, `spatial:equals`, RDFS
> entailment and default geometries, the aggregates, hulls and `isSimple`, `spatialF:`,
> UTM zones, persisted index files, W3C Basic Geo points, `GET /{ds}/geo`,
> `POST /$/geo/convert`, the UI maps, Oxigraph's GeoSPARQL tests); Phase 3 (GML and KML,
> other EPSG CRSs) not started.
>
> **User docs:** [API: GeoSPARQL](../API.md#geosparql) ·
> [API: hulls, aggregates, `spatialF:`, UTM and conversion](../API.md#hulls-aggregates-jena-filter-functions-utm-and-conversion) ·
> [API: spatial joins and nearest neighbours](../API.md#spatial-joins-and-nearest-neighbours) ·
> [API: query rewrite and RDFS entailment](../API.md#query-rewrite-spatialequals-and-rdfs-entailment) ·
> [API: maps in the web UI](../API.md#maps-in-the-web-ui) ·
> [Features](../FEATURES.md#sparql-arq-equivalent) ·
> [Benchmarks: spatial index commit cost](../BENCHMARKS.md#spatial-index-commit-cost) ·
> [Benchmarks: GeoSPARQL Compliance Benchmark](../BENCHMARKS.md#geosparql-compliance-benchmark)
>
> This is the design as written before implementation; the [Outcome](#outcome) section at the end
> records how it landed.

The design depends on the commit-identity work ([CI](CI-commit-identity.md), durable
`seq`), the budgets of [C01](C01-observability-and-budgets.md), and the planner and
executor as they stood at `e23a1f5`. It changes a README decision: "GeoSPARQL" leaves the
"Out of scope for v1" row (README §Divergences, `docs/AUDIT.md` rows for jena-geosparql and
`spargeo`) when Phase 1 lands.

## 1. Summary, goals, non-goals

Sparkles gets GeoSPARQL: geometry literals (`geo:wktLiteral`, `geo:geoJSONLiteral`), the
`geof:` function library of GeoSPARQL 1.1 (topological relations of the Simple Features,
Egenhofer and RCC8 families, `relate`, the non-topological and 1.1 measurement functions,
later the aggregates), Jena's `spatial:` property functions and `spatialF:` filter
functions, the Query Rewrite and RDFS Entailment extensions, and a spatial index that makes
the common shapes fast:

* a FILTER with a constant geometry (`sfWithin(?w, "POLYGON(…)")`, `distance(?w, C, u) < r`);
* a Jena property function (`?f spatial:nearby (51.5 -0.12 5 uom:kilometre 10)`);
* later, a spatial join between two variables (`FILTER(geof:sfContains(?region, ?point))`)
  and k-nearest-neighbour `ORDER BY geof:distance(…) LIMIT k`.

Literals stay authoritative and are stored as ordinary vocabulary terms, never rewritten.
Parsed geometries and the R-tree are derived data. The index follows the vector
segment's model, a base structure per generation plus an overlay, but the overlay is
maintained in the commit path the way [F03](F03-full-text-search.md) maintains its documents. That makes every
snapshot, including historical `?at=` snapshots, see exactly its own geometries. The index
is an optimization: a query gives the same answer with it, without it, and while it is
building.

**Goals**

* GeoSPARQL 1.1 conformance classes **Core**, **Topology Vocabulary** (all three relation
  families), **Geometry Extension** (serialization WKT, then GeoJSON), **Geometry Topology
  Extension** (WKT and GeoJSON; Simple Features, Egenhofer and RCC8), **RDFS Entailment
  Extension** (WKT) and **Query Rewrite Extension** (WKT and GeoJSON; all three families).
  The phase table follows. Sparkles states its conformance per class, as the standard
  requires (GeoSPARQL 1.1 Annex A).
* Jena compatibility where users see it: the `geo:`, `geof:`, `spatial:` and `spatialF:`
  IRIs and argument forms. Existing Jena GeoSPARQL queries run unchanged, and answers agree
  except where §11 lists a deliberate divergence (each of them is a Jena bug or a silent
  approximation).
* Correct geodesic measurement on geographic CRSs (WGS 84 ellipsoid, Karney's algorithms):
  distance, area, length, metric buffer.
* Snapshot-consistent, index-accelerated spatial selection with no staleness window and
  no `503` while an index builds.
* Pure Rust and permissive licenses only (MIT, Apache-2.0, BSD, CC0). No GEOS (LGPL), no
  PROJ C build by default.
* Admin surfaces (HTTP, CLI, UI) for the index, explain output, and a map view in the UI
  that needs no external tile server.

**Non-goals (all phases unless noted)**

* DGGS literals (`geo:dggsLiteral`, `geof:asDGGS`), the Geometry Extension DGGS conformance
  class.
* 3D topology, M-aware computation. Z and M are parsed, kept and reported (`is3D`,
  `isMeasured`, `minZ`/`maxZ`), and ignored by every computation, as GeoSPARQL 1.1 §10.2
  prescribes.
* Arbitrary EPSG CRSs in Phases 1–2. Phase 3 adds an optional pure-Rust transform backend
  (§4.2.4).
* Raster data, routing, map tiles, a tile server.
* Jena's spatial index file format, assembler vocabulary (`geosparql:GeosparqlDataset`) and
  GeoSPARQL-Fuseki CLI options. Sparkles configures the index itself (§2.8).
* QLever's `SERVICE spatialSearch:` syntax (§10; open question 14).

### 1.1 Conformance by phase

| Conformance class (1.1, `http://www.opengis.net/spec/geosparql/1.x/conf/…`) | Phase 1 | Phase 2 | Phase 3 |
|---|---|---|---|
| Core (`/conf/core`) | ✅ (vocabulary only; nothing to compute) | | |
| Topology Vocabulary (`/conf/topology-vocab-extension`), sf / eh / rcc8 | ✅ | | |
| Geometry Extension (`/conf/geometry-extension`), WKT | ✅ all functions except the aggregates, `boundingCircle`, `concaveHull`, `asGML`, `asKML` | ✅ complete | |
| Geometry Extension, GeoJSON | ✅ same as WKT | ✅ | |
| Geometry Extension, GML / KML | | | ✅ (GML 3.2 Simple Features profile; KML 2.2 geometry) |
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
| `geo:` | `http://www.opengis.net/ont/geosparql#` | datatypes, properties, the 24 topological properties (Query Rewrite) |
| `geof:` | `http://www.opengis.net/def/function/geosparql/` | functions |
| `sf:` | `http://www.opengis.net/ont/sf#` | geometry type IRIs (`geof:geometryType`, RDFS entailment) |
| `uom:` | `http://www.opengis.net/def/uom/OGC/1.0/` | units (also QUDT, §4.3) |
| `spatial:` | `http://jena.apache.org/spatial#` | Jena property functions |
| `spatialF:` | `http://jena.apache.org/function/spatial#` | Jena filter functions |

Sparkles adds no IRIs of its own for GeoSPARQL. Every term above belongs to OGC or Jena, so
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
  with equal geometries. `=` and `sameTerm` stay term identity, and DISTINCT does not merge
  them. Only the relation functions, such as `geof:sfEquals`, compare geometries.
* A literal whose lexical form is not a valid geometry of its datatype is stored anyway
  (RDF 1.2 §3.4.2: ill-typed literals are accepted). Functions raise a type error on it, the
  index skips it, and index status counts it under `skipped.malformed`.
* W3C Basic Geo (`wgs84_pos:lat` / `wgs84_pos:long` on one subject) is indexed as points
  when the index's `wgs84` option is on (Phase 2; Jena indexes it by default when a graph
  has no GeoSPARQL literals).

### 2.3 Functions (`geof:`)

Every function works in FILTER, BIND, SELECT expressions, ORDER BY and HAVING, with or
without an index. Argument errors are SPARQL type errors: unbound in BIND, false in FILTER
(SPARQL 1.1 §17.2, §17.6; GeoSPARQL 1.1 §10.9.1). "geom" means a `geo:wktLiteral` or
`geo:geoJSONLiteral` (later `gmlLiteral` / `kmlLiteral`). A geometry result has the
datatype and CRS of the first geometry argument (GeoSPARQL 1.1 §10.9.1). A function with
no geometry argument returns WKT in CRS84.

Geometric results are new literals in the canonical form of §4.1.5.

| Function | Result | Phase | Semantics (§4 has the details) |
|---|---|---|---|
| `distance(g1, g2, unit)` | `xsd:double` | 1 | shortest distance. Geographic CRS: geodesic on WGS 84 (§4.4.2). Projected: Euclidean in CRS units, converted. `0` when the geometries intersect |
| `metricDistance(g1, g2)` | `xsd:double` | 1 | `distance(g1, g2, uom:metre)` |
| `buffer(g, radius, unit)` | geom | 1 | radius ≥ 0, or < 0 for areal geometries. Linear unit on a geographic CRS: metric buffer through a local projection (§4.4.3). Angular unit on a geographic CRS: planar degrees (as in Jena) |
| `metricBuffer(g, radius)` | geom | 1 | `buffer(g, radius, uom:metre)` |
| `convexHull(g)` | geom | 1 | planar in the CRS of `g` |
| `concaveHull(g, targetPercent?)` | geom | 2 | `geo` concave hull; parameters documented (§4.4.6) |
| `boundingCircle(g)` | geom | 2 | smallest enclosing circle (Welzl), polygonized with 32 segments per quadrant |
| `envelope(g)` | geom | 1 | axis-aligned bounding box as a `POLYGON`; a point gives the point, a degenerate box a `LINESTRING` (as in Jena/JTS) |
| `boundary(g)` | geom | 1 | OGC boundary: points → empty, curves → endpoints (mod-2 rule), polygons → rings |
| `intersection` / `union` / `difference` / `symDifference(g1, g2)` | geom | 1 | overlay (§4.4.5); `g2` transformed into the CRS of `g1` |
| `centroid(g)` | geom (`POINT`) | 1 | planar centroid in the CRS of `g` (as in Jena; §4.4.4) |
| `getSRID(g)` | `xsd:anyURI` | 1 | the CRS IRI as written (CRS84 when absent). Jena returns `xsd:string` (§11 q3) |
| `transform(g, srs)` | geom | 1 | between the built-in CRSs (§4.2). Unknown or unsupported target: type error |
| `asWKT(g)` / `asGeoJSON(g)` | `wktLiteral` / `geoJSONLiteral` | 1 | conversion. GeoJSON output is always CRS84 (RFC 7946; GeoSPARQL Req 26) |
| `asGML(g, profile)` / `asKML(g)` | | 3 | |
| `area(g, unit)` / `metricArea(g)` | `xsd:double` | 1 | geodesic area on geographic CRSs (Karney), planar otherwise. `0` for non-areal geometries |
| `length(g, unit)` / `metricLength(g)` | `xsd:double` | 1 | geodesic or planar length of curves; areas give their boundary length; points 0 (§11 q11) |
| `perimeter(g, unit)` / `metricPerimeter(g)` | `xsd:double` | 1 | boundary length of areal geometries, `0` otherwise |
| `dimension(g)` | `xsd:integer` | 1 | topological dimension: 0, 1 or 2 (empty geometries: below) |
| `coordinateDimension(g)` / `spatialDimension(g)` | `xsd:integer` | 1 | 2, 3 (XYZ or XYM) or 4 / 2 or 3 |
| `is3D(g)` / `isMeasured(g)` / `isEmpty(g)` | `xsd:boolean` | 1 | from the literal's declared layout |
| `isSimple(g)` | `xsd:boolean` | 2 | OGC simplicity (no self-intersection except ring closure); own implementation (§4.4.7) |
| `geometryType(g)` | `xsd:anyURI` | 1 | `sf:Point`, `sf:LineString`, `sf:Polygon`, `sf:MultiPoint`, `sf:MultiLineString`, `sf:MultiPolygon`, `sf:GeometryCollection` (also `sf:LinearRing`, `sf:Triangle`, `sf:TIN`, `sf:PolyhedralSurface` when written so) |
| `numGeometries(g)` / `geometryN(g, n)` | `xsd:integer` / geom | 1 | direct members; atomic geometries count 1 and `geometryN(g, 1)` is `g`; `n` is 1-based; out of range: type error |
| `minX` `minY` `maxX` `maxY` (`g`) | `xsd:double` | 1 | in the literal's own axis order (for EPSG:4326, X is latitude; as in Jena) |
| `minZ` / `maxZ(g)` | `xsd:double` | 1 | type error when `g` has no Z |
| `relate(g1, g2, matrix)` | `xsd:boolean` | 1 | DE-9IM pattern match, `[012TF*]{9}` |
| 24 topological functions | `xsd:boolean` | 1 | §2.4 |
| `aggBoundingBox`, `aggBoundingCircle`, `aggCentroid`, `aggConvexHull`, `aggUnion`, `aggConcaveHull(g, pct)` | geom | 2 | custom aggregates (§5.8) |

`geof:dimension` of an empty geometry: GeoSPARQL leaves it open and Oxigraph returns `-1`.
Sparkles returns the declared dimension of the empty type (`POINT EMPTY` → 0,
`POLYGON EMPTY` → 2) and `-1` for `GEOMETRYCOLLECTION EMPTY` and the empty literal `""`. It
is never a type error.

Jena filter functions (`spatialF:`, Phase 2, identical argument forms): `convertLatLon(lat,
lon)`, `convertLatLonBox(latMin, lonMin, latMax, lonMax)` (both return EPSG:4326 WKT),
`equals(g1, g2)`, `nearby(g1, g2, radius, unit)` and `withinCircle` (distance `<` radius),
`distance(g1, g2, unit)`, `greatCircle(lat1, lon1, lat2, lon2, unit)`, `greatCircleGeom(g1,
g2, unit)`, `angle`, `angleDeg`, `azimuth`, `azimuthDeg`, `transform(g, datatype, srs)`,
`transformDatatype`, `transformSRS`. Their unit argument may be an IRI, an `xsd:anyURI`
literal or a string, as in Jena. `greatCircle*` follows the dataset's distance model (§4.4.2).

### 2.4 Topological relations

The relation functions take two geometries (`geof:sfIntersects(?a, ?b)`) and return
`xsd:boolean`. `g2` is transformed into the CRS of `g1`. When the two have no common
built-in CRS, the result is a type error. The DE-9IM matrix comes from `geo`'s `Relate`
(planar, in the CRS of `g1`; for geographic CRSs, planar in longitude/latitude, as in Jena,
QLever, Oxigraph and the standard's own computation model).

| Function | Holds when | Note |
|---|---|---|
| `sfEquals`, `ehEquals` | `T*F**FFF*` (topological equality) | The tables print `TFFFTFFFT`, which is false for two equal points because a point has an empty boundary. Sparkles uses JTS/`geo` topological equality (§11 q5) |
| `rcc8eq` | `TFFFTFFFT` | areas only |
| `sfDisjoint`, `ehDisjoint` | `FF*FF****` | 1.1 Table 2 misprints `FF**FF****` |
| `rcc8dc` | `FFTFFTTTT` | areas only |
| `sfIntersects` | `T********` ∨ `*T*******` ∨ `***T*****` ∨ `****T****` | 1.1 Table 6 misprints the touches pattern |
| `sfTouches`, `ehMeet` | `FT*******` ∨ `F**T*****` ∨ `F***T****` | false for point/point |
| `rcc8ec` | `FFTFTTTTT` | areas only |
| `sfWithin` | `T*F**F***` | |
| `sfContains` | `T*****FF*` | |
| `sfOverlaps` | `T*T***T**` for A/A and P/P; `1*T***T**` for L/L | false when the dimensions differ |
| `ehOverlap` | `T*T***T**` | |
| `rcc8po` | `TTTTTTTTT` | areas only |
| `sfCrosses` | `T*T***T**` for P/L, P/A, L/A; `0********` for L/L | the vocabulary tables say `0********`, the function tables `0*T***T**` for L/L; Sparkles follows the vocabulary tables and JTS. False for other dimension pairs |
| `ehCovers` | `T*TFT*FF*` | |
| `ehCoveredBy` | `TFF*TFT**` | |
| `ehInside` | `TFF*FFT**` | |
| `ehContains` | `T*TFF*FF*` | |
| `rcc8tppi` / `rcc8tpp` / `rcc8ntpp` / `rcc8ntppi` | `TTTFTTFFT` / `TFFTTFTTT` / `TFFTFFTTT` / `TTTFFTFFT` | areas only |

* "Areas only": RCC8 relations are defined for regions (A/A). With any non-areal
  argument the result is `false`, as in Jena and GeoSPARQL 1.1 Table 5.
* **Empty geometries.** DE-9IM applies: an empty geometry is disjoint from everything
  (`sfDisjoint`, `ehDisjoint` true; `rcc8dc` false because an empty geometry is not a
  region), and every other relation is false. Jena returns false for every relation,
  disjoint included (§11 q4).
* `relate(g1, g2, pattern)` matches the computed matrix against any 9-character pattern of
  `T F * 0 1 2` (case-insensitive). Another length or character is a type error. It does not
  short-circuit on empty geometries (the matrix of an empty geometry is all `F` except
  `EE = 2`).

### 2.5 Jena property functions (`spatial:`)

A triple pattern whose predicate is one of these IRIs is a property function, not a data
match (Phase 1; constant arguments only, as with `text:query` and `spk:vectorSearch`).

| Property function | Object list | Matches features whose geometry … |
|---|---|---|
| `spatial:nearby`, `spatial:withinCircle` | `(lat lon radius [unit [limit]])` | is at distance `<` radius of the EPSG:4326 point. Default unit `uom:kilometre` |
| `spatial:nearbyGeom`, `spatial:withinCircleGeom` | `(geom radius [unit [limit]])` | is at distance `<` radius of `geom` |
| `spatial:withinBox` | `(latMin lonMin latMax lonMax [limit])` | is within the box (`sfWithin`) |
| `spatial:withinBoxGeom` | `(geom [limit])` | is within `geom`'s envelope |
| `spatial:intersectBox` | `(latMin lonMin latMax lonMax [limit])` | intersects the box |
| `spatial:intersectBoxGeom` | `(geom [limit])` | intersects `geom`'s envelope |
| `spatial:north` `south` `east` `west` | `(lat lon [limit])` | has an envelope that intersects the half-plane strip beyond the point (Jena's definition: north = from the point's latitude to 90°, all longitudes; east/west = up to 180° of longitude from the point, wrapping at ±180°) |
| `spatial:northGeom` … `westGeom` | `(geom [limit])` | as above, from `geom`'s envelope edge |
| `spatial:equals` | subject and object are features or geometry literals | `sfEquals` (Phase 2, with Query Rewrite) |

* **Subject.** A variable, IRI or blank-node label: the feature. `?f` binds features `F`
  with `F p G` for a feature link `p` (default `geo:hasDefaultGeometry`,
  `geo:hasGeometry`; configurable, §2.8), where `G` has an indexed serialization that
  matches. With `wgs84` on, subjects of matching `lat`/`long` pairs also match. A constant
  subject restricts the answer to that feature. Like Jena, stand-alone geometries
  (not linked from a feature) are not answers of `spatial:` functions; the `geof:`
  functions and Query Rewrite cover them.
* **Output.** One solution per distinct matching feature (set semantics; Jena can repeat a
  feature that has two matching geometries). Under `GRAPH ?g { … }`, `?g` binds the graph
  of the serialization quad, and the feature link must be in the same graph.
* **Limit.** `limit` > 0 keeps the `limit` matches nearest to the query geometry (for the
  box and cardinal functions: nearest to its envelope centre), ties by subject id. Jena
  applies the limit in index order. Sparkles' order is deterministic and the useful one.
  `limit ≤ 0` or absent means all matches, subject to the row budget.
* **Exact refinement.** Every match is tested exactly against the stated relation. Jena
  skips the exact test for `withinBox`/`intersectBox` when the subject is unbound and
  returns envelope hits (false positives for non-rectangular geometries; §11 q9). The
  cardinal functions are envelope-based by definition.
* **Without an index** (index off, building, or failed) the answer is the same; it is
  computed by enumerating the feature links (§5.6.3). Jena raises "Dataset Context does not
  contain SpatialIndex". Sparkles answers instead, within the query's time and row budgets.
* **Errors** (`400`, prefix `spatial:<name>: `): an object that is not a list of the
  expected shape; a non-numeric coordinate or radius; latitude outside ±90 or longitude
  outside ±180; an unknown unit; a non-integer limit; a variable argument (Phase 3:
  bound-from-left arguments).

### 2.6 Query Rewrite Extension (Phase 2)

A triple pattern whose predicate is one of the 24 topological properties (`geo:sfWithin`,
`geo:ehMeet`, `geo:rcc8po`, …) matches:

* the asserted triples (an ordinary scan), and
* the derived triples of GeoSPARQL 1.1 §13 (rules `geor:sfWithin` etc.). Subject and object
  are spatial objects `so1`, `so2`. Each resolves to geometry literals:
  * a **feature**: `so geo:hasDefaultGeometry ?g . ?g asX ?lit`;
  * a **geometry**: `so asX ?lit`;
  * a **literal** in subject or object position (a Jena extension): the literal itself.

  Here `asX` ranges over the configured serialization predicates (default `geo:asWKT`,
  `geo:asGeoJSON`, `geo:hasSerialization`). The derived triple `(so1, geo:R, so2)` holds
  when some pair of their literals satisfies `geof:R`.

The answer is the set union (a triple that is both asserted and derived appears once), as
the entailment regime defines it. A feature with several default geometries relates when
any pair does (the 1.1 text allows several, and `:f1 geo:sfDisjoint :f1` can then be
true). The four rule cases include geometry–geometry and feature–geometry pairs, so
`?x geo:sfWithin ex:region` returns both the features and the geometries inside the region,
and a feature relates to its own geometry (as in Jena; filter with `?x a geo:Feature` if
needed).

* Like the rules, rewrite uses `geo:hasDefaultGeometry`, not `geo:hasGeometry`. RDFS
  entailment does not help data with only `hasGeometry` (`hasDefaultGeometry` ⊑
  `hasGeometry`, not the reverse). Such data needs `sparkles infer --geo-default-geometry`
  (Phase 2), which materializes `hasDefaultGeometry` for features with exactly one
  geometry, like Jena's `applyDefaultGeometry`.
* An unbound predicate (`ex:a ?p ex:b`) matches asserted triples only (1.1 §13.5 leaves it
  open).
* Rewrite is on by default (Jena's default). `geo.json` `queryRewrite: false` turns it
  off per dataset, and `serve --no-geo-rewrite` for the server. *(As built, rewrite is off by default, decided by the maintainer:
  `queryRewrite: true` turns it on per dataset; see the Outcome.)*
* Evaluation strategies (§5.6.4): both ends constant, a test; one end constant, an index
  window query; both variables, a spatial self-join (budgeted). Disjoint relations
  (`sfDisjoint`, `ehDisjoint`, `rcc8dc`) cannot use the index and enumerate all pairs
  within the row budget.

### 2.7 RDFS Entailment Extension (Phase 2)

Sparkles reasons by materialization (README decision), so the extension is the GeoSPARQL
ontology plus the existing RDFS profile:

* `sparkles-reasoner` embeds the GeoSPARQL 1.1 ontology (`geo`) and the Simple Features
  vocabulary (`sf`), published under the OGC Document License (permissive; attribution kept
  in the file header and `THIRD_PARTY_LICENSES.md`).
* `sparkles infer --profile rdfs --vocab geosparql` and `POST /$/reason/{ds}`
  `{"profile":"rdfs","vocabularies":["geosparql"]}` add them to the TBox, so the
  subclass/subproperty closure (`sf:Polygon ⊑ sf:Surface ⊑ sf:Geometry ⊑ geo:Geometry`,
  `geo:asWKT ⊑ geo:hasSerialization`, `geo:hasDefaultGeometry ⊑ geo:hasGeometry`, …) lands
  in `urn:x-sparkles:inferred`. The inferred graph is queried with `reasoning=true`, as for
  every other profile. The vocabulary triples themselves are not copied into user graphs.
* Optional rule `geometryTypes` (Phase 3): materialize `?g a sf:Polygon` etc. from the type
  of `?g`'s serialization. The standard does not require it (Req 48 asks for the hierarchy,
  not for typing from literals), but it is what users expect from `?g a sf:Polygon`.

### 2.8 HTTP (`/$/` extensions, documented in `docs/API.md`)

| Method | Path | Phase | Description |
|---|---|---|---|
| GET | `/$/geo/{ds}` | 1 | `GeoStatus`. `404` unknown dataset. Index off: `{ "enabled": false }` |
| PUT | `/$/geo/{ds}` | 1 | body `GeoConfig`: enable or reconfigure, returns `{ status, task }` (task kind `geo-index`). `400` invalid config, `403` read-only |
| DELETE | `/$/geo/{ds}` | 1 | disable; removes `geo.json` and derived files. `204` |
| POST | `/$/geo/{ds}/rebuild` | 1 | rebuild the current generation's base. `Task`; `409` when one runs |
| POST | `/$/datasets` | 1 | optional `geo: true \| GeoConfig` |
| GET | `/{ds}/geo?bbox=minLon,minLat,maxLon,maxLat&graph=&predicate=&limit=&tolerance=` | 2 | GeoJSON `FeatureCollection` of indexed geometries in a CRS84 box, for the UI map: `id` = subject, `properties` = `{ subject, feature?, graph, predicate }`, geometries simplified (Douglas–Peucker, `tolerance` in degrees, default from bbox size / 1024) and capped at `limit` (default 5,000, max 50,000; `truncated: true` beyond) |
| POST | `/$/geo/convert` | 2 | `{ literals: [{ value, datatype }] }` → CRS84 GeoJSON geometries or errors per item, for the UI to draw result columns |

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
`geo: GeoStatus | null`. Prometheus: `sparkles_geo_rows{dataset,part}`,
`sparkles_geo_build_seconds`, `sparkles_geo_candidates_total` and
`sparkles_geo_refined_total` (exact tests run), `sparkles_geo_matches_total`.

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

`load`, `update`, `query`, `infer`, `compact` and `clone` maintain or copy the index
(`geo.json` is a meta file, copied by `clone` and backed up by [F05](F05-snapshot-repositories.md) like `text.json`;
derived files are never backed up). `sparkles check` validates `geo.json` and, from Phase
2, the persisted files' headers and checksums (§5.5). Built without the feature,
`geo-index` exits 2 with `built without GeoSPARQL (cargo feature "geo")`.

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

* **Map result view.** The results panel gets a "Map" tab next to Table / Graph / Plan
  when a result column holds `wktLiteral` or `geoJSONLiteral` values. Geometries are drawn
  with a popup per feature showing the row's other bindings (`TermView`). A row click in the
  table highlights the geometry. Non-CRS84 literals are converted client-side for EPSG:4326
  (axis swap) and EPSG:3857; anything else goes through `POST /$/geo/convert`, and literals
  that cannot be drawn are listed under the map.
* **Explorer.** A resource with a geometry (directly, or via a feature link) shows a small
  map card. A "Nearby" action runs `spatial:nearbyGeom` with a radius picker and lists the
  results.
* **Dataset page.** A "Spatial index" panel like the full-text panel: state badge,
  rows (base / overlay / tail), skipped counts, CRS histogram, memory against budget,
  configured predicates and feature links, last build. Buttons: Rebuild, Configure,
  Disable (tasks appear in `TaskList`).
* **Query editor.** `geo:`, `geof:`, `uom:`, `spatial:` and `spatialF:` in the prefix
  completions, and examples in `examples.ts` (point in polygon, nearby, distance ranking).
* **Library.** MapLibre GL JS 6 (BSD-3-Clause), loaded with a dynamic `import()` only when
  a map opens, so other pages do not pay for it. Leaflet 1.9 (BSD-2-Clause) is the fallback
  if the bundle size is a problem (§11 q13).
* **Basemap.** No external tile server by default: the OSM tile usage policy forbids
  offline and bulk use and requires a unique User-Agent, and an air-gapped server must
  work. The UI ships Natural Earth 1:110m land, coastline and country boundaries (public
  domain) as a small vector style, precompressed by `ui/scripts/precompress.mjs`. An
  operator may set `serve --map-style-url URL` (a MapLibre style JSON). The server's CSP
  then allows that origin for `connect-src`/`img-src`; nothing external is contacted
  otherwise. Attribution, when the style needs it, comes from the style.
* **`api.ts`**: `geoStatus`, `geoConfigure`, `geoDisable`, `geoRebuild`, `geoBox`,
  `geoConvert`. `ui/mock/server.mjs` emulates `/$/geo/*` and `/{ds}/geo`.

## 3. Standards basis

* **OGC GeoSPARQL 1.1** (OGC 22-047r1, 2024; ISO 19186-1): conformance classes (Annex A),
  vocabulary (§§7–9), literals (§10.2–10.8: Req 14–17 WKT, 25–27 GeoJSON, 20–22 GML, 30–32
  KML), functions (§10.9, Annex B), relation patterns (Tables 2, 5–8), RDFS Entailment
  (§12, Req 47–49), Query Rewrite (§13, rules `geor:*`). **GeoSPARQL 1.0** (OGC 11-052r4)
  where 1.1 changed things (for example, the WKT separator was spaces only, and
  `hasDefaultGeometry` belonged to the Geometry Extension).
* **OGC/ISO Simple Features** (OGC 06-103r4 / ISO 19125-1) and **ISO 13249-3**: WKT
  grammar, geometry types, the boundary rule, DE-9IM (Clementini, Di Felice, van Oosterom
  1993; Egenhofer & Franzosa 1991; Randell, Cui & Cohn 1992 for RCC8).
* **RFC 7946** (GeoJSON): geometry objects, CRS84, no `crs` member, antimeridian guidance.
* **OGC CRS registry** (`http://www.opengis.net/def/crs/…`), EPSG axis orders for 4326
  (latitude, longitude) and CRS84 (longitude, latitude); OGC unit IRIs
  (`http://www.opengis.net/def/uom/OGC/1.0/…`) and QUDT units, which 1.1 §10.3 recommends.
* **Karney 2013**, "Algorithms for geodesics" (geodesic distance and area on the ellipsoid);
  WGS 84 parameters `a = 6378137 m`, `f = 1/298.257223563`.
* **SPARQL 1.1 Query**: §17.6 extension functions, §17.2 error semantics, §18 BGP and join
  semantics, §11 aggregates (custom aggregate IRIs), §13 datasets and the active graph,
  §4.2.3 collections (property-function arguments); SPARQL 1.1 Entailment Regimes (RDFS),
  which 1.1 Req 47 names.
* **RDF 1.2 Concepts** §3.4.2 (ill-typed literals) and §5 (datatypes).
* **R-trees**: Guttman 1984; STR packing (Leutenegger, Lopez & Edgington 1997); Hilbert
  packing (Kamel & Faloutsos 1993); best-first k-NN (Hjaltason & Samet 1999).
* **Apache Jena GeoSPARQL** (documentation and `jena-geosparql` sources, Apache-2.0): the
  `spatial:` and `spatialF:` vocabularies, argument forms, defaults and the behaviours §11
  compares.

## 4. Semantics

### 4.1 Literals

#### 4.1.1 `geo:wktLiteral`

```
wktLiteral := ws [ "<" IRI ">" lwsp ] geometry ws      | ws          (empty literal → empty geometry, Req 17)
lwsp       := 1*( %x20 / %x09 / %x0A / %x0D )            (1.1 allows any whitespace; 1.0 spaces)
geometry   := ISO 13249-3 / OGC 06-103r4 WKT, keywords case-insensitive,
              Z / M / ZM dimension markers, "EMPTY" at any level
```

* The IRI must be absolute. It is matched against the CRS table (§4.2) after alias
  normalization, and kept verbatim for `getSRID`.
* Sparkles strips the `<IRI>` prefix itself and hands the rest to the `wkt` crate (0.14:
  case-insensitive keywords, Z/M/ZM, `EMPTY`). Unlike Jena's hand-written splitter, any
  whitespace (tabs, newlines, repeated spaces) separates ordinates.
* Accepted types: `POINT`, `LINESTRING`, `POLYGON`, `MULTIPOINT` (with or without inner
  parentheses), `MULTILINESTRING`, `MULTIPOLYGON`, `GEOMETRYCOLLECTION`. Also `LINEARRING`,
  `TRIANGLE`, `TIN` and `POLYHEDRALSURFACE`, which are mapped to polygons and multipolygons
  for computation, but `geometryType` reports the written type. `CIRCULARSTRING`,
  `COMPOUNDCURVE`, `CURVEPOLYGON` and other curved types are ill-typed (`unsupported
  geometry type`).
* Ordinates are decimal or scientific numbers (`-1.5e3`), parsed as `f64`. NaN, infinities
  and hex are ill-typed.
* A missing multi-geometry member (`MULTIPOINT((1 2),)`) and unbalanced parentheses are
  ill-typed. Nesting deeper than 32 `GEOMETRYCOLLECTION` levels is ill-typed.
* Structural validity is not checked at parse time beyond what the type needs. A
  `LINESTRING` needs ≥ 2 points (or `EMPTY`); a polygon ring needs ≥ 4 points and must be
  closed (first = last). Otherwise the literal is ill-typed. Self-intersecting polygons are
  accepted (as in Jena); overlay and relate on them give `geo`'s results, and `isSimple`
  reports them.
* A mixed Z/non-Z coordinate layout inside one geometry is ill-typed.

#### 4.1.2 `geo:geoJSONLiteral`

* An RFC 7946 Geometry object (`Point`, `LineString`, `Polygon`, `MultiPoint`,
  `MultiLineString`, `MultiPolygon`, `GeometryCollection`), parsed with `geojson` 1.0.
  `Feature` and `FeatureCollection` are ill-typed (GeoSPARQL: "GeoJSON Geometry objects").
* The empty literal `""` and `null` are empty geometries (Req 27).
* Always CRS84 (Req 26). A `crs` member (pre-RFC 7946 GeoJSON) is ill-typed rather than
  silently ignored.
* Positions with 3 elements are XYZ; with 4 or more, or with 1, the literal is ill-typed
  (GeoJSON has no M).
* Ring orientation is not checked (RFC 7946 §3.1.6 says parsers should not reject it).

#### 4.1.3 Other datatypes

`gmlLiteral` (GML 3.2 Simple Features profile 10-100r3 levels 0 and 1, plus `gml:Curve` /
`gml:Surface` with linear segments only) and `kmlLiteral` (KML 2.2 `Point`, `LineString`,
`LinearRing`, `Polygon`, `MultiGeometry`) come in Phase 3. Until then, values of those
datatypes are ill-typed geometry arguments (type error) and are not indexed. They are
reported once in `GeoStatus.skipped` as `unsupportedDatatype` (Phase 3 field).

#### 4.1.4 Empty geometries and dimensions

`""`, `POINT EMPTY`, `GEOMETRYCOLLECTION EMPTY` and so on are empty geometries. `isEmpty`
is true. They are never indexed (they occupy no space) and are counted in `skipped.empty`.
Relations follow §2.4. `distance` involving an empty geometry is a type error (there is no
closest point). Measures are 0, and constructive functions return an empty geometry
of the result type (`intersection` of disjoint polygons → `POLYGON EMPTY`, in WKT, or
GeoJSON `{"type":"GeometryCollection","geometries":[]}`). `envelope`, `centroid`,
`boundary`, `convexHull` of an empty geometry → `GEOMETRYCOLLECTION EMPTY`. `minX` etc. of
an empty geometry → type error.

#### 4.1.5 Canonical output form

Sparkles never rewrites stored literals. Literals it creates (function results) are
canonical:

* WKT: `[<IRI> ]TYPE[ Z| M| ZM](…)`. The type is uppercase, there is no space before `(`,
  `, ` separates points and one space separates ordinates. The CRS prefix is omitted for
  CRS84 (as in Jena; Oxigraph always writes it). Numbers use Rust's shortest round-trip
  `f64` formatting, with integral values written without `.0` (`POINT(2 48.8566)`), `-0`
  written as `0`, and no exponent between 1e-6 and 1e21 (exponent forms such as `1e-7`
  otherwise). Empty: `POINT EMPTY` etc.
* GeoJSON: compact JSON, members in the order `type`, `coordinates` / `geometries`, the
  same number formatting, no `bbox`, polygons in the input's ring orientation (RFC 7946
  §3.1.6 recommends right-hand rings for output; Sparkles keeps the computed orientation
  and `asGeoJSON` forces exterior rings counter-clockwise, as Jena's writer does).
* Jena rounds transformed and constructed coordinates to 6 decimal places (`PRECISION_MODEL
  1e6`). Sparkles does not, so results can differ in the 7th decimal or beyond. Value-based
  comparison of results (`bench-answers.py`) must compare geometries with a tolerance.

### 4.2 Coordinate reference systems

#### 4.2.1 Built-in table (Phase 1)

| CRS IRI(s), after normalization | Kind | Axis order of the literal | Notes |
|---|---|---|---|
| `http://www.opengis.net/def/crs/OGC/1.3/CRS84` (the default) | geographic 2D | lon, lat | |
| `http://www.opengis.net/def/crs/OGC/0/CRS84h` | geographic 3D | lon, lat, h | Z = ellipsoidal height |
| `http://www.opengis.net/def/crs/EPSG/0/4326` | geographic 2D | **lat, lon** | Req 16: axes in the CRS's order |
| `http://www.opengis.net/def/crs/EPSG/0/4979` | geographic 3D | lat, lon, h | |
| `http://www.opengis.net/def/crs/EPSG/4326` (no `/0/`; legacy GeoSPARQL 1.0 examples) | geographic 2D | lon, lat | Jena's alias of CRS84; kept for compatibility |
| `http://www.opengis.net/def/crs/EPSG/0/3857` (and `…/900913`) | projected (Web Mercator, spherical) | x, y (metres) | closed-form forward/inverse |
| `http://www.opengis.net/def/crs/EPSG/0/326NN`, `…/327NN` (UTM zones on WGS 84) | projected | E, N (metres) | Phase 2: transverse Mercator with Krüger's 6th-order series (Karney 2011) |

Alias normalization: `https://www.opengis.net/…` → `http://…`;
`urn:ogc:def:crs:OGC:1.3:CRS84`, `urn:ogc:def:crs:EPSG::4326` and the like → the
`http://www.opengis.net/def/crs/…` form; `CRS:84` and `EPSG:4326` short forms. Aliases
change only CRS lookup, never the stored term.

#### 4.2.2 Axis order and internal coordinates

Internally every geometry is held as `(x, y)` = (east, north): (lon, lat) for geographic
CRSs, (E, N) for projected ones. EPSG:4326/4979 literals are swapped on parse and swapped
back when a result in that CRS is written. All computation, the index and GeoJSON use the
internal order. `minX`/`maxX`/`minY`/`maxY` report the literal's own axes (§2.3). The
documentation calls this out, since axis order is the most common GeoSPARQL mistake.

#### 4.2.3 Mixed CRSs and unknown CRSs

* A binary function with arguments in different built-in CRSs transforms `g2` into
  `g1`'s CRS (GeoSPARQL: calculations in the SRS of `geom1`). Transforms between built-in
  geographic CRSs are axis swaps (WGS 84 throughout). Geographic ↔ projected uses the
  projection formulas.
* A literal with a CRS IRI that is not in the table is still a valid geometry:
  `getSRID`, `geometryType`, `dimension`, `isEmpty`, `asWKT`, `numGeometries`/`geometryN`,
  `min*`/`max*`, `envelope`, `convexHull`, `boundary`, `centroid` and planar relations and
  overlay between two geometries of the same unknown CRS work in its native
  coordinates. Metric functions (`metricDistance`, `metricArea`, …), unit conversions, and
  any mix with another CRS are type errors. Such literals are not indexed
  (`skipped.unknownCrs`, with the IRIs counted in `GeoStatus.crs`). Jena instead logs a
  warning and treats the coordinates as CRS84 degrees, which silently gives wrong answers
  (§10).

#### 4.2.4 Additional CRSs (Phase 3)

An optional feature `geo-proj4` uses `proj4rs` (MIT/Apache-2.0, pure Rust, a proj4js
port: tmerc, lcc, laea, aea, stere, merc, …). CRS definitions come from an operator-supplied
file `crs.json` (`{ "<CRS IRI>": { "proj4": "+proj=…", "axis": "en" | "ne" } }`), so
Sparkles ships no EPSG dataset. The EPSG terms of use (no distribution for profit, notice to
recipients) are a licensing question for the maintainer (§11 q16), and
`crs-definitions` (CC0, but EPSG-derived) is therefore not bundled by default. `proj`
(MIT bindings to PROJ 9, MIT) stays rejected for the default build (§10) and could be a
further opt-in feature.

### 4.3 Units

Accepted unit IRIs (as an IRI, or an `xsd:anyURI` literal; `spatialF:` also accepts a
string):

| Kind | OGC (`uom:` prefix) | QUDT (`http://qudt.org/vocab/unit/`) | EPSG URNs (`urn:ogc:def:uom:EPSG::`) |
|---|---|---|---|
| length | `metre`/`meter` (1), `kilometre`/`kilometer` (1000), `centimetre`/`centimeter`, `millimetre`/`millimeter`, `mile`/`statuteMile` (1609.344), `nauticalMile` (1852), `yard` (0.9144), `foot` (0.3048), `inch` (0.0254), `surveyFootUS` (1200/3937) | `M`, `KiloM`, `CentiM`, `MilliM`, `MI`, `MI_N`, `YD`, `FT`, `IN`, `FT_US` | 9001, 9036, 1033, 1025, 9093, 9030, 9096, 9002, 9003 |
| angle | `radian`, `microRadian`, `degree`, `minute`, `second`, `grad` | `RAD`, `MicroRAD`, `DEG`, `ARCMIN`, `ARCSEC`, `GON` | 9101, 9109, 9102, 9103, 9104, 9105 |
| area | `squareMetre`/`square_metre`/`square_meter`, `squareKilometre`/`square_kilometre`, `hectare`, `acre` (OGC-namespace forms as Oxigraph accepts them) | `M2`, `KiloM2`, `HA`, `AC`, `ARE`, `MI2`, `FT2`, `YD2` | — |

* An unknown unit IRI is a type error. Jena throws an `UnitsURIException`, which becomes an
  expression error too.
* **Linear units on a geographic CRS** are supported everywhere: distance and length are
  geodesic metres converted to the unit; buffer uses §4.4.3. (Jena rejects metric
  buffers on geographic data and requires degrees.)
* **Angular units on a geographic CRS**: `distance` returns the great-circle central angle
  (haversine on the geodetic coordinates) between the closest points; `buffer` buffers in
  degrees planarly (Jena's behaviour).
* **Angular units on a projected CRS**: type error.
* Area functions accept area units only; a length unit there is a type error.

### 4.4 Computation model

#### 4.4.1 Engine, precision, robustness

* Geometry algorithms come from `geo` 0.33 (MIT/Apache-2.0): `Relate` (full DE-9IM),
  `indexed::PreparedGeometry` for repeated relates against one geometry, `BooleanOps` and
  `unary_union` (over `i_overlay`), `Buffer` (since 0.31), `ConvexHull`, `ConcaveHull`,
  `Centroid`, `BoundingRect`, `Simplify`, `GeodesicArea`, the metric-space `Distance` /
  `Length` API with `Euclidean` and `Geodesic` (Karney, via `geographiclib-rs`, MIT).
* Coordinates are `f64`. Predicates use `robust`'s adaptive-precision orientation tests (as
  `geo` does), so relate results are exact for the input coordinates. Overlay and buffer
  output coordinates are computed in floating point (no snapping to a precision grid).
  Results can differ from JTS/GEOS in the last bits and in degenerate configurations. The
  acceptance tests (§7) compare constructed geometries with `sfEquals` or with a
  coordinate tolerance, never textually.
* Relations on geographic CRSs are planar in (lon, lat). That is the standard's model and
  what every surveyed engine does. Geometries crossing the antimeridian must be written with
  longitudes beyond ±180 or split into a `MULTI*`, as RFC 7946 §3.1.9 recommends. Sparkles
  does not unwrap them.

#### 4.4.2 Distance

* **Projected or same unknown CRS**: Euclidean distance between the closest points
  (`geo` `Euclidean`), in CRS units, converted to the requested unit.
* **Geographic CRS, `distance: "geodesic"` (default)**:
  * point–point: the WGS 84 geodesic (Karney; `geo::Geodesic`);
  * otherwise: 0 if the geometries intersect. Otherwise both geometries are mapped to a
    local azimuthal equidistant projection centred on the midpoint of the gap between
    their envelopes (spherical AEQD, about 60 lines of our own code), the closest points are
    found in that plane (`geo` `ClosestPoint`), mapped back, and measured with the geodesic.
    The reported distance is always a true geodesic length between two points of the
    geometries, so it is never below the true distance. The target, verified by tests
    against GeographicLib on random pairs, is an overestimate under 0.1% for gaps up to
    1,000 km and under 0.5% beyond.
* **Geographic CRS, `distance: "haversine"`** (Jena compatibility): Jena's model. The
  closest pair is found planarly in degrees (with antimeridian adjustment), and the
  great-circle distance is computed with the haversine formula on a sphere of radius
  6,371,008.7714 m.
* The two models differ by up to 0.56% (1° of longitude on the equator: 111,319.491 m
  geodesic, 111,195.080 m haversine). Open question 2 records the default.
* **Lower bound for index pruning**: the haversine distance on a sphere of radius
  `a(1 − e²)` = 6,335,439 m (the smallest meridional radius of curvature) never exceeds the
  WGS 84 geodesic distance between the same geodetic coordinates. Both principal radii of
  curvature are at least that radius everywhere, so every curve is at least as long on the
  ellipsoid. The index uses this bound for radius windows and k-NN ordering (§5.7), so
  geodesic answers are exact.

#### 4.4.3 Buffer

* Projected CRS, or angular unit on a geographic CRS: planar `geo` `Buffer` in CRS units,
  round joins and caps, 8 segments per quarter circle (JTS's and Jena's default). Negative
  radii shrink areal geometries (empty when they vanish) and are a type error for points
  and lines.
* Linear unit on a geographic CRS (`metricBuffer`, `buffer(…, uom:metre)`):
  1. project into a spherical AEQD centred on the geometry's envelope centre;
  2. buffer planarly in metres;
  3. project back.
  The target, verified by tests against GeographicLib's direct solution, is a boundary
  within 0.5% of the radius from the true geodesic offset for geometries whose extent plus
  radius is under 1,000 km. Larger inputs are a type error with the message
  `buffer: geometry too large for a metric buffer (extent + radius > 1000 km)`, rather
  than a silently wrong shape. A buffer that would cover a pole is a type error.

#### 4.4.4 Area, length, perimeter, centroid

* `area` / `metricArea`: geographic → `GeodesicArea::geodesic_area_unsigned` on the
  ellipsoid (holes subtracted, multipolygon members summed; overlapping members are
  `unary_union`ed first, as QLever does). Projected → planar `Area`. Non-areal → 0.
* `length`: curves → geodesic (or planar) length; areal geometries → length of all rings;
  collections → sum of members; points → 0. QLever uses only the exterior ring for
  polygons (open question 11).
* `perimeter`: areal → length of all rings; others → 0.
* `centroid`: planar in the CRS of `g`, as in Jena and the standard's "calculations in the
  SRS". For geographic data spanning more than a few degrees this is not the geodesic
  centroid. `aggCentroid` (Phase 2) behaves the same way.

#### 4.4.5 Overlay

`intersection`, `union`, `difference` and `symDifference` use `geo` `BooleanOps` for
areal ∘ areal. Other dimension pairs:

* point ∘ anything: computed by point location (`CoordinatePosition`);
* line ∘ area: `BooleanOps::clip` (inside or outside);
* line ∘ line: intersection points/segments via `line_intersection` over a segment R-tree,
  union by noding both lines;
* collections: member-wise, then `unary_union`.

The result dimension follows OGC (`intersection` keeps the lowest-dimension parts that
exist). Results that `geo` cannot produce for a pair are a type error
(`intersection: unsupported for these geometry types`). The tests (§7) list which pairs
are covered.

#### 4.4.6 Hulls and simplicity (Phase 2)

* `concaveHull(g[, targetPercent])`: `geo` `ConcaveHull` (concaveman). The optional
  second argument, a Sparkles-documented parameter as GeoSPARQL requires, maps
  `targetPercent ∈ (0, 100]` to the concavity. The default is concavity 2.0 (`geo`'s
  default).
* `isSimple`: points always; multipoints when no two points are equal; curves when no two
  segments intersect except consecutive ones at their shared vertex (and the closing vertex
  of a ring); areal geometries when `Validation` reports no ring self-intersection.
  Collections: all members simple. The check is our own sweep over a segment R-tree,
  since `geo`'s `Validation` checks validity, not OGC simplicity.

### 4.5 Errors

Function errors are SPARQL type errors (the expression is in error). Planner-level errors
are `400` with `Error::Invalid`:

| Condition | Status | Message (prefix) |
|---|---|---|
| malformed geometry constant in a query (literal typed `geo:wktLiteral` that does not parse) | 400 | `geo: malformed wktLiteral at offset N: …`. A constant that can never evaluate is reported, not silently false. Ill-typed *data* never errors |
| `spatial:` argument errors | 400 | §2.5 |
| `spatial:` / rewrite with a variable argument (Phase 1) | 501 | `spatial:nearby: variable arguments are not supported yet` (`Error::Unsupported`) |
| over budget (vertices, overlay, index memory) | 507 | §4.7 |
| built without feature `geo`: `geof:` functions | — | unknown extension function: type error, as for any unknown IRI today, plus one warning per query in the plan (`geof:* needs cargo feature "geo"`) |
| built without feature `geo`: `spatial:` property function | 501 | `built without GeoSPARQL (cargo feature "geo")` |
| index admin while disabled | 400 | `spatial index is not enabled` |
| rebuild already running | 409 | `spatial index build already running` |

### 4.6 Index semantics and consistency

* **What is indexed.** A **row** is a visible quad `(s, p, o, g)` where:
  * `p` is a configured serialization predicate;
  * `g` is in the graph scope (by default every graph, including `urn:x-sparkles:inferred`,
    so `reasoning=false` excludes it through the graph filter as for text);
  * `o` is a well-typed, non-empty geometry literal of a supported datatype in a built-in
    CRS, within `maxGeometryBytes` and `maxVertices`.

  Each row carries the envelope of `o` in CRS84 (internal lon/lat), rounded outward to
  `f32`. W3C Basic Geo rows (Phase 2) are `(s, wgs84:lat, ·, g)` with a point built from the
  subject's `lat`/`long` pair in the same graph (a cross-product when there are several,
  as in Jena).
* **MVCC.** A spatial operator on snapshot S sees exactly S's rows: the generation base
  minus `S.delta.del`, plus the overlay rows present in `S.delta.ins` (§5.3). There is no
  staleness window and no `503`. Historical snapshots (`?at=`, [F06](F06-snapshots-and-point-in-time.md)) get the same guarantee:
  their generation's base is built on demand within the budget.
* **Exactness.** The index only produces candidates. Every answer is refined with the exact
  predicate from §2.4/§4.4, so results with and without the index are identical. The
  acceptance tests check this property on random data (§7, A20).
* **Graph scope** follows [F03](F03-full-text-search.md)/[F04](F04-vector-search.md): the active graph becomes a filter on `g` before any
  top-`k`. Under a merged default graph without a graph variable, rows with equal
  `(s, o)` are one solution.

### 4.7 Limits and budgets

| Setting | Default | Where | On excess |
|---|---|---|---|
| `maxGeometryBytes` | 16 MiB | `geo.json` | not indexed (`skipped.tooLarge`); functions still evaluate it if `maxVertices` allows |
| `maxVertices` (per geometry) | 1,000,000 | `geo.json` | not indexed; functions: type error `geometry too complex (N vertices)` |
| `maxOpVertices` (sum of input vertices of one overlay, buffer, hull or relate) | 2,000,000 | `StoreOptions` / `--geo-op-vertices` | type error `geometry operation too large` |
| buffer output | ≤ 8 segments per quarter circle × input vertices | constant | |
| WKT/GeoJSON nesting depth | 32 | constant | ill-typed |
| geometry column + trees memory (`geo_budget_bytes`) | 4 GiB | `StoreOptions`, `--geo-mb` | build refused, state `over-budget`, queries use the non-index plans |
| per-query geometry memo | 64 MiB, LRU | `Ctx` | evicts |
| `limit` of `spatial:` | unbounded (row budget) | | `ctx.check_rows` → `507` |
| spatial join candidates (Phase 2) | `max_rows` | `Ctx` | `507` with `spatial join produced more than N candidate pairs` |

* `geo` calls are not interruptible. Every operator calls `ctx.check()` every 256 exact
  tests and every 4,096 candidate rows, and `maxOpVertices` bounds the cost of a single call
  (relate on two polygons is O((n + m) log(n + m)) with `PreparedGeometry`; overlay and
  buffer similar with larger constants). A query of 1,000 relates against a 1M-vertex
  constant therefore stays responsive to timeouts.
* Constructed literals (buffers, unions) go to the query-local vocabulary and are charged
  to `ctx`'s memory budget by their byte size, so `SELECT (geof:buffer(?w, …) AS ?b)` over
  a large table fails with `507`, not an OOM.
* Index builds run on the rayon pool with `ctx`-like cancellation at store close, and
  check the budget before allocating (F04's admission model).

## 5. Design

### 5.1 Modules and features

| Where | Change |
|---|---|
| `crates/sparkles/Cargo.toml` | optional `geo = "0.33"` (default features off; `earcutr`/`spade` not needed), `wkt = "0.14"`, `geojson = "1"`, `geographiclib-rs = "0.2"` (already a `geo` dependency), `geo-index = "0.4"`; `[features] geo = ["dep:geo", "dep:wkt", "dep:geojson", "dep:geo-index"]` |
| `crates/sparkles-server/Cargo.toml` | `geo = ["sparkles/geo"]`, added to `default` |
| `sparkles/src/geo/mod.rs` (always compiled) | IRIs, `GeoConfig` (serde), `GeoStatus`, the `not_built()` error, CRS and unit tables (pure data, so the planner can recognize terms without the feature) |
| `geo/geom.rs` (`cfg(feature="geo")`) | `Geom`, parsing (WKT front end + `wkt`, GeoJSON), writing (§4.1.5), axis handling |
| `geo/crs.rs` | CRS table, alias normalization, transforms (axis swap, Web Mercator, AEQD, Phase 2 UTM) |
| `geo/ops.rs` | functions of §2.3–2.4 over `Geom` (DE-9IM table, distance models, buffer, measures, overlay dispatch) |
| `geo/column.rs` | the geometry column: id → parsed entry, per generation (§5.2) |
| `geo/index.rs` | `GeoBase` (packed R-tree over base rows), `Overlay`, `GeoView`, build, status |
| `geo/search.rs` | window, radius and k-NN search over base + overlay with refinement; join kernels (Phase 2) |
| `sparql/geopf.rs` (always compiled) | recognition of `spatial:*` property functions and (Phase 2) topological triple patterns, decoding into calls (reuses `textpf::take_calls`) |
| `sparql/plan.rs` | `Kind::SpatialScan`, `Kind::SpatialPf`, Phase 2 `Kind::SpatialJoin`, `Kind::SpatialRelate`, `Kind::SpatialKnn`; detection (§5.6); `Optimizations::spatial_pushdown` |
| `sparql/exec.rs` | dispatch to `geo::search`; without the feature: `Error::Unsupported` |
| `sparql/expr.rs` | `geof:` and `spatialF:` in `is_extension` / `extension()` through `geo::ops`, with id-aware geometry arguments (§5.8) |
| `sparql/exec.rs` aggregates (Phase 2) | `AggregateFunction::Custom` for the six `geof:agg*` IRIs (today it returns unbound) |
| `sparql/cache.rs` | spatial kinds are cacheable; key adds the spec and the `GeoView`'s `(generation uid, epoch)` (the snapshot version already pins the overlay) |
| `store.rs` | `Generation.geo: geo::GenerationGeo`; `Snapshot.geo: Option<Arc<GeoView>>`; `Store.geo: ArcSwapOption<GeoIndex>`; hooks in `open`, `publish_log` (`maintain_geo`), `rebuild_locked` (§5.4–5.5) |
| `check.rs`, `clone`, F05 meta files | `geo.json` |
| `sparkles-reasoner` (Phase 2) | embedded `geo` + `sf` ontologies, `--vocab geosparql`, `--geo-default-geometry` |
| server `http.rs`, `main.rs`, `state.rs`, `obs.rs` | §2.8–2.9 routes, CLI, metrics |
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

* For **base literals** (`Tag::Vocab`), it is built with the base index (§5.3) from the
  distinct object ids of the indexed predicates, parsed in parallel blocks
  (`Vocab::get_sorted` key batches, rayon). Phase 1 keeps the parsed `Geom` in memory, so
  refinement never re-parses text. Phase 2 persists the column (§5.5) as WKB-like records
  that are decoded on demand (no text parsing on restart).
* **Delta literals** (`Tag::Delta`) are parsed by the commit hook (§5.4) and inserted into
  the same generation's column (a `FxHashMap` behind a `RwLock`; delta ids are stable within
  a generation). They are never removed before the generation is dropped. Memory is bounded
  by the budget, and an over-budget overlay asks for compaction in the status message.
* **Other literals** (constants, computed values, non-indexed predicates, local vocab) go
  through the per-query memo `Ctx.geo_memo: FxHashMap<Id, Arc<Geom>>` (64 MiB LRU), so BIND
  over many rows parses each distinct literal once.

Memory: a point entry is about 120 bytes (entry + `Geom`). A polygon is about
`16 · vertices + 160` bytes. 1M points come to ≈ 120 MB, and 1M 50-vertex polygons to
≈ 960 MB. Phase 2's persisted column moves this to mmapped pages (`16 · vertices + 48`
bytes per entry on disk).

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

* The base is built from `Perm::Pso` with prefix `[p]` for each indexed predicate, over
  the generation's base only (as `vector::GenerationVectors::predicate` does), keeping rows
  whose object parses (§4.6). Boxes come from the column. The tree is a static packed
  R-tree (Flatbush layout via `geo-index`): build O(n log n), 16-entry nodes, about
  `n · 1.07 · (16 + 4)` bytes plus `rows`.
* **Overlay** rows come only from transactions (§5.4). A row in `overlay` or `tail` is
  valid for snapshot S iff its quad is in `S.delta.ins` (O(log n) `imbl` lookup on the PSO
  set). A base row is valid iff its quad is not in `S.delta.del`. `store::apply` keeps
  `ins` disjoint from base and `del ⊆ base`, so
  `rows(S) = (base − del) ∪ (overlay ∪ tail) ∩ ins` exactly.
* Searches query `base.tree`, `overlay.tree` and scan `tail` linearly. Tails stay short
  (§5.4), so the scan is cheap.

### 5.4 Commit path (`WriteTxn::publish_log` → `Store::maintain_geo`)

After the WAL fsync and before the snapshot is published (where `maintain_text` runs):

1. `touched` = logged quads whose predicate is indexed and whose graph is in scope (a
   per-generation `FxHashMap<Id, bool>` predicate cache makes non-geo commits cost one
   lookup per logged quad).
2. For each inserted quad, look up or parse its object in the column (delta literals are
   parsed here, once). Valid rows are pushed to a clone of the previous view's `tail`
   (persistent vector, O(1) amortized, structurally shared with older snapshots). Deletes
   need no work, since validity is checked against the snapshot.
3. If `tail.len() > max(4096, overlay.rows / 8)`, rebuild the overlay tree from
   `overlay.rows ∪ tail`, dropping rows not in the new snapshot's `delta.ins`, and start an
   empty tail. This is amortized O(log n) per insert and bounded by the overlay size, which
   compaction resets.
4. Publish with `snap.geo = Some(new view)`.

Parse failures never fail a write: an ill-typed literal is counted and skipped. A panic or
error in the hook marks the index `failed` (logged). The snapshot then carries
`geo: None`, and queries use the non-index plans (correct, slower) until `rebuild`, which
re-derives everything from RDF. Commit latency target: no measurable change for commits
without geometry quads, and under 1 ms extra for a commit inserting 1,000 points (§9).

The writer mutex serializes commits, so the column and the overlay need no further
locking beyond the column's `RwLock` (readers only read entries that existed when their
snapshot was published).

### 5.5 Generation switches, open, rebuild, persistence

* **Compaction and bulk commits** (`rebuild_locked`): after the new generation is built,
  and while the writer lock is still held, build its `GeoBase` and column (in parallel,
  from the new generation's PSO) before the snapshot is published, with an empty overlay.
  Compaction time grows by the build time (§9 measures it). Phase 2 reuses the previous
  generation's parsed geometries by literal key (`vocab` keys are identical across
  generations), so a compaction re-parses only new literals.
* **Open**: with `geo.json` present, the store opens with the base unbuilt. A background
  thread builds it. Until the `OnceCell` is filled, the planner treats the index as not
  ready and uses non-index plans (correct answers; explain says `spatial index building
  (37%)`). The overlay is reconstructed from the WAL replay's commits (the replayed log
  passes through `maintain_geo` like a live commit).
* **Rebuild / reconfigure**: build a new base for the current generation in the
  background, then swap it in by republishing the current snapshot with a new `GeoView`
  (`epoch + 1`). In Phase 1 rebuild holds the writer lock (as F03 Phase 1 does). The
  overlay is rebuilt from the delta under the new configuration (a scan of `delta.ins` for
  the indexed predicates).
* **Persistence (Phase 2)**: `gen-NNNN/geo/` with `column.spkg` and `rtree.spkg`, written
  by the build with `*.tmp` → fsync → rename → `sync_dir`, the F04 segment pattern. Header
  (64 bytes): magic `SPKGEO\0\x01`, `u32 format_version = 1`, `u32 kind`, `u64 rows`,
  `u64 config_hash` (FNV-1a of canonical `geo.json`), `u64 base_seq`, `u64 meta.quads`,
  reserved. Footer: `u64 rows`, `u64 xxh/fnv(header ‖ index section)`, magic. Validity
  on open needs a matching magic, version, `config_hash`, `base_seq`, row count and footer.
  Anything else → delete and rebuild. The files are mmapped (`memmap2`). `geo-index` trees
  are usable zero-copy from bytes. A read-only server never writes them (in-memory builds
  only). Removing the generation directory removes them. `sparkles check` verifies headers
  and footers (`--checksums` also the data sections).
* **In-memory stores** build in memory only.
* **Config**: `<root>/geo.json` (`GeoConfig` + `"formatVersion": 1`, `write_atomic`).

### 5.6 Planner

#### 5.6.1 FILTER pushdown with a constant geometry (Phase 1)

`scan_options(t)` gets the group's filters. For a triple `?x <p> ?w` where `p` is an
indexed predicate, `?w` is a variable, the index is ready for the snapshot's generation and
`ctx.opt.spatial_pushdown` is set, each filter conjunct over `?w` of one of these shapes
yields a **`SpatialScan`** option:

| Conjunct (either argument order where symmetric) | Window | Exact test |
|---|---|---|
| `geof:sfIntersects` / `sfWithin` / `sfContains` / `sfOverlaps` / `sfCrosses` / `sfTouches` / `sfEquals`, `eh*` except `ehDisjoint`, `rcc8*` except `rcc8dc` (`?w`, C) | envelope of C | the relation |
| `geof:relate(?w, C, "pattern")` where the pattern requires a non-empty intersection (one of II, IB, BI, BB is `T`/`0`/`1`/`2`) | envelope of C | relate |
| `geof:distance(?w, C, u) < r`, `<= r`, `r > …`, `r >= …`; `geof:metricDistance`; `spatialF:nearby(?w, C, r, u)` / `withinCircle` | envelope of C expanded by `r` (degrees from §4.4.2's lower-bound sphere, latitude-aware, split at the antimeridian, full longitude range near poles) | the comparison |
| `geof:sfIntersects(?w, geof:buffer(C, r, u))` and other relations whose constant argument is a constant expression | the constant is folded at plan time | the relation |

`C` is a constant geometry literal (or a constant-folded expression). Several conjuncts
intersect their windows. The pushed conjuncts move into the `SpatialSpec` and are
evaluated by the operator against the column (with `PreparedGeometry` built once for `C`).
Other conjuncts stay ordinary filters.

Cost model (the DP chooses between the plain scan and the spatial one by cost, as
`push_range` does for numeric ranges):

* `est_window` = count of rows whose box intersects the window, computed from the tree's
  upper levels: walk down to the level with ≥ 256 nodes and sum the subtree sizes of nodes
  intersecting the window. This is an upper bound within one node per boundary, and takes
  microseconds. Add the overlay rows likewise and the tail length.
* `est` = `est_window` × selectivity of the exact test (`sfWithin`/`sfContains` of points
  in a polygon: area(C) / area(box(C)), capped at 1; otherwise 0.5).
* `cost` = `est_window` × (1 + `REFINE_COST(kind, vertices(C))`) + 4 × tree levels, where
  `REFINE_COST` is 0.5 for a point-in-prepared-polygon test and grows with
  `log2(vertices)` for polygon–polygon relates. The plain alternative costs
  `rows(p) × (1 + REFINE_COST)`, because the filter then evaluates per row.

The `Node` description reads
`SpatialScan ?x ?w ← <asWKT> sfWithin POLYGON(5 pts) [window ≈ 1,240 of 1.0M rows; base+overlay]`.

#### 5.6.2 `spatial:` property functions (Phase 1)

`collect()` takes `spatial:*` triples and their argument lists out of the BGP
(`textpf::take_calls`, as for `spk:vectorSearch`) and pushes a `SpatialPf` leaf with
`SpatialPfSpec { func, query: Geom (or lat/lon converted with EPSG:4326), radius_m, limit,
subject: PathEnd, graph: GraphFilter, graph_var, dedup }`. Estimate: `min(limit,
est_window × FEATURE_FANOUT)`, with `FEATURE_FANOUT` from the predicate statistics of the
feature links (distinct subjects per object).

#### 5.6.3 Execution of `SpatialPf` and fallback

1. Window search over base + overlay (or, when the index is not ready, a scan of each
   indexed predicate with the window test on the column, or on parsed literals when the
   column is not built either).
2. Exact refinement per §2.5.
3. Map geometry subjects to features: one `POS` lookup per distinct geometry subject per
   feature-link predicate (`[link, geomSubject]` prefix), within the graph filter, with the
   same-graph rule under `GRAPH ?g`.
4. Deduplicate features. With `limit`, rank by exact distance to the query geometry (box
   and cardinal functions: to its envelope centre) and keep the first `limit`, ties by
   subject id.

For `nearby` with a `limit` and a large radius, step 1 runs as a best-first k-NN traversal
(§5.7) that stops once `limit` distinct features are proven, so `spatial:nearby (lat lon
20000 uom:kilometre 10)` does not refine the whole dataset.

#### 5.6.4 Phase 2 operators

* **Spatial join.** In `plan_group`, a conjunct `geof:R(?a, ?b)` (non-disjoint R),
  `geof:relate(?a, ?b, p)` with an intersection-requiring pattern, or
  `geof:distance(?a, ?b, u) <(=) r` with constant `r`, where `?a` and `?b` are bound by
  different join components, becomes a join edge: `join_order` treats the two components as
  connected through a `SpatialJoin` node instead of a cross product plus filter. Each child
  is either an indexed scan (the persistent index is probed directly; index nested-loop
  join) or any subplan (its distinct geometry ids are boxed from the column or parsed, and
  a packed tree is built over the smaller side). The algorithm is synchronous traversal of
  the two trees (or probe of each outer box), then exact refinement with the relation, a
  `PreparedGeometry` per outer geometry when its candidates exceed 8. Output rows are the
  row-id pairs joined back to both children's tables. Cost:
  `(n + m) · log(min(n, m)) + pairs × REFINE_COST`, and
  `est = n · m · overlap_ratio`, where the overlap ratio is estimated from a 1,024-row
  sample of each side (the two-level box count of §5.6.1 applied to the sample).
* **Query Rewrite** (§2.6): `collect()` turns a topological-property triple into a
  `SpatialRelate` item that expands into `Union(Scan(asserted), Derived)` with set
  semantics (`Distinct` over the two branches' `(so1, so2)`). `Derived` resolves
  `so → literal` with the four cases as a small UNION of scans (feature via
  `hasDefaultGeometry` then `asX`, geometry via `asX`, or a constant literal), then uses the
  spatial join (both sides variable), a `SpatialScan` (one side constant) or a single test
  (both constant). Disjoint relations use a nested-loop join with the row budget.
* **k-NN.** `ORDER BY ASC(geof:distance(?w, C, u)) LIMIT k` (or `metricDistance`, or a
  variable bound by `BIND` to one of them) over a group whose only producer of `?w` is an
  indexed scan, and whose other leaves join on that scan's subject (stars), becomes
  `SpatialKnn`. It is a best-first traversal of
  base/overlay trees by the lower bound of §4.4.2, emitting candidates in increasing lower
  bound in batches of `2k`. Each batch is joined with the rest of the group, and the
  traversal stops once `k` results whose exact distance is ≤ the next lower bound exist.
  SPARQL sorts an expression error (unbound) first in ascending order, so the operator
  first emits the scan's rows whose distance is an error: ill-typed, empty, unknown-CRS
  and unsupported literals. The column records these as `skipped` row lists (base and
  overlay) for exactly this purpose. A `FILTER(BOUND(?d))` or a distance bound in the group
  removes them, and the documented query shape includes one. Other shapes keep the generic
  sort.

### 5.7 Search kernels (`geo/search.rs`)

* **Window**: `tree.search(box)` on base and overlay, tail scan; validity against the
  snapshot (§5.3); the graph filter; dedup per `(s, o)` under merged default graphs; exact
  test on the column entry (box test first, then the relation). It runs in parallel over
  chunks of 4,096 candidates with `ctx.check()` between chunks, as the vector search does.
* **Radius**: window = box expanded by `r` on the lower-bound sphere; the exact test is the
  configured distance model.
* **k-NN**: a priority queue over tree nodes keyed by the lower-bound distance from the
  query to the node box (`geo-index` `neighbors_with_callbacks`, or our own traversal over
  its node layout if its callback API cannot take a custom metric). Leaf entries are
  refined in order. Ties by `(s, o, g)` raw id.
* **Antimeridian**: windows that cross ±180° are split into two boxes. Data rows are
  indexed as written (§4.4.1).

### 5.8 Functions in `expr.rs`

* `is_extension` accepts `geof:` and `spatialF:` when the feature is on.
* `extension()` dispatches to `geo::ops`. Geometry arguments go through
  `geom_arg(args, i, row, ctx)`:
  * when the argument expression is a variable, take its id: a base or delta id → the
    generation column (no parsing when present); otherwise `ctx.geo_memo`;
  * otherwise evaluate to a `Value::Other { lex, dt }` and parse through `ctx.geo_memo`,
    keyed by a 64-bit hash of `(dt, lex)`.
* Numeric results are `xsd:double` (inline when the low mantissa bits allow, else the local
  vocabulary). Booleans are inline. Geometries are local-vocabulary literals.
* Aggregates (Phase 2): `AggregateFunction::Custom(iri)` for `geof:aggBoundingBox`,
  `aggBoundingCircle`, `aggCentroid` (centroid of the union of inputs, planar),
  `aggConvexHull`, `aggUnion` (`unary_union`, with `maxOpVertices`), `aggConcaveHull`.
  They take one geometry expression. `DISTINCT` is honoured, and ill-typed inputs make the
  aggregate an error, as for the built-in numeric aggregates. Results are in the CRS of the
  first input, and inputs in other CRSs are transformed.
* Constant folding: calls whose arguments are all constants are evaluated once at plan
  time (needed for §5.6.1 windows; the existing `Expr` constant path).

### 5.9 Explain and profile

Plan nodes (`describe`):

* `SpatialScan` (Phase 1), as in §5.6.1;
* `SpatialPf ?f ← spatial:nearby POINT(-0.12 51.5) r=5 km limit 10 [features via hasDefaultGeometry|hasGeometry]`;
* `SpatialJoin ?a sfContains ?b [index nested loop on <asWKT>]` (Phase 2);
* `SpatialKnn ?w k=10 metricDistance POINT(…)` (Phase 2).

Profiles (`x-sparkles+json` and the UI's Plan tab) add per-operator counters:
`candidates`, `refined`, `matched`, `treeNodesVisited`, `index: ready | building (p%) |
off | failed`, and `fallback: true` when a non-index path ran. When a query has a spatial
filter that was not pushed down (wrong shape, variable radius, index off), explain adds a
warning naming the reason (the MCP server's explain-with-warnings surfaces it too).

## 6. Phasing

Estimates are in implementation days, on the same scale as F03/F04.

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

* Spatial joins, distance-within joins, k-NN rewrite (3 days).
* Query Rewrite Extension, `--geo-default-geometry`, RDFS Entailment Extension (ontology
  bundle, `--vocab geosparql`) (1.5 days).
* Aggregates (custom aggregate support in `exec.rs`), `boundingCircle`, `concaveHull`,
  `isSimple`, `spatialF:` functions, `spatial:equals`, UTM zones, W3C Basic Geo rows
  (1.5 days).
* Persisted column and trees, reuse of parsed geometries across compaction, `check`
  coverage (1 day).
* UI: map tab, explorer card and Nearby, dataset panel, `/{ds}/geo`, `/$/geo/convert`, mock
  endpoints (1.5 days).
* Oxigraph test-suite import, Jena-derived test cases, benchmark scripts (§8, §9);
  `docs/BENCHMARKS.md` section (0.5 day plus the run).

### Phase 3 (later, on demand)

* GML and KML literals and `asGML`/`asKML`; the GML entailment hierarchy (Req 49).
* `geo-proj4` feature with an operator CRS registry (§4.2.4).
* Variable and bound-from-left arguments for `spatial:` (like F04's `candidates:join`).
* k-NN with arbitrary joins (batch-and-verify generalized), spatial join with S2-cell or
  grid prefilters (QLever's cell-grid idea, ideas only), simplified inner/outer polygon
  approximations for refinement (Bast et al. 2025).
* `geometryTypes` materialization rule, `geo:hasMetricArea` etc. computed by rewrite.
* A Sparkles answer to QLever's `SERVICE spatialSearch:` syntax, if users ask (§11 q14).
* Running the GeoSPARQL Compliance Benchmark in CI, if the maintainer accepts the GPL-2.0
  fetch (§8).

## 7. Acceptance examples

Fixture (`ds`, spatial index enabled with defaults). Prefixes `ex:`, `geo:`, `geof:`,
`uom:`, `spatial:`, `spatialF:`, `sf:`. CRS84 unless stated.

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
implementation. Where they disagree with those rules, the rules win and the table gets
fixed. Approximate distances ("≈") are asserted against GeographicLib reference values,
not against these roundings.

## 8. Conformance testing

* **Sparkles' own tests** (§7) in `crates/sparkles/src/sparql/tests.rs` and a new
  `geo/tests.rs` (parser edge cases, CRS/axis handling, DE-9IM table, distance models
  against reference values, budgets).
* **Oxigraph's GeoSPARQL test suite** (`testsuite/oxigraph-tests/geosparql/`, 44 cases
  in W3C manifest form, MIT OR Apache-2.0): vendored under `testsuite/geosparql/oxigraph/`
  with its license notice, run by the existing W3C-manifest harness. Expected deviations
  (Oxigraph rejects EPSG:4326 and uses haversine, planar centroid etc.) are listed in an
  `expected-failures.txt` with the reason per case.
* **Jena's `jena-geosparql` unit tests** (Apache-2.0; ~1,360 JUnit methods): no harness
  port. Their expected values for parsers, relations and the `spatial:` functions
  (`NearbyPFTest`, `WithinBoxPFTest`, cardinal tests, `SpatialIndexTestData` cities) are
  re-expressed as Sparkles SPARQL tests where they encode observable behaviour, with a
  comment naming the Jena test, and Jena's `NOTICE` attribution added to
  `THIRD_PARTY_LICENSES.md` if any test data is copied verbatim. Divergences of §11 are
  asserted as Sparkles' behaviour with the Jena value in a comment.
* **GeoSPARQL 1.1 specification examples** (Annex C, OGC Document License, permissive):
  the example dataset and queries become a test fixture.
* **OGC Compliance Benchmark** (Jovanovik et al., github.com/OpenLinkSoftware/
  GeoSPARQLBenchmark): GeoSPARQL 1.0, 206 queries, 406 expected-result files, dataset in
  RDF/XML, GML and GeoJSON. It is GPL-2.0-only, so it is neither vendored nor linked.
  An opt-in developer script `scripts/geosparql-benchmark.sh` (Phase 2) clones it at a
  pinned commit into `target/geosparql-benchmark/`, loads its dataset into a scratch
  server, runs the queries over HTTP and prints the per-requirement score. It is a
  development tool, like `shellcheck` (README, PROVENANCE), never shipped. It needs the
  maintainer's approval (§11 q12). Target: every WKT requirement passing (GML parts fail
  until Phase 3), above GeoSPARQL Fuseki 3.17's published 177/206. The benchmark compares
  floating-point answers exactly; mismatches caused only by the distance model or the 6-
  decimal rounding are reported separately.
* **OGC `ets-geosparql11`** (Apache-2.0) is a TEAM Engine skeleton without tests as of
  2026-09. Revisit when it has tests.

## 9. Performance targets and benchmark plan

Machine and method as `docs/BENCHMARKS.md` (engines alone, warm page cache, hyperfine,
answers fingerprinted before timing; geometries compared by value with a tolerance in
`bench-answers.py`, which gains a WKT-aware comparison).

**Data**: `scripts/gen-geo.py N` (seeded):

* `N` point features clustered around 500 "cities" (log-normal populations; CRS84, 1% as
  EPSG:4326 to exercise axis swapping);
* `N/10` line features (random walks, 2–200 vertices);
* `N/20` polygon features (star polygons, 4–500 vertices, 10% with holes, 5%
  multipolygons);
* a three-level administrative hierarchy of polygons tiling the world (40 "countries",
  1,600 "states", 64,000 "counties"; 20–2,000 vertices, shared borders so touches/within
  are exercised);
* features with `hasDefaultGeometry`, 50% also with labels and types, so joins with
  ordinary patterns are realistic; 10% W3C Basic Geo points.

Sizes: N = 1M (≈ 9M triples) and N = 10M. A real-data check uses Natural Earth admin
boundaries (public domain) plus a GeoNames- or OSM-derived point set built locally (ODbL
data, never committed).

**Queries** (each also run on Jena GeoSPARQL Fuseki with its spatial index, on QLever
where it has the feature, and on Oxigraph for functions without an index):

| Id | Query | Target (N = 1M, 16 threads) |
|---|---|---|
| Q1 | points `sfWithin` a constant county polygon (≈ 1k results) | p50 ≤ 5 ms |
| Q2 | points within 5 km of a constant point (`distance <`) | ≤ 3 ms |
| Q3 | `spatial:nearby (lat lon 50 uom:kilometre 10)` | ≤ 2 ms |
| Q4 | `spatial:withinBox` over 1% of the world | ≤ 20 ms |
| Q5 (P2) | count points per state (1M × 1,600 `sfContains` join) | ≤ 2 s |
| Q6 (P2) | county–county `sfTouches` self-join (64k) | ≤ 5 s |
| Q7 (P2) | k-NN 10 nearest points to a constant (`ORDER BY metricDistance LIMIT 10`) | ≤ 3 ms |
| Q8 | `SUM(geof:metricArea(?w))` over 50k polygons | ≤ 300 ms |
| Q9 | Q1 joined with labels and types (3-pattern star) | ≤ 10 ms |
| Q10 | Q1 with the index disabled (scan + filter) | report only (the speedup) |

**Writes and maintenance**:

* index build at enable: 1M points ≤ 1.5 s; 1M mixed features (above) ≤ 10 s;
* peak build memory ≤ 2× the final index memory;
* compaction time increase ≤ 1.5× the build time (Phase 2: ≤ 0.3× with parsed-geometry
  reuse);
* single-triple non-geo `INSERT DATA`: no regression beyond noise; a point insert: ≤ +1 ms
  over a non-geo insert; 1,000-polygon insert: ≤ +20 ms;
* open time with persisted files (Phase 2): ≤ 200 ms extra for 1M rows;
* memory: ≤ 150 bytes per point row and ≤ `16 · vertices + 200` bytes per polygon row
  (Phase 1, in memory); report RSS next to Jena's.

Results go to a "GeoSPARQL" section of `docs/BENCHMARKS.md`, with the "Where Sparkles
loses" list updated (QLever's libspatialjoin is likely faster on huge self-joins).

## 10. Rejected alternatives

* **GEOS through the `geos` crate**: `geos` is MIT, but libgeos is LGPL-2.1, and static
  linking (the `static` feature) or shipping it conflicts with the permissive-only policy.
  `geo` covers the needed algorithms.
* **PROJ through `proj`/`proj-sys`** as the default CRS engine: the PROJ library is MIT,
  but the build needs CMake, a C++ toolchain and SQLite (libtiff with the network feature),
  and it ships the EPSG dataset under its own terms of use. Possible later as an opt-in
  feature; `proj4rs` is the pure-Rust option (§4.2.4).
* **QLever-style inline `GeoPoint` ids** (30-bit quantized lat/lng in the 60-bit payload,
  z-order or lat-major): lossy (≈ 2 cm), drops the lexical form, and breaks Sparkles' exact
  term identity (README decision on canonical inlining). It also needs a fifth-from-last
  tag (4 of 16 left). A point literal costs one vocabulary entry plus a column entry.
* **A geo-split vocabulary** (QLever's `.geometry` sub-vocabulary with `.geoinfo` records):
  changes the vocabulary format and id layout. The per-generation geometry column gives
  the same precomputation without touching the vocabulary.
* **Canonicalizing geometry literals on load** (for example, always writing CRS84): breaks
  term identity, and users' literals must round-trip.
* **A spatial index maintained like F03** (one mutable structure, results filtered against
  the snapshot, removed entries kept until a seal): an R-tree has no cheap segment model
  comparable to Tantivy's. Base + overlay + persistent tail gives MVCC by construction.
* **F04's per-query overlay** (no commit-path work; the delta is parsed and scanned per
  query): every query after a write would re-parse the delta's geometries. Parsing once in
  the commit path costs microseconds per inserted geometry.
* **`rstar` (dynamic R*-tree) for the base**: slower to build than a packed tree and not
  usable zero-copy from an mmapped file. Packed Hilbert/STR trees are what Flatbush, JTS's
  `STRtree` and `geo-index` use for static data.
* **Jena's feature-keyed, envelope-only index** (one STRtree per graph, items = feature
  nodes): stand-alone geometries are invisible, `?g` cannot be reported, the exact test is
  skipped for boxes, and the index is never updated incrementally. Sparkles keys by
  serialization quad and maps to features at query time.
* **Jena's silent CRS84 fallback for unknown CRSs**, its planar-degree-only buffer on
  geographic data, envelope-only `withinBox` answers, and returning `false` for every
  relation on empty geometries: wrong or surprising answers that GeoSPARQL does not require
  (§11 lists each as a divergence with a default).
* **Approximating distance in Web Mercator metres** (QLever's `geof:distance` path through
  `webMercMeterDist`): scale error grows with latitude (×2 at 60°).
* **S2 (`s2` 0.2, Apache-2.0)**: a partial port without polygons ("lines and polygons
  aren't implemented yet"). Cell-based prefilters are a Phase 3 idea, not a dependency.
* **QLever's `SERVICE spatialSearch:` as the primary interface**: it overloads SERVICE (as
  F04 argued for vector search). The standard FILTER form plus planner rewrites covers the
  same joins. A compatibility shim stays an open question.
* **Vendoring the GeoSPARQL Compliance Benchmark**: GPL-2.0-only.
* **OSM's tile servers as the default basemap**: the tile usage policy forbids offline and
  bulk use, requires a unique User-Agent and Referer, and blocks without notice. Air-gapped
  servers must render maps.
* **A separate "geometry" literal datatype of Sparkles' own** (as F04 did for vectors): the
  OGC datatypes exist and are what data uses.

## 11. Open questions (defaults chosen here)

1. **Index opt-in.** The spatial index is enabled per dataset (`geo.json`), like text
   search; functions always work. Alternative: enable automatically when a load finds
   `geo:asWKT`/`geo:asGeoJSON` quads.
2. **Distance model.** Default `geodesic` (WGS 84, Karney), with `haversine` (Jena's
   R = 6,371,008.7714 m) as a per-dataset option. Jena-identical numbers would need
   `haversine` as the default.
3. **`getSRID` datatype.** `xsd:anyURI` (the standard) rather than Jena's `xsd:string`.
4. **Empty geometries.** DE-9IM (empty is disjoint from everything) rather than Jena's
   "always false".
5. **`sfEquals`/`ehEquals`.** Topological equality `T*F**FFF*`, so equal points are equal.
   The literal table pattern `TFFFTFFFT` would make two equal points unequal.
6. **Legacy `…/EPSG/4326` (no `/0/`).** Treated as CRS84 lon/lat (Jena's alias) rather
   than as EPSG:4326 lat/lon.
7. **Unknown CRSs.** Parsed, usable same-CRS and planar, not indexed, error for metric
   functions. Jena falls back to CRS84 with a warning.
8. **Units.** OGC, QUDT and EPSG URN units are all accepted. GeoSPARQL 1.1 recommends QUDT;
   Jena accepts OGC and EPSG; QLever QUDT.
9. **`withinBox` / `intersectBox`.** Exact tests always (Jena returns envelope hits when the
   subject is unbound).
10. **Feature links of `spatial:`.** `hasDefaultGeometry` and `hasGeometry` (Jena). Should
    1.1's `hasCentroid` and `hasBoundingBox` (subproperties of `hasGeometry`) count too?
    Default: no (they are not "the" geometry).
11. **`length` of a polygon.** All ring lengths (= perimeter). QLever: exterior ring only.
    Standard text: "the longest length from any one dimension", which is ambiguous.
12. **GeoSPARQL Compliance Benchmark (GPL-2.0)**: allow the opt-in fetch-at-test-time
    script, never vendored? Recommended yes.
13. **Map library and basemap size.** MapLibre GL JS (lazy-loaded) with a bundled Natural
    Earth 1:110m basemap (a few hundred KB precompressed) versus Leaflet (smaller,
    canvas/SVG) and no basemap.
14. **QLever `SERVICE spatialSearch:` compatibility.** Not planned. Add a translation shim
    if QLever users ask.
15. **Strict writes.** Accept ill-typed geometry literals and count them (RDF 1.2), with a
    possible [C10](C10-write-time-validation.md)-style opt-in rejection later.
16. **EPSG data.** For Phase 3, ship no EPSG-derived definitions (operator-supplied
    `crs.json`) unless the maintainer accepts the EPSG terms (or `crs-definitions`'
    CC0 label) for bundled UTM-style definitions. The built-in UTM zones are formulas with
    zone parameters, not EPSG data.
17. **Query Rewrite on by default.** As in Jena. It changes the answers of existing queries
    that use `geo:sf*` as plain predicates on data with geometries (more rows, never fewer).
    A dataset can turn it off.
18. **Index scope includes `urn:x-sparkles:inferred`.** As for text (filtered by
    `reasoning=false`).
19. **Overlay tail threshold.** `max(4096, overlay/8)`; measure in Phase 1.
20. **Phase 1 commit latency** of parsing large polygons in the commit path: a 1M-vertex
    insert parses in about 100 ms. Acceptable, or defer parsing of literals above 64 KiB to
    the first query?

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
  github.com/opengeospatial/ogc-geosparql (no repository license file; the ontologies at
  http://www.opengis.net/ont/geosparql and …/ont/sf declare the OGC Document License,
  https://www.ogc.org/license, permissive). **GeoSPARQL 1.0**, OGC 11-052r4
  (https://docs.ogc.org/is/11-052r4/11-052r4.pdf).
* **OGC definitions server** (unit IRIs that resolve: metre, degree, radian, unity) and
  QUDT unit IRIs (http://qudt.org/vocab/unit/).
* **RFC 7946** (GeoJSON). Simple Features (OGC 06-103r4 / ISO 19125-1) and ISO 13249-3 are
  cited from the GeoSPARQL text and general knowledge; their WKT case rule was not
  re-checked.
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
  tests. Documentation: https://docs.qlever.dev/geosparql/. Ideas only, no code: its
  geometry dependency `ad-freiburg/spatialjoin` is reported as Apache-2.0 on GitHub, while
  the license of the `pb_util` library it pulls in could not be verified (one survey
  recalled GPL-3.0). Nothing from either is used.
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
  Apache-2.0). `geo` capability facts from its CHANGES.md: `PreparedGeometry` 0.29
  (moved to `geo::indexed` in 0.32), `Validation` 0.30, `Buffer` 0.31, `Covers` 0.32,
  `MakeValid` 0.33; geodesic/haversine distance is Point–Point only.
* **EPSG Dataset Terms of Use** (https://epsg.org/terms-of-use.html).
* **Map libraries and data**: Leaflet (BSD-2-Clause, 1.9.4), MapLibre GL JS (BSD-3-Clause,
  6.11.2), OpenLayers (BSD-2-Clause, 10.10.0); Natural Earth terms of use (public domain);
  OSM Foundation tile usage policy (https://operations.osmfoundation.org/policies/tiles/).
* **Test suites**: OGC `ets-geosparql11` (Apache-2.0, template only); GeoSPARQL Compliance
  Benchmark (github.com/OpenLinkSoftware/GeoSPARQLBenchmark, GPL-2.0-only; README, LICENSE
  and file layout only).
* **Papers**: Jovanovik, Homburg, Spasić, "A GeoSPARQL Compliance Benchmark", ISPRS IJGI
  10(7):487, 2021, doi:10.3390/ijgi10070487 (results table); Bast, Brosi, Kalmbach,
  "Efficient Spatial Joins on Large Geometry Sets", SIGSPATIAL 2025,
  doi:10.1145/3748636.3762757, and the 2024 SIGSPATIAL spatial-join paper (abstract only;
  full text could not be fetched); Karney, "Algorithms for geodesics", J. Geodesy 87:43–55,
  2013, doi:10.1007/s00190-012-0578-z; Karney, "Transverse Mercator with an accuracy of a
  few nanometers", J. Geodesy 85:475–485, 2011 (cited from general knowledge); Leutenegger,
  Lopez, Edgington, "STR", ICDE 1997, doi:10.1109/ICDE.1997.582015; Beckmann et al., "The
  R*-tree", SIGMOD 1990; Guttman, "R-trees", SIGMOD 1984, Kamel & Faloutsos 1993 (Hilbert
  packing) and Hjaltason & Samet 1999 (distance browsing), cited from general knowledge;
  Egenhofer & Franzosa, IJGIS 5(2), 1991, doi:10.1080/02693799108927841; Randell, Cui,
  Cohn, KR 1992; Clementini, Di Felice, van Oosterom, SSD 1993,
  doi:10.1007/3-540-56869-7_16.
* **Not consulted**: Fluree. No Fluree repository, source, tests, documentation, website
  or other material was opened or used for this spec. No GPL or LGPL source code was read
  (the GeoSPARQL benchmark was looked at only for its license, size and layout; GEOS and
  QLever's `pb_util` not at all).

## 13. Provenance entry (for `PROVENANCE.md`)

The entry as drafted with the spec, before implementation. The current entry, with the
dependencies that actually shipped, is in [PROVENANCE.md](PROVENANCE.md#geosparql).

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
  No GPL or LGPL source was read. Fluree was not consulted.
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

**Delivered.** Phases 1 and 2 landed on 2026-10-01 (dependencies in
[PROVENANCE.md](PROVENANCE.md#geosparql)); Phase 3 (GML/KML, a `proj4rs` CRS backend,
variable `spatial:` arguments, cell prefilters) is not started.

**Decided by the maintainer:** geodesic WGS 84 measures by default, haversine per dataset
(§11 q2); the index opt-in per dataset (q1); no EPSG data shipped (q16); MapLibre with a
bundled Natural Earth basemap (q13); the GPL-2.0 Compliance Benchmark only fetched at test
time, behind `SPARKLES_ALLOW_GPL_BENCHMARK=1`, never vendored (q12). Against q17, Query
Rewrite is off by default (`"queryRewrite": true` per dataset; `serve --no-geo-rewrite`
remains a server-wide switch), so enabling an index never changes what an existing query
means. After Phase 1: two empty geometries are never `sfEquals`; an empty geometry is
only disjoint, matching Jena, whose filter functions return false whenever a side is empty.

**Deviations.**
* Sparkles' own WKT and GeoJSON readers and writers replace the `wkt` and `geojson` crates
  (both dropped): errors need byte offsets, and GeoSPARQL literals use `LINEARRING`,
  `TRIANGLE`, `TIN`, `POLYHEDRALSURFACE`, untagged 3D positions and a CRS IRI prefix.
  Constructed geometries are 2D.
* Metric buffers project with an ellipsoidal, not spherical, AEQD (§4.4.3). A constructed
  literal over the memory budget is a type error, not `507`.
* Index files are keyed by a hash of the settings that change what is indexed, not of the
  whole `geo.json` (§5.5), so changing `distance` or `queryRewrite` keeps them. `?at=`
  snapshots run without the index (by scanning) instead of building a base on demand (§4.6).
* Chosen during implementation (open to revision): `concaveHull`'s percentage maps
  linearly to the concavity and `aggConcaveHull` takes one argument (a SPARQL aggregate
  takes one expression); `spatial:equals` is always available; the `--vocab geosparql`
  axioms are written from the standard and land in the inferred graph; `GET /{ds}/geo`
  scans when the index is not ready; `POST /$/geo/convert` is open to any caller.

**Conformance.** The §7 examples and seeded index-versus-plain-plan comparisons run as
tests; W3C and SHACL results were unchanged. Oxigraph's GeoSPARQL suite: 37 of 44, the
other 7 listed with reasons in `testsuite/geosparql/oxigraph/expected-failures.txt`. The
[Compliance Benchmark](../BENCHMARKS.md#geosparql-compliance-benchmark) on the Phase 1
build: 74 of 206, and 72 of the 77 WKT queries without entailment or rewrite (the misses:
the two empty-equality queries, decided above, and three distance, metre-buffer and
benchmark-error cases); it was not re-run after Phase 2.

**Performance.** No measurable commit latency from the index ([commit
cost](../BENCHMARKS.md#spatial-index-commit-cost)); the §9 query targets are not yet measured.
