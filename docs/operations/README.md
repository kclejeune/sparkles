# Deployment and operations

Sparkles runs as a single binary packaged for NixOS, Home Manager, Docker, and Kubernetes (Helm), with layered configuration, outbound notifications, shared memory pools and quotas, Prometheus and OpenTelemetry observability, incremental encrypted backups, and degraded-safe startup and migration. The [capability matrix](../FEATURES.md#capability-status) records delivery status for each area.

| Section | What it covers |
|---|---|
| [Deployment](#deployment) | NixOS, Home Manager, Docker/OCI images, Helm chart, and server artifact candidates. |
| [Settings and notifications](#settings-and-notifications) | Layered dataset/server settings, sealed secrets, declared prefixes, and outbound notifications. |
| [Memory and resource limits](#memory-and-resource-limits) | Shared caches, derived-table cache, query admission pool, per-query budgets, and storage quotas. |
| [Observability](#observability) | Audit events, structured access logs, Prometheus metrics, and OpenTelemetry traces/metrics/logs. |
| [Backups](#backups) | Deduplicated file-system and S3 backup repositories, encryption, verification, and lifecycle policies. |
| [Upgrades and on-disk formats](#upgrades-and-on-disk-formats) | Format registry, `sparkles migrate`, parallel degraded startup, health probes, and graceful shutdown. |
| [Interrupted dataset deletes](#interrupted-dataset-deletes) | Recovery decisions, retained copies and startup warnings. |
| [API compatibility](#api-compatibility) | Compatibility policy and current development-baseline checks. |

## Operations

### Deployment

* **NixOS and Home Manager.** The Nix flake builds `sparkles` with the embedded web UI, `sparkles-fmt-wasm`, shell completions, and man pages. The NixOS module `services.sparkles` runs a hardened systemd service covering datasets, authentication, backups, TLS, metrics, OpenTelemetry, and an optional nginx virtual host ([USAGE.md](../USAGE.md#deploying-on-nixos)). The Home Manager module `programs.sparkles` installs the CLI, generates memory, backup, and formatter configs, and configures Claude Code and Codex memory hooks and skills ([USAGE.md](../USAGE.md#home-manager)).
* **Docker and OCI images.** `Dockerfile` builds an unprivileged image containing the server and UI, and `compose.yaml` binds loopback with a named volume ([USAGE.md](../USAGE.md#docker)). The `ocr` target bundles pinned, hash-verified PDFium and ONNX Runtime libraries; the flake builds both targets as OCI images.
* **Kubernetes (Helm).** `deploy/helm/sparkles` deploys a single-replica StatefulSet with a read-only root filesystem, Values/Secrets configuration, three options for supplying local embedding models, and live reloads via `serve --watch-config` ([USAGE.md](../USAGE.md#kubernetes-helm)).
* **Server artifact candidates.** An unpublished GNU `x86_64` candidate workflow packages the binary with man pages, completions, OpenAPI, licence notices, and detached debug symbols, verified by unsigned `manylinux_2_28` build and Rocky Linux 8 / Debian 11 container smoke tests requiring a nonzero UID/GID and host system CA certificates (`ca-certificates`, needed even for loopback commands like `sparkles ping`). Published binaries, signed provenance, additional platforms, and published container images remain open (roadmap [item 59](../ROADMAP.md); see [DEVELOPMENT.md](../DEVELOPMENT.md#server-artifact-candidates)).

### Settings and notifications

* **Layered dataset settings.** Assistant, memory, ingest, and prefix settings merge built-in defaults, declarative config (`serve --settings`, supporting per-dataset rules, field locks, and `SIGHUP` reload), and runtime overrides via `/$/settings/{ds}/{kind}` with `If-Match` concurrency checks ([C19](../specs/C19-layered-settings.md), [API.md](../API.md#settings), [USAGE.md](../USAGE.md#declarative-dataset-settings)). Every read reports each field's source layer. `sparkles settings` inspects, edits, resets, and applies settings files from the CLI ([USAGE.md](../USAGE.md#changing-dataset-settings-from-the-command-line)), and the NixOS module validates generated files at build time.
* **Server model settings and secrets.** Server administrators configure model providers, role lists, and budgets live via `/$/server/settings/models`, `sparkles settings --global`, or the web UI (with Anthropic, OpenAI, and Ollama presets) and store write-only secrets via `/$/server/secrets/{name}` or `sparkles secrets` ([API.md](../API.md#server-settings)). Secrets are sealed with `serve --secrets-key`, the systemd credential `sparkles-secrets-key`, or a generated key in the data directory. HTTPS providers can trust an internal CA (`tls.caCert`) or disable verification (`tls.insecureSkipVerify`, logged and marked unverified in the UI; [USAGE.md](../USAGE.md#internal-certificates-and-tls-options)).
* **Declared prefixes.** Built-in well-known and Sparkles prefixes are active by default and can be extended or locked in settings files, `/$/settings`, the UI Settings tab, or Fuseki's `/{ds}/prefixes`; loaded RDF data never overwrites declared prefixes ([C20](../specs/C20-declared-prefixes.md), [USAGE.md](../USAGE.md#declared-prefixes)).
* **Outbound notifications.** Signed Standard Webhooks, ntfy, and email channels alert on stale memory reviews, failed or overdue backup/GC/verification runs, and exhausted daily provider token budgets ([C21](../specs/C21-outbound-notifications.md), [API.md](../API.md#notifications), [USAGE.md](../USAGE.md#sending-notifications)). Channels and routes live in server settings, deliver from independent queues with retry backoff, and can be inspected and tested via `sparkles notify status|test` or the web UI Server page.

### Memory and resource limits

* **Shared server caches.** Block, result, `SERVICE`, and history caches share server-wide limits across all datasets while reporting per-dataset usage, hits, and misses ([X08](../specs/X08-disk-spill-and-server-memory.md#gates)). Setting `--memory-profile small` reduces RSS by about 70 MiB on a 10.5M-triple dataset.
* **Derived-table cache.** `--derived-table-cache-mb` sets the process-wide budget for tables retained by demand reasoning. Both memory profiles default to 256 MiB. Zero disables retention. Callers with `metrics` permission see capacity, estimated retained bytes, table counts, hits and misses in `/$/stats` and `/$/server/memory`. The estimate includes deduplicated tables and chunks, spare capacity, cache keys, allocated hash buckets and allocator metadata. Eviction prefers unborrowed entries. Evicted tables and chunks held by running queries reduce the allowance until callers release them. RSS is reported independently. Each query has a separate derivation budget.
* **Query memory pool and admission.** `--total-query-memory-mb` bounds estimated memory across all active queries and updates (or via `MemoryOptions` in embedded catalogs). Requests queue in arrival order, return `503` with `Retry-After` if `--query-admission-wait` expires, and fail with `server-memory` when the pool cannot cover them. Finished eager `SELECT`/`ASK` queries retain only their result estimate, and cursor charges release with live state. Monitor the pool at `GET /$/server/memory` and via `sparkles_query_pool_*` and `sparkles_query_admission_*` metrics.
* **Per-query budgets and internal accounting.** Per-query limits bound estimated intermediate memory, response size, returned rows, and total rows produced ([C01](../specs/C01-observability-and-budgets.md)). Callers may tighten but never raise server limits; over-budget queries fail with `507`, and disconnected clients cancel active queries and writes. Memory estimates include the decoded value cache (capped at 64 MiB or 1/8 of query budget), computed terms and memos, parsed geometries, dictionary decoding workspace, `EXISTS` memos/key sets, and compiled regex state. Across queries, the server caches up to 64 compiled regex programs (16 MiB) plus up to 16 MiB of warm search caches outside individual query budgets.
* **Storage quotas.** `--max-dataset-mb` sets a default disk quota for persistent datasets, overridable per dataset via `/$/quota/{ds}` or `sparkles quota` ([API.md](../API.md#storage-quotas)). Writes exceeding quota fail with `507` before committing while reads and deletes stay allowed.

### Observability

* **Audit events.** Dataset creation, deletion, and cloning, plus branch creation and deletion, emit `INFO` events under `sparkles::audit` with actor and operation identities and bounded failure codes (never request payloads or credentials). Local CLI operations log as `local:<user>` to stderr, remote CLI commands rely on server events, and automatic scratch-branch expiry logs `system:scratch-expiry` with `reason=idle-expiry`. `RUST_LOG` controls filtering.
* **Access logs and Prometheus metrics.** Every HTTP request receives an `X-Request-Id` and one structured text or JSON access-log line. `/$/metrics` exports Prometheus metrics and `/$/metrics?format=json` exposes the matching JSON snapshot, including rows produced by all operators of queries and updates, peak query memory histograms, and index rebuild counts and duration histograms by reason. `--metrics-fuseki-names` adds Fuseki request metric names, and `--metrics-addr` serves metrics on a dedicated listener.
* **OpenTelemetry.** OpenTelemetry support compiles under the `otel` cargo feature and activates at runtime via `--otel` or `OTEL_*` environment variables. Trace export honors and propagates W3C `traceparent` on `SERVICE` and `LOAD` requests and records spans for commits, background tasks, query phases, and operator trees. Metric export bridges Prometheus series including operator rows produced by queries and updates, peak query memory histograms, and rebuild series, and log export optionally sends trace-correlated OTLP logs.

### Backups

`sparkles-backup` (enabled by default via the `backup` cargo feature) manages online backups to local file systems or S3-compatible stores including AWS S3, MinIO, Cloudflare R2, and Ceph RGW ([F05](../specs/F05-snapshot-repositories/README.md), [API.md](../API.md#backup-repositories), [USAGE.md](../USAGE.md#backup-repositories)).

* **Incremental deduplicated storage.** Backups hold the writer lock only briefly (in-memory datasets back up via a temporary generation), deduplicate data in content-addressed 32 MiB chunks, and upload only appended WAL and catalog bytes.
* **Optional repository encryption.** Standard binaries support local-key or passphrase-encrypted repositories on an opt-in basis. On Linux and Android, key memory uses dump-excluded locked pages; encrypted macOS CLI commands disable process core dumps (`RLIMIT_CORE = 0`, inherited by children) before reading keys and use locked pages.
* **Restore and verification.** Restores create a new dataset or swap in place (returning `503` with `Retry-After` during the swap), enforce dataset-id rules, verify integrity before publishing, and support three verification levels. Backups and persistent clones preserve view definitions captured under the writer lock; restored views start suspended until an operator resumes and revalidates their principal and query, and unsafe, duplicate, unknown, or corrupt definition entries abort restore before publication.
* **Lifecycle policies and shared repositories.** Cron and `every` schedules run in IANA time zones with catch-up, retention pruning, two-phase garbage collection, and offline catalog execution. Lease locks rely on storage-server timestamps so multiple servers and CLIs can share one repository. Manage repositories, backups, and policies through `/$/repositories`, `/$/backups/{ds}`, `/$/backup-policies`, `sparkles repo|backup`, and the web UI Backups page, with `sparkles_backup_*` metrics and audit events.

### Upgrades and on-disk formats

* **Format registry and migration.** A central registry in `sparkles-core` tracks read and write versions for every file kind, exposed at `GET /$/server` ([X12](../specs/X12-release-engineering.md), [USAGE.md](../USAGE.md#upgrading-databases)). Unreadable newer formats fail cleanly with upgrade instructions while healthy datasets keep serving. Generation indexes support multi-version reading so format upgrades take effect on compaction and older backups remain restorable. `sparkles migrate` inspects stopped-server file versions and upgrades on a verified copy before swapping in while preserving dataset UUIDs and history. Fixtures in `testsuite/formats/` test backward compatibility across open, query, write, compact, and restore.
* **Parallel startup and health probes.** The server opens its listening socket first and opens datasets in parallel ([API.md](../API.md#startup)). `/$/ready` reports each dataset as `open`, `opening` (returning `503` with `Retry-After` on access), or `failed`. Failed datasets stay closed while others serve, marking the server `degraded` and unready unless `--ready-ignore-failed` is set (`--require-all-datasets` exits instead). Use `/$/ping` for startup and liveness probes and `/$/ready` (or `/$/ready/{ds}`) for readiness checks ([USAGE.md](../USAGE.md#endpoints-and-operations)). Degraded derived-index reporting and WAL replay progress remain planned (roadmap [item 402](../ROADMAP.md)).
* **Graceful shutdown.** On `SIGTERM` or `SIGINT`, the server stops accepting new connections, allows in-flight requests up to `--shutdown-grace` seconds to finish, and cancels remaining work without committing aborted writes.

### Interrupted dataset deletes

Managed persistent deletes use the catalog's `delete.json` journal. Startup checks the
journal and registry before recovering any pending rename, restore or delete. An
unsupported format, invalid journal or registry, or symlink in place of `delete.json`,
`config.json`, or the `databases` directory prevents the catalog from opening. A prepared
delete or any staged `databases/.replaced-<name>-<task>` directory requires a valid
registry, even when the registry contains no datasets.

Before committing a delete, the catalog moves the dataset to
`databases/.kept-<name>-<token>` and synchronizes the database directory. The persistent
dataset UUID in `config.json` helps startup identify the original registration. Startup
takes the store's process lock and verifies the staged copy. For a prepared delete, it
restores the original path when the same UUID remains registered, or removes the staged
copy when the registry records the deletion or a replacement. If a later registration
reuses the name and UUID, the catalog durably records the old delete's commit in journal
format 4 before publishing that registration. Recovery only reclaims that committed copy.
A busy or unverifiable copy keeps its journal entry for the next startup. Format 3
journals treat a sole verified `.deleted-<token>` trash copy as proof that the older
delete committed, allowing a same-UUID replacement to open while reclaiming only the
trash copy. When format 3 leaves an ambiguous kept copy or conflicting paths next to a
matching registration, startup leaves the dataset closed and retains its files for
inspection. Ambiguous earlier journals and competing staged restore copies also remain
on disk.

An indeterminate registry commit keeps the name unavailable until the catalog reopens.
A restore with that outcome preserves its published root. An in-place restore records a
swap revision in format 3 of `config.json` and writes a versioned swap marker in its
`.replaced-` predecessor directory before moving the restored copy into place. All
catalog builds synchronize the registry and reconcile marked swaps on startup, restoring
the predecessor when the predecessor revision survived or a live rollback failed and
removing the predecessor when the restored revision committed. Live and startup rollbacks
synchronize the database directory after moving the live path aside and before clearing
the swap marker. Unmarked format 2 pairs reconcile against the registered UUID before
deleting either copy and refuse ambiguous identity combinations. Unmarked format 1
rows retain roll-forward recovery, unmarked roll-forward synchronizes the database
directory under the live store lock before removing a predecessor, and obsolete
unmarked copies without a persistent registry row are retired without populating the
live path. A staged `.replaced-` copy carrying a validated `keepReplaced` marker is
moved to its `.kept-` name under the store lock when that destination is free, or
retained and reported when the destination is occupied, leaving the live alias untouched.
Publication synchronizes the journal
namespace before replacing the registry, and startup synchronizes the registry before
acting on the delete outcome it observes. Power-loss recovery requires a filesystem and
platform that support durable directory synchronization.

Warnings report `path`, `retained_bytes`, and whether the byte count is complete.
Startup leaves unrecognized kept entries in place. Copies marked by a `keepReplaced`
restore are recognized even after the active dataset is renamed or deleted. Older copies
without a marker may appear in the warnings. Stop the catalog and all standalone users
of a dataset before moving or editing files inside the managed catalog directory.

### API compatibility

The [API compatibility policy](../DEVELOPMENT.md#api-compatibility) covers HTTP routes, CLI commands, and public library APIs. HTTP OpenAPI descriptions and standard CLI command trees are checked against a development commit baseline rather than a published release. Library and binding compatibility checks, CLI JSON/exit-code checks, MCP and metrics guards, deprecation signals, and released-artifact verification remain planned (roadmap [item 60](../ROADMAP.md)). On-disk reader compatibility windows are managed separately.

## Configuration and usage

| Topic | Reference |
|---|---|
| NixOS, Home Manager, Docker, and Kubernetes deployment | [NixOS](../USAGE.md#deploying-on-nixos), [Home Manager](../USAGE.md#home-manager), [Docker](../USAGE.md#docker), [Kubernetes (Helm)](../USAGE.md#kubernetes-helm) |
| Layered settings, secrets, TLS, and declared prefixes | [Declarative settings](../USAGE.md#declarative-dataset-settings), [CLI settings](../USAGE.md#changing-dataset-settings-from-the-command-line), [TLS options](../USAGE.md#internal-certificates-and-tls-options), [Declared prefixes](../USAGE.md#declared-prefixes) |
| Outbound notifications and backup repositories | [Sending notifications](../USAGE.md#sending-notifications), [Backup repositories](../USAGE.md#backup-repositories) |
| Upgrading databases, health probes, and server startup | [Upgrading databases](../USAGE.md#upgrading-databases), [Endpoints and operations](../USAGE.md#endpoints-and-operations), [Startup API](../API.md#startup) |

## Known gaps

Operational limits for single-node deployment, backup scope, encryption at rest, and runtime key protection are listed under [Server, access control and operations](../server/README.md#server-access-control-and-operations). Release binary, container image, and package publication limits are listed under [Bindings and packaging](../integrations/README.md#bindings-and-packaging). Degraded derived-index and WAL-replay startup reporting is tracked in roadmap [item 402](../ROADMAP.md), and full API compatibility checks in roadmap [item 60](../ROADMAP.md).

## Roadmap

Core design specifications for this area are [C01](../specs/C01-observability-and-budgets.md), [C19](../specs/C19-layered-settings.md), [C20](../specs/C20-declared-prefixes.md), [C21](../specs/C21-outbound-notifications.md), [F05](../specs/F05-snapshot-repositories/README.md), [F11](../specs/F11-encryption-at-rest.md), [X08](../specs/X08-disk-spill-and-server-memory.md), and [X12](../specs/X12-release-engineering.md).

The central [roadmap](../ROADMAP.md) owns priorities and dependencies, and its [completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs) link partially complete specs to their remaining work and acceptance criteria.
