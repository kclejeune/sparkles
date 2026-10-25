# F05 — Backup repositories: incremental backup and restore, policies, GC

This spec has a [bundle landing page](F05-snapshot-repositories/README.md).
The headings below preserve existing bookmarks and link to the corresponding
sections of the design and delivery record.

## 1. Summary, goals, non-goals

See the [section in its current document](F05-snapshot-repositories/SPEC.md#1-summary-goals-non-goals).

## 2. User-visible behavior

See the [section in its current document](F05-snapshot-repositories/SPEC.md#2-user-visible-behavior).

### 2.1 Repositories

See the [section in its current document](F05-snapshot-repositories/SPEC.md#21-repositories).

### 2.2 Backups

See the [section in its current document](F05-snapshot-repositories/SPEC.md#22-backups).

### 2.3 Restore

See the [section in its current document](F05-snapshot-repositories/SPEC.md#23-restore).

### 2.4 Verify

See the [section in its current document](F05-snapshot-repositories/SPEC.md#24-verify).

### 2.5 HTTP API

See the [section in its current document](F05-snapshot-repositories/SPEC.md#25-http-api).

### 2.6 Lifecycle policies (Phase 2)

See the [section in its current document](F05-snapshot-repositories/SPEC.md#26-lifecycle-policies-phase-2).

### 2.7 Garbage collection (Phase 2)

See the [section in its current document](F05-snapshot-repositories/SPEC.md#27-garbage-collection-phase-2).

### 2.8 CLI

See the [section in its current document](F05-snapshot-repositories/SPEC.md#28-cli).

### 2.9 Config file (`--backup-config`, TOML)

See the [section in its current document](F05-snapshot-repositories/SPEC.md#29-config-file---backup-config-toml).

### 2.10 Limits, retries, cancellation

See the [section in its current document](F05-snapshot-repositories/SPEC.md#210-limits-retries-cancellation).

### 2.11 Observability

See the [section in its current document](F05-snapshot-repositories/SPEC.md#211-observability).

## 3. Standards and behavioral basis

See the [section in its current document](F05-snapshot-repositories/SPEC.md#3-standards-and-behavioral-basis).

## 4. Semantics

See the [section in its current document](F05-snapshot-repositories/SPEC.md#4-semantics).

### 4.1 Invariants

See the [section in its current document](F05-snapshot-repositories/SPEC.md#41-invariants).

### 4.2 Commit identity in the manifest

See the [section in its current document](F05-snapshot-repositories/SPEC.md#42-commit-identity-in-the-manifest).

### 4.3 What "restore in place when stopped" means

See the [section in its current document](F05-snapshot-repositories/SPEC.md#43-what-restore-in-place-when-stopped-means).

## 5. Design

See the [section in its current document](F05-snapshot-repositories/SPEC.md#5-design).

### 5.1 Crates and modules

See the [section in its current document](F05-snapshot-repositories/SPEC.md#51-crates-and-modules).

### 5.2 Repository layout (format 1)

See the [section in its current document](F05-snapshot-repositories/SPEC.md#52-repository-layout-format-1).

### 5.3 Capture (consistency without blocking writers)

See the [section in its current document](F05-snapshot-repositories/SPEC.md#53-capture-consistency-without-blocking-writers).

### 5.4 Blob planning, dedup and incrementality

See the [section in its current document](F05-snapshot-repositories/SPEC.md#54-blob-planning-dedup-and-incrementality).

### 5.5 Create

See the [section in its current document](F05-snapshot-repositories/SPEC.md#55-create).

### 5.6 Listing and the manifest cache

See the [section in its current document](F05-snapshot-repositories/SPEC.md#56-listing-and-the-manifest-cache).

### 5.7 Locks

See the [section in its current document](F05-snapshot-repositories/SPEC.md#57-locks).

### 5.8 Restore validation (untrusted input)

See the [section in its current document](F05-snapshot-repositories/SPEC.md#58-restore-validation-untrusted-input).

### 5.9 Changes to existing code

See the [section in its current document](F05-snapshot-repositories/SPEC.md#59-changes-to-existing-code).

## 6. UI (SvelteKit, `ui/`)

See the [section in its current document](F05-snapshot-repositories/SPEC.md#6-ui-sveltekit-ui).

## 7. Phasing

See the [section in its current document](F05-snapshot-repositories/SPEC.md#7-phasing).

## 8. Rejected alternatives

See the [section in its current document](F05-snapshot-repositories/SPEC.md#8-rejected-alternatives).

## 9. Acceptance examples

See the [section in its current document](F05-snapshot-repositories/SPEC.md#9-acceptance-examples).

## 10. Performance targets and measurement

See the [section in its current document](F05-snapshot-repositories/SPEC.md#10-performance-targets-and-measurement).

## 11. Security

See the [section in its current document](F05-snapshot-repositories/SPEC.md#11-security).

## 12. Open questions

See the [section in its current document](F05-snapshot-repositories/SPEC.md#12-open-questions).

## 13. Sources

See the [section in its current document](F05-snapshot-repositories/SPEC.md#13-sources).

## Outcome

See the [section in its current document](F05-snapshot-repositories/SPEC.md#outcome).

### Roadmap follow-ups (2026-10-12)

See the [section in its current document](F05-snapshot-repositories/OUTCOME.md#roadmap-follow-ups-2026-10-12).

### Remaining history and retention contract

See the [section in its current document](F05-snapshot-repositories/OUTCOME.md#remaining-history-and-retention-contract).

### Recovery of managed definitions

See the [section in its current document](F05-snapshot-repositories/OUTCOME.md#recovery-of-managed-definitions).
