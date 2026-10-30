# F05 — Backup repositories: incremental backup and restore, policies, GC — Delivery record

Current delivery status is summarized in the [landing page](README.md).
The [design Outcome overview](SPEC.md#outcome) links the supported scope to this record.
The recorded dates and measurements below describe their original delivery stages.

## Historical opening status

> **Status:** implemented in part (Phase 1, most of Phase 2)
>
> **Phases:** Phase 1 shipped: repositories, incremental backups, restore, verify, the
> CLI, the server and the Backups panel. Most of Phase 2 shipped with it: lifecycle
> policies, garbage collection, locks and the full Backups page, with `gcs` and `azure` as
> experimental build features. Full backups of in-memory datasets followed. These were not
> built: "back up all datasets" as one request, the CLI's `--server` mode,
> content-defined chunking, repository copy and server backups. An optional local-key
> encrypted repository engine is now built under [F11](../F11-encryption-at-rest.md);
> local key providers, offline operator commands and trusted TOML server integration
> are also built. API-managed encrypted configuration and later encrypted phases remain open.
>
> **User docs:** [API: Backup repositories](../../API.md#backup-repositories) ·
> [Usage: Backup repositories](../../USAGE.md#backup-repositories) ·
> [Features](../../operations/README.md#operations) ·
> [Benchmarks: Backup repositories](../../benchmarks/writes/RESULTS.md#backup-repositories-105m-triples)
>
> This is the design as written before implementation. The [Outcome](SPEC.md#outcome) section at
> the end records how it landed.

## Outcome

**Delivered.** Backup repositories landed on 2026-09-30:
* the `sparkles-backup` crate, behind the `backup` cargo feature of `sparkles-server`
  (on by default), using `object_store` 0.14.2, `croner` 4 and `chrono-tz`;
* the server's `/$/repositories`, `/$/backups/{ds}` and `/$/backup-policies` routes;
* `sparkles repo` and `sparkles backup create|list|show|restore|verify|delete|policy`;
* a Backups page with Backups, Repositories, Policies and Activity tabs, plus a panel on
  the dataset page.

Offline `backup policy run P --data DIR --backup-config FILE` also uses the shared
catalog policy engine. It requires an existing stopped catalog, holds its directory
lock and retains the TOML repository configuration. It resolves credentials and
optional encryption providers only for the selected repository, then passes that
opened repository to the shared engine without registering it in the catalog.
Configured repository names and persisted location/UUID identities are validated
before execution. Manual execution permits scheduler-disabled policies. A plain
worker thread runs the blocking facade while the CLI runtime services Ctrl-C;
Tokio blocking workers still carry an async runtime handle and cannot call this
facade. Results include selected datasets, unchanged skips and retention. Backup
or retention errors produce a nonzero exit status. Process-level tests cover
two selected datasets, shared run identity, unchanged skips, a held catalog lock,
missing catalogs, read-only failure reports and released repository locks.

The offline `--replace` swap holds the target's `sparkles.lock` through the store's
lock guard since roadmap item 95 (2026-10-24). It took the lock as a plain file
before, so a child forked before exec could keep the lock after the swap released it,
and a rolled-back swap could leave the original database locked. The guard unlocks the
file explicitly when the process that took it drops it, and the busy probe of
`backup restore --to` unlocks the same way.

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

**Performance.** [Benchmarks: Backup repositories](../../benchmarks/writes/RESULTS.md#backup-repositories-105m-triples)
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
[F11](../F11-encryption-at-rest.md), which replaces the sketch in §7 Phase 3. Its Phase 1
is the encrypted repository of [F11 §5](../F11-encryption-at-rest.md#5-encrypted-backup-repositories-phase-1).
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

### Roadmap follow-ups (2026-10-12)

**Whole-server backup and restore, every dataset in one request, a remote backup CLI,
repository copy, content-defined chunking and retained-history storage controls** (item 61, Tier 6,
[ROADMAP](../../ROADMAP.md)).

* **What is missing today.** The status block's list still holds. The repository routes
  are `GET /$/repositories/{repo}/backups` (a listing only), `gc`, `locks`, `test` and
  `verify`; there is no `POST` that backs up several datasets
  (`docs/openapi.json`). `sparkles repo` has `add`, `key`, `list`, `show`, `test`,
  `verify`, `remove`, `gc` and `locks`, with no `copy`, and `sparkles backup` works on a
  data directory or a stopped server only, with no `--server`
  (`crates/sparkles-server/src/backup/cli.rs`). Pieces are fixed 32 MiB slices of each
  file. Server state (the catalog's `config.json`, auth state, `models.json`, settings
  and notification files, repository registrations) is not backed up.
* **Every dataset in one request.** `POST /$/repositories/{repo}/backups` with
  `{datasets: [...] | "*", message?}` starts one task that backs up each dataset in
  turn, sharing a `run` id, as §7 Phase 2 designs; policies already do this.
* **Server backups.** A backup kind `server` beside the dataset backups. Its manifest
  lists the server's own files from the data directory: the catalog registry, the
  dataset-independent settings, `models.json`, notification settings, backup
  repositories and policies, and the auth store (users and token hashes, not session
  secrets). Runtime model secrets are left out unless the server seals them
  ([C19](../C19-layered-settings.md#roadmap-follow-ups-2026-10-12)); sealed values are
  included and restore only with the same key. A server backup leaves out
  `secrets.key` and any `.secrets.key.*.tmp`, and copies `secrets/` as sealed files.
  C19's item 68 Outcome records why. `POST /$/server/backups {repository,
  datasets?}` makes a server backup plus one backup per listed dataset under one run
  id. `sparkles backup restore-server RUN --data DIR` restores into an empty data
  directory, offline, and recreates every dataset of the run; it refuses a non-empty
  directory.
* **Remote backup CLI.** `--server URL` on `backup create`, `list`, `show`, `restore`,
  `verify` and `delete`, sending the existing routes through the shared client with the
  saved credentials of [C09 §7.4](../C09-dataset-access-control.md), and waiting for the
  task with progress unless `--no-wait`.
* **Repository copy.** `sparkles repo copy SRC DST [--dataset D] [--backups NAME...]
  [--since DATE]` copies manifests and the blobs they reference that `DST` lacks,
  checked by id, then writes the manifests last so that a partial copy is invisible.
  Between two encrypted repositories blob ids are keyed per repository ([F11
  §5.2](../F11-encryption-at-rest.md)), so the copy re-encrypts and re-keys each blob and
  cannot deduplicate by id; it deduplicates by the plaintext id inside the run. The
  server route is a task, `POST /$/repositories/{repo}/copy`.
* **Content-defined chunking.** Designed in [F11 §6](../F11-encryption-at-rest.md#6-content-defined-chunking-and-write-only-repositories-phase-2):
  FastCDC for `vocab.dat` behind the 30% gate of §5.4. It applies to plaintext
  repositories too, and the manifest records the chunker per file so old backups stay
  readable.


### Remaining history and retention contract

Item 61 also completes the optional inclusion of F06 retained generations, pins and
retention metadata from the original Phase 3. Capture those references consistently
with the backup's head and protect their referenced files until transfer completes.
Restore must answer every included retained commit and preserve its pins, without
claiming to recover states that were already expired. The default remains a
current-state backup. Version any new manifest contract and test both modes against
older readers and unknown required fields.

Complete storage-class selection and S3 Object Lock/WORM controls from Phase 3.
Distinguish the repository's logical expiry from the object's enforced retention,
so GC does not promise deletion before it is permitted. Acceptance covers locked
blobs shared by backups, denied or partial deletion, expiry, retries and an archived
object's restore-before-read behavior.

Resolve X01's reserved zstd repository encoding explicitly when selecting codecs for
the versioned manifest and copy path. Measure an adopted implementation against
raw/LZ4 for transfer size, CPU and restore time, or record why it remains unadopted.
Older raw/LZ4 backups must remain readable, and unknown required codec tags must be
refused. The measured rejected X11 column codecs are separate decisions and are not
reopened by this checkpoint.

These are planned acceptance requirements. Item 61 does not complete while the
optional history mode or storage-retention controls promised by Phase 3 are silently
omitted. Changing that accepted scope needs an explicit maintainer decision recorded here.

### Recovery of managed definitions

Roadmap item 220 landed on 2026-10-30. Backups and persistent clones carry the nested
definitions of the views of [F13](../F13-incremental-views-and-provenance.md). The
mechanism is generic over the
definition kinds that `formats::DEFINITIONS` registers. The definitions of
[F22](../F22-analysis-lifecycle.md) and the settings and states of
[C22b](../C22b-report-configurations.md) do not exist yet. Each needs only a registry
entry, a live source and its own pause semantics when it is built.

**Manifest entries.** A definition is an entry of the manifest's `files` with the path
`views/{name}/view.json`, kind `meta`, one blob, its size and its SHA-256. Its
`definition` member names the kind (`view`) and the version of the definition file
(`format`). An entry may set `required: false`, and a reader that does not know such a
kind skips it after checking that its path is a plain relative one. A manifest with
definition entries lists `definitions` in its `requires`. The manifest keeps `format`
1, so a backup without views has the layout older releases wrote. The format registry
lists the entry as `backup-definition`, the features in `BACKUP_MANIFEST_FEATURES` and
the kinds in `DEFINITIONS`. An entry always holds a definition as it runs, at format 1
for a view.

The manifest format stays 1 on purpose. Older releases ignore unknown members, so their
garbage collection parses such a manifest and keeps its blobs. Their validation refuses
it, because the definition path is not one of the paths they allow. Their restore
therefore refuses it before writing anything, and their verification reports the backup
as invalid, so a whole-repository verification by an older release also counts the
blobs only that backup references as orphans. Raising the manifest format would instead
make an older server's garbage collection stop at the first new manifest. A reader of this release
refuses a manifest that requires an unknown feature, a required definition of an unknown
kind and a definition file newer than it reads, each with `422 incompatible-format`.

**Capture boundary.** The capture reads the definitions while it holds the store's
writer lock, at the same point as the commit, the WAL length and the prefixes. The lock
order is the store's writer lock, then the store's registry of definition sources, then
the view runtime's definition snapshot. The runtime replaces that snapshot while it holds
its own registry lock after each definition change, and never waits for the writer lock
while holding either. The snapshot is a leaf lock held only to clone a reference, so a
capture holds the writer lock for no longer than before. Definitions are uploaded after
the lock is released, as the data files are. A definition changed during or after the
capture is not the captured one. A store without a live source, such as a stopped
database that `backup create --loc` opens, reads the definitions from its directory
under the writer lock, while its process lock keeps other writers out, and records a
paused definition as it would run. Definitions that cannot be read refuse the capture
rather than being left out. After the lock is released, and before anything is
uploaded or a clone writes anything, every captured definition is checked as a restore
would check it. A definition that a restore would refuse fails the backup with
`400 invalid-request`, so no backup is published that cannot be restored. Defining a
view refuses a definition larger than 1 MiB, the limit a restore enforces. Backups of in-memory
datasets carry their views, which their runtime keeps in memory.

**Restore.** The manifest is validated before anything is written. Restore refuses a
definition path that is not exactly the kind's `DIR/NAME/FILE` with a valid name,
duplicate paths, an entry not announced by `requires`, a definition larger than 1 MiB
and a definition that is not a meta file. Definition files are staged beneath the
unpublished restore directory. Each directory is created there, an existing path that
is not a plain directory is refused, and files are created with `create_new`, so nothing
is written through a link. Every blob is checked against its id and length and every
file against its SHA-256. Each definition must then parse, carry a version this release
reads, match the version its entry names, and deserialize as its kind's schema with
options this release supports. A view's principal and stored query are resolved when it
resumes, not at restore. A missing or corrupt blob or a failed check
removes the directory, so the dataset is never published.

**Paused views.** Each restored definition is rewritten as a paused `view.json` of format
2, which is format 1 with a `paused` member that holds the reason `restored` and the
time. A clone writes the reason `cloned`. Releases from before item 220 read `view.json`
only at format 1, so they refuse a paused one: the dataset's views do not attach there,
while its data is served, which is how an older reader refuses a newer `view.json`.
An older binary therefore never runs a view that waits for its checks, whether it opens
a restored or cloned directory or the original. A paused view keeps its
definition but has no circuit, runs no passes and answers reads with
`503 view-suspended`. The pause survives restarts. `POST /$/views/{ds}/{name}/rebuild`
resumes the view, which rewrites its `view.json` at format 1 atomically and syncs its
directory before the suspension is lifted. A `--read-only` server refuses rebuilds with
`403`, as it refuses view changes. The view then attaches on the next round, which resolves its principal
under the server's current policy and its stored query, so a principal that does not
exist or a query that does not resolve leaves the view `failed`. Replacing the view's
definition also lifts the pause. Derived view checkpoints are not backed up, and a
resumed view is built from the data.

**Clones.** A persistent clone captures the definitions under the same writer lock as its
snapshot, before it takes the history lease of a shared generation, so a capture that
fails leaves no lease behind. It writes them paused and syncs the files and their
directories before it publishes `CURRENT`. `BackupCapture::write_to` syncs them the
same way. A clone of a past state
carries the current definitions. In-memory clones carry no view definitions, as they
carry no stored queries. Roadmap item 401 tracks them.

C26 jobs and outbox entries, credential references and the remapping of references for a
clone's new identity are not built and remain with
[C26](../C26-durable-jobs-and-delivery.md). The acceptance tests cover a round trip of
two views with identical rows after resumption, the pause across a restart, a clone, an
in-memory dataset, a backup without views that keeps the older layout, every refusal
(traversal, duplicate, newer version, unknown kind, unknown feature, missing blob,
corrupt blob, wrong digest, links during staging, schema mismatches, oversized
definitions at definition and backup time), the paused format and its rewrite on resume,
a read-only server's refused rebuild, the lease after a failed clone capture and a definition change racing many
captures. That test checks that each capture holds the definition current at its commit
or the one that followed it. A failpoint test changes a definition while the capture
holds the writer lock and after it releases the lock, and the capture keeps the earlier
definition in both cases.
