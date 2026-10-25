# G01: GeoSPARQL (OGC GeoSPARQL 1.1, Jena spatial extensions, spatial index) — Delivery record

Current delivery status is summarized in the [landing page](README.md).
The [design Outcome overview](SPEC.md#outcome) links the supported scope to this record.
The recorded dates and measurements below describe their original delivery stages.

## Historical opening status

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
> build feature. The rest of Phase 3 is not built, as the [Outcome](SPEC.md#outcome) lists.
>
> **User docs:** [API: GeoSPARQL](../../API.md#geosparql) ·
> [API: hulls, aggregates, `spatialF:`, UTM and conversion](../../API.md#hulls-aggregates-jena-filter-functions-utm-and-conversion) ·
> [API: spatial joins and nearest neighbours](../../API.md#spatial-joins-and-nearest-neighbours) ·
> [API: query rewrite and RDFS entailment](../../API.md#query-rewrite-spatialequals-and-rdfs-entailment) ·
> [API: maps in the web UI](../../API.md#maps-in-the-web-ui) ·
> [API: CRSs from proj4 definitions](../../API.md#crss-from-proj4-definitions) ·
> [Features](../../FEATURES.md#geosparql) ·
> [Benchmarks: spatial index commit cost](../../BENCHMARKS.md#spatial-index-commit-cost) ·
> [Benchmarks: GeoSPARQL Compliance Benchmark](../../BENCHMARKS.md#geosparql-compliance-benchmark)
>
> This is the design as written before implementation. The [Outcome](SPEC.md#outcome) section at
> the end records how it landed.

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

The rest of Phase 3 is not built. That is k-NN with a query geometry bound per row, simplified polygon
approximations, `geo:hasMetricArea` and similar properties by rewrite, QLever's
`SERVICE spatialSearch:` syntax, and the Compliance Benchmark in CI.

**Decided by the maintainer.**
* Measures are geodesic on WGS 84 by default, with haversine as a per-dataset option
  (§11 q2).
* The index is opt-in per dataset (q1).
* EPSG-derived definitions ship in the server (q16). On 2026-10-03 the maintainer
  turned the `geo-epsg` feature on in the default features of `sparkles-server`, and so
  in the `sparkles` binary and the Nix package. It stays off in the library, so an
  embedder opts in to the EPSG terms of use, as Apache SIS users do. The section on EPSG
  definitions below gives the accuracy policy and the size.
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
  * EPSG codes resolve through the proj4 table of `crs-definitions`, which is derived
    from EPSG, rather than through the EPSG database. The `geo-epsg` feature links it.
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

**EPSG definitions and datum accuracy.** The `geo-epsg` feature resolves a projected
EPSG code on first use from the 6,184 definitions of `crs-definitions` 0.5.0. Every
transform goes through WGS 84, so a definition's datum shift decides how accurate its
transforms are. Sparkles sorts the definitions, and those of `--geo-crs`, by that shift.
* A definition whose shift needs grid files is refused, because Sparkles has no grids.
  The table has 205 NAD27 definitions, which imply the NADCON and NTv2 grids, and 34
  NZGD49 ones that name `nzgd2kgrid0005.gsb`. The refusal names the CRS and the grids. A
  literal in such a CRS is in an unknown CRS, and a query that reads it or transforms to
  it gets a `geo-crs-unsupported` plan warning with the reason. A `--geo-crs` definition
  with a grid stops the command with the same reason.
* A definition with a Helmert shift is accepted, and its transforms are approximate.
  That covers `+towgs84` with nonzero parameters, zero ones on an ellipsoid other than
  WGS 84 and GRS 80, and the proj4 datums that imply a shift, such as `OSGB36`. The table
  has 1,543 of them. A query that reads a geometry in such a CRS or transforms to one gets
  a `geo-crs-approximate` plan warning that names the CRS, and `--geo-crs` logs such
  definitions when a command starts. Against PROJ 9.9 with the national grids, EPSG:27700
  is 0.5 m off OSTN15 in Edinburgh, 1.8 m in Greenwich and 4.3 m at Land's End, and
  EPSG:28992 is within 0.1 m of RDNAPTRANS 2018 in Amsterdam and Eindhoven.
* A definition on WGS 84, or on a GRS 80 datum without a shift, is exact up to the
  projection formulas. The table has 3,095 of them. Lambert-93 (EPSG:2154) and ETRS89 /
  UTM 32N (EPSG:25832) match PROJ to the millimetre.
* A definition on another ellipsoid that gives no shift at all takes its coordinates as
  WGS 84 ones, which can be hundreds of metres off. The table has 278 of them. Refusing
  them would drop CRSs whose literals still relate correctly to each other, so they are
  accepted with the `geo-crs-approximate` warning, which states the size of the error.
* The table's other 1,029 definitions are refused too. 956 are geographic or geocentric
  CRSs, and `proj4rs` cannot read 73.

The feature is part of the spatial index files' identity, so a server that gains it
rebuilds its spatial indexes once when the datasets open. The Python bindings do not
turn on `geo`, so they leave the table out.

The table makes the release binary 5.4 MB larger. On x86_64 Linux the stripped
`sparkles` binary is 69.9 MB with the feature and 64.4 MB without it, 8.5% more. With the
release profile's line tables it is 436.9 MB against 431.3 MB. Most of the table is the
WKT text of each definition, which Sparkles reads only for the axis order and the name.

**Conformance.** The §7 examples and seeded comparisons of the index plan against the plain
plan run as tests, and so do the GML and KML examples printed in GeoSPARQL 1.1. The W3C
and SHACL results did not change. Sparkles passes 37 of the 44 cases in Oxigraph's
GeoSPARQL suite. The other 7 are listed with reasons in
`testsuite/geosparql/oxigraph/expected-failures.txt`. On the Phase 1 build, the
[Compliance Benchmark](../../BENCHMARKS.md#geosparql-compliance-benchmark) scored 74 of 206.
After Phase 3 it scores 187 of 206, against the 177 GeoSPARQL Fuseki 3.17 published.
The extension requirements R25 to R30 ran against a database with RDFS entailment and
query rewrite, and the others against the plain data. Without the extensions the score is
166. The 19 misses are the empty-equality queries decided above, distances and a metre
buffer computed another way, and rewrite answers that DE-9IM does not give.

**Performance.** The index adds no measurable commit latency ([commit
cost](../../BENCHMARKS.md#spatial-index-commit-cost)). The §9 query targets are set at
N = 1M and have not been measured at that size.
[BENCHMARKS.md](../../BENCHMARKS.md#spatial-queries) publishes Q1 to Q9 at 100,000 points
from `forge` on 2026-10-08, with the index 1.3 to 68 times faster than without it and
the same answers both ways. The cell grid made 200,000 `sfContains` tests against a 1,024-vertex polygon
4.3 times faster (60 ms instead of 256 ms) and 5.4 times faster against a 16,384-vertex
one, for about 16 bytes per region vertex, up to 64 KiB per region.

### Roadmap follow-ups (2026-10-12)

Nearest-neighbour ordering around a constant also handles independent join components.
The planner joins each component on its own keys before crossing it with a batch of
nearest candidates. Joins and filters can discard a whole batch, and the search continues
until enough rows are proven or the index is exhausted. Duplicate solutions retain their
multiplicity, and the existing query budgets apply to each template run and its output.
Independent components run again for each batch, without an additional materialized cache.
Their local filters remain inside the component, preserving unbound variables and avoiding
an unfiltered cross product. Application callbacks and components whose repeatability
cannot be established keep the generic plan, including components with volatile functions.
This includes the ARQ UUID aliases, unresolved dynamic function calls and effectful ARQ
calls such as `wait`, `print` and `execTime`. Result caching and nearest-neighbour
repeatability share the expression-purity classifier. Result caching also retains its
stricter refusal of query-clock values across queries. Connected filters are applied as soon as their original required bindings are available, before
unrelated components are crossed. Filters carried inside an index probe receive the same
purity check as ordinary filters. Their visible bindings are the probe scan's columns,
so a variable supplied by another join input cannot change an unbound test. When those
conditions cannot be established, the query keeps the generic plan. Differential tests
cover volatile copy groups, scoped unbound probes and rejection before a cross product
under the row budget.
The per-row nearest-neighbour join described below remains open under item 24.

**The rest of Phase 3 and geometry limits** (item 24, Tier 3,
[ROADMAP](../../ROADMAP.md)). The Outcome lists the first items below as not built, and
FEATURES.md lists the geometry limits. The deliberate divergences in
[COMPARISON.md](../../COMPARISON.md#geosparql) stay the default; item 70 below adds an
opt-in mode for Jena's answers.

* **Per-row nearest-neighbour joins.** Constant-distance ordering supports connected
  patterns and independent components as described above. The remaining query asks for
  the `k` nearest `?b` for each geometry `?a` from another pattern. It uses a `LATERAL`
  subquery with `ORDER BY … LIMIT k`, as defined in [G06](../G06-arq-query-extensions.md),
  or QLever's syntax below. The planner runs one k-NN search per
  distinct left geometry over the right side's tree. Acceptance: the five nearest
  restaurants of every station in a dataset of 10,000 stations and 100,000 restaurants
  equal the generic plan's answer and run at least 50 times faster.
* **Polygon approximations.** `geo/ops/cells.rs` already approximates a region of 32
  or more vertices by a grid of interior, outside and boundary cells, which decides
  most points and small geometries against it. Two cases still run the exact test for
  most pairs: two large regions, whose envelopes rarely cover only interior or only
  outside cells of each other, and a region whose boundary cells are a large share of
  its grid (long thin shapes, coastlines). The improvement keeps, per indexed region,
  the grid and two more approximations: a simplified outer ring (Douglas-Peucker on an
  outward buffer, at most 64 vertices) and a few large interior rectangles, from the
  grid's interior cells merged into maximal rectangles. A pair whose outers do not meet
  is disjoint, a pair whose interior rectangles meet intersects, and a region inside
  another's interior rectangles is contained; only the rest run the exact test. The
  approximations count toward the index's bytes and its identity hash. Acceptance: a
  join of 10,000 administrative regions against 10,000 parcels returns the exact
  plan's pairs with at least half of the exact tests skipped, the counter of skipped
  tests in the plan.
* **Datum shift grids.** `geo/crs.rs` refuses a definition whose shift needs grids
  (`+nadgrids`, or `+datum=NAD27`), 239 of the EPSG table's definitions. The server
  gains `--geo-grids DIR`, a directory of NTv2 (`.gsb`) and NADCON grid files that the
  operator provides; Sparkles ships none, since the national grids carry their own
  licenses. A definition whose grids are all present is accepted and transformed with
  the grid's bilinear interpolation, as PROJ does; one whose grids are missing is
  refused as now, with the missing file named. The grid files join the index files'
  identity. Acceptance: EPSG:27700 with OSTN15's NTv2 file is within 0.1 m of PROJ 9.9
  at the three points the Outcome measured.
* **QLever's spatial SERVICE syntax.** §11 q14 left a shim open. `SERVICE
  spatialSearch: { … }` with QLever's configuration (`spatialSearch:left`,
  `spatialSearch:right`, `spatialSearch:numNearestNeighbors`,
  `spatialSearch:maxDistance`, `spatialSearch:bindDistance`, and the right side as a
  nested group) is translated at planning time into the FILTER or k-NN forms above,
  the way `SERVICE path:search` became a leaf in [F07](../F07-path-search.md). It is
  recognized before remote SERVICE planning, so the outbound policy does not apply,
  and an option without a translation is a `400` naming it. The test runs QLever's
  documented examples and compares them with the FILTER form.
* **Metric-property rewrites.** GeoSPARQL 1.1 defines `geo:hasMetricArea`,
  `geo:hasMetricLength`, `geo:hasMetricPerimeterLength`, `geo:hasMetricSize` and
  related properties, which data seldom asserts. With query rewrite on, a triple
  pattern `?g geo:hasMetricArea ?a` over a geometry with no asserted value binds
  `?a` to `geof:metricArea` of its serialization, as the topological rewrite does for
  the relations; an asserted value wins. The rewrite is in `georewrite.rs` and shares
  its switch.
* **The Compliance Benchmark outside the repository's CI.** The benchmark is
  GPL-2.0-only, and `scripts/geosparql-benchmark.sh` states that it is never part of the
  repository, its releases or its CI; it is cloned on request into a git-ignored
  directory. That policy stays. The score is tracked instead by X06's benchmark runs
  on the benchmark machine, outside the repository's CI, which run the script with
  `SPARKLES_ALLOW_GPL_BENCHMARK=1` at each refresh and record the score per
  requirement in BENCHMARKS.md. A drop below the recorded 187 of 206, or a query that
  passed and now fails, is reported in the refresh like any other regression.
* **Geometry limits.** [FEATURES.md](../../FEATURES.md#known-gaps) lists them, and each is
  a separate piece of work:
  * *Geographic CRSs on datums other than WGS 84.* The EPSG table's 956 geographic and
    geocentric definitions are refused (*EPSG definitions* above), so a literal in,
    say, EPSG:4277 (OSGB36 latitude and longitude) is in an unknown CRS. Geographic
    definitions are accepted with the datum shift of their projected siblings
    (Helmert or grid, as below), and measures run on WGS 84 after the shift.
  * *Exact Helmert shifts.* A Helmert shift is exact for the published parameters; the
    approximation the warning reports is the parameters' own error against the national
    grids, not the computation. The `geo-crs-approximate` warning stays for definitions
    whose shift is a stand-in for a grid (OSGB36 for OSTN15) and is dropped for those
    whose authority defines the Helmert transformation as the exact one.
  * *Curved GML segments and solids.* `geo/xml.rs` reads straight segments only. Arcs
    (`gml:Arc`, `gml:ArcString`, `gml:Circle`) are linearized at read time with a
    chord error bound (default 0.1 m, or 1e-6 of the extent in a CRS without units),
    recorded as a plan warning; `gml:Solid` reads as its boundary surfaces.
  * *3D constructions.* Constructed geometries are 2D (*Deviations* above). Buffers,
    hulls and overlays keep the Z values of input vertices that survive unchanged and
    interpolate Z along the input segment for new vertices; a construction whose
    output has new vertices in an area interior returns 2D, as now, with a warning.
  * *Splitting lines at crossings.* `geof:union` of two crossing lines keeps the
    covered points but does not split the lines at each crossing, unlike JTS. The
    curve-against-curve step of `geo/ops/overlay.rs` already computes the crossings by
    segment intersection; it nodes both lines at them before assembling the result,
    so the output's line strings meet only at their ends, as in JTS.
**A Jena compatibility mode** (item 70, Tier 7, [ROADMAP](../../ROADMAP.md)). The decisions in
[COMPARISON.md](../../COMPARISON.md#geosparql) give different answers from Jena on purpose,
and the Compliance Benchmark's misses trace to some of them (the Conformance paragraph
above: empty-geometry equality, distances, a metre buffer and rewrite answers). Two are
already switches: `geo.json` `"distance": "haversine"` gives Jena's sphere and
`"queryRewrite": true` gives its rewrite. A per-dataset `"compat": "jena"` in `geo.json`
sets both and adds the rest:

* every relation is false when either side is empty, and `sfEquals` compares with the
  `TFFFTFFFT` pattern, so two equal points are not equal, as in Jena;
* a literal in an unknown CRS is read as CRS84 degrees with a warning in the plan,
  instead of being left out of metric functions and the index;
* `geof:getSRID` returns `xsd:string`;
* `spatialF:angle` returns Jena's value in every quadrant, including the quarter turn
  off south-east and north-west of the first point;
* `spatial:withinBox` and `spatial:intersectBox` with an unbound subject return the
  index's envelope hits without the exact refinement.

The mode is part of the index files' identity where it changes what is indexed (unknown
CRSs). It never changes a dataset without the setting. Acceptance: with the mode on,
each of the benchmark's misses that the Conformance paragraph attributes to these
choices passes, and the score with the mode is reported beside the default's.
