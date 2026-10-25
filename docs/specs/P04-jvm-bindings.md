# P04: JVM bindings behind Apache Jena's API

This spec has a [bundle landing page](P04-jvm-bindings/README.md).
The headings below preserve existing bookmarks and link to the corresponding
sections of the design and delivery record.

## 1. Summary, goals, non-goals

See the [section in its current document](P04-jvm-bindings/SPEC.md#1-summary-goals-non-goals).

## 2. Layers and build

See the [section in its current document](P04-jvm-bindings/SPEC.md#2-layers-and-build).

### 2.1 Layout

See the [section in its current document](P04-jvm-bindings/SPEC.md#21-layout).

### 2.2 The `sparkles-ffi` crate

See the [section in its current document](P04-jvm-bindings/SPEC.md#22-the-sparkles-ffi-crate).

### 2.3 What crosses the boundary

See the [section in its current document](P04-jvm-bindings/SPEC.md#23-what-crosses-the-boundary).

### 2.4 The batch encoding

See the [section in its current document](P04-jvm-bindings/SPEC.md#24-the-batch-encoding).

### 2.5 Generated Kotlin bindings

See the [section in its current document](P04-jvm-bindings/SPEC.md#25-generated-kotlin-bindings).

### 2.6 The `sparkles-jena` library

See the [section in its current document](P04-jvm-bindings/SPEC.md#26-the-sparkles-jena-library).

## 3. Jena integration

See the [section in its current document](P04-jvm-bindings/SPEC.md#3-jena-integration).

### 3.1 The dataset

See the [section in its current document](P04-jvm-bindings/SPEC.md#31-the-dataset).

### 3.2 Graphs, prefixes and blank nodes

See the [section in its current document](P04-jvm-bindings/SPEC.md#32-graphs-prefixes-and-blank-nodes).

### 3.3 Transactions

See the [section in its current document](P04-jvm-bindings/SPEC.md#33-transactions).

### 3.4 The query engine

See the [section in its current document](P04-jvm-bindings/SPEC.md#34-the-query-engine).

### 3.5 Falling back to ARQ

See the [section in its current document](P04-jvm-bindings/SPEC.md#35-falling-back-to-arq).

### 3.6 The update engine

See the [section in its current document](P04-jvm-bindings/SPEC.md#36-the-update-engine).

### 3.7 Union default graph

See the [section in its current document](P04-jvm-bindings/SPEC.md#37-union-default-graph).

### 3.8 Context symbols

See the [section in its current document](P04-jvm-bindings/SPEC.md#38-context-symbols).

### 3.9 The assembler and Fuseki

See the [section in its current document](P04-jvm-bindings/SPEC.md#39-the-assembler-and-fuseki).

### 3.10 Models, listeners and inference

See the [section in its current document](P04-jvm-bindings/SPEC.md#310-models-listeners-and-inference).

### 3.11 Differences from TDB2

See the [section in its current document](P04-jvm-bindings/SPEC.md#311-differences-from-tdb2).

## 4. The Java API

See the [section in its current document](P04-jvm-bindings/SPEC.md#4-the-java-api).

### 4.1 Entry points and options

See the [section in its current document](P04-jvm-bindings/SPEC.md#41-entry-points-and-options).

### 4.2 Java interop rules

See the [section in its current document](P04-jvm-bindings/SPEC.md#42-java-interop-rules).

### 4.3 Exceptions

See the [section in its current document](P04-jvm-bindings/SPEC.md#43-exceptions).

### 4.4 Lifetimes and closing

See the [section in its current document](P04-jvm-bindings/SPEC.md#44-lifetimes-and-closing).

### 4.5 Sparkles features beyond SPARQL

See the [section in its current document](P04-jvm-bindings/SPEC.md#45-sparkles-features-beyond-sparql).

### 4.6 Migrating from TDB2

See the [section in its current document](P04-jvm-bindings/SPEC.md#46-migrating-from-tdb2).

## 5. Performance

See the [section in its current document](P04-jvm-bindings/SPEC.md#5-performance).

### 5.1 Where the bridge's overhead matters

See the [section in its current document](P04-jvm-bindings/SPEC.md#51-where-the-bridges-overhead-matters).

### 5.2 Batch sizes and memory

See the [section in its current document](P04-jvm-bindings/SPEC.md#52-batch-sizes-and-memory).

### 5.3 Targets

See the [section in its current document](P04-jvm-bindings/SPEC.md#53-targets).

### 5.4 Benchmark plan

See the [section in its current document](P04-jvm-bindings/SPEC.md#54-benchmark-plan).

## 6. Packaging

See the [section in its current document](P04-jvm-bindings/SPEC.md#6-packaging).

### 6.1 Versions

See the [section in its current document](P04-jvm-bindings/SPEC.md#61-versions).

### 6.2 Artifacts and native libraries

See the [section in its current document](P04-jvm-bindings/SPEC.md#62-artifacts-and-native-libraries).

### 6.3 Loading the native library

See the [section in its current document](P04-jvm-bindings/SPEC.md#63-loading-the-native-library).

### 6.4 Nix

See the [section in its current document](P04-jvm-bindings/SPEC.md#64-nix).

### 6.5 CI and publishing

See the [section in its current document](P04-jvm-bindings/SPEC.md#65-ci-and-publishing).

## 7. Testing

See the [section in its current document](P04-jvm-bindings/SPEC.md#7-testing).

## 8. Phasing

See the [section in its current document](P04-jvm-bindings/SPEC.md#8-phasing).

## 9. Acceptance examples

See the [section in its current document](P04-jvm-bindings/SPEC.md#9-acceptance-examples).

## 10. Rejected alternatives

See the [section in its current document](P04-jvm-bindings/SPEC.md#10-rejected-alternatives).

## 11. Open questions

See the [section in its current document](P04-jvm-bindings/SPEC.md#11-open-questions).

## 12. Sources

See the [section in its current document](P04-jvm-bindings/SPEC.md#12-sources).

## Outcome

See the [section in its current document](P04-jvm-bindings/SPEC.md#outcome).

### Graph overview

See the [section in its current document](P04-jvm-bindings/OUTCOME.md#graph-overview).

### Roadmap follow-ups (2026-10-12)

See the [section in its current document](P04-jvm-bindings/OUTCOME.md#roadmap-follow-ups-2026-10-12).

### Remaining integration contract

See the [section in its current document](P04-jvm-bindings/OUTCOME.md#remaining-integration-contract).
