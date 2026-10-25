# G02: ShEx validation

This spec has a [bundle landing page](G02-shex/README.md).
The headings below preserve existing bookmarks and link to the corresponding
sections of the design and delivery record.

## 1. Summary, goals, non-goals

See the [section in its current document](G02-shex/SPEC.md#1-summary-goals-non-goals).

## 2. User-visible behavior

See the [section in its current document](G02-shex/SPEC.md#2-user-visible-behavior).

### 2.1 Schemas

See the [section in its current document](G02-shex/SPEC.md#21-schemas).

### 2.2 Shape maps

See the [section in its current document](G02-shex/SPEC.md#22-shape-maps).

### 2.3 HTTP: `POST /{ds}/shex`

See the [section in its current document](G02-shex/SPEC.md#23-http-post-dsshex).

### 2.4 Result format

See the [section in its current document](G02-shex/SPEC.md#24-result-format).

### 2.5 CLI

See the [section in its current document](G02-shex/SPEC.md#25-cli).

### 2.6 Rust library API

See the [section in its current document](G02-shex/SPEC.md#26-rust-library-api).

### 2.7 Write-time validation with ShEx (Phase 2)

See the [section in its current document](G02-shex/SPEC.md#27-write-time-validation-with-shex-phase-2).

### 2.8 UI

See the [section in its current document](G02-shex/SPEC.md#28-ui).

### 2.9 Routing, auth, limits, metrics

See the [section in its current document](G02-shex/SPEC.md#29-routing-auth-limits-metrics).

## 3. Standards basis

See the [section in its current document](G02-shex/SPEC.md#3-standards-basis).

### 3.1 ShEx 2.1 (Final Community Group Report, 2019-10-08, shex.io/shex-semantics)

See the [section in its current document](G02-shex/SPEC.md#31-shex-21-final-community-group-report-2019-10-08-shexioshex-semantics).

### 3.2 Readings where the 2.1 text and the test suite differ

See the [section in its current document](G02-shex/SPEC.md#32-readings-where-the-21-text-and-the-test-suite-differ).

### 3.3 Apache Jena `jena-shex` (Apache-2.0), and where Sparkles differs

See the [section in its current document](G02-shex/SPEC.md#33-apache-jena-jena-shex-apache-20-and-where-sparkles-differs).

## 4. Semantics

See the [section in its current document](G02-shex/SPEC.md#4-semantics).

### 4.1 Schema checks (at compile time)

See the [section in its current document](G02-shex/SPEC.md#41-schema-checks-at-compile-time).

### 4.2 Imports

See the [section in its current document](G02-shex/SPEC.md#42-imports).

### 4.3 Node constraints

See the [section in its current document](G02-shex/SPEC.md#43-node-constraints).

### 4.4 Shapes and triple expressions

See the [section in its current document](G02-shex/SPEC.md#44-shapes-and-triple-expressions).

### 4.5 Typing, recursion and negation

See the [section in its current document](G02-shex/SPEC.md#45-typing-recursion-and-negation).

### 4.6 Semantic actions, annotations, EXTERNAL, START

See the [section in its current document](G02-shex/SPEC.md#46-semantic-actions-annotations-external-start).

### 4.7 Shape-map expansion

See the [section in its current document](G02-shex/SPEC.md#47-shape-map-expansion).

### 4.8 Errors

See the [section in its current document](G02-shex/SPEC.md#48-errors).

## 5. Validation algorithm

See the [section in its current document](G02-shex/SPEC.md#5-validation-algorithm).

### 5.1 Pipeline

See the [section in its current document](G02-shex/SPEC.md#51-pipeline).

### 5.2 Compilation (IR)

See the [section in its current document](G02-shex/SPEC.md#52-compilation-ir).

### 5.3 Neighbourhoods from snapshots

See the [section in its current document](G02-shex/SPEC.md#53-neighbourhoods-from-snapshots).

### 5.4 Node constraints on ids

See the [section in its current document](G02-shex/SPEC.md#54-node-constraints-on-ids).

### 5.5 Matching a neighbourhood

See the [section in its current document](G02-shex/SPEC.md#55-matching-a-neighbourhood).

### 5.6 Typing: stratified worklist refinement

See the [section in its current document](G02-shex/SPEC.md#56-typing-stratified-worklist-refinement).

#### 5.6.1 Discovery

See the [section in its current document](G02-shex/SPEC.md#561-discovery).

#### 5.6.2 Refinement per stratum

See the [section in its current document](G02-shex/SPEC.md#562-refinement-per-stratum).

### 5.7 Parallelism

See the [section in its current document](G02-shex/SPEC.md#57-parallelism).

### 5.8 Reasons (explain mode)

See the [section in its current document](G02-shex/SPEC.md#58-reasons-explain-mode).

### 5.9 Budgets, timeouts, cancellation

See the [section in its current document](G02-shex/SPEC.md#59-budgets-timeouts-cancellation).

## 6. Design sketch

See the [section in its current document](G02-shex/SPEC.md#6-design-sketch).

### 6.1 Crate `sparkles-shex`

See the [section in its current document](G02-shex/SPEC.md#61-crate-sparkles-shex).

### 6.2 Shared code moved out of `sparkles-shacl`

See the [section in its current document](G02-shex/SPEC.md#62-shared-code-moved-out-of-sparkles-shacl).

### 6.3 Server plumbing

See the [section in its current document](G02-shex/SPEC.md#63-server-plumbing).

### 6.4 Guard (Phase 2)

See the [section in its current document](G02-shex/SPEC.md#64-guard-phase-2).

## 7. rudof: depend, reuse, or neither

See the [section in its current document](G02-shex/SPEC.md#7-rudof-depend-reuse-or-neither).

## 8. Conformance testing

See the [section in its current document](G02-shex/SPEC.md#8-conformance-testing).

## 9. Performance targets and benchmark

See the [section in its current document](G02-shex/SPEC.md#9-performance-targets-and-benchmark).

## 10. Phasing

See the [section in its current document](G02-shex/SPEC.md#10-phasing).

## 11. Acceptance examples

See the [section in its current document](G02-shex/SPEC.md#11-acceptance-examples).

## 12. Rejected alternatives

See the [section in its current document](G02-shex/SPEC.md#12-rejected-alternatives).

## 13. Open questions

See the [section in its current document](G02-shex/SPEC.md#13-open-questions).

## 14. Sources

See the [section in its current document](G02-shex/SPEC.md#14-sources).

## Appendix: provenance entry (for [PROVENANCE.md](PROVENANCE.md))

See the [section in its current document](G02-shex/SPEC.md#appendix-provenance-entry-for-provenancemd).

## Implementation note (2026-10-01): matcher algorithm

See the [section in its current document](G02-shex/SPEC.md#implementation-note-2026-10-01-matcher-algorithm).

## Outcome

See the [section in its current document](G02-shex/SPEC.md#outcome).

### Roadmap follow-ups (2026-10-12)

See the [section in its current document](G02-shex/OUTCOME.md#roadmap-follow-ups-2026-10-12).
