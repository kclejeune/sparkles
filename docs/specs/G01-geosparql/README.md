# G01: GeoSPARQL (OGC GeoSPARQL 1.1, Jena spatial extensions, spatial index)

> **Status:** implemented in part (Phases 1–2, part of Phase 3)

Spatial functions, indexed filters and joins, maps and indexed constant-geometry nearest-neighbour ordering are available. Per-row nearest-neighbour queries and the remaining geometry, CRS and compatibility extensions are tracked under items 24 and 70.

## Documents

* [Design and acceptance](SPEC.md).
* [Delivery record](OUTCOME.md).
* [Provenance](PROVENANCE.md).

## User guides

[API: GeoSPARQL](../../API.md#geosparql) ·
[API: hulls, aggregates, `spatialF:`, UTM and conversion](../../API.md#hulls-aggregates-jena-filter-functions-utm-and-conversion) ·
[API: spatial joins and nearest neighbours](../../API.md#spatial-joins-and-nearest-neighbours) ·
[API: query rewrite and RDFS entailment](../../API.md#query-rewrite-spatialequals-and-rdfs-entailment) ·
[API: maps in the web UI](../../API.md#maps-in-the-web-ui) ·
[API: CRSs from proj4 definitions](../../API.md#crss-from-proj4-definitions) ·
[Features](../../search/README.md#geosparql) ·
[Benchmarks: spatial index commit cost](../../BENCHMARKS.md#spatial-index-commit-cost) ·
[Benchmarks: GeoSPARQL Compliance Benchmark](../../BENCHMARKS.md#geosparql-compliance-benchmark)

The [roadmap](../../ROADMAP.md#completion-paths-for-partially-complete-specs)
tracks remaining work. The delivery record preserves the detailed phase history.
