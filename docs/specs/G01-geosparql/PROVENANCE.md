# GeoSPARQL

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

See the [spec landing page](README.md), [design](SPEC.md) and
[delivery record](OUTCOME.md) for their respective contracts.

- **Spec:** [`G01-geosparql.md`](README.md). Its sources are:
  - the OGC GeoSPARQL 1.1 standard (OGC document license);
  - Apache Jena's `jena-geosparql` (Apache-2.0), read for behaviour, the `spatial:` and
    `spatialF:` surface and its divergences. No code was copied;
  - Oxigraph's `spargeo` (Apache-2.0/MIT) and QLever's spatial support (Apache-2.0),
    both read;
  - the Sparkles code.
- **Implementation, Phase 1** (2026-10-01): the `geo` feature of `sparkles`, which is on
  in the server. It is built on three crates:
  - `geo` 0.33.1 (MIT OR Apache-2.0), with default features off, renamed `georust`;
  - `geo-index` 0.4.0 (MIT OR Apache-2.0);
  - `geographiclib-rs` 0.2.7 (MIT), a port of Karney's GeographicLib.

  `wkt` and `geojson` were tried and dropped. The new transitive crates (`i_overlay`,
  `robust`, `rstar`, `geo-traits`, `geo-types`, `float_next_after`, `heapless`,
  `accurate`, …) are all MIT and/or Apache-2.0, and there is no `earcutr` or `spade`.
  `THIRD_PARTY_LICENSES.md` lists them.
  - The WKT and GeoJSON readers and writers are hand-written from the GeoSPARQL 1.1 and
    Simple Features grammars and RFC 7946. The `wkt` crate gives no error offsets and
    has no TRIANGLE/TIN. The CRS and unit tables come from the OGC, EPSG and QUDT
    registries.
  - Topology uses DE-9IM (`geo`'s relate). Geodesic distance, area and buffers use
    GeographicLib (Karney 2013), and GeographicLib's published values serve as test
    oracles.
  - The spatial index (packed R-tree base, overlay and tail) and the planner and
    executor are written from the spec and the Sparkles store and planner code.
  - **Test data and tools:** the GeoSPARQL Compliance Benchmark is GPL-2.0 and is never
    vendored. `scripts/geosparql-benchmark.sh` fetches it at test time into `target/`
    at pinned commit 879e0746, and only with `SPARKLES_ALLOW_GPL_BENCHMARK=1`. Its README
    and file names were consulted to write the runner. Its code was not.
- **Phase 2** (2026-10-01): spatial joins, nearest neighbours, query rewrite, hulls and
  aggregates, conversion and the UI's maps. Built from the spec, the Phase 2 plan and the
  Sparkles code.
  - **UI dependencies:** `maplibre-gl` 6.11.2 (BSD-3-Clause) and its `@maplibre/*` packages
    (ISC, MIT, MIT OR Apache-2.0), listed in `THIRD_PARTY_LICENSES-UI.md`.
  - **Data:** the bundled basemap is Natural Earth 1:110m, release 5.1.2 (public domain),
    reduced by `scripts/basemap.mjs` (`ui/src/lib/basemap/README.md`).
  - The GeoSPARQL vocabulary axioms behind `--vocab geosparql` are written by hand from
    the standard. No OGC files are vendored.
- **Phase 3** (2026-10-02): geometry types from literals, variable `spatial:` arguments,
  the cell grid for spatial tests, GML and KML literals, and CRSs from proj4 definitions.
  Built from the spec and the Sparkles code.
  - The GML and KML readers and writers are written from the GML 3.2 Simple Features
    profile and KML 2.2. They read XML with `quick-xml` 0.37 (MIT), which the RDF/XML
    parser already used. The GML class hierarchy follows the substitution groups of the
    GML 3.2 schemas, and no schema file is vendored. The tests use the examples printed in
    the GeoSPARQL 1.1 standard (OGC 22-047r1).
  - The cell grid takes only the idea of QLever's grid approximation of regions.
  - **Dependencies:** `proj4rs` 0.2.0 (MIT OR Apache-2.0), with its default features off,
    behind the `geo-proj4` feature that the server's `geo` feature turns on. Its only new
    dependency is `thiserror` 2. `crs-definitions` 0.5.0 (CC0-1.0, derived from the EPSG
    dataset) is linked by the `geo-epsg` feature. The maintainer decided on 2026-10-03
    to turn it on in the server's default features and leave it off in the library's.
    The EPSG terms of use allow redistribution at no charge with the terms passed on, so
    `scripts/third-party-licenses.py` writes them into `THIRD_PARTY_LICENSES.md` with
    an acknowledgement of IOGP's ownership. Sparkles has no `deny.toml`. The license
    check is `scripts/third-party-licenses.py`, and both licenses are permissive.
