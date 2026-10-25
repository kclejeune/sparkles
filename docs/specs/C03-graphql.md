# C03: GraphQL read adapter

This spec has a [bundle landing page](C03-graphql/README.md).
The headings below preserve existing bookmarks and link to the corresponding
sections of the design and delivery record.

## 1. Summary

See the [section in its current document](C03-graphql/SPEC.md#1-summary).

## 2. Overview

See the [section in its current document](C03-graphql/SPEC.md#2-overview).

## 3. Schemas

See the [section in its current document](C03-graphql/SPEC.md#3-schemas).

### 3.1 The mapping schema is configuration

See the [section in its current document](C03-graphql/SPEC.md#31-the-mapping-schema-is-configuration).

### 3.2 Directives

See the [section in its current document](C03-graphql/SPEC.md#32-directives).

### 3.3 Validation of a mapping schema

See the [section in its current document](C03-graphql/SPEC.md#33-validation-of-a-mapping-schema).

### 3.4 The API schema

See the [section in its current document](C03-graphql/SPEC.md#34-the-api-schema).

### 3.5 Drafts

See the [section in its current document](C03-graphql/SPEC.md#35-drafts).

## 4. Mapping rules

See the [section in its current document](C03-graphql/SPEC.md#4-mapping-rules).

### 4.1 Names

See the [section in its current document](C03-graphql/SPEC.md#41-names).

### 4.2 Identifiers

See the [section in its current document](C03-graphql/SPEC.md#42-identifiers).

### 4.3 Scalars

See the [section in its current document](C03-graphql/SPEC.md#43-scalars).

### 4.4 Single and multiple values

See the [section in its current document](C03-graphql/SPEC.md#44-single-and-multiple-values).

### 4.5 Language-tagged literals

See the [section in its current document](C03-graphql/SPEC.md#45-language-tagged-literals).

### 4.6 Inverse relations

See the [section in its current document](C03-graphql/SPEC.md#46-inverse-relations).

## 5. The query API

See the [section in its current document](C03-graphql/SPEC.md#5-the-query-api).

### 5.1 Root fields

See the [section in its current document](C03-graphql/SPEC.md#51-root-fields).

### 5.2 Connections

See the [section in its current document](C03-graphql/SPEC.md#52-connections).

### 5.3 Arguments of nested fields

See the [section in its current document](C03-graphql/SPEC.md#53-arguments-of-nested-fields).

### 5.4 Interfaces, unions and type conditions

See the [section in its current document](C03-graphql/SPEC.md#54-interfaces-unions-and-type-conditions).

### 5.5 Filters

See the [section in its current document](C03-graphql/SPEC.md#55-filters).

### 5.6 Ordering

See the [section in its current document](C03-graphql/SPEC.md#56-ordering).

## 6. Compilation and execution

See the [section in its current document](C03-graphql/SPEC.md#6-compilation-and-execution).

### 6.1 One snapshot

See the [section in its current document](C03-graphql/SPEC.md#61-one-snapshot).

### 6.2 Fetch groups

See the [section in its current document](C03-graphql/SPEC.md#62-fetch-groups).

### 6.3 Engine options and shared budgets

See the [section in its current document](C03-graphql/SPEC.md#63-engine-options-and-shared-budgets).

### 6.4 Explanation

See the [section in its current document](C03-graphql/SPEC.md#64-explanation).

### 6.5 Caching

See the [section in its current document](C03-graphql/SPEC.md#65-caching).

## 7. Limits

See the [section in its current document](C03-graphql/SPEC.md#7-limits).

### 7.1 Before execution

See the [section in its current document](C03-graphql/SPEC.md#71-before-execution).

### 7.2 During execution

See the [section in its current document](C03-graphql/SPEC.md#72-during-execution).

## 8. Introspection

See the [section in its current document](C03-graphql/SPEC.md#8-introspection).

## 9. Errors

See the [section in its current document](C03-graphql/SPEC.md#9-errors).

### 9.1 Response format

See the [section in its current document](C03-graphql/SPEC.md#91-response-format).

### 9.2 Execution errors and null propagation

See the [section in its current document](C03-graphql/SPEC.md#92-execution-errors-and-null-propagation).

### 9.3 HTTP status

See the [section in its current document](C03-graphql/SPEC.md#93-http-status).

## 10. HTTP and access control

See the [section in its current document](C03-graphql/SPEC.md#10-http-and-access-control).

### 10.1 Routes

See the [section in its current document](C03-graphql/SPEC.md#101-routes).

### 10.2 Permissions

See the [section in its current document](C03-graphql/SPEC.md#102-permissions).

### 10.3 Observability

See the [section in its current document](C03-graphql/SPEC.md#103-observability).

## 11. Stored GraphQL queries

See the [section in its current document](C03-graphql/SPEC.md#11-stored-graphql-queries).

## 12. Mutations are out of scope

See the [section in its current document](C03-graphql/SPEC.md#12-mutations-are-out-of-scope).

## 13. Subscriptions

See the [section in its current document](C03-graphql/SPEC.md#13-subscriptions).

## 14. User interfaces

See the [section in its current document](C03-graphql/SPEC.md#14-user-interfaces).

### 14.1 Command line

See the [section in its current document](C03-graphql/SPEC.md#141-command-line).

### 14.2 Web UI (Phase 2)

See the [section in its current document](C03-graphql/SPEC.md#142-web-ui-phase-2).

### 14.3 Library

See the [section in its current document](C03-graphql/SPEC.md#143-library).

## 15. Performance expectations

See the [section in its current document](C03-graphql/SPEC.md#15-performance-expectations).

## 16. Phasing

See the [section in its current document](C03-graphql/SPEC.md#16-phasing).

### Phase 1: the adapter

See the [section in its current document](C03-graphql/SPEC.md#phase-1-the-adapter).

### Phase 2: tools and speed

See the [section in its current document](C03-graphql/SPEC.md#phase-2-tools-and-speed).

### Phase 3: subscriptions

See the [section in its current document](C03-graphql/SPEC.md#phase-3-subscriptions).

## 17. Rejected alternatives

See the [section in its current document](C03-graphql/SPEC.md#17-rejected-alternatives).

## 18. Acceptance examples

See the [section in its current document](C03-graphql/SPEC.md#18-acceptance-examples).

## 19. Open questions

See the [section in its current document](C03-graphql/SPEC.md#19-open-questions).

## 20. Sources

See the [section in its current document](C03-graphql/SPEC.md#20-sources).

## Outcome

See the [section in its current document](C03-graphql/SPEC.md#outcome).

### Roadmap follow-ups (2026-10-12)

See the [section in its current document](C03-graphql/OUTCOME.md#roadmap-follow-ups-2026-10-12).

### Completion contract

See the [section in its current document](C03-graphql/OUTCOME.md#completion-contract).
