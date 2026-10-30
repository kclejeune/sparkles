# F05 — Backup repositories: incremental backup and restore, policies, GC

> **Status:** implemented in part (Phase 1, most of Phase 2)

Per-dataset repository backups, restore, verification and policies are available, with optional encrypted repositories through F11. Backups and persistent clones carry view definitions, which come back paused. Whole-server capture, repository transfer and retained history remain under items 61 and 65, and in-memory clones of views under item 401.

## Documents

* [Design and acceptance](SPEC.md).
* [Delivery record](OUTCOME.md).
* [Provenance](PROVENANCE.md).

## User guides

[API: Backup repositories](../../API.md#backup-repositories) ·
[Usage: Backup repositories](../../USAGE.md#backup-repositories) ·
[Features](../../operations/README.md#operations) ·
[Benchmarks: Backup repositories](../../benchmarks/writes/RESULTS.md#backup-repositories-105m-triples)

The [roadmap](../../ROADMAP.md#completion-paths-for-partially-complete-specs)
tracks remaining work. The delivery record preserves the detailed phase history.
