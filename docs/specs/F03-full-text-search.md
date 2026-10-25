# F03: Full-text search (Tantivy, `text:query`)

This spec has a [bundle landing page](F03-full-text-search/README.md).
The headings below preserve existing bookmarks and link to the corresponding
sections of the design and delivery record.

## 1. Summary

See the [section in its current document](F03-full-text-search/SPEC.md#1-summary).

### Goals

See the [section in its current document](F03-full-text-search/SPEC.md#goals).

### Non-goals (all phases unless noted)

See the [section in its current document](F03-full-text-search/SPEC.md#non-goals-all-phases-unless-noted).

## 2. User-visible behavior

See the [section in its current document](F03-full-text-search/SPEC.md#2-user-visible-behavior).

### 2.1 SPARQL

See the [section in its current document](F03-full-text-search/SPEC.md#21-sparql).

### 2.2 HTTP (`/$/` extensions, documented in `docs/API.md`)

See the [section in its current document](F03-full-text-search/SPEC.md#22-http--extensions-documented-in-docsapimd).

### 2.3 CLI

See the [section in its current document](F03-full-text-search/SPEC.md#23-cli).

### 2.4 Rust API (`sparkles`, feature `text`)

See the [section in its current document](F03-full-text-search/SPEC.md#24-rust-api-sparkles-feature-text).

### 2.5 UI (SvelteKit, `ui/`, Phase 2)

See the [section in its current document](F03-full-text-search/SPEC.md#25-ui-sveltekit-ui-phase-2).

## 3. Standards basis

See the [section in its current document](F03-full-text-search/SPEC.md#3-standards-basis).

## 4. Semantics

See the [section in its current document](F03-full-text-search/SPEC.md#4-semantics).

### 4.1 What is indexed

See the [section in its current document](F03-full-text-search/SPEC.md#41-what-is-indexed).

### 4.2 Consistency

See the [section in its current document](F03-full-text-search/SPEC.md#42-consistency).

### 4.3 Query string (Phase 1 subset)

See the [section in its current document](F03-full-text-search/SPEC.md#43-query-string-phase-1-subset).

### 4.4 Errors (all `{ "error": … }` bodies per `docs/API.md`)

See the [section in its current document](F03-full-text-search/SPEC.md#44-errors-all--error---bodies-per-docsapimd).

### 4.5 Limits and budgets

See the [section in its current document](F03-full-text-search/SPEC.md#45-limits-and-budgets).

## 5. Design

See the [section in its current document](F03-full-text-search/SPEC.md#5-design).

### 5.1 Modules

See the [section in its current document](F03-full-text-search/SPEC.md#51-modules).

### 5.2 Persistence (format version 1)

See the [section in its current document](F03-full-text-search/SPEC.md#52-persistence-format-version-1).

### 5.3 Commit path (`WriteTxn::publish_log`)

See the [section in its current document](F03-full-text-search/SPEC.md#53-commit-path-writetxnpublish_log).

### 5.4 Generation switches (`rebuild_locked`)

See the [section in its current document](F03-full-text-search/SPEC.md#54-generation-switches-rebuild_locked).

### 5.5 Full rebuild

See the [section in its current document](F03-full-text-search/SPEC.md#55-full-rebuild).

### 5.6 Open and recovery (`Store::open`, before WAL replay)

See the [section in its current document](F03-full-text-search/SPEC.md#56-open-and-recovery-storeopen-before-wal-replay).

### 5.7 Search (`text::search`)

See the [section in its current document](F03-full-text-search/SPEC.md#57-search-textsearch).

## 6. Phasing

See the [section in its current document](F03-full-text-search/SPEC.md#6-phasing).

### Phase 1 (MVP, about one day)

See the [section in its current document](F03-full-text-search/SPEC.md#phase-1-mvp-about-one-day).

### Phase 2

See the [section in its current document](F03-full-text-search/SPEC.md#phase-2).

### Phase 3 and later

See the [section in its current document](F03-full-text-search/SPEC.md#phase-3-and-later).

## 7. Acceptance examples

See the [section in its current document](F03-full-text-search/SPEC.md#7-acceptance-examples).

## 8. Rejected alternatives

See the [section in its current document](F03-full-text-search/SPEC.md#8-rejected-alternatives).

## 9. Open questions (defaults chosen here)

See the [section in its current document](F03-full-text-search/SPEC.md#9-open-questions-defaults-chosen-here).

## 10. Sources

See the [section in its current document](F03-full-text-search/SPEC.md#10-sources).

## Outcome

See the [section in its current document](F03-full-text-search/SPEC.md#outcome).

### Roadmap follow-ups (2026-10-12)

See the [section in its current document](F03-full-text-search/OUTCOME.md#roadmap-follow-ups-2026-10-12).
