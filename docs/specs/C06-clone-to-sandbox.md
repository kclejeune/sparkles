# C06: Clone-to-sandbox

> **Status:** implemented in part
>
> **Phases:** Phase 1 (`sparkles clone`, `POST /$/datasets/{ds}/clone` as a task, name
> reservation, cleanup, inferences copied or dropped, the UI's Clone action) shipped.
> From Phase 2, task cancellation shipped, and a free-space guard covers clones; the
> rest of Phases 2 and 3 is not built.
>
> **User docs:** [API: Clone](../API.md#clone) · [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation; the [Outcome](#outcome) section at the end
> records how it landed.

This is a clean-room spec, independent of commit identity
([CI](CI-commit-identity.md)). When CI exists, the clone:

- is a **new lineage**, with a fresh dataset id and its own root commit;
- records the source's dataset id and `seq` as `forkedFrom`, as CI's open question 1
  asks.

## 1. Summary

Users want to try updates, reasoning profiles or bulk loads against a copy of a
dataset without risking the original. Today that takes `sparkles dump` then
`sparkles load` by hand, or a backup and a restore. Both relabel every blank node,
lose the reasoning status, and depend on the operator picking a moment when nobody
writes.

**Goals**

- `sparkles clone --loc SRC --to DST` (offline) and
  `POST /$/datasets/{ds}/clone?name=NEW` (server). Each creates a new, independent,
  persistent dataset from **one consistent snapshot** of the source.
- The source is left untouched: no writes, no compaction, no lock held beyond an
  instant.
- The copy preserves:
  - every quad of every graph (default and named, including blank-node graph names);
  - RDF 1.2 triple terms;
  - blank-node identity: the same `_:b<hex>` labels, so relationships and labels both
    survive;
  - the dataset's prefixes.
- A failure leaves no half-created destination, and the destination name is reserved
  while the clone runs.
- A **Clone** action on the dataset page of the UI.

**Non-goals (Phase 1)**

- Copying history: WAL records, older generations, commit ancestry.
- Copy-on-write branching or merging back (a later, unplanned feature).
- Partial clones (one graph, a filter).
- In-memory destinations.
- Cloning across servers.

## 2. User-visible behavior

### 2.1 What is copied

| Item | Copied? | Notes |
|---|---|---|
| All quads visible in the captured snapshot (base ⊕ delta) | yes | Written as a freshly built, compacted generation `gen-0001` with an empty delta |
| Named graphs, including graphs named by blank nodes | yes | |
| Triple terms (`<<( s p o )>>`), including blank nodes inside them | yes | Keys are rebuilt from the source keys, so blank-node payloads inside triple terms are unchanged |
| Blank-node ids (`_:b<hex>` labels) | yes, identical | The next blank-node counter is carried over, so new blank nodes never collide |
| IRI and literal term ids, block layout, vocabulary order | **no** | Rebuilt. These are internal; all external identities (IRIs, literals, `_:b` labels) are equal |
| Prefixes (`meta.json` prefixes + `prefixes.json`) | yes | |
| Inferred graph `urn:x-sparkles:inferred` | by default (`inferences=copy`) | `inferences=drop` omits it |
| Reasoning status (`reasoning.json`) | with `inferences=copy` | Profile and inferred count are copied. `at` stays the source's time. The freshness position is rebased (§4.4) |
| WAL, older generations, delta vocabulary, `sparkles.lock` | no | The destination starts clean |
| Query result cache, block cache | no | In memory only |
| Registry entry, tasks | new entry for the destination only | |
| Commit history and dataset id ([CI](CI-commit-identity.md)) | no | The destination gets a **new** dataset id and a root commit `seq 0` (CI kind `create`). The source's id and `seq` are kept only as `forkedFrom` in `origin.json` |
| Future: text / vector indexes ([F03](F03-full-text-search.md), [F04](F04-vector-search.md)) | no | Their *configuration* should be copied and the indexes rebuilt in the destination. Decide in those specs |

The destination directory gets `origin.json` (format version 1):

```json
{ "originFormat": 1, "clonedAt": "2026-09-30T12:00:00Z",
  "source": { "name": "prod", "path": "/data/databases/prod", "version": 812,
              "generation": "gen-0007", "quads": 1048576 },
  "forkedFrom": { "id": "3f1c9a2e-…", "seq": 57 },
  "inferences": "copy" }
```

`forkedFrom` is `null` until CI exists. It is then `Store::dataset_id()` and the
captured snapshot's `Snapshot::commit`.

### 2.2 CLI

```
sparkles clone --loc SRC --to DST [--inferences copy|drop]
```

- `SRC` must be a Sparkles database directory, meaning it contains `CURRENT`.
  Otherwise the command errors with `SRC is not a Sparkles database (no CURRENT)` and
  creates nothing. Without this check, `Store::open` would create an empty database.
- `DST` must not exist, or must be an empty directory: `DST exists and is not empty`.
- Opening `SRC` takes its exclusive lock, as every CLI command does. If a server has
  it open, the existing message applies (`… is in use by another process (pid N); stop
  it or talk to it over HTTP`), and the user runs the HTTP clone instead.
- Prints `cloned SRC (version V, N quads, G graphs) to DST in 3.21s`.
- Exit status 0 on success, 1 on any error, with `DST` absent or empty afterwards.

### 2.3 HTTP

`POST /$/datasets/{ds}/clone`. Parameters come from the query string, a form body, or
a JSON body `{ "name": "...", "inferences": "copy"|"drop" }`:

| Param | Required | Meaning |
|---|---|---|
| `name` | yes | Name of the new dataset (`valid_name` rules) |
| `inferences` | no, default `copy` | `copy` or `drop` |

Responses:

| Status | When | Body |
|---|---|---|
| 202 | Started | `Task` with `kind: "clone"`, `dataset: "{ds}"`, `target: "{name}"`; header `Location: /$/datasets/{name}` |
| 400 | Missing or invalid `name`, bad `inferences` | `{"error": "invalid dataset name '…'"}` |
| 403 | Server is `--read-only` | `{"error": "server is read-only"}` |
| 404 | Unknown source dataset | |
| 409 | `name` is registered, reserved by a running clone, or `<data>/databases/{name}` already exists on disk | `{"error": "dataset /x already exists"}`, `"… is being created by task 12"`, or `"directory databases/x exists but is not a registered dataset; remove it first"` |

While the task runs, `GET /$/datasets/{name}` returns 404, and `POST /$/datasets` with
that name returns 409. When the task finishes it is `done` with message
`"cloned /prod at version 812 (1048576 quads) into /prod-sandbox"`, and the dataset is
registered and persisted in `config.json`. A `failed` task leaves no directory and no
registry entry, and releases the name.

`Task` gains an optional `target: string`. The TS type `TaskKind` gains `"clone"`.

### 2.4 Rust API

```rust
// crates/sparkles/src/store.rs
pub struct CloneOptions {
    /// graphs to leave out (e.g. the inferred graph); named by the caller
    pub exclude_graphs: Vec<NamedNode>,
    pub cancel: Option<Arc<AtomicBool>>,
    pub progress: Option<Arc<dyn Fn(f32, &str) + Send + Sync>>,
}
pub struct CloneReport { pub version: u64, pub generation: String, pub quads: u64,
                         pub graphs: u64, pub millis: u64 }
impl Store {
    /// Build a new database in `dir` (must not exist or be empty) from one snapshot.
    /// The caller builds into a temporary directory and renames it (§4.3).
    pub fn clone_to(&self, dir: &Path, opts: &CloneOptions) -> Result<CloneReport>;
}
```

`clone_to` writes only the data files. `origin.json` and `reasoning.json` belong to the
caller (the server/CLI layer that owns `reasoning.json` today).

### 2.5 UI

The dataset page header gets a **Clone** button next to Query and Delete. It is
disabled when the server is read-only. The dialog asks for:

- the new name, pre-filled `{ds}-sandbox`, with the dataset-name validation that
  `DatasetDialogs.svelte` already uses;
- an "Include inferences" checkbox, checked by default and shown only when the source
  has reasoning.

On submit the page tracks the task in the task list like compaction or backup. On
`done` it shows a toast "Cloned into /x" with an **Open** link to `/datasets/x`. A 409
shows its message inline under the name field. The new dataset's page shows a small
"cloned from /prod at version 812, 2026-09-30 12:00" line. This needs `origin` in
`DatasetInfo` (optional field, read from `origin.json`).

## 3. Standards basis

- **RDF 1.1 / 1.2 Concepts.** Blank nodes are local to a graph or dataset. A copy that
  keeps the same blank-node identities (not merely an isomorphic one) is the strongest
  preservation, and it matches Sparkles' promise that `_:b<hex>` labels round-trip
  through the protocol (README decisions table). Triple terms are terms and are copied
  as terms.
- **RDF Dataset Canonicalization (RDFC-1.0).** Used only as a *test oracle*: the
  canonical N-Quads of source and clone must be equal. Sparkles already enables
  `oxrdf`'s `rdfc-10` feature.
- **Fuseki admin protocol conventions** (`/$/datasets`, `Task` for long operations,
  `202`). Sparkles already follows them for compact and backup.

## 4. Semantics

### 4.1 Consistency

The clone contains exactly the quads of one `Snapshot`. The capture takes the source
writer lock only long enough to read `(snapshot(), next_bnode)` together, so no commit
can fall between them. It then releases the lock, so source updates continue during the
clone and are not included. The report and `origin.json` carry the snapshot's
`version` and generation, plus `forkedFrom.seq` (`Snapshot::commit`) once CI exists.

Concurrent source operations are safe:

- **Compaction and bulk commits** switch `CURRENT` and unlink the old generation.
  The captured `Arc<Generation>` keeps its mmaps valid (Unix unlink semantics; the
  store already relies on this for readers).
- **Deleting the source dataset** during a clone is the same case on Unix. On Windows,
  delete fails while mmaps are open. That is acceptable, and the delete error is
  reported.
- **Cloning a clone** is allowed.

### 4.2 Source untouched

`clone_to` never writes under the source root. For the CLI there is one caveat:
`Store::open` runs its normal crash recovery (it truncates a torn WAL tail and
creates `wal.log` if it is missing). That is not a data change.

### 4.3 Atomic creation and cleanup

- The build happens in a hidden sibling directory:
  - server: `<data>/databases/.clone-{name}-{task}`;
  - CLI: `{DST}.clone-tmp-{pid}`.

  The server name cannot collide with a dataset, because `valid_name` rejects a
  leading `.`.
- The finished tree is made durable (`Builder::finish` syncs the files, then
  `sync_dir`), renamed to the final path, and the parent directory is synced. On any
  error, cancellation or panic, the temporary directory is removed (a drop guard).
- **Server startup** removes leftover `<data>/databases/.clone-*` directories.
  They are never registered, so this is always safe.
- **Registration** happens after the rename, under `AppState::manage`: open the store,
  insert, `save_registry_locked`. If that fails, the renamed directory is removed and
  the task fails.
- **Crash between the rename and the registry save.** The directory exists but is not
  registered. A later clone or create with that name returns 409 with the "exists but
  is not a registered dataset" message. Nothing is adopted silently. See open
  question 2.

### 4.4 Reasoning status

- **With `inferences=copy`:**
  - the inferred graph is copied;
  - `reasoning.json` is written into the destination;
  - the freshness position ([C08](C08-inference-freshness.md)) is rebased. If the source inferences were fresh at
    the captured snapshot, the clone's status records `commit` = the clone's initial
    position (root commit `0` under CI) and `datasetId` = the clone's new id, so it
    reads as fresh.
  - if the source status was stale, the clone's status records
    `"inheritedStale": true`. C08 reports that as `stale: true`,
    `commitsSince: null`, `staleReason: "inherited from source at clone time"`.
  - if the source status was unknown, it stays unknown (no `commit`).

  Before C08 lands, `profile`, `inferred` and `at` are copied as they are.
- **With `inferences=drop`:** the inferred graph is excluded, and no `reasoning.json`
  is written.
- **Without `reasoning.json`** in the source, an `urn:x-sparkles:inferred` graph is
  still copied as ordinary data under `copy`. This mirrors the source exactly.

### 4.5 Limits and configuration

- **Destination.** Always persistent, in the server's data directory. Phase 1 does not
  offer a `type=mem` destination (`400 "clone into an in-memory dataset is not
  supported yet"`).
- **Sources.** Any dataset: persistent, `--loc`-attached, or `--mem`.
- **Disk.** The clone needs about the source's compacted size, plus the builder's
  temporary sort files (`BuildOptions::sort_mem_quads` spills). There is no preflight
  in Phase 1: an out-of-space error fails the task and cleans up.
- **Concurrency.** Clones run as tasks in the existing task thread model. There is no
  limit on concurrent clones in Phase 1 (open question 3).
- **Cancellation.** Tasks are not cancellable today, and there is no endpoint for it.
  The `cancel` flag serves the CLI's Ctrl-C handler and future task cancellation.

## 5. Design sketch

**`crates/sparkles/src/store.rs`**

1. **Refactor.** Extract from `rebuild_locked` the loop that pushes every quad of a
   snapshot into a `Builder` encoder, translating `Vocab`/`Delta` ids to keys
   (`Slot::Key`) and passing inline and blank-node ids through (`Slot::Id`). The new
   `fn write_snapshot(builder: &Builder, snap: &Snapshot, skip_graph: impl Fn(u64) -> bool,
   extra: &[[Id;4]]) -> Result<()>` is used by both `rebuild_locked` and `clone_to`.

   Passing blank-node ids through preserves `_:b<hex>` identity. Triple-term
   keys already embed blank-node payloads (`id::write_term_key_with`), so copying the
   key bytes preserves them too.
2. **`clone_to(dir)`** builds directly into `dir`, which must be absent or empty. The
   caller creates `dir` as the hidden temporary sibling of §4.3, adds `origin.json` and
   `reasoning.json`, and does the final rename.
   - capture state:
     `let w = self.writer.lock(); let snap = self.snapshot(); let nb = w.next_bnode; drop(w);`
   - build `dir/gen-0001` with `BuildOptions { first_bnode: nb, ..self.opts.build }`;
   - `builder.add_prefixes(self.prefixes())`, then `finish()`;
   - create an empty `gen-0001/wal.log`;
   - `write_atomic(dir/CURRENT, "gen-0001")`, then write `dir/prefixes.json`;
   - `sync_dir(dir)`.

   A drop guard in `clone_to` empties `dir` on every early return. The caller's guard
   removes the temporary directory itself if anything later fails. `delta.vocab` need
   not be created: `DeltaVocab::open` treats a missing file as empty.
3. **In-memory sources.** They work unchanged: `write_snapshot` reads through
   `Snapshot::key`/`scan`, whatever the backing.

**`crates/sparkles-server/src/state.rs`**

- Add `reserved: Mutex<BTreeMap<String, String /* task id */>>` and
  `fn reserve(&self, name) -> Result<Reservation>`. `Reservation` is an RAII guard that
  releases the name on drop. `reserve` takes `manage` and fails when:
  - the name is registered;
  - the name is reserved;
  - `databases/{name}` exists.
- `create` and `attach` also check `reserved`.
- `fn adopt(&self, name, dir, reservation) -> Result<Arc<Dataset>>`: under `manage`,
  `open_dataset(name, Persistent, None)`, insert, save the registry, drop the
  reservation.
- `AppState::new` removes `databases/.clone-*`.
- `Task` gains `target: Option<String>`.

**`crates/sparkles-server/src/http.rs`**

- Route `.route("/$/datasets/{ds}/clone", post(clone_dataset))`.
- The handler does the parameter checks, then `st.reserve(name)`, which gives the 409s
  synchronously. It then runs `start_task("clone", ds, …)`. The task:
  - calls `ds.store.clone_to(tmp, …)`, passing the inferred graph in
    `exclude_graphs` when `drop`;
  - writes `origin.json` and (for `copy`) `reasoning.json` into `tmp` before the
    rename;
  - renames `tmp` to `databases/{name}`, then calls `st.adopt(…)`.
- `dataset_info` adds `origin` when `origin.json` exists.

**`crates/sparkles-server/src/main.rs`:** `Cmd::Clone { loc, to, inferences }`. It
checks for `CURRENT`, opens the source, runs `clone_to(tmp)`, writes `origin.json` and
`reasoning.json` (via `state::read_reasoning_file` / `write_reasoning_file`), then
renames.

**Persistence format.** The destination is an ordinary Sparkles database
(`FORMAT_VERSION` 2 generation). `origin.json` is new, versioned (`originFormat: 1`),
optional, and informational.

**Crash safety.** Temporary directories are either removed on error or swept at
startup. The rename is the commit point for the files, and the registry save is the
commit point for the server.

### Why Phase 1 rebuilds instead of copying files

Three ways to produce the destination:

- **(a) `dump` → N-Quads → `load`.** Simple, but it relabels every blank node: the bulk
  loader scopes labels per source document and assigns fresh ids. It also serializes
  and parses every term, and needs a temporary text file. It preserves the graph only
  up to isomorphism.
- **(b) Rebuild from the snapshot with the builder** (chosen). One ordered scan, no
  parsing. It preserves blank-node ids, compacts the delta away, handles in-memory
  sources, and supports `inferences=drop` by skipping a graph. It reuses the proven
  compaction path, so its cost is that of `sparkles compact` on the source (an external
  sort per permutation).
- **(c) Copy or link the immutable generation files** when the captured delta is empty.
  Near-instant with hard links or reflinks, O(bytes) with a plain copy. It applies only
  when the delta is empty, `inferences=copy` is chosen, and source and destination
  are on the same filesystem (for links). It also needs care:
  - `delta.vocab` and `wal.log` in the generation directory are mutable and must be
    recreated, not linked;
  - `meta.next_bnode` may be lower than the writer's counter, which is safe only
    because an empty delta references no newer blank nodes;
  - hard links make a future in-place repair tool affect both datasets.

Phase 1 ships (b) only: one code path, every case, and no new invariants. (c) is a
Phase 2 optimization behind the same API. It uses `FICLONE` reflinks, falling back to
plain copies, and uses hard links only with an explicit `--link` flag. It is tested
against (b) for equality.

## 6. Phasing

**Phase 1 (MVP, about one day):**

- `write_snapshot` refactor plus `Store::clone_to`;
- `sparkles clone`;
- `POST /$/datasets/{ds}/clone` with reservation, adoption and startup sweep;
- `origin.json` and `inferences=copy|drop`;
- the `Task.target` field;
- the UI Clone dialog and the "cloned from" line;
- tests for §7 A–G.

**Phase 2:**

- the file-copy/reflink fast path for an empty delta;
- a disk-space preflight (`statvfs` against the source's `disk_bytes()` × 1.2), 507 on
  shortfall;
- in-memory destinations (`type=mem`);
- task cancellation;
- a limit on concurrent clones;
- a `--graph` filter for partial clones.

**Phase 3:** clone at a past commit (`?at=` from
[F06](F06-snapshots-and-point-in-time.md)), cross-server clone via archives
([F05](F05-snapshot-repositories.md)), and branch/merge semantics.

## 7. Acceptance examples

**A. Round-trip fidelity** (library test). Source TriG:

```trig
@prefix ex: <http://ex.org/> .
ex:s ex:p _:x . _:x ex:q "v"@en--ltr .
GRAPH ex:g { ex:s ex:r <<( _:x ex:p 1 )>> }
GRAPH _:gb { _:x ex:in _:gb }
```

Bulk-load it, then run `INSERT DATA { ex:t ex:p ex:u }` so that one quad is in the
delta. Run `clone_to`, then open the destination. Expected:

- `dump_nquads(src)` and `dump_nquads(dst)`, sorted, are byte-identical. Identical
  `_:b<hex>` labels also make their RDFC-1.0 canonical forms equal.
- `SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } }` is equal on both.
- `ASK { GRAPH ?g { ?s ?p <<( ?x ?q ?y )>> } }` is true on the destination.
- The destination has an empty delta and exactly one generation `gen-0001`.

**B. Blank-node labels stable.** `SELECT ?o { <http://ex.org/s> <http://ex.org/p> ?o }`
returns `_:b1f` (say) on the source, and the same label on the clone. Then
`INSERT DATA { <http://ex.org/n> <http://ex.org/p> [] }` on the clone creates a blank
node whose label is not among the source's (counter carried over).

**C. Server happy path:**

```
POST /$/datasets/prod/clone?name=prod-sandbox
→ 202 {"id":"7","kind":"clone","dataset":"prod","target":"prod-sandbox","state":"running",…}
   Location: /$/datasets/prod-sandbox
GET /$/tasks/7          → state "done", message "cloned /prod at version 3 (5 quads) into /prod-sandbox"
GET /$/datasets/prod-sandbox → 200 DatasetInfo, quads 5, origin.source.name "prod"
POST /prod-sandbox/update  (INSERT DATA { <a> <b> <c> })
GET /$/datasets/prod    → quads still 5
```

After a server restart, `prod-sandbox` is still registered.

**D. Name conflicts:**

- clone into an existing dataset name → 409;
- two clone requests with the same `name` back to back → the second gets 409 `… is being
  created by task N`;
- `POST /$/datasets` with `dbName` equal to the reserved name during the clone → 409;
- `mkdir <data>/databases/ghost` then clone `name=ghost` → 409 "exists but is not a
  registered dataset";
- `name=.hidden` or `name=a/b` → 400.

**E. Failure cleanup.** Inject a failure after the build and before the rename (a test
hook, or a read-only temporary parent). The task is `failed`, no
`databases/.clone-*` or `databases/{name}` remains, and the name is reusable
immediately. Separately, create `databases/.clone-x-9` by hand and restart the server:
the directory is gone and nothing is registered.

**F. Consistency under concurrent writes.** Start a clone of a 1M-quad dataset. While it
runs, POST 100 updates to the source, each inserting one quad. The clone's quad count
equals the source count at the captured version (`origin.source.quads`), and it is less
than or equal to the final source count. The source ends with all 100 inserts.

**G. Inferences:**

- On a source with rdfs reasoning, `inferences=copy` → the clone's DatasetInfo has
  `reasoning.profile "rdfs"`, and a query for an inferred type returns rows.
- `inferences=drop` → `reasoning` is null, `GRAPH <urn:x-sparkles:inferred> {…}` is
  empty, and the quad count equals the source's minus its inferred triples.

**H. CLI:**

- `sparkles clone --loc db --to db2` → exit 0, and `sparkles stats --loc db2` shows the
  same quad count.
- Running it again → exit 1 `DST exists and is not empty`.
- `sparkles clone --loc nope --to x` → exit 1 `is not a Sparkles database`, and neither
  `nope` nor `x` is created.

## 8. Rejected alternatives

- **Dump/load as the Phase 1 mechanism.** It relabels blank nodes and is slower
  (§5). It remains the documented manual route for moving between machines.
- **File copy as the Phase 1 mechanism.** It covers only the empty-delta case and
  needs new invariants about mutable files in a generation directory (§5).
- **Holding the source writer lock for the whole clone.** It would give the same
  consistency but block source updates for the duration, and the snapshot is already
  immutable.
- **Registering the destination first, then filling it.** Clients could query a
  half-built dataset. Reservation without registration avoids that.
- **Copying WAL and history.** It exposes internal ids and ties the clone to the
  source's generation numbering. Under CI it would also give two lineages the same
  commit history. A fresh id plus `forkedFrom` states the relationship accurately.

## 9. Open questions

1. Should the default be `inferences=drop`, so sandboxes start without inherited
   inferences and re-run reasoning? The spec defaults to `copy`, so the clone behaves exactly like the
   source.
2. Should startup offer `--adopt-orphans` to register `databases/{name}` directories
   that contain `origin.json` but no registry entry? The spec leaves orphans alone and
   reports 409.
3. Should concurrent clones be limited (disk and CPU)? Unlimited in Phase 1.
4. Should `POST /$/datasets` accept `{"cloneFrom": "prod"}` as an alias? It would suit
   Fuseki-style clients, but duplicates the route.
5. Should the clone copy the source's result-cache and block-cache settings per
   dataset? Settings are server-wide today, so this does not apply yet.

## 10. Sources

- Sparkles repository:
  - [CI-commit-identity.md](CI-commit-identity.md) and [C08-inference-freshness.md](C08-inference-freshness.md) (sibling
    clean-room specs): dataset id, root commit, `forkedFrom`, and the reasoning status
    fields.
  - `crates/sparkles/src/store.rs`: `Store::{open, write, rebuild_locked, dump_nquads,
    backup}`, `bnode_for`/`parse_bnode_label`, `WriteTxn::{intern_scoped, encode_quad}`,
    the WAL format, `write_atomic`/`sync_dir`.
  - `crates/sparkles/src/builder.rs`: per-document `LabelScope`, `Slot`,
    `BuildOptions::first_bnode`, `IndexMeta`.
  - `crates/sparkles/src/id.rs`: the triple-term key encoding with blank-node payloads.
  - `crates/sparkles/src/index.rs` and `vocab.rs`: generation file names.
  - `crates/sparkles-server/src/state.rs`: registry, `manage` lock, `create`/`attach`/
    `delete`, `valid_name`, reasoning file, tasks.
  - `crates/sparkles-server/src/http.rs`: dataset admin handlers and status codes.
  - `crates/sparkles-server/src/main.rs`: CLI structure, `dump`/`backup`/`infer`.
  - `ui/src/routes/datasets/[name]/+page.svelte` and `ui/src/lib/api.ts`.
  - `README.md` (decisions table on blank-node labels and triple terms), `docs/API.md`,
    and the project's clean-room feature order (internal planning notes).
- W3C RDF 1.1 / RDF 1.2 Concepts (blank-node scope, triple terms) and RDF Dataset
  Canonicalization (RDFC-1.0) as a test oracle. Cited from working knowledge; not
  re-fetched.
- Linux `ioctl_ficlone(2)` / `copy_file_range(2)` man pages (reflink fallback, Phase 2).
  Cited from working knowledge.
- Fluree was **not** consulted: no code, documentation, or product pages.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30, after durable commit identity, so the clone
was a new lineage from the start: a new dataset id and root commit `0`, with the
source's id and the copied commit recorded as `forkedFrom` (in `dataset.json`, read
through `Store::forked_from`, and in `origin.json`). There was no "before CI" stage with
`forkedFrom: null`.

- `Store::clone_to` builds the copy through the same rebuild path compaction uses
  (factored into a shared `write_snapshot`): every quad of every graph, triple terms,
  prefixes and identical blank-node ids, with the blank-node counter carried over. The
  writer lock is held only to capture the snapshot.
- `POST /$/datasets/{ds}/clone?name=NEW[&inferences=copy|drop]` runs as a task. The name
  is reserved up front (`409` while registered, being created by another task, or an
  unregistered directory exists); the copy is built in `databases/.clone-NAME-TASK` and
  renamed into place; failures leave nothing behind and leftovers are swept at startup.
- With `inferences=copy` the reasoning status is rebased as §4.4 describes: fresh stays
  fresh at the clone's root, stale is inherited as stale
  ([C08](C08-inference-freshness.md)).
- `sparkles clone --loc SRC --to DST [--inferences copy|drop]` works offline; the dataset
  page gained a Clone action, a "cloned from" line and an Open link.

**Follow-ups.** A clone of a dataset with full-text search first lost its index
configuration, so `text:query` silently stopped working there; the clone now copies the
text configuration and rebuilds its index on first open (the table row in §2.1 left
this to the text spec). Later work made clone tasks cancellable until the copy is in
place (`DELETE /$/tasks/{id}`), and a clone that would leave less than
`--min-free-disk-mb` free is refused with `507` and leaves nothing behind, which covers
the purpose of the planned disk-space preflight.

**Open questions.** The defaults stayed as specified: `inferences=copy`, orphaned
directories are reported with `409` rather than adopted, concurrent clones are not
limited, and there is no `cloneFrom` alias on `POST /$/datasets`.

**Not built.** The file-copy/reflink fast path, in-memory destinations (`type=mem` is
refused with `400`), a limit on concurrent clones, partial clones by graph (the library
can exclude graphs, which `inferences=drop` uses, but no flag exposes it), and all of
Phase 3: cloning at a past commit, cross-server clones, branching and merging.
