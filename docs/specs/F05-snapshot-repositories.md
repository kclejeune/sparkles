# F05 — Backup repositories: incremental backup and restore, policies, GC

> **Status:** implemented in part
>
> **Phases:** Phase 1 shipped: repositories, incremental backups, restore, verify, the
> CLI, the server and the Backups panel. Most of Phase 2 shipped with it: lifecycle
> policies, garbage collection, locks and the full Backups page, with `gcs` and `azure` as
> experimental build features. Full backups of in-memory datasets followed. These were not
> built: "back up all datasets" as one request, the CLI's `--server` mode, offline
> `backup policy run`, and all of Phase 3 (client-side encryption, content-defined
> chunking, repository copy, server backups).
>
> **User docs:** [API: Backup repositories](../API.md#backup-repositories) ·
> [Usage: Backup repositories](../USAGE.md#backup-repositories) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui) ·
> [Benchmarks: Backup repositories](../BENCHMARKS.md#backup-repositories-105m-triples)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This spec builds on durable commit identity
([CI](CI-commit-identity.md)), named snapshots and retained generations
([F06](F06-snapshots-and-point-in-time.md)), the integrity checker (`sparkles::check`)
and dataset access control ([C09](C09-dataset-access-control.md)).

## 1. Summary, goals, non-goals

Today Sparkles has two ways to copy a dataset out of a server:

* `POST /$/backup/{ds}` and `sparkles backup --loc DB --out DIR` write a gzipped N-Quads
  dump (`Store::backup`). The dump is a full copy every time and is slow to reload,
  because reloading is a full bulk load. It also loses commit history, blank-node ids and
  the dataset id.
* `POST /$/datasets/{ds}/clone` and `sparkles clone` rebuild a new, local database.

This feature adds **backup repositories**. A repository is a directory, or an
S3-compatible bucket prefix, that holds deduplicated, content-addressed copies of dataset
files. A **backup** is one dataset at one commit, described by an immutable manifest. The
feature includes:

* **Incremental, deduplicated backups.** Generation files are immutable once built
  (`gen-NNNN/*`), so a backup uploads only files the repository does not already have.
  The append-only files (`wal.log`, `delta.vocab`, `commits.bin`) upload only the bytes
  appended since the previous backup.
* **Consistent backups of a live dataset** at exactly one commit. The writer lock is held
  for about a millisecond to capture byte offsets, and the F06 machinery pins the
  generation until the upload ends.
* **Restore** to a new dataset name, which doubles as "clone from backup". A restore can
  also replace a dataset in place, with the server swapping it, or write to a directory
  offline for disaster recovery.
* **Several repositories** per server, from the API, the CLI or a config file. A
  repository can be read-only, for restoring on another server.
* **Verification** at three depths: every blob exists, every blob's SHA-256 matches, or
  a full restore into a temporary directory followed by `sparkles::check`.
* **Lifecycle policies** (Phase 2): cron or interval schedules with a time zone, a
  dataset selection, a name template, retention (`expireAfter`, `minCount`,
  `maxCount`), run history, catch-up after downtime, and metrics for alerting.
* **Garbage collection** (Phase 2): two-phase mark-and-sweep of unreferenced blobs under
  a repository lock, with a grace period.
* A **CLI** (`sparkles repo …`, `sparkles backup …`) that works on a repository without
  a server, an **HTTP API** (`/$/repositories`, `/$/backups/{ds}`, `/$/backup-policies`)
  and a **Backups area in the UI**.

Sparkles already uses "snapshot" for F06 named snapshots, which are pins inside a
database. To keep the two apart, the repository copies are called **backups** (§8).

Goals:

* No change to the write path. The writer lock is held for O(files) system calls per
  backup.
* Crash safety: an interrupted or cancelled backup leaves no visible backup. A restored
  database is byte-identical to the source at the backed-up commit, or the restore fails.
* Repository safety with concurrent writers: names are unique through conditional
  creates, and GC never deletes a blob that a visible or in-flight backup references.
* The repository format is versioned and defined by Sparkles. It is not derived from
  Elasticsearch's.

The open items of the request were decided as follows:

| Item | Decision |
|---|---|
| Dedup unit | Whole immutable files, split into fixed 32 MiB pieces, plus appended segments of append-only files. Content-defined chunking is Phase 3, after measurement (§5.4). |
| Hash | SHA-256, from the `sha2` workspace dependency. S3 can verify SHA-256 checksums server-side. |
| Full-text index | Excluded. `text.json` is kept, and the index is rebuilt when the restored store opens. |
| Scope of a backup | One dataset. A policy or a "back up all" request makes one backup per dataset, grouped by a run id. |
| Client-side encryption | Phase 3. The v1 format reserves the encoding byte and a manifest field. Phase 1 relies on S3 SSE and filesystem encryption. |
| Locks | Lease objects (shared or exclusive), plus conditional creates for names. Staleness uses the storage server's `last_modified`, not client clocks. |

Non-goals:

* Continuous replication or WAL shipping between backups, which would allow
  point-in-time recovery to any commit. A backup captures one commit.
* Atomic multi-dataset backups. Datasets have independent commit streams, so there is no
  cross-dataset transaction to be consistent with.
* Backing up F06 history: retained generations, pins (`history.json`) and the retention
  window. A restore starts with only the current generation. History is a Phase 3
  option.
* Backing up in-memory datasets. Phase 2 would build a temporary generation with
  `clone_to` and upload it as a full backup.
* Deduplication across compactions. A compaction renumbers ids and rewrites every index
  file (§5.4).
* Archive storage classes (Glacier restores), S3 Object Lock/WORM, and
  repository-to-repository copy (Phase 3).

## 2. User-visible behavior

### 2.1 Repositories

A repository has a server-local **name** (`[a-z0-9][a-z0-9_-]{0,63}`), a **type** and
settings. Its identity is the **repository id**, a UUID stored in the repository marker
object and written once at initialization.

| Type | Location | Backend | Phase |
|---|---|---|---|
| `fs` | absolute local or network path (NFS, SMB mount) | `object_store::local::LocalFileSystem` with fsync enabled | 1 |
| `s3` | `bucket` + optional `prefix`. AWS, MinIO, Cloudflare R2, Ceph RGW and other S3-compatible services through `endpoint`. | `object_store::aws::AmazonS3` | 1 |
| `gcs`, `azure` | bucket/container + prefix | `object_store` `gcp` / `azure` features | 2 (experimental, not in CI) |
| `memory` | none | `object_store::memory::InMemory` | tests only (hidden) |

A repository can be registered from three sources:

* **HTTP API or UI.** The repository is persisted in `<data>/backup/repositories.json`,
  written with `state::write_file_atomic`, and has `source: "api"`.
* **Config file.** `serve --backup-config FILE` reads a TOML file (§2.9). Its
  repositories are read-only through the API, have `source: "config"`, and are re-read
  on SIGHUP together with policies.
* **CLI.** `sparkles repo add` edits a TOML file in the same format (§2.8), so the same
  file can be given to `serve --backup-config`.

At registration (`POST /$/repositories`, `sparkles repo add`) the location is
**initialized or attached**:

* If it is empty, create `sparkles-repo.json` with `PutMode::Create` (§5.2).
* If it holds a marker, read it and check `format`. A major version newer than this
  build gives `422 incompatible-repository`.
* If it is non-empty and has no marker, the answer is `409 not-a-repository`. Sparkles
  never writes into a prefix it did not create.
* If the server already has the same repository id under another name, the answer is
  `409 repository-exists`.

Then a **connection test** runs. `POST /$/repositories/{repo}/test` runs it on demand.
The test creates `probe/<uuid>` with `PutMode::Create`, then creates it again and
expects "already exists", which detects conditional-write support. It then reads the
object back, lists `probe/` and deletes the object. The report gives each step's latency,
`conditionalWrites: bool` and any error. A failed test still registers the repository,
shown as `reachable: false`. `?verify=false` skips the test.

**Read-only repositories** (`readonly: true`) allow list, show, restore and verify at the
`exists` and `data` levels. They never write, not even lock objects. This is the mode for
a second server that restores from a repository another server writes, for DR drills or
staging refreshes. A concurrent delete or GC on the writing side can remove a backup
while it is being restored. The restore then fails cleanly on the missing blob and can be
retried.

**Two writable servers on one repository** are supported. Conditional creates keep names
unique, and locks exclude GC (§5.7). The UI still recommends one writer per repository
and read-only access elsewhere. When the test reports `conditionalWrites: false`, as on
older S3-compatible services, the UI marks the repository `singleWriter`. Name
uniqueness then falls back to HEAD-then-PUT, and the operator must ensure a single
writer.

### 2.2 Backups

A backup is identified by `(repository, name)`. The name uses the F06 grammar
`[A-Za-z0-9][A-Za-z0-9._-]{0,63}` and is unique within a repository. If none is given, it
defaults to `{dataset}-{time}` (for example `wiki-20260930t140311z`).

A backup of dataset `ds` at commit `s` holds:

| Content | How |
|---|---|
| `gen-NNNN/` of the current generation: `vocab.dat`, `vocab.off`, `<perm>.dat` and `<perm>.meta` for the 7 permutations (`spo sop pso pos osp ops gspo`), `meta.json`, `stats.json`, `commit.json` | Immutable. Stored whole, split into 32 MiB pieces. |
| `gen-NNNN/wal.log` | The prefix up to the end of commit `s`'s record. |
| `gen-NNNN/delta.vocab` | The prefix of flushed entries at capture, a superset of what the WAL prefix references. |
| `commits.bin` | The prefix through the record of commit `s`. |
| `CURRENT`, `dataset.json` | As on disk. |
| `prefixes.json` | Rendered from the store's in-memory prefixes at capture, because prefix changes make no commit. |
| `text.json`, `reasoning.json`, `origin.json` | If present. `reasoning.json` is dropped when its `commit` is greater than `s`, which means a reasoning commit landed after the capture. |
| **excluded** | `sparkles.lock`, `text/`, `text.new`, `text.old`, `text.dirty`, `history.json`, non-current `gen-*` (F06 retained generations), `*.tmp`, `*.deleting`, `commits.bin.corrupt-*` |

A restored directory is therefore exactly the source database as it was right after
commit `s` was acknowledged, minus derived and history state. Opening it replays `s`'s
WAL prefix ([CI §5.3](CI-commit-identity.md)). With `text.json` present, the full-text
index is rebuilt at open.

Every backup is **logically complete**. Restoring it needs only its manifest and the
blobs the manifest names, and deleting any other backup never damages it.
"Incremental" describes only the bytes uploaded (`addedBytes`).

**Consistency with concurrent writes.** Writes continue during the upload. The backup
reflects commit `s`, the head at capture time, and the manifest records it with
`Sparkles-Commit`-style metadata (`commit.seq`, `commit.timestamp`, `datasetId`).

**Compaction and bulk commits during an upload** are safe. The captured generation is
leased (§5.3) and kept until the upload ends, even after `CURRENT` has moved on.

**One running backup per (dataset, repository).** A second request gets
`409 backup-in-progress` with the running task's id.

**In-memory datasets** get `501 backup-unsupported`. Phase 2 adds full backups for them.

### 2.3 Restore

Restore makes a dataset from a backup, in one of three modes:

| Mode | Where | Conditions |
|---|---|---|
| **New dataset** (also "clone from backup") | `<data>/databases/<target>`, registered like a clone | `target` must not exist and must not be reserved (`409 dataset-exists`). |
| **In place** (`replace: true`) | replaces the registered persistent dataset `target` | The dataset must not be `--loc`-attached (`409 not-managed`) and must have no other backup task. The server stops the dataset for the swap (below). |
| **Offline** (CLI) | any directory: `--to DIR` | `DIR` is absent or empty. With `--replace`, `DIR`'s `sparkles.lock` must be acquirable, which means no process holds it. |

All modes run the same library code:

1. Take a shared repository lock, except on read-only repositories. Read and validate
   the manifest (§5.8). Check that `indexFormat == builder::FORMAT_VERSION`, or fail with
   `422 incompatible-format: backup uses index format N; this build reads M`. Check that
   free disk space is at least `logicalBytes` × 1.1, or fail with
   `507 insufficient-storage`.
2. Download every file into a sibling temporary directory
   (`<data>/databases/.restore-<target>-<task>` on a server, `DIR.restore-<pid>`
   offline). Concatenate each file's blobs, verify each blob's SHA-256 and length before
   writing it, and verify the file's own `sha256`. Downloads run in parallel under the
   repository's concurrency and bandwidth limits.
3. Apply the **identity rule** (below).
4. `fsync` every file and directory. Write `restore.json`, an informational record of
   the repository name and id, the backup name, the source `{datasetId, seq, name}`, the
   `identity` and the time.
5. Run `sparkles::check::check(tmp, quick = check != "full")`, and fail on `error`.
   `check: "none"` skips this step.
6. Open the `Store`. Opening replays the WAL, and the full-text index is rebuilt if it
   is configured. The store's head must equal `manifest.commit.seq` and its quad count
   `manifest.commit.quads`, or the restore fails with `500 restore-mismatch`.
7. Publish:
   * **New**: rename into `databases/<target>`, `adopt` the reservation, and save the
     registry. This reuses the existing clone code path (`AppState::reserve`/`adopt`).
   * **In place**: mark the dataset `restoring`, so new requests to it get
     `503 dataset is being restored` with `Retry-After: 5`. Remove it from the registry
     map and wait up to 30 s for in-flight requests to release it
     (`Arc::strong_count == 1`). If they do not, re-insert it and fail with
     `409 dataset-busy`. Drop the `Store`, which releases `sparkles.lock`. Rename
     `databases/<t>` → `databases/.replaced-<t>-<task>`, then the temporary directory →
     `databases/<t>`, and fsync `databases/`. Open and insert the new dataset, then
     remove `.replaced-…` unless `keepReplaced: true`. If the new store fails to open,
     rename back, reopen the old one and fail.
   * **Offline `--replace`**: the same two renames, holding `DIR`'s lock across them.
8. Any failure removes the temporary directory. At startup, recovery removes any
   `databases/.restore-*`. A `databases/.replaced-X` with no `databases/X` means the
   process crashed between the two renames. It is renamed back to `X` with a WARN.

**Identity rule** (`identity: "auto" | "new" | "keep"`, default `auto`). CI requires that
`(datasetId, seq)` names exactly one state, so restoring must not let a lineage reuse a
`seq` for different data:

* `auto` keeps the dataset id when no dataset registered on this server has that id.
  That covers a fresh server, a DR restore, and a restore after the original was
  deleted. Otherwise `auto` acts as `new`. In particular, restoring a backup next to, or
  in place of, its still-live source gets a new id.
* `new` mints a fresh id. `sparkles::commit::reidentify(dir, new_id, forked_from)`
  rewrites `dataset.json` (`origin: "restore"`, `forkedFrom: {id, seq}`), the
  `datasetId` in `gen-NNNN/commit.json`, and the `commits.bin` header (UUID and header
  CRC). Commit metadata 0..s and the seqs are kept, and the next commit is s + 1 under
  the new id. WAL records carry no dataset id, so they need no change.
* `keep` is refused with `409 duplicate-dataset-id` if another registered dataset has
  the id. In place, it is refused if the target's head is greater than `s`, because
  those commits would be re-issued.

The response and the restored `DatasetInfo` show `forkedFrom` for `new`. They always
show `restoredFrom: {repository, backup, datasetId, seq}`, read from `restore.json`.

**Permissions for DR.** C09 levels do not depend on whether a dataset exists. Restoring a
backup of `wiki` onto a fresh server therefore needs `admin` on the name `wiki`, which
normally means `server-admin`. It does not need an existing dataset.

### 2.4 Verify

| Level | Checks | Cost |
|---|---|---|
| `exists` (default) | The manifest parses and validates. Every referenced blob is present with the stored length. One `LIST blobs/` covers the whole repository, or a single backup uses a `HEAD` per blob when that is cheaper. | LIST requests only |
| `data` | `exists`, plus every blob is downloaded, decoded and hashed. | reads every byte (egress) |
| `restore` | `data`, plus a full restore into `<data>/tmp/verify-<task>` (a temporary directory offline), `sparkles::check` in full mode, and the head and quad-count match. The directory is then removed. | `data` + local disk + check time |

`POST /$/repositories/{repo}/verify` runs `exists` or `data` for every backup. It also
reports **orphans**, the blobs that no manifest references. Orphans are GC candidates,
not errors.

The report:

```ts
type VerifyReport = {
  level: "exists" | "data" | "restore"; status: "ok" | "warning" | "error";
  backups: { name: string; status: "ok" | "error"; missing: string[]; corrupt: string[];
             check?: CheckReport /* sparkles::check JSON, level restore */ }[];
  orphans?: { blobs: number; bytes: number };        // repository verify only
  requests: { list: number; head: number; get: number }; millis: number;
};
```

### 2.5 HTTP API

**Paths.** `/$/snapshots/{ds}[/{name}]` belongs to F06 named snapshots, so repository
copies do not use "snapshot" in paths. Per-dataset operations live under
`/$/backups/{ds}/…`, with the dataset name as the first path parameter. That lets the C09
middleware authorize them with the existing R/A levels on `{ds}`, like `/$/compact/{ds}`
and `/$/snapshots/{ds}`. Repository and policy administration is server-wide. The
Fuseki-compatible `POST /$/backup/{ds}` (singular) keeps writing gzipped N-Quads,
unchanged.

| Method | Path | C09 need | Result |
|---|---|---|---|
| GET | `/$/repositories` | Caller, filtered | `{ repositories: Repository[] }`. `server-admin` gets full objects. Principals with `admin` on at least one dataset get only `{name, type, readonly, reachable}`, enough to choose a target. Others get `[]`. |
| POST | `/$/repositories` | S(server-admin) | Body `RepositoryConfig`. `?verify=false` skips the test. Returns `201` + `Location` + `Repository`, with the test report. |
| GET | `/$/repositories/{repo}` | S(server-admin) | `Repository` |
| PUT | `/$/repositories/{repo}` | S(server-admin) | Replaces the settings of an API repository. The location (type, path, bucket, prefix, endpoint) cannot change: `409 location-immutable`. |
| DELETE | `/$/repositories/{repo}` | S(server-admin) | `204`. Only unregisters the repository and leaves its contents untouched. `409` while a task or policy uses it. |
| POST | `/$/repositories/{repo}/test` | S(server-admin) | `200 TestReport` |
| POST | `/$/repositories/{repo}/verify` | S(server-admin) | body `{level: "exists"\|"data"}`. `202 Task` → `detail: VerifyReport` |
| GET | `/$/repositories/{repo}/backups` | S(server-admin) | Every backup in the repository, including those of datasets not on this server (for DR). Filters: `?dataset=NAME`, `?datasetId=`, `?policy=`, and `?limit=&before=` (by `completed`). |
| POST | `/$/repositories/{repo}/gc` | S(server-admin) | Phase 2. Body `{dryRun?: bool, graceHours?: number}`. `202 Task` → `detail: GcReport` |
| GET | `/$/repositories/{repo}/locks` | S(server-admin) | Phase 2. `{ locks: Lock[] }` |
| DELETE | `/$/repositories/{repo}/locks/{id}` | S(server-admin) | Phase 2. Breaks a lock: `204`, audited |
| GET | `/$/backups/{ds}` | R | Backups whose `dataset.name == ds` or whose `dataset.id` equals the live dataset's id, across all repositories this principal may see. `?repository=` narrows the search. Returns `{ dataset, datasetId, backups: BackupSummary[] }`, newest first, each with `sameLineage`. |
| POST | `/$/backups/{ds}` | A | Body `{repository, name?, note?}`. Returns `202 Task` + `Location: /$/backups/{ds}/{repo}/{name}`. Errors: `409 backup-exists`, `409 backup-in-progress`, `409 repository-read-only`, `501 backup-unsupported` (mem). |
| GET | `/$/backups/{ds}/{repo}/{backup}` | R | `Backup`, the manifest view (§5.5). `404` unless the manifest's `dataset.name == ds` or its id equals the live dataset's id. |
| DELETE | `/$/backups/{ds}/{repo}/{backup}` | A | `204`. Deletes the manifest only, and the blobs go at the next GC. `409 backup-busy` while a restore or verify of it runs on this server. |
| POST | `/$/backups/{ds}/{repo}/{backup}/restore` | A, plus A on `target` (handler, the C09 clone rule) | Body `RestoreRequest`. Returns `202 Task` + `Location: /$/datasets/{target}`. |
| POST | `/$/backups/{ds}/{repo}/{backup}/verify` | A | Body `{level}`. `202 Task` → `detail: VerifyReport` |
| GET, POST | `/$/backup-policies` | S(server-admin) | Phase 2. `{ policies: Policy[] }` / create: `201` |
| GET, PUT, DELETE | `/$/backup-policies/{policy}` | S(server-admin) | Phase 2 |
| POST | `/$/backup-policies/{policy}/run` | S(server-admin) | Phase 2. Runs the policy now without moving the schedule. `202 Task` |
| POST | `/$/backup-policies/{policy}/retention` | S(server-admin) | Phase 2. `?dryRun=true` → `200 { delete: BackupSummary[], keep: … }` |
| GET | `/$/backup-policies/{policy}/runs` | S(server-admin) | Phase 2. `{ runs: PolicyRun[] }`, newest first, `?limit=` |
| POST | `/$/backup-policies/preview` | S(server-admin) | Phase 2. body `{schedule, timezone, count?: 5}` → `{ next: string[], description: string }` |
| DELETE | `/$/tasks/{id}` | handler: A on `task.dataset` (`server-admin` for tasks without a dataset) | **New and generic**: cancels the task. `202` if the task accepts cancellation. `409 not-cancellable` if it does not, or if it has finished. |

For `/$/backups/{ds}/{repo}/{backup}`, the handler checks that the backup belongs to
`{ds}` (by name or live id). Without that check, `admin` on `x` would allow restoring a
backup of `secret` via `/$/backups/x/repo/secret-backup`. A failed check returns `404`,
which hides the backup the way C09 hides datasets. All new route templates are added to C09's `ROUTES`
table (`auth/routes.rs`) with the needs above. Unlisted templates fail closed to
`server-admin`.

**`--read-only` servers**:

* Backup create and verify work, because they read datasets and write only to
  repositories, as `/$/backup/{ds}` does today.
* Backup delete and GC work, but not on read-only repositories.
* Restore and every repository or policy mutation over the API return
  `403 server is read-only`. Config-file repositories and policies still load.
* The scheduler runs.

**Types:**

```ts
type RepositoryConfig = {
  name: string;
  type: "fs" | "s3" | "gcs" | "azure";
  path?: string;                                 // fs: absolute; not inside <data>
  bucket?: string; prefix?: string;              // s3/gcs/azure
  region?: string; endpoint?: string;            // s3: endpoint for MinIO/R2/…
  pathStyle?: boolean; allowHttp?: boolean;      // s3: path-style addressing; http:// endpoints
  credentials?: Credentials;                     // default { source: "default" }
  sse?: null | "AES256" | "aws:kms"; kmsKeyId?: string;   // s3 server-side encryption
  conditionalWrites?: boolean;                   // default true; false = single writer
  readonly?: boolean;                            // default false
  maxConcurrency?: number;                       // parallel object requests; 8 (s3), 4 (fs)
  maxUploadBytesPerSec?: number | null;          // null = unlimited
  maxDownloadBytesPerSec?: number | null;
};
type Credentials =
  | { source: "default" }                        // object_store env + provider chain (AWS_*, web identity, instance metadata)
  | { source: "env"; accessKeyIdVar: string; secretAccessKeyVar: string; sessionTokenVar?: string }
  | { source: "file"; path: string };            // JSON {accessKeyId, secretAccessKey, sessionToken?}, re-read at each use
type Repository = RepositoryConfig & {
  source: "api" | "config";
  id: string | null;                             // repository UUID; null until reachable
  status: { reachable: boolean; checked: string; error?: string;
            conditionalWrites?: boolean; singleWriter: boolean };
  stats: null | { backups: number; datasets: number; storedBytes: number;
                  logicalBytes: number; dedupRatio: number; asOf: string };   // from the last listing or GC
};
type TestReport = { ok: boolean; conditionalWrites: boolean;
  steps: { step: "create" | "create-again" | "read" | "list" | "delete"; ok: boolean; millis: number; error?: string }[] };
type BackupSummary = {
  name: string; repository: string;
  dataset: { name: string; id: string };
  commit: { seq: number; timestamp: string; quads: number; ref: string };   // ref "commit:42"
  created: string; completed: string; millis: number;
  logicalBytes: number;        // sum of file sizes
  addedBytes: number;          // stored bytes of blobs this backup uploaded first
  policy: string | null; run: string | null; note: string | null;
  sameLineage?: boolean;       // only under /$/backups/{ds}
};
type Backup = BackupSummary & {
  format: 1; generation: string; indexFormat: number; parent: string | null;
  server: { version: string };
  files: { path: string; kind: "immutable" | "append" | "meta"; size: number; sha256: string;
           blobs: { id: string; size: number }[] }[];
  derived: { text: null | { rebuildOnRestore: true } };
};
type RestoreRequest = {
  target?: string;                               // default {ds}
  replace?: boolean;                             // default false
  identity?: "auto" | "new" | "keep";            // default auto (§2.3)
  check?: "quick" | "full" | "none";             // default quick
  keepReplaced?: boolean;                        // default false
};
type Task = {                                    // existing type, extended
  id: string;
  kind: "compact" | "backup" | "reason" | "load" | "clone"
      | "backup-create" | "backup-restore" | "backup-verify" | "backup-gc" | "backup-policy";
  dataset: string; target?: string;
  state: "queued" | "running" | "done" | "failed" | "cancelled";   // queued, cancelled: new
  startedAt: string; finishedAt?: string; message?: string; progress?: number;
  cancellable?: boolean;                                            // new
  detail?: object;              // new: BackupSummary, VerifyReport, GcReport, PolicyRun
};
```

`backup-create` reports progress `0.02` at capture, `0.05` at planning, `0.05–0.95`
during the upload (by bytes) and `0.97` for the manifest. The message reads
`uploading 120/310 MB · 14 new blobs · 3 reused`. `detail` gets the `BackupSummary` when
the task finishes.

**Errors** use `{ "error": message, "code": … }`, as in F06. The codes are `invalid-name`,
`invalid-config`, `no-such-repository`, `no-such-backup`, `repository-exists`,
`not-a-repository`, `location-immutable`, `backup-exists`, `backup-in-progress`,
`backup-busy`, `repository-read-only`, `repository-locked` (409; the body names the
holder), `dataset-exists`, `dataset-busy`, `not-managed`, `duplicate-dataset-id`,
`incompatible-repository`, `incompatible-format`, `invalid-backup` (422: manifest
validation), `insufficient-storage` (507), `backup-unsupported` (501),
`repository-unavailable` (502, for backend errors, with query strings redacted from
URLs in the backend message), and `restore-mismatch` (500).


### 2.6 Lifecycle policies (Phase 2)

```ts
type Policy = {
  name: string;                        // [a-z0-9][a-z0-9_-]{0,63}
  repository: string;
  datasets: string[];                  // names or C09-style globs; default ["*"]; mem datasets are skipped (reported)
  schedule: string;                    // cron (5 fields, or 6 with seconds) or "every <dur>" (≥ 1m)
  timezone: string;                    // IANA name; default "UTC"
  nameTemplate: string;                // default "{policy}-{dataset}-{time}"
  retention: { expireAfter: string | null; minCount: number; maxCount: number | null };
  skipUnchanged: boolean;              // default false: skip a dataset whose head equals its last policy backup's commit (same id)
  gcAfterRetention: boolean;           // default false; at most one GC per repository per 24 h
  catchUp: "one" | "none";             // default "one"
  enabled: boolean;
  source: "api" | "config";
  state: { nextRun: string | null; lastScheduledFor: string | null; lastRun: PolicyRun | null;
           lastSuccess: string | null; consecutiveFailures: number; runningTask: string | null };
};
type PolicyRun = {
  id: string; policy: string; trigger: "schedule" | "catch-up" | "manual";
  scheduledFor: string | null; started: string; finished: string | null;
  result: "ok" | "partial" | "failed" | "skipped";   // skipped: overlap or policy disabled mid-run
  datasets: { dataset: string; backup: string | null; result: "ok" | "failed" | "skipped";
              reason?: string; addedBytes?: number; millis?: number }[];
  retention: { deleted: string[]; error?: string } | null;
  gc: { task: string } | null;
};
```

* **Schedule.** Cron uses `croner` (MIT) with the policy's IANA time zone, through
  `chrono-tz`. A test pins these rules:
  * a local time that does not exist (spring forward) runs at the first instant after
    the gap;
  * an ambiguous local time (fall back) runs once, at its first occurrence;
  * `every 6h` counts from the Unix epoch in UTC (`00:00, 06:00, …`), so it is stable
    across restarts.
* **Name template.** The placeholders are:
  * `{policy}`, `{dataset}`, `{seq}` (the head commit), `{run}` (the first 8 hex digits of
    the run id);
  * `{time}`: the scheduled instant in UTC, `YYYYMMDDtHHMMSSz`;
  * `{date:FMT}`: a strftime subset (`%Y %m %d %H %M %S %j %V`) in the policy's time
    zone.

  The rendered name must match the backup-name grammar, which is checked at save time
  with a sample. On a `409 backup-exists`, `-2`, `-3`, … are appended.
* **Execution.** A scheduler thread in the server sleeps until the earliest `nextRun`,
  for at most 60 s at a time so that it notices clock changes. For each due, enabled
  policy it starts one `backup-policy` task. That task backs up the selected datasets one
  after another, sharing the backup concurrency slots (§2.10). It then applies retention
  and optionally starts GC. If the policy's previous run is still running, the new run is
  recorded as `skipped` (`overlap`).
* **Missed runs.** `lastScheduledFor` is persisted. At startup and after each wake, if
  the most recent scheduled instant ≤ now is later than `lastScheduledFor`, the policy is
  due. With `catchUp: "one"`, one run (`trigger: "catch-up"`) covers all missed instants.
  It starts 60 s after startup, so the server is ready first. With `"none"`, the missed
  instants are recorded as skipped and only the next one runs. A backward clock jump
  never re-runs an instant, because `lastScheduledFor` is monotonic.
* **Retention** runs after every run and on `POST …/retention`. It applies only to
  backups whose manifest names this policy. Manual and other policies' backups are never
  touched. Per dataset id, it sorts that policy's backups by `completed`, newest first,
  and:
  * keeps the first `minCount` (default 1) unconditionally;
  * of the rest, deletes those at an index ≥ `maxCount`, or older than `expireAfter`
    (`completed` + `expireAfter` < now).

  A backup that a restore or verify task on this server is using is kept until the next
  evaluation. Failed runs produce no manifests, so they never count toward `minCount`.
* **State and history.** `<data>/backup/policies.json` holds API policies.
  `<data>/backup/policy-state.json` holds `lastScheduledFor`, `lastSuccess` and
  `consecutiveFailures`. `<data>/backup/runs.json` is a ring of the last 1000
  `PolicyRun`s. All are written with `write_file_atomic`.
* **Alerting.** The metrics are
  `sparkles_backup_policy_last_success_timestamp_seconds`, `…_consecutive_failures` and
  `…_runs_total{result}` (§2.11). A WARN log line (`sparkles::backup`) is written for each
  failed dataset and each failed run. `docs/API.md` gives a Prometheus alert example:
  `time() - sparkles_backup_policy_last_success_timestamp_seconds > 2 * <period>`.

### 2.7 Garbage collection (Phase 2)

GC runs on `POST /$/repositories/{repo}/gc`, on `sparkles repo gc`, or through
`gcAfterRetention`. It has two phases, so the exclusive lock is held only for the sweep:

1. **Mark**, under a *shared* lock: list `backups/`, read every manifest (cached, §5.6),
   and build the referenced blob set R₁. List `blobs/`. The candidates C are the blobs
   not in R₁ whose `last_modified` is older than the grace period (default 24 h). The
   grace period covers writers that do not take locks: Phase 1 binaries writing to a
   repository that a Phase 2 server collects, or a writer whose lock went stale while it
   was paused.
2. **Sweep**, under an *exclusive* lock:
   * re-list `backups/`, read only the manifests not seen in mark, and remove their blobs
     from C;
   * delete C in batches (`object_store::ObjectStore::delete_stream`, which uses S3
     `DeleteObjects` of up to 1000 keys);
   * delete stale locks and `probe/` leftovers;
   * write `gc/last.json` (the report).

   `dryRun` stops before deleting and reports C.

```ts
type GcReport = { dryRun: boolean; manifests: number; referencedBlobs: number;
  listedBlobs: number; candidates: number; deleted: number; deletedBytes: number;
  keptYoung: number; storedBytesAfter: number;
  requests: { list: number; get: number; delete: number }; millis: number; lockWaitMillis: number };
```

**Cost on S3.** GC makes ⌈blobs/1000⌉ + ⌈manifests/1000⌉ LIST requests, twice, plus one
GET per manifest not in the local cache and ⌈candidates/1000⌉ `DeleteObjects` requests.
At 10.5 M quads with 30 kept generations, that is ≈ 30 × 25 files, or under 1000 blobs,
so a handful of requests. Backups waiting for the exclusive lock retry for up to
`lockWait` (10 min) and then fail with `409 repository-locked`.

**Versioned buckets.** GC deletes leave delete markers and noncurrent versions. The
documentation recommends a bucket lifecycle rule that expires noncurrent versions.

### 2.8 CLI

The offline commands work on a repository directly, without a server. `--repo` takes
either a repository **name** from the backup config file or a **URL**. The config file
comes from `--backup-config FILE` or `$SPARKLES_BACKUP_CONFIG`, and defaults to
`$XDG_CONFIG_HOME/sparkles/backup.toml`. The URL forms are:

* `file:///srv/backups/r`;
* `s3://bucket/prefix?region=eu-central-1&endpoint=http://127.0.0.1:9000&path_style=true&allow_http=true`;
* `memory://` (tests);
* `gs://…` and `az://…` (Phase 2).

URLs that carry credentials, as userinfo or as keys in the query, are rejected.
Credentials come from the environment or from `credentials` in the config file.

```
sparkles repo add NAME (--path DIR | --s3 BUCKET [--prefix P] [--region R] [--endpoint URL]
                  [--path-style] [--allow-http] [--credentials default|env|file:PATH])
                  [--readonly] [--no-init]              # edits the backup config file, initializes, tests
sparkles repo list   [--format text|json]
sparkles repo show   NAME
sparkles repo test   NAME|URL
sparkles repo verify NAME|URL [--level exists|data]     # exit 0 ok, 1 errors, 2 warnings (orphans)
sparkles repo remove NAME                               # from the config file; contents untouched
sparkles repo gc     NAME|URL [--dry-run] [--grace 24h] # Phase 2
sparkles repo locks  NAME|URL [--break ID]              # Phase 2

sparkles backup create  --loc DB --repo R [--name N] [--note TEXT]
sparkles backup list    --repo R [--dataset NAME | --dataset-id UUID] [--policy P] [--format text|json]
sparkles backup show    --repo R NAME [--format text|json]
sparkles backup delete  --repo R NAME
sparkles backup restore --repo R NAME (--to DIR [--replace] | --data DATA [--as DS])
                        [--identity auto|new|keep] [--check quick|full|none]
sparkles backup verify  --repo R NAME [--level exists|data|restore]
sparkles backup policy  (list | show P | run P | history P | preview SCHEDULE [--tz Z])   # Phase 2; reads --backup-config
sparkles backup --loc DB --out DIR                       # unchanged: gzipped N-Quads (legacy form)
```

* `backup create --loc` opens the database like every CLI command. If a server holds
  the lock, it prints the existing "in use by another process … talk to it over HTTP"
  message. Phase 2 adds `--server URL --dataset DS` for create, list and restore,
  through the [C09 §7.4](C09-dataset-access-control.md) remote client.
* `backup restore --data DATA --as DS` restores into `DATA/databases/DS` and adds the
  registry entry to `DATA/config.json`. The server must be stopped. Restore refuses when
  `DATA/databases/DS/sparkles.lock` is held. Open question 9 covers a lock on the whole
  data directory. Without `--as`, the manifest's dataset name is used. In the CLI, the
  `auto` identity rule considers the datasets in `DATA/config.json`. With `--to`, `auto`
  means `keep`.
* `sparkles backup` keeps its current meaning when given `--loc`/`--out` and no
  subcommand (clap `args_conflicts_with_subcommands`).
* `repo add` and `repo remove` rewrite the config file atomically, through a serde round
  trip. Comments are not preserved, and the file header says so.
* Progress goes to stderr, one line per 5 % or per 2 s. Example output:

```
$ sparkles backup create --loc db --repo local --name b2
backup b2 of ds (3f1c9a2e…) at commit 4 · 23 files · 286.1 MB logical · 71.3 KB added · 4 new blobs · 0.41 s

$ sparkles backup list --repo local
name  dataset  commit  completed                 logical   added     policy
b2    ds       4       2026-09-30T14:05:12.101Z  286.1 MB  71.3 KB   -
b1    ds       3       2026-09-30T14:03:11.482Z  286.0 MB  286.0 MB  -
repository 7d0e…  2 backups · 1 dataset · stored 286.1 MB · dedup 2.00×
```

### 2.9 Config file (`--backup-config`, TOML)

```toml
# /etc/sparkles/backup.toml — no secrets here; credentials come from the environment or files
version = 1

[repositories.local]
type = "fs"
path = "/srv/backups/sparkles"

[repositories.s3-main]
type = "s3"
bucket = "kg-backups"
prefix = "prod/sparkles"
region = "eu-central-1"
# endpoint = "http://127.0.0.1:9000"   # MinIO, R2, …
# path_style = true
credentials = { source = "file", path = "/run/secrets/sparkles-s3.json" }
sse = "aws:kms"
kms_key_id = "arn:aws:kms:eu-central-1:111122223333:key/…"
max_concurrency = 8
max_upload_bytes_per_sec = 104857600

[repositories.dr-source]
type = "s3"
bucket = "kg-backups"
prefix = "prod/sparkles"
readonly = true

[policies.nightly]                       # Phase 2
repository = "s3-main"
datasets = ["*"]
schedule = "30 2 * * *"
timezone = "Europe/Berlin"
name_template = "{policy}-{dataset}-{date:%Y%m%d}"
retention = { expire_after = "30d", min_count = 7, max_count = 60 }
gc_after_retention = true
```

Validation follows [C09 §4.1](C09-dataset-access-control.md): `deny_unknown_fields`, errors with line and column, and a
WARN if the file is readable by group or others. Names are unique across the file and the
API registry. An API registration with a config name fails with `409 repository-exists`.

### 2.10 Limits, retries, cancellation

| Limit | Default | Where |
|---|---|---|
| Concurrent backup/restore/verify/GC tasks per server | 2 | `serve --backup-max-tasks`. Extra tasks are `queued`. |
| Parallel object requests per repository | 8 (s3), 4 (fs) | `maxConcurrency` (`object_store::limit::LimitStore`) |
| Upload / download bandwidth per repository | unlimited | `maxUploadBytesPerSec` / `maxDownloadBytesPerSec`, a token bucket shared by the repository's tasks |
| Piece size | 32 MiB | the repository marker (fixed at init) |
| Segments per append-only file before consolidation | 64 | §5.4 |
| Lock refresh / stale age / wait | 5 min / 30 min / 10 min | §5.7 |

**Retries.** `object_store::RetryConfig` handles transient errors (5xx, throttling,
connection resets) with exponential backoff, up to 10 retries within 3 min per request.
Backups add two rules on top:

* a blob whose download fails its hash is fetched again up to 2 times, then fails;
* an upload that fails after retries fails the task. Blobs already uploaded stay and are
  reused by the next attempt.

**Cancellation.** `DELETE /$/tasks/{id}`, or Ctrl-C in the CLI, sets the task's cancel
flag. The task checks it between object requests and every 8 MiB of streamed data.

**Partial failures.** The manifest is written last, with `PutMode::Create`. A failed or
cancelled backup therefore never has a visible manifest. It leaves only unreferenced
blobs, which GC collects after the grace period. A failed restore removes its temporary
directory. An in-place restore cancelled before the swap leaves the dataset untouched.
Once the swap has started, the restore cannot be cancelled (`cancellable: false`).

### 2.11 Observability

Metrics are served as Prometheus at `/$/metrics` and exported to OTel as observable
instruments, like the existing ones. The label sets are closed. `repository` is capped
like `dataset`, by `--metrics-max-datasets` with overflow into `$other`.

| Name | Type | Labels |
|---|---|---|
| `sparkles_backup_operations_total` | counter | `repository`, `operation` = `create`\|`restore`\|`verify`\|`delete`\|`gc`, `result` = `ok`\|`failed`\|`cancelled` |
| `sparkles_backup_operation_duration_seconds` | histogram | `operation` |
| `sparkles_backup_bytes_uploaded_total` / `…_downloaded_total` | counter | `repository` |
| `sparkles_backup_blobs_uploaded_total`, `…_blobs_reused_total` | counter | `repository` |
| `sparkles_backup_object_requests_total` | counter | `repository`, `op` = `put`\|`get`\|`head`\|`list`\|`delete`, `result` = `ok`\|`error` |
| `sparkles_backup_last_success_timestamp_seconds` | gauge | `dataset`, `repository` |
| `sparkles_backup_capture_lock_seconds` | histogram | (writer-lock hold time of captures) |
| `sparkles_backup_repository_stored_bytes`, `…_logical_bytes`, `…_backups` | gauge | `repository` (from the last listing or GC) |
| `sparkles_backup_lock_conflicts_total` | counter | `repository` |
| `sparkles_backup_policy_runs_total` | counter | `policy`, `result` |
| `sparkles_backup_policy_last_success_timestamp_seconds`, `…_next_run_timestamp_seconds`, `…_consecutive_failures` | gauge | `policy` |

* **Spans.** Each task is already the root span `task {kind}`, linked to its request.
  Its children are `backup.capture` (`sparkles.commit`, `sparkles.backup.lock_ms`),
  `backup.plan` (files, reused, new), `backup.upload` (bytes, blobs), `backup.manifest`,
  `restore.download`, `restore.check`, `restore.open`, `restore.swap`, `verify.list`,
  `verify.read`, `gc.mark`, `gc.sweep` and `repo.lock`. The common attributes are
  `db.namespace`, `sparkles.repository` and `sparkles.backup.name`. Object requests are
  too many to be spans, so they are counted and summed as span attributes.
* **Logs.** `sparkles::backup` logs INFO at each task's start and end, with summary
  fields. It logs WARN on retries that eventually succeed (at most one per task per
  minute), broken locks and stale-lock takeovers. The **audit** events
  (`sparkles::audit`) are `repository_added`, `repository_removed`, `backup_deleted`,
  `restore_started`, `restore_finished` (target, identity), `gc_finished`, `lock_broken`
  and `policy_changed`, each with the principal.
* The access log is unchanged. The new routes appear with their templates, and
  `obs::route_op` classifies them as `admin`.

## 3. Standards and behavioral basis

* **S3 conditional writes** (AWS S3 User Guide, "How to prevent object overwrites with
  conditional writes"):
  * `If-None-Match: *` on `PutObject` and `CompleteMultipartUpload` fails with
    `412 Precondition Failed` when the key exists, and "the first write operation to
    finish succeeds";
  * `409 Conflict` can occur with a concurrent delete, and a `PutObject` may be retried
    then;
  * `If-Match` compares ETags.

  `object_store` exposes this as `PutMode::Create` / `PutMode::Update`. For S3-compatible
  services it is enabled with `S3ConditionalPut::ETagMatch` ("Cloudflare R2 and minio
  support conditional put using the standard HTTP precondition headers").
* **S3 consistency** (AWS S3 User Guide, "Amazon S3 data consistency model"):
  * strong read-after-write consistency for PUT, DELETE and LIST;
  * "Updates to a single key are atomic";
  * concurrent PUTs to one key are last-writer-wins, and "there is no way to make atomic
    updates across keys".

  Hence a manifest is one key, created last. Name uniqueness uses `If-None-Match`, and
  lock visibility relies on LIST after PUT.
* **Behavior borrowed from Elasticsearch/Kibana public documentation.** Only behavior was
  borrowed (§10):
  * incremental snapshots that copy only new immutable files, each snapshot logically
    independent, and deletion removing only exclusively used data. Sparkles does this
    with immutable generations and GC;
  * repositories registered before use, with verification, and a second cluster
    registering the repository read-only;
  * restore with renaming, and no restore over an open index. Sparkles restores to a new
    name or swaps the dataset on the server;
  * policies with a cron schedule, date-based name templates, a repository and a
    selection, and retention by `expire_after`, `min_count` and `max_count` that applies
    only to the policy's own snapshots;
  * "run now" without moving the schedule, and execution history and statistics;
  * a UI with snapshot, repository, policy and restore views.

  Sparkles defines its own names, endpoints, JSON and repository layout.
* **Content-addressed backup design** (restic design reference, BSD-2-Clause):
  * blobs named by the SHA-256 of their content;
  * write ordering: data before index before snapshot, so a crash leaves only
    unreferenced data;
  * exclusive and non-exclusive lock files that go stale after 30 minutes;
  * only prune deletes data;
  * content-defined chunking (Rabin, 512 KiB–8 MiB, ≈ 1 MiB average), considered for
    Phase 3.

  Sparkles differs in three ways:
  * no pack files, because its files are few and large;
  * no separate index, because manifests list blob ids;
  * staleness measured by the storage server's clock.
* **RFC 9110** status codes (409, 412 semantics, 422, 501, 502, 503 with `Retry-After`,
  507). **RFC 3339** timestamps. **RFC 9562** UUIDs. **IANA time zone database** names.

## 4. Semantics

### 4.1 Invariants

1. A backup is visible iff its manifest object exists. It is created last, never
   modified, and deleted as one object.
2. Every blob a visible manifest references was fully written, and its SHA-256 matched,
   before the manifest was created. GC never deletes a blob referenced by a manifest that
   existed when its sweep re-listed (§2.7).
3. A restore writes the target only after every byte has been verified against the
   manifest. `CURRENT` is written with the rest, and the directory becomes visible only
   through a rename.
4. The captured commit `s` is acknowledged (durable) in the source, the WAL prefix ends
   at the end of `s`'s commit record, and the `commits.bin` prefix ends at `s`'s record.
   So the restored store's head is exactly `s`.
5. A restored lineage never reuses a `(datasetId, seq)` pair for different data on the
   same server (the identity rule, §2.3).

### 4.2 Commit identity in the manifest

`commit` is the full CI `Commit` object of `s` (seq, parent, timestamp, kind, counts,
generation, bulk, exact). `dataset.id` is `Store::dataset_id()`. Together they name the
state. Two backups with the same `(dataset.id, commit.seq)` hold the same data, although
their files differ if a compaction happened between them.

### 4.3 What "restore in place when stopped" means

Sparkles has no "closed dataset" state, so the server stops the dataset itself. It
downloads and checks first, then briefly takes the dataset out of service (§2.3 step 7).
Offline, the database lock serves as the "stopped" check.

## 5. Design

### 5.1 Crates and modules

* `crates/sparkles` (core, no network dependencies):
  * `store.rs`: `Store::backup_capture() -> Result<BackupCapture>` (§5.3);
    `DeltaVocab::flush()`; `Catalog::flushed_len()`.
  * `history.rs`: in-memory **leases**
    (`HistoryState.leases: BTreeMap<u64, Lease { seq, label }>`). `protected()` includes
    them as `Hold::Lease(label)`, which `heldBy` shows as `backup:<name>`. Leases are
    never written to `history.json`.
  * `commit.rs`: `reidentify(root, new_id, forked_from)` and the dataset origin
    `"restore"`.
* `crates/sparkles-backup` (new, Apache-2.0) depends on `sparkles`, `object_store`
  (features `fs` and `aws`, with `gcp` and `azure` behind crate features), `tokio`,
  `sha2`, `lz4_flex`, `serde_json`, `uuid`, `futures`, and `croner` + `chrono-tz`
  (Phase 2). Its modules are:
  * `repo.rs`: config, URL parsing, backend construction, marker, the connection test;
  * `blob.rs`: blob format, hashing, pieces;
  * `manifest.rs`: types and validation;
  * `lock.rs`;
  * `create.rs`, `restore.rs`, `verify.rs`, `gc.rs`;
  * `cache.rs`: the manifest cache;
  * `throttle.rs`: the token bucket;
  * `policy.rs`: schedule and retention evaluation as pure functions, and name
    templates.

  Its public API is async:

```rust
pub struct Repository { /* Arc<dyn ObjectStore>, id, config, cache */ }
impl Repository {
    pub async fn open(cfg: &RepoConfig, cache_dir: Option<&Path>) -> Result<Repository>; // attach or init
    pub async fn test(&self) -> Result<TestReport>;
    pub async fn list(&self, f: &ListFilter) -> Result<Vec<BackupSummary>>;
    pub async fn manifest(&self, name: &str) -> Result<Manifest>;
    pub async fn create(&self, cap: BackupCapture, o: &CreateOptions) -> Result<BackupSummary>;
    pub async fn restore(&self, name: &str, dir: &Path, o: &RestoreOptions) -> Result<RestoreReport>;
    pub async fn verify(&self, names: &[String], o: &VerifyOptions) -> Result<VerifyReport>;
    pub async fn delete(&self, name: &str) -> Result<bool>;
    pub async fn gc(&self, o: &GcOptions) -> Result<GcReport>;                     // Phase 2
}
pub struct CreateOptions { pub name: String, pub note: Option<String>, pub policy: Option<(String, String)>,
                           pub extra: Vec<(String, Vec<u8>)> /* reasoning.json, origin.json */,
                           pub cancel: Arc<AtomicBool>, pub progress: Option<ProgressFn> }
```

* `crates/sparkles-server`:
  * `backup/mod.rs`: the registry of repositories and policies, config loading, the
    task-slot semaphore;
  * `backup/http.rs`: routes;
  * `backup/scheduler.rs` (Phase 2);
  * `backup/cli.rs`: the `repo` and `backup` subcommands;
  * `state.rs`: `Task` gains `cancellable`, `detail`, and the `queued` and `cancelled`
    states. `TaskHandle` gains `cancel_flag()` and `set_detail()`. A generic cancel route
    is added, and `clone` wires its existing `CloneOptions::cancel` to it.
  * Server tasks run on std threads, as they do now, and drive the async API with
    `tokio::runtime::Handle::block_on` on the server's runtime handle. The CLI builds a
    current-thread runtime.
  * The Cargo feature `backup` is on by default, and `object_store` is optional behind
    it.

### 5.2 Repository layout (format 1)

```
<prefix>/
  sparkles-repo.json            marker, created once (PutMode::Create)
  blobs/<hh>/<64 hex>           content blobs, immutable; <hh> = first two hex digits
  backups/<name>.json           manifests, immutable, created last (PutMode::Create)
  locks/<uuid>.json             lease objects (§5.7)
  probe/<uuid>                  connection-test objects, deleted by the test (or by GC)
  gc/last.json                  last GC report (informational, overwritten)
```

`sparkles-repo.json`:

```json
{ "format": 1, "kind": "sparkles-backup-repository",
  "id": "7d0e5c1a-…", "created": "2026-09-30T14:00:00.000Z", "createdBy": "sparkles 0.1.0",
  "hash": "sha256", "pieceBytes": 33554432, "encryption": null }
```

A reader refuses `format > 1`. Unknown top-level fields are ignored, so additive changes
stay format 1. This build refuses a repository whose `encryption` is not `null`. Phase 3
sets that field.

A **blob object** is laid out as follows, little-endian:

| Bytes | Field |
|---|---|
| 0..4 | magic `SPKB` |
| 4 | format `1` |
| 5 | encoding: `0` raw, `1` LZ4 frame (`lz4_flex`), `2..` reserved (Phase 3 encryption) |
| 6..8 | zero |
| 8..16 | plaintext length |
| 16.. | payload |

* The **blob id** is the lowercase hex SHA-256 of the plaintext. Ids do not depend on
  encoding, so two writers that compress differently still dedup.
* The writer compresses a piece with LZ4 and keeps the result only if it saves ≥ 10 %.
  The WAL, catalog, delta vocabulary and JSON compress well. Index blocks mostly do not.
* Readers check the magic, decode, then check the length and the SHA-256.

A **manifest** is `backups/<name>.json`, UTF-8 JSON of at most 16 MiB:

```json
{ "format": 1, "kind": "sparkles-backup",
  "name": "b2", "id": "0b6e…", "repositoryId": "7d0e5c1a-…",
  "dataset": { "name": "ds", "id": "3f1c9a2e-…", "type": "persistent" },
  "commit": { "seq": 4, "parent": 3, "ref": "commit:4", "timestamp": "2026-09-30T14:05:11.990Z",
              "kind": "update", "inserted": 1, "deleted": 0, "quads": 3, "generation": "gen-0001",
              "bulk": false, "exact": true },
  "generation": "gen-0001", "indexFormat": 2,
  "created": "2026-09-30T14:05:11.995Z", "completed": "2026-09-30T14:05:12.101Z", "millis": 106,
  "server": { "version": "0.1.0" }, "parent": "b1",
  "policy": null, "run": null, "note": null,
  "files": [
    { "path": "CURRENT", "kind": "meta", "size": 8, "sha256": "…", "blobs": [ { "id": "…", "size": 8 } ] },
    { "path": "gen-0001/spo.dat", "kind": "immutable", "size": 41234567, "sha256": "…",
      "blobs": [ { "id": "…", "size": 33554432 }, { "id": "…", "size": 7680135 } ] },
    { "path": "gen-0001/wal.log", "kind": "append", "size": 231, "sha256": "…",
      "blobs": [ { "id": "…", "size": 198 }, { "id": "…", "size": 33 } ] } ],
  "stats": { "logicalBytes": 300012345, "addedBytes": 73012, "files": 23, "blobs": 31,
             "newBlobs": 4, "reusedBlobs": 27 },
  "derived": { "text": { "rebuildOnRestore": true } },
  "encryption": null }
```

`files[].sha256` is the hash of the whole restored file. It is redundant with the blob
hashes, but it lets `sparkles backup show` be checked against a restored directory with
`sha256sum`.

### 5.3 Capture (consistency without blocking writers)

`Store::backup_capture(label)` works on persistent stores and returns
`Error::Unsupported` for others:

1. Take the **writer mutex**, as `clone_to` does. Refuse if it is poisoned.
2. With the lock held:
   * `snap = current`, `head = w.head`;
   * `wal_len` = the current WAL's file length. The WAL is flushed and `sync_data`'d per
     commit, and no transaction is open while the lock is held;
   * `gen.dvocab.flush()`, then `dvocab_len`;
   * `catalog.flushed_len()`, which flushes `pending` appends. If the catalog stays
     incomplete ([CI §4.3](CI-commit-identity.md)), fail with `503 catalog-lagging`,
     which is retryable;
   * `prefixes` = a clone of the in-memory map;
   * **lease**: under the history mutex,
     `leases.insert(id, Lease { seq: head.seq, label })`. The owner generation of
     `head.seq` is the current generation, so GC now treats it as needed;
   * open read handles for every file of the generation directory and `commits.bin`.
3. Release the lock. The hold time is ~25 `open` calls, one `fstat` per append-only
   file, and two buffer flushes, typically under 1 ms.
   `sparkles_backup_capture_lock_seconds` records it.
4. Outside the lock, read `CURRENT`, `dataset.json`, `text.json` and `origin.json`.
   These change only on rebuild, clone or configuration, and are validated against
   `snap`. `CURRENT` must name `snap.generation.name`; otherwise the capture is retried
   once. The server adds `reasoning.json` from `Dataset.reasoning`, applying the drop
   rule of §2.2.

```rust
pub struct BackupCapture { pub dataset_id: Uuid, pub commit: CommitInfo, pub generation: String,
                           pub index_format: u32, pub files: Vec<CapturedFile>,
                           pub prefixes: BTreeMap<String, String>, lease: LeaseGuard }
pub struct CapturedFile { pub path: String, pub kind: FileKind /* Immutable|Append|Meta */,
                          pub len: u64, pub file: std::fs::File }
```

Files are read with `pread` through the open handles, so a compaction's rename or delete
cannot affect the backup on Unix. The lease also keeps the directory itself. That makes
the backup work on Windows, and lets `HistoryStatus` show why the generation is kept.
Dropping the `LeaseGuard` removes the lease and calls `Store::collect_history()`. This is
a new best-effort F06 GC point: it tries to lock the writer mutex (`try_lock`) and
otherwise leaves the collection to the next GC point.

Append-only files are read only up to their captured lengths, and immutable files whole.
A generation file whose length changes during the read fails the backup with
`Error::Corrupt`. That should be impossible.

### 5.4 Blob planning, dedup and incrementality

**Immutable files** are split at every 32 MiB offset. Each piece is hashed from the
local file, with SHA-256 running at ~1.5–2 GB/s on CPUs with SHA extensions. Then the
piece is:

* reused if the **parent manifest** references that id. The parent is the newest backup
  of the same dataset id in this repository, read from the cache. Referenced blobs
  cannot be collected while the parent is visible, and the shared lock covers a delete
  that races with this backup (§5.7);
* otherwise, if it is over 1 MiB, checked with a `HEAD` first, so an existing blob is
  not re-sent. This dedups across datasets and after interrupted runs;
* otherwise, a `PUT` with `PutMode::Create`. An "already exists" error (412) counts as
  success.

**Append-only files** (`wal.log`, `delta.vocab`, `commits.bin`) are stored as ordered
segments. Let the parent's entry for the same path cover `[0, X)` in segments
`g₁…gₖ`:

* The parent is usable for the file iff it has the same `dataset.id`, the same
  `generation` and the same `gen-NNNN/commit.json` blob id, and the local length is
  ≥ X. For `commits.bin`, the first 64 header bytes must match instead of the blob id.
* The local file is hashed once, streaming, over `[0, len)`. That gives per-segment
  hashes for the parent's boundaries plus the whole-file hash. If every parent segment's
  hash matches, `g₁…gₖ` are reused, and `[X, len)` becomes new segments, split at
  32 MiB. If any segment differs, for example in a catalog restarted by repair or a
  hand-edited file, the file is stored from scratch as pieces and a WARN is logged.
* **Consolidation.** When k + new > 64, the file is stored from scratch as 32 MiB
  pieces. That bounds manifest size and restore request counts for hourly backups of one
  long-lived generation.

**Meta files** (`CURRENT`, `dataset.json`, `prefixes.json`, `text.json`,
`reasoning.json`, `origin.json`) are one blob each, and the blob is usually already
present.

**Whole files are used instead of content-defined chunking (CDC)** for four reasons:

* Between compactions, generation files never change, so whole-file dedup is exact, and
  WAL growth is handled by segments. An incremental backup after 1 k updates uploads
  ≈ 100 KB.
* A compaction renumbers vocabulary ids and rewrites all seven permutations. CDC would
  find almost nothing to share in the `.dat` files, and possibly some in `vocab.dat`
  (sorted, front-coded terms).
* CDC adds a chunker and many small objects (≈ 300 per 300 MB at a 1 MiB average), and
  therefore packs and an index, as in restic. That is too much for Phase 1, for a gain
  nobody has measured.
* Phase 3 measures vocabulary similarity across compactions and adds CDC for
  `vocab.dat` only if the saving exceeds 30 %.

**Pieces have a fixed size of 32 MiB** because:

* each piece is one single `PUT` (the S3 limit is 5 GB), so there are no multipart
  uploads and no dangling incomplete uploads to clean up;
* retries and resumption cost at most 32 MiB;
* pieces upload and download in parallel within one file;
* memory is bounded at `maxConcurrency` × 32 MiB (256 MiB for S3 by default). This buffer
  is needed because a piece is hashed before it is named. `fs` repositories stream from
  the file handle instead.

### 5.5 Create

1. Take a shared lock (§5.7).
2. Resolve the name. If `backups/<name>.json` exists, fail with `409 backup-exists`. The
   create in step 6 is authoritative; this check only fails early.
3. Capture (§5.3).
4. Plan (§5.4) and upload with `maxConcurrency` in flight and the bandwidth bucket.
   Progress is reported by bytes.
5. Build the manifest. Its `stats` count only the blobs this backup created
   (`addedBytes` = stored size) and the ones it reused.
6. `PUT backups/<name>.json` with `PutMode::Create`. If the object already exists, a
   concurrent writer took the name, and the answer is `409 backup-exists`. The uploaded
   blobs become orphans unless another backup references them.
7. Insert the manifest into the local cache, drop the lease, release the lock, and update
   the metrics.

### 5.6 Listing and the manifest cache

Listing is `LIST backups/`. Each `ObjectMeta` (key, size, e_tag, last_modified) is looked
up in the local cache:

* the server's cache is `<data>/backup/cache/<repository-id>/<name>.json`;
* the CLI's is `$XDG_CACHE_HOME/sparkles/backup/<repository-id>/`;
* entries are keyed by `(name, e_tag or last_modified, size)`, so a deleted-and-recreated
  name is refetched.

Misses are fetched with `GET`, up to `maxConcurrency` in parallel. A cold listing of
1000 backups is 1 LIST + 1000 small GETs, about 2 s on S3. A warm one is 1 LIST. The
repository has no mutable index object, because an index would be a contention point for
concurrent writers and would need `If-Match` loops (§8).

### 5.7 Locks

A lock object `locks/<uuid>.json` holds
`{ "kind": "shared"|"exclusive", "holder": {"host", "pid", "server": data-dir hash,
"version"}, "operation": "create"|"restore"|"verify"|"delete"|"gc", "created" }`.

* **Acquire shared.** `PUT` its own lock (`PutMode::Create`), then `LIST locks/`. If a
  non-stale exclusive lock exists, delete its own lock and retry with jittered backoff
  (1 s → 30 s), up to `lockWait`. Then fail with `409 repository-locked`.
* **Acquire exclusive.** The same, except that any other non-stale lock conflicts. Two
  exclusive acquirers see each other and both back off with jitter, so progress is
  probabilistic, as with restic's lock retry.
* **Refresh.** Every 5 minutes the holder re-PUTs its own key (`PutMode::Overwrite`).
* **Stale.** A lock is stale when its `last_modified` is older than 30 minutes, measured
  by the storage server's clock as LIST reports it. Acquirers ignore stale locks, and GC
  and `repo locks --break` delete them. For `fs` repositories, `last_modified` is the
  file mtime.
* **Release.** `DELETE` its own key. This is best effort, and a leftover goes stale.
* **Which operations lock.** Create, delete, restore and verify take shared locks, and
  GC's sweep takes an exclusive lock. Restore and verify need the shared lock so that a
  GC sweep does not remove blobs of a backup being deleted concurrently. Read-only
  repositories take no locks (§2.1).
* **Phase 1** writes and honors shared locks and refuses to start while a non-stale
  exclusive lock exists. Only Phase 2 adds exclusive acquisition (GC).

### 5.8 Restore validation (untrusted input)

A manifest comes from storage that someone else may control. Before any write:

* the manifest is ≤ 16 MiB and parses. Unknown fields are ignored, as in the marker, and
  every known field is validated;
* `kind` is `sparkles-backup` and `format` is 1;
* every `path` matches `^(CURRENT|dataset\.json|commits\.bin|prefixes\.json|text\.json|reasoning\.json|origin\.json|gen-\d{4,8}/[A-Za-z0-9._-]{1,64})$`,
  there are no duplicates, the one generation directory named equals `generation`, and
  `CURRENT`'s content (checked after download) equals `generation`;
* `size` equals the sum of the blob sizes, every blob size is ≤ `pieceBytes`, and every
  blob id is 64 lowercase hex digits;
* the total `size` is ≤ the free space check (§2.3);
* `dataset.id` and `commit` are well formed. After download, `generation`'s
  `commit.json` must name the same dataset id and a `baseSeq` ≤ `commit.seq`.

A violation fails with `422 invalid-backup` and names the field. Paths are joined only
after validation, and files are created with `create_new` inside the temporary
directory.

### 5.9 Changes to existing code

* `crates/sparkles-core/src/store.rs`: `backup_capture`, `BackupCapture`, `CapturedFile`,
  `collect_history`. `rebuild_locked` and `collect_locked` are unchanged except that
  `needed` sees leases.
* `crates/sparkles-core/src/history.rs`: `leases` and `Hold::Lease`. `protected()` includes
  leased seqs, and `HistoryStatus` lists them.
* `crates/sparkles-core/src/vocab.rs`: `DeltaVocab::flush() -> Result<u64>`.
* `crates/sparkles-core/src/commit.rs`: `Catalog::flushed_len`, `reidentify`, and the
  `restore` origin. `DatasetFile` gains an optional `restoredFrom`.
* `crates/sparkles-core/src/check.rs`: no change. Restore calls `check::check` and treats
  `Status::Warning` as success. It warns on a missing text index, which the open rebuilds.
* `crates/sparkles-server/src/state.rs`: tasks (above), a `restoring` state on `Dataset`
  (an `AtomicBool` that `dataset()` checks, answering 503), and startup recovery of
  `.restore-*`/`.replaced-*`.
* `crates/sparkles-server/src/http.rs`: the new routes (§2.5) and
  `DELETE /$/tasks/{id}`.
* `crates/sparkles-server/src/auth/routes.rs` (C09): `ROUTES` entries and needs.
* `crates/sparkles-server/src/obs.rs`: metrics, and `route_op` → `admin`.
* `crates/sparkles-server/src/main.rs`: the `Repo` and `Backup` subcommand groups, with
  the legacy `backup` form kept, and `serve --backup-config` and `--backup-max-tasks`.
* `ui/`: §6. `docs/API.md`: a "Backup repositories" section. `README.md`: a status row
  and the CLI list. A `PROVENANCE.md` entry.

## 6. UI (SvelteKit, `ui/`)

The top navigation gains a **Backups** entry (`routes/backups/+page.svelte`). It is
visible to `server-admin`, and to dataset admins in a reduced form through C09's
permission-aware actions. The page has four tabs:

* **Backups**: a table of backups across repositories, with filters for repository,
  dataset and policy, and search.
  * The columns are name, dataset, commit (`#42 · 3 h ago`), completed, logical size,
    added size, dedup (`logical / added`, e.g. `98×`), policy and status. The dataset
    column shows a lineage badge when the id differs from the live dataset's. The status
    is ok or error from the last verify.
  * The row actions are **Restore**, **Verify** and **Delete**, which needs a typed
    confirmation.
  * A details drawer shows the manifest summary, the files table (path, kind, size,
    blobs) and the parent backup.
* **Repositories**: cards with type, location (the bucket and prefix, or the path),
  read-only and single-writer badges, reachability, stored and logical bytes, dedup ratio
  and backup count.
  * **Add repository** is a form with type-specific fields and a credentials source
    selector. Phase 1 never has a secret field; the form explains the env and file
    sources.
  * **Test connection** shows the step list with latencies and the conditional-write
    result before saving.
  * The tab also has **Verify** and, in Phase 2, **Run GC**, which runs a dry run first
    and shows the bytes to free.
* **Policies** (Phase 2): a list with next run, last result, consecutive failures and an
  enabled toggle. The editor has:
  * the repository;
  * the datasets: a multi-select plus a glob input with a live match list;
  * a schedule builder: presets (hourly, daily at, weekly on, custom cron), a time zone
    select, and a **next-5-runs preview** with a human description from
    `POST /$/backup-policies/preview`, so the UI needs no cron library;
  * the name template with a live sample;
  * retention (expire after, min, max) with an explanatory sentence ("keeps at least 7;
    deletes backups older than 30 days, and any beyond 60");
  * catch-up and GC options.

  Buttons: **Run now** and **Preview retention**.
* **Activity**: running and recent backup tasks, policy run history and GC reports. The
  tasks use the existing `TaskList` component, filtered to `backup-*` kinds, with
  progress bars and a **Cancel** button. Each policy run expands to per-dataset results.

**Dataset page** (`routes/datasets/[name]`). A **Backups** panel lists this dataset's
backups (`GET /$/backups/{ds}`). It has **Back up now**, which asks for a repository,
then a name and note, and **Restore…**.

**Restore dialog** (`RestoreDialog.svelte`), in steps:

1. The backup (preselected), with its commit and size.
2. The target: a *new dataset* (default name `{ds}-restored-{date}`) or *replace
   `{ds}`*. Replacing shows the current head against the backup's commit ("commits
   42–57 will be lost") and requires typing the dataset name.
3. Options: identity (auto, with an explanation), the post-restore check (quick or
   full), and whether to keep the replaced copy.
4. Submit. The dialog then follows the task and links to the new dataset.

Components:

* new: `BackupsPanel.svelte`, `RestoreDialog.svelte`, `RepositoryDialog.svelte`,
  `PolicyEditor.svelte`, `SchedulePreview.svelte`;
* `lib/api.ts`: typed functions for §2.5;
* `lib/format.ts`: bytes and dedup ratio;
* tests: `backups.test.ts` for the retention sentence, name template sample and dedup
  formatting.

## 7. Phasing

**Phase 1 (MVP, ~5 days; buffer to 6).**

* Day 1, core:
  * `backup_capture` with leases, `DeltaVocab::flush`, `Catalog::flushed_len` and
    `reidentify`;
  * `sparkles-backup` skeleton: config and URL parsing, `fs`, `s3` and `memory`
    backends, the marker, blob format and hashing, pieces;
  * unit tests for capture under concurrent commits and compaction.
* Day 2:
  * create: planning with parent reuse, append segments, consolidation, upload with
    concurrency, throttle and cancel, and the manifest create;
  * list with the cache, show and delete;
  * shared locks.
* Day 3:
  * restore: validation, parallel verified download, the identity rule, check, open and
    compare, and offline `--to`/`--replace`/`--data`;
  * verify (`exists`, `data`, `restore`);
  * the CLI's `repo add|list|show|test|verify|remove` and
    `backup create|list|show|delete|restore|verify`.
* Day 4, server:
  * repositories registry and `--backup-config`;
  * `/$/repositories` (CRUD, test, verify, list backups);
  * `/$/backups/{ds}` (list, create, show, delete, restore, verify);
  * restore to a new name and in place (swap and startup recovery);
  * task cancel, `detail`, `queued`, `--backup-max-tasks`;
  * C09 route entries;
  * core metrics and spans.
* Day 5:
  * UI: the Backups page (Backups and Repositories tabs, add and test repository, create,
    restore dialog, verify, delete) and the dataset Backups panel;
  * `docs/API.md` and `README.md`;
  * acceptance tests A1–A22 on the memory and fs backends, with MinIO-gated S3 tests;

  * the benchmark script and a first measurement.

**Phase 2.**

* Policies: config and API, the scheduler thread with catch-up, time zones and DST rules,
  name templates, retention with dry run, run history, policy metrics and the alert
  example.
* GC: two-phase mark and sweep, exclusive locks, grace period, dry run, `locks`
  listing and break.
* Full UI: the Policies tab and editor with schedule preview, the Activity tab with run
  history and GC reports, **Run GC**.
* `gcs` and `azure` repositories (experimental). Full backups of in-memory datasets.
* "Back up all datasets": `POST /$/repositories/{repo}/backups {datasets}` runs one task
  that makes one backup per dataset, all sharing a `run` id.
* The CLI's `--server URL` for backup create, list and restore, through the
  [C09 §7.4](C09-dataset-access-control.md) client, and the offline
  `backup policy run`.

**Phase 3.**

* Client-side encryption. The recommended design is a random 256-bit repository master
  key, wrapped by `age` X25519 recipients or a passphrase (argon2id) and stored as
  `keys/<id>.json`:
  * blobs are sealed with AES-256-GCM (`aws-lc-rs`, already in the tree) under per-blob
    nonces;
  * blob ids become HMAC-SHA256(id key, plaintext), so identical content in two
    repositories is not linkable;
  * manifests are encrypted as well;
  * `encryption` in the marker names the scheme.

  A recipient-only (public-key) mode, in which a server can write backups it cannot
  read, is attractive. But verify at the `data` level and dedup against existing blobs
  would then need the id key (open question 11).
* CDC for `vocab.dat` if measurements justify it (§5.4).
* Repository-to-repository copy (`sparkles repo copy SRC DST [--backups …]`), dedup-aware,
  for cross-region or offsite copies. S3 Object Lock, a WORM mode in which GC deletes
  only after retention, and storage-class selection.
* Optional inclusion of F06 retained generations and pins. Server-level backups of the
  registry, reasoning and auth config, minus secrets.
* Continuous WAL segment shipping between backups, for point-in-time restore to any
  commit. It would reuse the append-segment machinery, but needs its own spec.

## 8. Rejected alternatives

* **N-Quads dumps as the backup format** (today's `/$/backup`). Each dump is a full
  copy, a restore is a full bulk load (minutes at 10⁷ quads), and blank-node ids, commit
  history and the dataset id are lost. Dumps are kept only for Fuseki compatibility.
* **Uploading the database directory as a tar per backup.** It gives no dedup, no
  incremental WAL and no per-blob verification.
* **Mirroring Elasticsearch's repository layout, blob naming or REST paths.** That is
  license-encumbered and a poor fit. A Sparkles backup has one dataset, one commit and
  few large immutable files.
* **Calling repository copies "snapshots" or using `/$/snapshots/…`.** That collides
  with F06 named snapshots (`/$/snapshots/{ds}/{name}`, `sparkles snapshot …`).
  "Backup" extends the existing `/$/backup` vocabulary.
* **`/$/repositories/{repo}/backups/{name}` for all per-dataset operations.** C09's
  middleware could not authorize them by dataset level, so every handler would need
  bespoke checks. Only the server-admin listing uses that path.
* **CDC and pack files in Phase 1** (§5.4). They gain little between compactions, where
  whole-file dedup is exact, and nothing across compactions, for much more format.
* **Multipart uploads.** Dangling incomplete uploads need lifecycle rules, and fixed
  32 MiB pieces give parallelism and bounded retries without them.
* **BLAKE3.** It is faster, but a new dependency. SHA-256 is already in the workspace
  and fast enough: hashing takes ≈ 0.2 s per 300 MB, against ≥ 1 s to upload it. It also
  matches S3's checksum algorithms.
* **A mutable repository index object** (a list of backups). Concurrent writers would
  contend on `If-Match`, and a lost update hides a backup. LIST plus immutable manifests
  plus a local cache is simpler and always correct.
* **Client-clock lock staleness.** Hosts with skewed clocks would steal live locks. The
  storage server's `last_modified` gives one clock.
* **Keeping the dataset id on every restore.** In place, or next to the live source, it
  would reuse `(datasetId, seq)` for different data and break CI's guarantee.
* **Always minting a new id.** It would break DR, where clients and search watermarks
  expect the same lineage.
* **Backing up the Tantivy index.** The index is derived, churns more, and is rebuilt at
  open. `text.json` suffices.
* **Blocking writes for the whole backup.** Immutable generations and the append-only WAL
  make a millisecond capture sufficient.
* **Filesystem snapshots (LVM, ZFS, btrfs) or rsync.** They are platform-specific, not
  incremental on object storage, and outside the store's crash model.
* **Inline secrets in the API or UI (Phase 1).** Secrets would then live in
  `repositories.json`. References to the environment or a file keep them out of
  Sparkles' files and logs.


## 9. Acceptance examples

Unless stated otherwise, the setup is `sparkles serve --data /tmp/s` with a fresh
persistent dataset `ds` and three commits: 1 `INSERT DATA { <urn:a> <urn:p> 1 }`,
2 `INSERT DATA { <urn:b> <urn:p> 2 }` and 3 `DELETE DATA { <urn:a> <urn:p> 1 }`. The
repository `local` has type `fs` and lives at `/tmp/r`, a tempdir. Library-level tests
use `memory://`. The S3 tests run against MinIO when `SPARKLES_TEST_S3_ENDPOINT` is set
(§10), and are skipped otherwise.

**A1. Register and test.**
* `POST /$/repositories {"name":"local","type":"fs","path":"/tmp/r"}` → `201`,
  `status.reachable: true`, `conditionalWrites: true`, and `/tmp/r/sparkles-repo.json`
  exists with `format: 1`.
* Registering `/tmp/r` again as `local2` → `409 repository-exists`.
* A non-empty directory without a marker → `409 not-a-repository`.
* A relative path, or one inside `/tmp/s` → `400 invalid-config`.

**A2. First backup.**
* `POST /$/backups/ds {"repository":"local","name":"b1"}` → `202`, `Location:
  /$/backups/ds/local/b1`. The task ends `done`, and `detail.commit.seq == 3`.
* `GET /$/backups/ds/local/b1`: `dataset.id` equals `Sparkles-Dataset-Id`, and the files
  include `CURRENT`, `commits.bin`, `gen-0001/wal.log`, and all 7 `.dat` and 7 `.meta`.
  `addedBytes` ≈ `logicalBytes` (a fresh repository), and there is no `text/` or
  `sparkles.lock`.

**A3. Incremental backup.**
* Commit 4, `INSERT DATA { <urn:c> <urn:p> 3 }`, then backup `b2`: `commit.seq == 4`.
* Every immutable file of `b2` has the same blob ids as in `b1`. `wal.log`'s blobs are
  `b1`'s segments plus one new segment, and `newBlobs` ≤ 5 (WAL, catalog and
  delta-vocabulary segments, plus changed meta files).
* `addedBytes` < 10 KB.

**A4. Restore to a new name (clone from backup).**
* `POST /$/backups/ds/local/b1/restore {"target":"ds-r"}` → `202`. After the task:
  * `GET /$/datasets/ds-r` has `restoredFrom.backup == "b1"` and
    `forkedFrom == {id: <ds id>, seq: 3}` (identity `auto` → new, since `ds` is live);
  * `SELECT ?s { ?s ?p ?o }` on `ds-r` → `[urn:b]`, with `Sparkles-Commit: 3` and a
    `Sparkles-Dataset-Id` different from `ds`'s;
  * `GET /$/commits/ds-r` lists seqs 3..0.
* The next update on `ds-r` gets seq 4.

**A5. Consistency under concurrent writes (unit, `memory://`).**
* One thread commits single-quad updates in a loop, while backups `c1…c5` are taken.
* Each restore: head == `manifest.commit.seq`, `quads == manifest.commit.quads`,
  `check` → ok, and the N-Quads dump equals `dump_nquads_at(commit:seq)` of the source
  (F06).
* The p99 writer-lock hold of the captures is < 5 ms.

**A6. Compaction during the upload (unit).**
* A failpoint pauses the upload after capture. Commit, then `compact()` (`gen-0001` →
  `gen-0002`). `history()` shows `gen-0001` held by `backup:<name>`.
* Resume: the backup completes with `generation: gen-0001`, and its restore equals the
  captured state.
* After the lease drops, the next GC point removes `gen-0001`, and `history.bytes == 0`.

**A7. Name uniqueness across writers (unit, `memory://` and MinIO).**
* Two `Repository` handles on the same store create `n1` concurrently for different
  datasets: exactly one succeeds, and the other gets `backup-exists`.
* The winner's manifest is intact, and a repository verify lists the loser's
  unreferenced blobs as orphans.

**A8. Cancel.**
* With `maxUploadBytesPerSec: 1048576`, start `b3`, then `DELETE /$/tasks/{id}` → `202`,
  and the task ends `cancelled`. No `backups/b3.json` exists, and `GET /$/backups/ds`
  does not list it.
* Re-run `b3`: `reusedBlobs > 0` (the blobs uploaded before the cancel), and it succeeds.

**A9. Backend failures (unit).**
* A wrapper store that fails every `PUT` of `blobs/*` after the second → the task
  `failed`, no manifest, and `sparkles_backup_operations_total{operation="create",
  result="failed"}` is 1.
* A wrapper that fails twice with a retryable 503, then succeeds → the backup succeeds,
  and `object_requests_total{result="error"}` is 2.

**A10. Verify levels.**
* Delete one immutable blob file under `/tmp/r/blobs/…`. `POST …/b1/verify
  {"level":"exists"}` → `detail.status: "error"`, with `missing: [<id>]`. `b2`, which
  shares it, also fails in a repository verify.
* Restore the blob, then flip one payload byte: `exists` → ok, `data` → `corrupt:
  [<id>]`.
* On an intact repository, `restore` → ok, with a `check` report of status `ok`.

**A11. A corrupt blob fails the restore.**
* With the corrupted blob of A10, a restore of `b1` to `x` → the task `failed` with
  `checksum mismatch in blob <id> (gen-0001/spo.dat)`.
* No `databases/x` or `databases/.restore-x-*` remains, and `x` is not registered.

**A12. In-place restore.**
* `ds` at head 6. `POST …/b1/restore {"replace":true}` → after the task:
  * `ds` has head 3 and a new dataset id with `forkedFrom.seq == 3`;
  * no `databases/.replaced-ds-*` remains;
  * the next update is seq 4 under the new id.
* During the swap, a looping client sees only `200` (old or new state) or `503` with
  `Retry-After`, never `500` or `404`.
* A restore in place with `"identity":"keep"` while the head is 6 > 3 → `409
  duplicate-dataset-id`.

**A13. Crash during the swap (unit).**
* Failpoint after the first rename. On restart, `databases/.replaced-ds-*` is renamed
  back to `ds`, and `ds` serves its pre-restore head.
* Failpoint after both renames: `ds` is the restored dataset, and the leftover
  `.replaced-*` is removed (the name exists).

**A14. DR with a read-only repository on another server.**
* Server B: `--data /tmp/s2`, repository `local` at `/tmp/r` with `"readonly":true`.
* `GET /$/repositories/local/backups` lists `b1` and `b2` for `ds`.
* Restore `b2` as `ds` → the dataset id equals the source's (no dataset on B has it),
  head 4.
* `POST /$/backups/ds {"repository":"local"}` on B → `409 repository-read-only`. No
  object is created under `/tmp/r/locks/` by B at any point.

**A15. Hostile manifests (unit).**
* Manifests with the path `../x`, `gen-0001/../../x`, or `/etc/passwd`, a duplicate
  path, sizes that do not sum, a blob id `ABC`, a 20 MiB manifest, or `CURRENT` content
  not equal to `generation` → `422 invalid-backup` naming the field.
* Nothing is created outside the temporary directory, and the temporary directory is
  removed.

**A16. Incompatible formats.**
* A manifest with `indexFormat` = `FORMAT_VERSION + 1` → `422 incompatible-format`.
* A marker with `format: 2` → registration fails with `422 incompatible-repository`.
* A marker with `encryption` set → `422`.

**A17. Unsupported and conflicting requests.**
* A backup of a `--mem` dataset → `501 backup-unsupported`.
* A second concurrent backup of `ds` to `local` → `409 backup-in-progress` with
  `task: <id>`.
* Deleting `b1` while its restore runs → `409 backup-busy`.
* A restore to an existing name without `replace` → `409 dataset-exists`.

**A18. Permissions (C09 enabled).**
* Users: carol has `admin` on `wiki*`, dave has `read` on `wiki`, and alice is
  `server-admin`.
* carol:
  * `POST /$/backups/wiki` → `202`;
  * `GET /$/repositories` → names and types only;
  * `POST /$/repositories` → `403`;
  * a restore of a `wiki` backup with `target: "secret"` → `403 no admin access to the
    target name /secret`;
  * `GET /$/backups/wiki/local/<backup of secret>` → `404`.
* dave: `GET /$/backups/wiki` → `200`; `POST` → `403`; `DELETE /$/tasks/<carol's task>`
  → `403`.
* alice: every route is allowed.
* The router test asserts that every new template is in `ROUTES`.

**A19. Delete.**
* `DELETE /$/backups/ds/local/b1` → `204`. It is no longer listed, and
  `/tmp/r/backups/b1.json` is gone.
* All blobs remain, and `b2` still restores and verifies (`data`).
* Re-creating the name `b1` works, and the cache refetches it (a new e_tag or mtime).

**A20. Offline DR through the CLI.**
* With the server stopped:
  * `sparkles backup list --repo file:///tmp/r` → `b2 ds 4 …`, `b1 ds 3 …`;
  * `sparkles backup restore --repo file:///tmp/r b2 --to /tmp/dr/ds` → exit 0;
  * `sparkles query --loc /tmp/dr/ds 'SELECT (COUNT(*) AS ?n) {?s ?p ?o}'` → 2;
  * `sparkles check --loc /tmp/dr/ds` → exit 0.
* `--replace` onto a directory held by a running server → the "in use by another
  process" error, exit 1.
* `sparkles backup restore --repo file:///tmp/r b2 --data /tmp/s3 --as ds`, then
  `sparkles serve --data /tmp/s3` → `ds` listed with head 4.

**A21. S3 semantics.**
* A2–A4, A7, A10 and A19 are repeated on `memory://` (always) and on
  `s3://sparkles-test/<uuid>?endpoint=$SPARKLES_TEST_S3_ENDPOINT&allow_http=true&path_style=true`
  (MinIO, when set).
* The connection test reports `conditionalWrites: true` on both.
* A wrapper store that ignores `PutMode::Create` → the test reports `false`, and the
  repository is shown `singleWriter: true`.

**A22. Locks (Phase 1 part).**
* During a create, one shared lock object exists; it is gone afterwards.
* Plant `locks/x.json` of kind `exclusive`:
  * with `last_modified` now, a create waits, then fails with `409 repository-locked`
    (`lockWait` set to 2 s in the test);
  * with `last_modified` 31 min ago (mtime set on `fs`), the create ignores it and
    succeeds.

**A23. Policy schedule (Phase 2, unit with a fake clock).**
* Policy `p`: `*/10 * * * *`, UTC, template `{policy}-{dataset}-{time}`. With the clock
  at 12:05 → `nextRun` 12:10.
* Advancing to 12:10 runs a `backup-policy` task that creates `p-ds-20260930t121000z`.
  `runs[0].trigger == "schedule"`, `result == "ok"`.
* `POST /$/backup-policies/p/run` at 12:12 → `trigger: "manual"`, and `nextRun` is still
  12:20.

**A24. Missed runs (Phase 2).**
* An hourly policy, last run 09:00. Stop the server at 09:30 and restart at 12:40: after
  60 s, exactly one run with `trigger: "catch-up"`, `scheduledFor: 12:00`, then the next
  at 13:00.
* With `catchUp: "none"`: no run until 13:00, and the run history records `skipped` for
  12:00.

**A25. Time zone and DST (Phase 2, unit).**
* `30 2 * * *` in `Europe/Berlin`, previewed from 2026-10-24:
  * one run on 2026-10-25 at 02:30 CEST (00:30Z), the ambiguous time, run once;
  * 2027-03-28 (02:30 does not exist) → 03:00 CEST (01:00Z).
* `every 6h` → 00:00Z, 06:00Z, …, whatever the start time.

**A26. Retention (Phase 2).**
* Policy `p` with `minCount 2, maxCount 3, expireAfter "2d"`. Backups of `ds` by `p`,
  completed 0.5, 1, 3, 4 and 5 days ago, plus a manual backup 10 days ago.
* Retention deletes the 4- and 5-day ones (index ≥ 3), and also the 3-day one (expired,
  beyond `minCount`).
* It keeps the 0.5- and 1-day ones, and never touches the manual one.
* `?dryRun=true` returns the same set and deletes nothing.

**A27. GC (Phase 2).**
* After A19's delete of `b1` and a compaction, backup `b4` (`gen-0002`). Delete `b2`.
  `POST /$/repositories/local/gc {"dryRun":true,"graceHours":0}` → the candidates are
  exactly the `gen-0001` blobs no longer referenced.
* The real run deletes them. `b4` verifies at `data` level, and `storedBytesAfter`
  decreases by `deletedBytes`.
* With the default grace (24 h), a fresh orphan is kept (`keptYoung ≥ 1`).

**A28. GC against a concurrent backup (Phase 2, unit).**
* A failpoint between mark and sweep. Meanwhile, a new backup whose manifest references
  candidate blob X is created (it re-uploaded X through `HEAD` or `PUT` "exists").
* The sweep re-lists, sees the new manifest, and does not delete X. The new backup
  verifies.

**A29. Metrics.** After A2–A10: `/$/metrics` has
`sparkles_backup_operations_total{repository="local",operation="create",result="ok"} 2`,
`sparkles_backup_last_success_timestamp_seconds{dataset="ds",repository="local"}`,
`sparkles_backup_capture_lock_seconds_count`, and `…_bytes_uploaded_total` > 0. With
OTel's test exporter: a `task backup-create` span with children `backup.capture`,
`backup.plan`, `backup.upload`, `backup.manifest`.

**A30. Performance (bench, not CI; §10).**

## 10. Performance targets and measurement

The workload is 10.5 M quads from `scripts/gen-data.py 1000000`, bulk-loaded, with an
index of ≈ 286 MB (`docs/BENCHMARKS.md`). Measurements use a release build, a warm page
cache and NVMe, on an otherwise quiet machine with one engine running at a time, as the
benchmarking notes require.

| Case | Target | Notes |
|---|---|---|
| Capture (writer-lock hold) | p99 < 5 ms | `sparkles_backup_capture_lock_seconds` |
| First full backup → `fs` on the same NVMe | < 5 s | Hashing takes ≈ 0.2 s. Copying and fsync dominate. |
| First full backup → MinIO on localhost | < 10 s | 8 parallel 32 MiB PUTs |
| First full backup → AWS S3, same region | < 20 s | Informational. Not in the bench script. |
| Incremental after 1 k single-quad updates (no compaction) | < 1 s (`fs`), < 2 s (MinIO); < 200 KB added | ≈ 66 KB WAL + 64 KB catalog + delta vocabulary |
| Incremental after a compaction | ≈ first full | Inherent (§5.4). Reported so that users can schedule around it. |
| Restore → openable dataset, `check: quick` | < 10 s (`fs`), < 15 s (MinIO) | Excludes the full-text rebuild, which is reported separately. |
| Verify `exists` | < 1 s for 100 backups | one LIST plus the cache |
| Update throughput during a backup | ≥ 90 % of baseline | the update-heavy workload of `docs/BENCHMARKS.md` |

**How to measure.** `scripts/backup-bench.sh`, a Phase 1 deliverable, runs these steps:

1. Generate and load: `python3 scripts/gen-data.py 1000000 > /tmp/d.nt`, then
   `sparkles load --loc /tmp/db /tmp/d.nt`.
2. `fs`: `time sparkles backup create --loc /tmp/db --repo file:///tmp/r --name full`.
   Then apply 1000 single-quad updates with `sparkles update` in a loop, or through a
   server with `curl`, and time the `incr` backup. Then `time sparkles backup restore --repo
   file:///tmp/r incr --to /tmp/rest`, and `sparkles check --loc /tmp/rest`.
3. MinIO runs only as an external process. It is AGPL-3.0 and is never linked or
   shipped. Start it with `nix shell nixpkgs#minio nixpkgs#minio-client`, then
   `MINIO_ROOT_USER=test MINIO_ROOT_PASSWORD=testtest12 minio server /tmp/minio
   --address 127.0.0.1:9000 --console-address 127.0.0.1:9001 &`,
   `mc alias set t http://127.0.0.1:9000 test testtest12`, `mc mb t/sparkles`. Then
   repeat step 2 with
   `--repo 's3://sparkles/bench?endpoint=http://127.0.0.1:9000&allow_http=true&path_style=true'`
   and `AWS_ACCESS_KEY_ID=test AWS_SECRET_ACCESS_KEY=testtest12 AWS_REGION=us-east-1`.
   Stop it by port (`fuser -k 9000/tcp`).
4. The script prints one line per case, with seconds, bytes added, and requests by type
   from `--format json`. The results go to `docs/BENCHMARKS.md` ("Backup and restore").
   A missed target is recorded and does not block the release.

In CI, the unit and acceptance tests use `memory://` and tempdir `fs` repositories. The
S3 job is opt-in (`SPARKLES_TEST_S3_ENDPOINT`), and a Nix check can start MinIO as a
service. Open question 7 covers an in-process emulator instead.

## 11. Security

* Sparkles never stores **credentials** in Phase 1. They come from three sources:
  * the default provider chain (AWS environment variables, web identity, instance
    metadata, as `object_store`'s builder supports);
  * named environment variables;
  * a JSON file re-read at each repository open, so rotation works. A WARN is logged if
    it is group- or world-readable.
* The API accepts only these references, and `GET` never returns file contents or
  environment values.
* Credentials are never logged. Backend errors are rendered without headers and without
  the query strings of request URLs, which can hold presigned-style parameters. Config
  errors name the variable or file, not its value.
* **URLs.** `--repo` and `endpoint` reject userinfo and credential query parameters.
  `http://` endpoints, such as MinIO on localhost, need `allowHttp: true`, and the UI
  shows a warning badge for them.
* **Local paths.** `fs` repository paths must be absolute. They must not lie inside a
  database directory, or inside `<data>`, where the repository would back up its own
  backups or be deleted with a dataset. Registering one needs `server-admin`, because
  [C09 §3.5](C09-dataset-access-control.md) treats local file access as server-admin.
* **Untrusted repositories.** Every manifest is validated (§5.8), every blob is verified
  before use, and writes are confined to a fresh temporary directory. Restore never
  executes content. Reasoning rules in `reasoning.json` are data, applied only by an
  explicit re-run.
* **Server-side encryption** on S3:
  * `sse: "AES256"` (SSE-S3, the AWS default for new objects);
  * `sse: "aws:kms"` with `kmsKeyId` (SSE-KMS), through the encryption options of
    `object_store`'s S3 builder. The option names need checking against the pinned
    version at implementation.
* **Client-side encryption** comes in Phase 3 (§7). Until then, the README says that a
  repository holds the dataset in plain form, and that SSE or an encrypted filesystem is
  the protection at rest.
* **Integrity.** SHA-256 covers each blob and each file. `verify` at the `data` level
  detects bit rot. At the `restore` level it also detects logical damage, through
  `sparkles::check`.
* **Denial of service.** The task slots (`--backup-max-tasks`), per-repository
  concurrency and bandwidth limits, and the C09 rate-limit `admin` class all apply to
  the new routes.

## 12. Open questions

1. **Identity default.** `auto` keeps the id only when no local dataset has it. Should a
   restore to a *new name* always mint a new id (clone semantics) even on a fresh server,
   leaving `keep` for explicit DR? Default: `auto` as specified.
2. **Piece size.** 32 MiB is chosen for bounded memory and retry cost. 64 MiB halves the
   requests for large indexes. Measure on S3 in Phase 1.
3. **CDC for `vocab.dat`** after compaction (Phase 3). This needs a measurement of
   cross-compaction similarity.
4. **Manifest cache vs a repository catalog object** at very large backup counts
   (> 10 k). An append-only `catalog/<date>.jsonl` per writer could speed cold listings.
   Default: LIST plus the cache.
5. **Fuseki `/$/backup/{ds}`**: should it gain `?repository=R` as an alias for a
   repository backup? Default: no. The two stay distinct.
6. **Backups on `--read-only` servers** are allowed, as `/$/backup` is today. Should
   deleting backups and GC be refused there as well? Default: allowed on writable
   repositories.
7. **An in-process S3 emulator for CI**, such as `s3s-fs` (Apache-2.0), instead of an
   external MinIO process (AGPL-3.0)? Its conditional-write support needs checking.
   Default: `memory://` in CI, MinIO opt-in.
8. **Dependency weight**: `object_store` with `aws` pulls its own HTTP stack. Check for a
   second `reqwest`/`hyper` version against the workspace's `reqwest` 0.13 and
   `aws-lc-rs`, and pin a compatible `object_store` 0.14.x.
9. **A data-directory lock** so that offline `backup restore --data` can detect a running
   server. Today only the per-database `sparkles.lock` exists.
10. **Inline secrets** through the API and UI (Phase 2), stored in
    `<data>/backup/secrets.json` (0600, like C09's `auth/`). This is convenient, but
    secrets would then live on the server's disk. Default: no.
11. **Encryption scheme** (Phase 3): a symmetric key file, or `age` recipients for
    write-only backups? With recipients only, dedup against existing blobs and
    `data`-level verify need the HMAC id key on the writer.
12. **Server backups**: include the registry (`config.json`) and C09 policy (minus
    secrets) in a "server backup" group? Default: Phase 3.
13. **Backup-triggered GC of F06 history**: the lease release calls a best-effort GC. Is
    `try_lock` enough, or should the server run the hourly history tick F06 Phase 2
    plans? Default: `try_lock` plus the next GC point.
14. **Retention clock**: `expireAfter` uses `completed` (the backup time). Should it
    rather use the commit timestamp (data age)? Default: `completed`.

## 13. Sources

* **Sparkles repository, read for this spec:**
  * `crates/sparkles-core/src/store.rs`:
    * `Generation` (`open`, `open_sealed`, files `vocab.dat`, `vocab.off`, `meta.json`,
      `stats.json`, `delta.vocab`);
    * `Store` fields and `open`;
    * the WAL format constants;
    * `WriteTxn::commit`/`publish_log` (flush plus `sync_data` per commit,
      `dvocab.sync()`);
    * `rebuild_locked` (publication order, history hook), `collect_locked`, `compact`,
      `clone_to`, `backup`, `dump_nquads`, `wal_bytes`, `write_atomic`, `dir_size`,
      `lock_dir`;
  * `crates/sparkles-core/src/history.rs`: `At`, `valid_name`, `Hold`, `HistoryState`
    (`protected`, `needed`), `GenEntry`, the `history.json` format;
  * `crates/sparkles-core/src/check.rs`: the module documentation, `CheckOptions`, `Status`,
    `check`;
  * `crates/sparkles-core/src/index.rs`: `Perm::name`, and the `.dat`/`.meta` file names;
  * `crates/sparkles-core/src/vocab.rs`: the `DeltaVocab` format (length-prefixed records),
    `open_read_only`, the buffered writer;
  * `crates/sparkles-core/src/commit.rs`: `Catalog` (`append`, `pending`), the dataset-file
    origins;
  * `crates/sparkles-core/src/store.rs` `open_text`: the full-text index is opened or rebuilt
    at open;
  * `crates/sparkles-server/src/http.rs`: the router (including `/$/snapshots`,
    `/$/history`, `/$/backup`), `backup`, `clone_dataset`;
  * `crates/sparkles-server/src/state.rs`: `Task`, `AppState`, `start_task_as`,
    `TaskHandle`, `reserve`/`adopt`, the registry `config.json`, `write_file_atomic`;
  * `crates/sparkles-server/src/clone.rs` (header), `otel.rs` (`task_span`), `obs.rs`
    (metric names);
  * `crates/sparkles-server/src/main.rs`: the `Cmd` enum and `serve` flags;
  * in the access-control work, then in progress: `crates/sparkles-server/src/auth/routes.rs`
    (`Need`, `ROUTES`) and `crates/sparkles-server/Cargo.toml` (`toml`, `aws-lc-rs`,
    features);
  * `ui/src` route and component listing;
  * `docs/API.md`: the Server, OpenTelemetry and Datasets (admin) sections, and the
    `Task` type;
  * `docs/BENCHMARKS.md`: the index size at 10.5 M (286 MB);
  * `README.md`: backup mentions;
  * `Cargo.toml`: the workspace dependencies (`sha2`, `lz4_flex`, `uuid`, `tokio`,
    `reqwest`, `chrono`);
  * the specs [F06](F06-snapshots-and-point-in-time.md) and
    [CI](CI-commit-identity.md) in full, [C09](C09-dataset-access-control.md) §§3–4, 7,
    9, and [PROVENANCE.md](PROVENANCE.md). No other planning document was read.
* **Fetched, public user documentation used as evidence of desired behavior only**
  (Elasticsearch and Kibana are source-available under non-permissive licenses; no source
  code was read):
  * "Snapshot and restore", https://www.elastic.co/docs/deploy-manage/tools/snapshot-and-restore;
  * "Create, monitor and delete snapshots", https://www.elastic.co/docs/deploy-manage/tools/snapshot-and-restore/create-snapshots;
  * "Restore a snapshot", https://www.elastic.co/docs/deploy-manage/tools/snapshot-and-restore/restore-snapshot;
  * "Manage snapshot repositories", https://www.elastic.co/docs/deploy-manage/tools/snapshot-and-restore/manage-snapshot-repositories
    (it lists repository types only);
  * Kibana Guide, "Snapshot and restore", https://www.elastic.co/guide/en/kibana/current/snapshot-repositories.html;
  * a web search restricted to elastic.co, whose result titles only were used to find
    the Kibana page.
* **Fetched, permissive library and service documentation:**
  * `object_store` crate documentation, docs.rs, version 0.14.2 (MIT/Apache-2.0):
    * the crate root (backends, `PutMode`, multipart, `LimitStore`, `RetryConfig`),
      https://docs.rs/object_store/latest/object_store/;
    * `aws::S3ConditionalPut`, https://docs.rs/object_store/latest/object_store/aws/enum.S3ConditionalPut.html;
    * `local::LocalFileSystem` (atomic writes, `with_fsync`),
      https://docs.rs/object_store/latest/object_store/local/struct.LocalFileSystem.html;
  * AWS S3 User Guide:
    * "How to prevent object overwrites with conditional writes",
      https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html;
    * "What is Amazon S3?" (the data consistency model),
      https://docs.aws.amazon.com/AmazonS3/latest/userguide/Welcome.html;
  * restic documentation, "References" (repository design; restic is BSD-2-Clause;
    design ideas only), https://restic.readthedocs.io/en/stable/100_references.html;
  * `croner` crate documentation, docs.rs, version 4.0.0 (MIT),
    https://docs.rs/croner/latest/croner/.
* **Cited from general knowledge, not fetched** (to be checked at implementation):
  * `chrono-tz` (MIT/Apache-2.0);
  * `lz4_flex` frame format (MIT, already a dependency);
  * `age` (MIT/Apache-2.0);
  * BLAKE3 (CC0/Apache-2.0);
  * S3 `DeleteObjects` (1000 keys), SSE-S3/SSE-KMS, additional checksums (SHA-256), the
    5 GB single-PUT limit;
  * the borg chunker (buzhash, BSD-3-Clause; CDC background only);
  * `s3s-fs` (Apache-2.0);
  * the MinIO license (AGPL-3.0; external test process only);
  * RFC 9110, RFC 3339, RFC 9562, the IANA time zone database.
* No Elasticsearch or Kibana source code was opened, searched or fetched. Only the
  public user documentation pages listed above were read, and only for behavior.

## Outcome

**Delivered.** Backup repositories landed on 2026-09-30:
* the `sparkles-backup` crate, behind the `backup` cargo feature of `sparkles-server`
  (on by default), using `object_store` 0.14.2, `croner` 4 and `chrono-tz`;
* the server's `/$/repositories`, `/$/backups/{ds}` and `/$/backup-policies` routes;
* `sparkles repo` and `sparkles backup create|list|show|restore|verify|delete|policy`;
* a Backups page with Backups, Repositories, Policies and Activity tabs, plus a panel on
  the dataset page.

The maintainer decided that the whole UI of §6 belongs in the first delivery, which
pulled lifecycle policies, garbage collection and lock management forward from Phase 2.
`gcs` and `azure` repositories exist as build features, untested in CI.

**Deviations from the spec.**
* **Leases hold a generation, not a commit** (decided by the maintainer). A lease keyed
  by commit number does not keep the old generation after a compaction, because the new
  generation's base covers that commit. The lease now names the generation, and
  `GET /$/history/{ds}` shows it as a `backup:<name>` hold.
* **The validation files** (`validation.json`, `validation-shapes.ttl`) are backed up
  with the other metadata files, although §2.2 had left them out. The maintainer decided
  this.
* The blob header gives the codec (raw or LZ4) and the encryption (none) a byte each,
  and the format is still 1. During an in-place restore, every route that names the
  dataset answers `503` + `Retry-After`. Verification, GC and policy runs are
  server-wide tasks, and tasks gained `queued`, `cancelled` and `DELETE /$/tasks/{id}`.
  The data-directory lock of open question 9 shipped in this delivery.
* Backups are matched to `/{ds}` by dataset id, not by name. Repositories registered
  through the API are held to operator limits in the config file: named credential
  sources, the outbound policy for S3 endpoints, and `[api] fs_roots`, which the
  maintainer decided stays optional. `--read-only` servers refuse restores and policy
  runs.

**Open questions** kept the spec's defaults. S3 tests are opt-in through
`SPARKLES_TEST_S3_ENDPOINT`, and history collection runs when a lease is released and on
an hourly scheduler tick.

**Tests.** Library and server tests use `memory://` and temporary `fs` repositories. The
UI has Playwright tests against the mock and the real server. The W3C and SHACL suites
were unchanged. A follow-up added NixOS module options with a VM round-trip test.

**Performance.** [Benchmarks: Backup repositories](../BENCHMARKS.md#backup-repositories-105m-triples)
measured 10.5M quads with an `fs` repository on the same disk. A full backup took 0.56 s
(534 MB/s, against a target of < 5 s). An incremental backup after 1,000 single-quad
commits took 0.34 s and added 55.3 KB (targets < 1 s and < 200 KB). A restore with a
quick check took 0.79 s (target < 10 s), and a `data`-level verify 0.08 s. S3, cold
caches and databases larger than memory were not measured.

**Backups of in-memory datasets** were added later. An in-memory dataset is backed up
through a temporary database that `Store::memory_backup_capture` builds in
`<data>/tmp/memory-backup-*`. Under the writer lock it takes the head snapshot, then
writes it as `gen-0001` with the bulk builder, as a compaction does. The upload goes
through the normal path, and the lease of the capture removes the directory. A server
removes leftovers at start. These choices differ from the sketch in §1:
* **The dataset keeps its identity.** `clone_to` gives a copy a new id with `forkedFrom`,
  so the capture does not use it. The temporary database has the dataset's id and its
  head commit, which `gen-0001/commit.json` records with origin `memory`. Its
  `commits.bin` holds only that commit, because the commit records an in-memory dataset
  keeps are not tied to a generation on disk. The restored dataset's history therefore
  starts at the backup's commit.
* **The manifest's `dataset.type` is `mem`**, and backup summaries gained
  `dataset.type`.
* **Metadata.** The capture renders `prefixes.json`, `text.json` and `geo.json` from the
  running store. The server adds `validation.json` with the shapes or schema copy that a
  persistent dataset would keep. The ShEx guard of an in-memory dataset now keeps its
  schema text for this. `reasoning.json` follows the same rule as for a persistent
  dataset.
* **Restore** works as for any backup and creates or replaces a persistent dataset. An
  in-memory dataset cannot be replaced (`409 not-managed`).
* **Policies back up matching in-memory datasets** instead of skipping them. Each server
  start gives an in-memory dataset a new id, so retention counts the backups of each
  server run separately. The API docs advise `expireAfter` with `minCount: 0` for such
  policies.
* **Cost.** The writer lock is held only for the snapshot. The snapshot keeps the
  captured state in memory until the build ends. The temporary copy needs about as much
  disk as a compacted generation. Its build checks `--min-free-disk-mb` as it writes and
  fails with `507 insufficient-storage`. Every backup rebuilds the generation. An
  unchanged dataset gives the same index files, so the repository deduplicates them, but
  after a write most pieces differ.
* `501 backup-unsupported` is no longer returned. `POST /$/backup/{ds}` already wrote
  N-Quads dumps of in-memory datasets and is unchanged.

Tests cover the capture (content, head, id, prefixes, the text index setting, cleanup,
cancellation), a round trip through an `fs` repository with a `restore`-level verify, the
server's create, verify and restore with SHACL validation, the validation files of both
languages, and policy runs.

**Not built.** The items listed under Phases at the top were not built.

**Follow-up: Phase 3 encryption and chunking are specified in F11.** Client-side
encryption and content-defined chunking now have their own design in
[F11](F11-encryption-at-rest.md), which replaces the sketch in §7 Phase 3. Its Phase 1
is the encrypted repository of [F11 §5](F11-encryption-at-rest.md#5-encrypted-backup-repositories-phase-1).
Blob ids become HMAC-SHA256 under a repository key, blobs and manifests are sealed with
AES-256-GCM, and key slots wrap a repository master key. That answers open question 11.
Its Phase 2 adds FastCDC for `vocab.dat` behind the 30% gate of §5.4, which answers open
question 3, and write-only repositories with age recipients. A backup of a dataset that
is encrypted at rest carries the logical plaintext of each file, so the byte-identical
restore of §1 becomes a logically identical one for such datasets.

**Follow-up: Fuseki's `/$/backups` alias.** Fuseki routes `POST /$/backups/{ds}` to the
same action as `POST /$/backup/{ds}`, an N-Quads dump, and its clients send no body. Such a
request reached the repository API and failed with `404 no-such-repository`. Now
`POST /$/backups/{ds}` is a repository backup only when its body is JSON, by its
`application/json` content type or because it is a JSON object, and is Fuseki's dump
otherwise. Every repository backup names its `repository` in a JSON body, and the UI sends
`application/json`, so no existing request changed meaning. The other `/$/backups/{ds}…`
routes are unchanged. Moving the repository routes was rejected because the UI, the docs
and running scripts already use them, and because Fuseki has no other route under
`/$/backups/` that they would collide with.
