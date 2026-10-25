# G01: GeoSPARQL (OGC GeoSPARQL 1.1, Jena spatial extensions, spatial index)

This spec has a [bundle landing page](G01-geosparql/README.md).
The headings below preserve existing bookmarks and link to the corresponding
sections of the design and delivery record.

## 1. Summary, goals, non-goals

See the [section in its current document](G01-geosparql/SPEC.md#1-summary-goals-non-goals).

### 1.1 Conformance by phase

See the [section in its current document](G01-geosparql/SPEC.md#11-conformance-by-phase).

## 2. User-visible behavior

See the [section in its current document](G01-geosparql/SPEC.md#2-user-visible-behavior).

### 2.1 Namespaces and reserved terms

See the [section in its current document](G01-geosparql/SPEC.md#21-namespaces-and-reserved-terms).

### 2.2 Data

See the [section in its current document](G01-geosparql/SPEC.md#22-data).

### 2.3 Functions (`geof:`)

See the [section in its current document](G01-geosparql/SPEC.md#23-functions-geof).

### 2.4 Topological relations

See the [section in its current document](G01-geosparql/SPEC.md#24-topological-relations).

### 2.5 Jena property functions (`spatial:`)

See the [section in its current document](G01-geosparql/SPEC.md#25-jena-property-functions-spatial).

### 2.6 Query Rewrite Extension (Phase 2)

See the [section in its current document](G01-geosparql/SPEC.md#26-query-rewrite-extension-phase-2).

### 2.7 RDFS Entailment Extension (Phase 2)

See the [section in its current document](G01-geosparql/SPEC.md#27-rdfs-entailment-extension-phase-2).

### 2.8 HTTP (`/$/` extensions, documented in `docs/API.md`)

See the [section in its current document](G01-geosparql/SPEC.md#28-http--extensions-documented-in-docsapimd).

### 2.9 CLI

See the [section in its current document](G01-geosparql/SPEC.md#29-cli).

### 2.10 Rust API (`sparkles`, feature `geo`)

See the [section in its current document](G01-geosparql/SPEC.md#210-rust-api-sparkles-feature-geo).

### 2.11 UI (SvelteKit, `ui/`, Phase 2)

See the [section in its current document](G01-geosparql/SPEC.md#211-ui-sveltekit-ui-phase-2).

## 3. Standards basis

See the [section in its current document](G01-geosparql/SPEC.md#3-standards-basis).

## 4. Semantics

See the [section in its current document](G01-geosparql/SPEC.md#4-semantics).

### 4.1 Literals

See the [section in its current document](G01-geosparql/SPEC.md#41-literals).

#### 4.1.1 `geo:wktLiteral`

See the [section in its current document](G01-geosparql/SPEC.md#411-geowktliteral).

#### 4.1.2 `geo:geoJSONLiteral`

See the [section in its current document](G01-geosparql/SPEC.md#412-geogeojsonliteral).

#### 4.1.3 Other datatypes

See the [section in its current document](G01-geosparql/SPEC.md#413-other-datatypes).

#### 4.1.4 Empty geometries and dimensions

See the [section in its current document](G01-geosparql/SPEC.md#414-empty-geometries-and-dimensions).

#### 4.1.5 Canonical output form

See the [section in its current document](G01-geosparql/SPEC.md#415-canonical-output-form).

### 4.2 Coordinate reference systems

See the [section in its current document](G01-geosparql/SPEC.md#42-coordinate-reference-systems).

#### 4.2.1 Built-in table (Phase 1)

See the [section in its current document](G01-geosparql/SPEC.md#421-built-in-table-phase-1).

#### 4.2.2 Axis order and internal coordinates

See the [section in its current document](G01-geosparql/SPEC.md#422-axis-order-and-internal-coordinates).

#### 4.2.3 Mixed CRSs and unknown CRSs

See the [section in its current document](G01-geosparql/SPEC.md#423-mixed-crss-and-unknown-crss).

#### 4.2.4 Additional CRSs (Phase 3)

See the [section in its current document](G01-geosparql/SPEC.md#424-additional-crss-phase-3).

### 4.3 Units

See the [section in its current document](G01-geosparql/SPEC.md#43-units).

### 4.4 Computation model

See the [section in its current document](G01-geosparql/SPEC.md#44-computation-model).

#### 4.4.1 Engine, precision, robustness

See the [section in its current document](G01-geosparql/SPEC.md#441-engine-precision-robustness).

#### 4.4.2 Distance

See the [section in its current document](G01-geosparql/SPEC.md#442-distance).

#### 4.4.3 Buffer

See the [section in its current document](G01-geosparql/SPEC.md#443-buffer).

#### 4.4.4 Area, length, perimeter, centroid

See the [section in its current document](G01-geosparql/SPEC.md#444-area-length-perimeter-centroid).

#### 4.4.5 Overlay

See the [section in its current document](G01-geosparql/SPEC.md#445-overlay).

#### 4.4.6 Hulls and simplicity (Phase 2)

See the [section in its current document](G01-geosparql/SPEC.md#446-hulls-and-simplicity-phase-2).

### 4.5 Errors

See the [section in its current document](G01-geosparql/SPEC.md#45-errors).

### 4.6 Index semantics and consistency

See the [section in its current document](G01-geosparql/SPEC.md#46-index-semantics-and-consistency).

### 4.7 Limits and budgets

See the [section in its current document](G01-geosparql/SPEC.md#47-limits-and-budgets).

## 5. Design

See the [section in its current document](G01-geosparql/SPEC.md#5-design).

### 5.1 Modules and features

See the [section in its current document](G01-geosparql/SPEC.md#51-modules-and-features).

### 5.2 Geometry representation and the geometry column

See the [section in its current document](G01-geosparql/SPEC.md#52-geometry-representation-and-the-geometry-column).

### 5.3 Index structures

See the [section in its current document](G01-geosparql/SPEC.md#53-index-structures).

### 5.4 Commit path (`WriteTxn::publish_log` → `Store::maintain_geo`)

See the [section in its current document](G01-geosparql/SPEC.md#54-commit-path-writetxnpublish_log--storemaintain_geo).

### 5.5 Generation switches, open, rebuild, persistence

See the [section in its current document](G01-geosparql/SPEC.md#55-generation-switches-open-rebuild-persistence).

### 5.6 Planner

See the [section in its current document](G01-geosparql/SPEC.md#56-planner).

#### 5.6.1 FILTER pushdown with a constant geometry (Phase 1)

See the [section in its current document](G01-geosparql/SPEC.md#561-filter-pushdown-with-a-constant-geometry-phase-1).

#### 5.6.2 `spatial:` property functions (Phase 1)

See the [section in its current document](G01-geosparql/SPEC.md#562-spatial-property-functions-phase-1).

#### 5.6.3 Execution of `SpatialPf` and fallback

See the [section in its current document](G01-geosparql/SPEC.md#563-execution-of-spatialpf-and-fallback).

#### 5.6.4 Phase 2 operators

See the [section in its current document](G01-geosparql/SPEC.md#564-phase-2-operators).

### 5.7 Search kernels (`geo/search.rs`)

See the [section in its current document](G01-geosparql/SPEC.md#57-search-kernels-geosearchrs).

### 5.8 Functions in `expr.rs`

See the [section in its current document](G01-geosparql/SPEC.md#58-functions-in-exprrs).

### 5.9 Explain and profile

See the [section in its current document](G01-geosparql/SPEC.md#59-explain-and-profile).

## 6. Phasing

See the [section in its current document](G01-geosparql/SPEC.md#6-phasing).

### Phase 1 (about 7 days)

See the [section in its current document](G01-geosparql/SPEC.md#phase-1-about-7-days).

### Phase 2 (about 8 days)

See the [section in its current document](G01-geosparql/SPEC.md#phase-2-about-8-days).

### Phase 3 (later, on demand)

See the [section in its current document](G01-geosparql/SPEC.md#phase-3-later-on-demand).

## 7. Acceptance examples

See the [section in its current document](G01-geosparql/SPEC.md#7-acceptance-examples).

## 8. Conformance testing

See the [section in its current document](G01-geosparql/SPEC.md#8-conformance-testing).

## 9. Performance targets and benchmark plan

See the [section in its current document](G01-geosparql/SPEC.md#9-performance-targets-and-benchmark-plan).

## 10. Rejected alternatives

See the [section in its current document](G01-geosparql/SPEC.md#10-rejected-alternatives).

## 11. Open questions (defaults chosen here)

See the [section in its current document](G01-geosparql/SPEC.md#11-open-questions-defaults-chosen-here).

## 12. Sources

See the [section in its current document](G01-geosparql/SPEC.md#12-sources).

## 13. Provenance entry (for `PROVENANCE.md`)

See the [section in its current document](G01-geosparql/SPEC.md#13-provenance-entry-for-provenancemd).

## Outcome

See the [section in its current document](G01-geosparql/SPEC.md#outcome).

### Roadmap follow-ups (2026-10-12)

See the [section in its current document](G01-geosparql/OUTCOME.md#roadmap-follow-ups-2026-10-12).
