# Spatial queries results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Spatial queries

This Sparkles-only run uses the geo fixture with 100,000 points, its polygons and 180,640
indexed geometry literals. The server returns the same coordinate-normalized results
with the spatial index on and off. Each query had one warm-up and five curl and hyperfine
samples, so the times include process startup. The index builds in 0.54 s and takes
13.8 MiB. `scripts/bench-geo.sh` reproduces the run.

| query | rows | with the index (ms) | without (ms) | speedup | rewrites off (ms) |
|---|---:|---:|---:|---:|---:|
| geo-q1-within | 2861 | 21.9 ± 4.0 | 793.7 ± 48.6 | 36.3× | — |
| geo-q2-distance | 85 | 16.0 ± 2.9 | 419.6 ± 41.6 | 26.2× | — |
| geo-q3-nearby | 10 | 15.9 ± 1.5 | 400.1 ± 15.7 | 25.1× | — |
| geo-q4-withinbox | 1 | 31.2 ± 3.1 | 344.4 ± 6.5 | 11.0× | — |
| geo-q5-points-per-state | 562 | 47.2 ± 2.2 | 100.9 ± 5.1 | 2.1× | — |
| geo-q6-touching-counties | 1 | 241.2 ± 4.0 | 309.5 ± 4.3 | 1.3× | — |
| geo-q7-knn | 10 | 7.2 ± 2.3 | 492.5 ± 10.8 | 68.0× | 304.2 ± 43.1 |
| geo-q8-area | 1 | 756.2 ± 0.9 | 971.6 ± 9.9 | 1.3× | — |
| geo-q9-star | 1374 | 23.8 ± 5.3 | 821.7 ± 22.2 | 34.6× | — |

### GeoSPARQL Compliance Benchmark

The GeoSPARQL Compliance Benchmark (Jovanovik, Homburg and Spasić, 2021) has 206 queries
over the 30 requirements of GeoSPARQL 1.0 and a 338-triple dataset, and
`scripts/geosparql-benchmark.sh` runs it. Each requirement runs against the configuration
of its conformance class. R25 to R30 test RDFS entailment and query rewrite, so they run
against a database with `infer --profile rdfs --vocab geosparql` and
`"queryRewrite": true`. The other requirements run against the asserted data with the
spatial index and neither extension, because entailment and rewrite add answers that
those queries do not expect. With every query against the extended database, Sparkles
scores 147.

An answer counts as correct when it matches one of the expected result files. Solutions
are compared as multisets, numbers within a relative 1e-6, and geometry literals by their
coordinates rounded to 6 decimals. GML and KML literals are converted to GeoJSON by the
server's `POST /$/geo/convert` first, and the comparison ignores ring starts and
directions, line directions, vertices on straight runs and member order. R17 has no query.

| Requirement | Correct | Queries |
|---|---:|---:|
| R1 core: SPARQL protocol | 1 | 1 |
| R2 core: spatial object class | 1 | 1 |
| R3 core: feature class | 1 | 1 |
| R4 topology vocabulary: Simple Features relations | 8 | 8 |
| R5 topology vocabulary: Egenhofer relations | 8 | 8 |
| R6 topology vocabulary: RCC8 relations | 8 | 8 |
| R7 geometry class | 1 | 1 |
| R8 feature properties | 2 | 2 |
| R9 geometry properties | 6 | 6 |
| R10 WKT literal | 1 | 1 |
| R11 WKT literal default CRS | 1 | 1 |
| R12 WKT axis order | 1 | 1 |
| R13 empty WKT literal | 0 | 2 |
| R14 `asWKT` | 1 | 1 |
| R15 GML literal | 1 | 1 |
| R16 empty GML literal | 0 | 2 |
| R18 `asGML` | 1 | 1 |
| R19 query functions | 22 | 28 |
| R20 `getSRID` | 2 | 2 |
| R21 `relate` | 4 | 4 |
| R22 Simple Features functions | 32 | 32 |
| R23 Egenhofer functions | 32 | 32 |
| R24 RCC8 functions | 32 | 32 |
| R25 RDFS entailment: basic graph patterns | 3 | 3 |
| R26 RDFS entailment: WKT geometry types | 2 | 2 |
| R27 RDFS entailment: GML geometry types | 1 | 1 |
| R28 query rewrite: Simple Features | 6 | 8 |
| R29 query rewrite: Egenhofer | 5 | 8 |
| R30 query rewrite: RCC8 | 4 | 8 |
| **Total** | **187** | **206** |

The mean of the per-requirement scores is 88.5% over the 29 requirements with queries.
GeoSPARQL Fuseki 3.17 published 177 of 206. The 19 queries that fail disagree with
choices Sparkles makes on purpose:

* R13 and R16 (4 queries) expect two empty geometries to be `sfEquals`. In Sparkles an
  empty geometry is only disjoint, as in Jena.
* Four R19 queries expect distances that are neither geodesic nor haversine, and two
  expect a 10-metre buffer to be 10 degrees wide.
* Nine R28 to R30 queries expect relations that DE-9IM and the standard's tables do not
  give. They count a region as an RCC8 tangential proper part of itself, count a region
  strictly inside another as `ehCoveredBy` it, leave a feature's own geometries and the
  points inside it out of `sfIntersects`, and leave the EPSG:4326 point at latitude
  31.95, longitude -88.38 out of the geometries disjoint from feature B.
