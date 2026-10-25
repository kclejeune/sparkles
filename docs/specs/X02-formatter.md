# X02: Opinionated formatter (`sparkles fmt`, `POST /$/format`, the UI's Format button)

This spec has a [bundle landing page](X02-formatter/README.md).
The headings below preserve existing bookmarks and link to the corresponding
sections of the design and delivery record.

## 1. Summary, goals, non-goals

See the [section in its current document](X02-formatter/SPEC.md#1-summary-goals-non-goals).

## 2. User-visible behavior

See the [section in its current document](X02-formatter/SPEC.md#2-user-visible-behavior).

### 2.1 CLI

See the [section in its current document](X02-formatter/SPEC.md#21-cli).

### 2.2 Configuration and ignore files

See the [section in its current document](X02-formatter/SPEC.md#22-configuration-and-ignore-files).

### 2.3 Language detection

See the [section in its current document](X02-formatter/SPEC.md#23-language-detection).

### 2.4 HTTP: `POST /$/format`

See the [section in its current document](X02-formatter/SPEC.md#24-http-post-format).

### 2.5 UI

See the [section in its current document](X02-formatter/SPEC.md#25-ui).

## 3. Formatting model (shared by all languages)

See the [section in its current document](X02-formatter/SPEC.md#3-formatting-model-shared-by-all-languages).

### 3.1 Invariants: what formatting may change

See the [section in its current document](X02-formatter/SPEC.md#31-invariants-what-formatting-may-change).

### 3.2 Document IR and printer

See the [section in its current document](X02-formatter/SPEC.md#32-document-ir-and-printer).

### 3.3 Comments and blank lines

See the [section in its current document](X02-formatter/SPEC.md#33-comments-and-blank-lines).

### 3.4 Pragmas

See the [section in its current document](X02-formatter/SPEC.md#34-pragmas).

### 3.5 Cursor mapping (Prettier's `cursorOffset`)

See the [section in its current document](X02-formatter/SPEC.md#35-cursor-mapping-prettiers-cursoroffset).

## 4. Rules per language

See the [section in its current document](X02-formatter/SPEC.md#4-rules-per-language).

### 4.1 SPARQL 1.2 (queries and updates)

See the [section in its current document](X02-formatter/SPEC.md#41-sparql-12-queries-and-updates).

#### Example S1: SELECT with OPTIONAL, FILTER, UNION and comments

See the [section in its current document](X02-formatter/SPEC.md#example-s1-select-with-optional-filter-union-and-comments).

#### Example S2: aggregates, a subquery and property paths

See the [section in its current document](X02-formatter/SPEC.md#example-s2-aggregates-a-subquery-and-property-paths).

#### Example S3: VALUES, BIND, MINUS, NOT EXISTS, a long FILTER and `$` variables

See the [section in its current document](X02-formatter/SPEC.md#example-s3-values-bind-minus-not-exists-a-long-filter-and--variables).

#### Example S4: CONSTRUCT

See the [section in its current document](X02-formatter/SPEC.md#example-s4-construct).

#### Example S5: update with DELETE/INSERT WHERE and INSERT DATA

See the [section in its current document](X02-formatter/SPEC.md#example-s5-update-with-deleteinsert-where-and-insert-data).

#### Example S6: SPARQL 1.2 reification, comments in a list, and an ignore pragma

See the [section in its current document](X02-formatter/SPEC.md#example-s6-sparql-12-reification-comments-in-a-list-and-an-ignore-pragma).

### 4.2 Turtle 1.2 and TriG 1.2 (and SHACL shapes graphs)

See the [section in its current document](X02-formatter/SPEC.md#42-turtle-12-and-trig-12-and-shacl-shapes-graphs).

#### Example T1: prefixes, `rdf:type`, literal shorthands, comments and blank lines

See the [section in its current document](X02-formatter/SPEC.md#example-t1-prefixes-rdftype-literal-shorthands-comments-and-blank-lines).

#### Example T2: long object lists, collections, nested blank nodes and long strings

See the [section in its current document](X02-formatter/SPEC.md#example-t2-long-object-lists-collections-nested-blank-nodes-and-long-strings).

#### Example T3: TriG with RDF 1.2 annotations, reified triples and triple terms

See the [section in its current document](X02-formatter/SPEC.md#example-t3-trig-with-rdf-12-annotations-reified-triples-and-triple-terms).

#### Example T4: `--sort` (opt-in) with sections

See the [section in its current document](X02-formatter/SPEC.md#example-t4---sort-opt-in-with-sections).

#### Example T5: a SHACL shapes graph

See the [section in its current document](X02-formatter/SPEC.md#example-t5-a-shacl-shapes-graph).

#### Example T6: T1 under non-default keys

See the [section in its current document](X02-formatter/SPEC.md#example-t6-t1-under-non-default-keys).

### 4.3 N-Triples 1.2 and N-Quads 1.2

See the [section in its current document](X02-formatter/SPEC.md#43-n-triples-12-and-n-quads-12).

#### Example L1: N-Quads

See the [section in its current document](X02-formatter/SPEC.md#example-l1-n-quads).

### 4.4 JSON-LD 1.1 (Phase 2)

See the [section in its current document](X02-formatter/SPEC.md#44-json-ld-11-phase-2).

#### Example J1

See the [section in its current document](X02-formatter/SPEC.md#example-j1).

### 4.5 RDF/XML: rejected

See the [section in its current document](X02-formatter/SPEC.md#45-rdfxml-rejected).

## 5. Normalizations

See the [section in its current document](X02-formatter/SPEC.md#5-normalizations).

## 6. Safety checks

See the [section in its current document](X02-formatter/SPEC.md#6-safety-checks).

## 7. Architecture and crate layout

See the [section in its current document](X02-formatter/SPEC.md#7-architecture-and-crate-layout).

### 7.1 A lossless syntax tree

See the [section in its current document](X02-formatter/SPEC.md#71-a-lossless-syntax-tree).

### 7.2 oxfmt and JSON-LD

See the [section in its current document](X02-formatter/SPEC.md#72-oxfmt-and-json-ld).

### 7.3 Crates, API, dependencies

See the [section in its current document](X02-formatter/SPEC.md#73-crates-api-dependencies).

### 7.4 WASM target (Phase 3)

See the [section in its current document](X02-formatter/SPEC.md#74-wasm-target-phase-3).

### 7.5 Editor integrations (Phase 2)

See the [section in its current document](X02-formatter/SPEC.md#75-editor-integrations-phase-2).

## 8. Performance

See the [section in its current document](X02-formatter/SPEC.md#8-performance).

## 9. Testing

See the [section in its current document](X02-formatter/SPEC.md#9-testing).

## 10. Phasing and estimates

See the [section in its current document](X02-formatter/SPEC.md#10-phasing-and-estimates).

## 11. Acceptance examples

See the [section in its current document](X02-formatter/SPEC.md#11-acceptance-examples).

## 12. Rejected alternatives

See the [section in its current document](X02-formatter/SPEC.md#12-rejected-alternatives).

## 13. Decisions and open questions

See the [section in its current document](X02-formatter/SPEC.md#13-decisions-and-open-questions).

### 13.1 Decided (2026-09-30)

See the [section in its current document](X02-formatter/SPEC.md#131-decided-2026-09-30).

### 13.2 Open

See the [section in its current document](X02-formatter/SPEC.md#132-open).

## 14. Sources

See the [section in its current document](X02-formatter/SPEC.md#14-sources).

## Outcome

See the [section in its current document](X02-formatter/SPEC.md#outcome).

### Roadmap follow-ups (2026-10-12)

See the [section in its current document](X02-formatter/OUTCOME.md#roadmap-follow-ups-2026-10-12).
