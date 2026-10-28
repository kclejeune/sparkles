# Deployment and operations capabilities

This page describes the available capabilities and their current limits. The
[capability matrix](../FEATURES.md#capability-status) records delivery status within
each stated scope.

## Operations

### Deployment

* The Nix flake builds the `sparkles` package with the web UI, the formatter's
  WebAssembly module, shell completions and man pages. Its NixOS module
  `services.sparkles` runs the server as a hardened systemd service with datasets,
  authentication, backups, TLS, metrics, OpenTelemetry and an optional nginx virtual host
  ([USAGE.md](../USAGE.md#deploying-on-nixos)).
* The Home Manager module `programs.sparkles` installs the CLI for one user, writes the
  memory, backup and formatter config files, and can declare the memory hooks and skill
  of Claude Code and Codex ([USAGE.md](../USAGE.md#home-manager)).
* The `Dockerfile` builds an image with the server and UI that runs as an unprivileged
  user, and `compose.yaml` serves it on the loopback address with the data in a named
  volume ([USAGE.md](../USAGE.md#docker)). An `ocr` target adds PDFium and ONNX Runtime at
  pinned, checksummed versions, and the flake builds both images as OCI images.
* A Helm chart in `deploy/helm/sparkles` runs the server as a single-replica StatefulSet
  with a read-only root filesystem, configuration from values and Secrets, three ways to
  deliver local models, and reloads through `serve --watch-config`
  ([USAGE.md](../USAGE.md#kubernetes-helm)).

An unpublished GNU x86_64 candidate workflow assembles archives with generated man
pages, completions, OpenAPI, license notices and separate debug symbols. Its gates
require a manylinux 2.28 build and installed query and persistence checks in pinned
Rocky Linux 8 and Debian 11 containers. Those gates have not established platform
support. Published server downloads, signed provenance, the other server platforms
and published container images remain open under [item 59](../ROADMAP.md). Candidate
build records are unsigned metadata. [DEVELOPMENT](../DEVELOPMENT.md#server-artifact-candidates)
describes the commands and evidence boundaries.
Installed acceptance requires a nonzero host UID and GID. The smoke refuses root
callers and records the selected numeric user in its receipt.

### Settings and notifications

* A dataset's assistant, memory, ingest and prefix settings combine built-in defaults,
  the declared settings file of `serve --settings`, and runtime changes through
  `/$/settings/{ds}/{kind}` ([C19](../specs/C19-layered-settings.md),
  [API.md](../API.md#settings), [USAGE.md](../USAGE.md#declarative-dataset-settings)). The file
  declares defaults and per-dataset entries and can lock fields. Each answer names the
  source of every field, writes take `If-Match`, and SIGHUP reloads the file.
* `sparkles settings` reads settings with each field's source, sets and resets fields,
  edits the runtime layer, lists overrides, and applies a settings file
  ([USAGE.md](../USAGE.md#changing-dataset-settings-from-the-command-line)). The NixOS
  module generates the file, checks it at build time, and reloads the server when it
  changes.
* Server administrators change model providers, role lists and budgets through
  `/$/server/settings/models` and store write-only keys through
  `/$/server/secrets/{name}` without a restart ([API.md](../API.md#server-settings)). Keys
  are sealed with the key of `serve --secrets-key`, the systemd credential
  `sparkles-secrets-key`, or a key generated in the data directory.
  `sparkles settings --global`, `sparkles secrets` and the UI make the same changes, and
  presets fill in Anthropic, OpenAI and Ollama providers.
* A provider with an `https` endpoint can trust an internal CA through `tls.caCert`, or
  turn certificate checks off with `tls.insecureSkipVerify`, which the server logs and the
  UI marks as unverified ([USAGE.md](../USAGE.md#internal-certificates-and-tls-options)).
* Declared prefixes default to the well-known prefixes and the namespaces Sparkles
  defines ([C20](../specs/C20-declared-prefixes.md), [USAGE.md](../USAGE.md#declared-prefixes)).
  The settings file declares and locks them, and a dataset admin changes them through the
  settings API, the Settings tab or Fuseki's `/{ds}/prefixes`. Loaded data never replaces
  a declared prefix.
* The server pushes signed Standard Webhooks and ntfy notifications when memory review
  waits too long, a backup policy run fails, or a provider's daily token budget is used
  up ([C21](../specs/C21-outbound-notifications.md), [API.md](../API.md#notifications),
  [USAGE.md](../USAGE.md#sending-notifications)). Channels and routes are a server settings
  kind, failures are retried with backoff, and `sparkles notify status` and
  `sparkles notify test` show and test channels.

### Memory and resource limits

* The block, result, SERVICE and history caches are sized once for the server and shared
  by all datasets, with per-dataset bytes, hits and misses
  ([X08](../specs/X08-disk-spill-and-server-memory.md#gates)). The `small` memory profile,
  set with `--memory-profile`, saves about 70 MiB of RSS at 10.5M triples.
* A query memory pool, set with `--total-query-memory-mb`, bounds the estimated memory of
  all queries and updates together. Requests wait in arrival order for a share, answer
  `503` with `Retry-After` after `--query-admission-wait`, and fail with the
  `server-memory` budget when the pool cannot cover them. A finished eager SELECT or ASK
  keeps only its result's estimate, and cursor charges release with their live state.
  `GET /$/server/memory` and the `sparkles_query_pool_*` and
  `sparkles_query_admission_*` metrics report it. Embedded catalogs take a pool through
  `MemoryOptions`.
* Per-query budgets cap estimated intermediate memory, response size, rows, and the rows
  all operators produce ([C01](../specs/C01-observability-and-budgets.md)). A request can
  lower a budget but never raise it, and a query over budget fails with `507`. Queries and
  writes stop when their client disconnects.
* Persistent datasets have storage quotas, a default with `--max-dataset-mb` and per
  dataset through `/$/quota/{ds}` and `sparkles quota` ([API.md](../API.md#storage-quotas)).
  A write that would exceed the quota is refused with `507` before it commits, while
  reads and deletes continue.

### Observability

* Dataset creation, deletion and cloning and branch creation and deletion emit INFO
  events under `sparkles::audit`. Each completed operation carries its actor and proven
  identity metadata; authorized failures carry a bounded code without request payloads
  or credentials. Clone success follows target registration rather than task acceptance.
* Local dataset and branch commands use `local:<user>` and the existing stderr tracing
  output, with audit INFO enabled by default. Remote commands rely on the server event.
  Automatic scratch expiry records `system:scratch-expiry` and `reason=idle-expiry`.
  Deletion records omit counts and branch UUIDs that cannot be proved atomically.
  `RUST_LOG` retains control over event filtering.
* Every request gets an `X-Request-Id` and one structured access-log line in text or
  JSON. `/$/metrics` serves Prometheus metrics and `/$/ready` readiness. The metrics
  include each query's rows and peak memory and index rebuilds by reason and duration.
  `--metrics-fuseki-names` adds Fuseki's request metric names, and `--metrics-addr` serves
  the metrics on a second address.
* OpenTelemetry, with the `otel` cargo feature, is off at run time unless `--otel` or
  `OTEL_*` variables enable it. Traces go out over OTLP, W3C `traceparent` is read from
  requests and sent on SERVICE and LOAD, and spans cover commits, background tasks, query
  phases and the operator tree. The Prometheus registry is bridged, and OTLP logs with
  trace correlation are optional.

### Backups

* `sparkles-backup`, with the `backup` cargo feature, on by default, keeps online backups
  in a file system or S3, including AWS, MinIO, R2 and Ceph RGW
  ([API.md](../API.md#backup-repositories), [USAGE.md](../USAGE.md#backup-repositories),
  [F05](../specs/F05-snapshot-repositories/README.md)). A backup holds the writer lock for only a few
  system calls, and an in-memory dataset is backed up through a temporary generation.
* Backups are incremental and deduplicated in content-addressed 32 MiB pieces, and only
  the appended bytes of the WAL and catalog are copied. Repositories can be encrypted with
  a local key or passphrase. The standard binary includes encryption support, and
  each repository opts in through operator configuration. Linux and Android use
  locked pages excluded from dumps. Encrypted macOS CLI commands explicitly disable
  process core dumps before reading keys, then use locked pages. The process and its
  children inherit the zero hard limit.
* A restore goes to a new dataset or replaces one in place, follows dataset-id rules, and
  runs an integrity check before publishing. During an in-place swap, requests get `503`
  with `Retry-After`. Backups can be verified at three levels.
* Lifecycle policies run on cron or `every` schedules in an IANA time zone, with catch-up,
  retention and two-phase GC, and can run offline against a stopped catalog. Lease locks
  use the storage server's clock, so several servers and the CLI can share a repository.
* `/$/repositories`, `/$/backups/{ds}`, `/$/backup-policies`, `sparkles repo`,
  `sparkles backup` and the UI's Backups page manage backups, which report
  `sparkles_backup_*` metrics and audit events.

### Upgrades and on-disk formats

* One registry in `sparkles-core` lists every kind of file with the version a build
  writes and the versions it reads, and `GET /$/server` reports it
  ([X12](../specs/X12-release-engineering.md), [USAGE.md](../USAGE.md#upgrading-databases)). A
  newer version is refused with a message that says what to do, and a dataset in an
  unreadable format stays closed while the others are served.
* The generation index is read from a set of formats, so a format change lands at the
  next compaction, and backups restore when their index format is in that set.
* `sparkles migrate` lists a stopped server's file versions and runs upgrade steps on a
  checked copy that it swaps in, keeping the dataset id and history. Fixtures in
  `testsuite/formats/` test that older files open, query, write, compact and restore.
* On SIGTERM or SIGINT the server stops accepting connections and gives requests in
  flight `--shutdown-grace` seconds to finish. It then cancels the rest, and a cancelled
  write commits nothing.

## Known gaps

The [server, security and operations gaps](../server/README.md#server-access-control-and-operations)
cover operational controls, backup scope and security boundaries. The
[bindings and packaging gaps](../integrations/README.md#bindings-and-packaging)
cover release artifacts.

## Configuration and usage

The following guides describe configuration and supported UI, command-line, HTTP
or library workflows. Exact HTTP contracts remain in the API reference.

* Read [Deploying on NixOS](../USAGE.md#deploying-on-nixos).
* Read [Home Manager](../USAGE.md#home-manager).
* Read [Docker](../USAGE.md#docker).
* Read [Kubernetes (Helm)](../USAGE.md#kubernetes-helm).
* Read [Declarative dataset settings](../USAGE.md#declarative-dataset-settings).
* Read [Changing dataset settings from the command line](../USAGE.md#changing-dataset-settings-from-the-command-line).
* Read [Internal certificates and TLS options](../USAGE.md#internal-certificates-and-tls-options).
* Read [Declared prefixes](../USAGE.md#declared-prefixes).
* Read [Sending notifications](../USAGE.md#sending-notifications).
* Read [Backup repositories](../USAGE.md#backup-repositories).

## Roadmap

The central roadmap owns priority and dependencies. Its
[completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs)
link partially complete specs to their remaining work and acceptance criteria.
