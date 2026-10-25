# F19: Mapping languages and virtual graphs

This spec has a [bundle landing page](F19-mappings-and-virtual-graphs/README.md).
The headings below preserve existing bookmarks and link to the corresponding
sections of the design and delivery record.

## 1. Summary, goals, non-goals

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#1-summary-goals-non-goals).

## 2. User-visible behaviour

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#2-user-visible-behaviour).

### 2.1 Command line

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#21-command-line).

### 2.2 Uploads and stored mappings

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#22-uploads-and-stored-mappings).

### 2.3 CSV in the Graph Store protocol

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#23-csv-in-the-graph-store-protocol).

### 2.4 Virtual graphs (Phase 2)

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#24-virtual-graphs-phase-2).

## 3. The mapping model

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#3-the-mapping-model).

### 3.1 One intermediate form

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#31-one-intermediate-form).

### 3.2 R2RML

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#32-r2rml).

### 3.3 RML

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#33-rml).

### 3.4 YARRRML

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#34-yarrrml).

### 3.5 CSVW and templates

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#35-csvw-and-templates).

## 4. Sources

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#4-sources).

### 4.1 CSV and TSV

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#41-csv-and-tsv).

### 4.2 JSON and JSON Lines

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#42-json-and-json-lines).

### 4.3 Parquet

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#43-parquet).

### 4.4 SQL connections

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#44-sql-connections).

## 5. Materialization

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#5-materialization).

### 5.1 Plan

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#51-plan).

### 5.2 SQL sources

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#52-sql-sources).

### 5.3 Joins

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#53-joins).

### 5.4 Limits and errors

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#54-limits-and-errors).

### 5.5 Running a stored mapping again

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#55-running-a-stored-mapping-again).

## 6. CSVW: standard mode and key checks

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#6-csvw-standard-mode-and-key-checks).

### 6.1 Standard mode

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#61-standard-mode).

### 6.2 Primary keys

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#62-primary-keys).

### 6.3 Foreign keys

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#63-foreign-keys).

## 7. Templates: Tarql functions and target-dataset lookups

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#7-templates-tarql-functions-and-target-dataset-lookups).

### 7.1 Tarql's functions

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#71-tarqls-functions).

### 7.2 Templates that read the target dataset

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#72-templates-that-read-the-target-dataset).

### 7.3 Functions in RML and YARRRML

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#73-functions-in-rml-and-yarrrml).

## 8. Stored mappings

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#8-stored-mappings).

## 9. Virtual graphs (Phase 2)

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#9-virtual-graphs-phase-2).

### 9.1 What the planner sees

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#91-what-the-planner-sees).

### 9.2 Unfolding

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#92-unfolding).

### 9.3 Template inversion

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#93-template-inversion).

### 9.4 Types and filters

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#94-types-and-filters).

### 9.5 Joins with stored data

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#95-joins-with-stored-data).

### 9.6 Parquet and Iceberg

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#96-parquet-and-iceberg).

### 9.7 Cost estimates

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#97-cost-estimates).

### 9.8 Consistency

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#98-consistency).

### 9.9 Caching

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#99-caching).

### 9.10 Access control and writes

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#910-access-control-and-writes).

### 9.11 Budgets and explanation

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#911-budgets-and-explanation).

## 10. Architecture

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#10-architecture).

## 11. Phases

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#11-phases).

## 12. Acceptance examples

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#12-acceptance-examples).

## 13. Testing and measurement

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#13-testing-and-measurement).

### 13.1 Conformance and equivalence

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#131-conformance-and-equivalence).

### 13.2 Performance

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#132-performance).

## 14. Rejected alternatives

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#14-rejected-alternatives).

## 15. Open questions

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#15-open-questions).

## 16. Sources

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#16-sources).

## Outcome

See the [section in its current document](F19-mappings-and-virtual-graphs/SPEC.md#outcome).
