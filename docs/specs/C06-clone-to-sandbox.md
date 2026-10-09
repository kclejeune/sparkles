# C06: Clone-to-sandbox

> **Status:** implemented in part
>
> **Phases:** Phases 1 and 2 shipped. Phase 1 covers `sparkles clone`,
> `POST /$/datasets/{ds}/clone` as a task, name reservation, cleanup, copying or dropping
> inferences, and the UI's Clone action. Phase 2 added task cancellation, a free-space
> guard, the fast path that shares the source's index files, in-memory destinations, a
> limit on concurrent clones and partial clones by graph. From Phase 3, cloning at a past
> commit shipped with point-in-time reads, and branches and merges shipped as
> [F09](F09-branches-and-merges.md). Cloning across servers is not built.
>
> **User docs:** [API: Clone](../API.md#clone) · [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation; the [Outcome](#outcome) section at the end
> records how it landed.

This spec does not depend on commit identity
([CI](CI-commit-identity.md)), but once CI exists the clone:

- is a **new lineage**, with a fresh dataset id and its own root commit;
- records the source's dataset id and `seq` as `forkedFrom`, as CI's open question 1
  asks.

## 1. Summary

Users want to try updates, reasoning profiles or bulk loads on a copy of a dataset
without risking the original. Today they run `sparkles dump` and then `sparkles load` by
hand, or take a backup and restore it. Both routes relabel every blank node and lose the
reasoning status. Both also need the operator to pick a moment when nobody writes.

**Goals**

- `sparkles clone --loc SRC --to DST` works offline, and
  `POST /$/datasets/{ds}/clone?name=NEW` works on the server. Each creates a new,
  independent, persistent dataset from **one consistent snapshot** of the source.
- The source is left untouched. The clone writes nothing to it, does not compact it,
  and holds its lock only for an instant.
- The copy preserves:
  - every quad of every graph, default and named, including graphs named by blank
    nodes;
  - RDF 1.2 triple terms;
  - blank-node identity, so the `_:b<hex>` labels and the relationships between blank
    nodes both survive;
  - the dataset's prefixes.
- A failure leaves no half-created destination. The destination name is reserved while
  the clone runs.
- The dataset page in the UI gets a **Clone** action.

**Non-goals (Phase 1)**

- Copying history: WAL records, older generations or commit ancestry.
- Copy-on-write branching, or merging changes back. That would be a later feature, and
  it is not planned.
- Partial clones, such as one graph or a filter.
- In-memory destinations.
- Cloning across servers.

## 2. User-visible behavior

### 2.1 What is copied

| Item | Copied? | Notes |
|---|---|---|
| All quads visible in the captured snapshot (base ⊕ delta) | yes | They are written as a freshly built, compacted generation `gen-0001` with an empty delta. |
| Named graphs, including graphs named by blank nodes | yes | |
| Triple terms (`<<( s p o )>>`), including blank nodes inside them | yes | Keys are rebuilt from the source keys, so blank-node payloads inside triple terms do not change. |
| Blank-node ids (`_:b<hex>` labels) | yes, identical | The next blank-node counter is carried over, so new blank nodes never collide with copied ones. |
| IRI and literal term ids, block layout, vocabulary order | **no** | These are internal and are rebuilt. Every external identity (IRIs, literals, `_:b` labels) stays equal. |
| Prefixes (`meta.json` prefixes + `prefixes.json`) | yes | |
| Inferred graph `urn:x-sparkles:inferred` | by default (`inferences=copy`) | `inferences=drop` leaves it out. |
| Reasoning status (`reasoning.json`) | with `inferences=copy` | The profile and inferred count are copied. `at` keeps the source's time. The freshness position is rebased (§4.4). |
| WAL, older generations, delta vocabulary, `sparkles.lock` | no | The destination starts clean. |
| Query result cache, block cache | no | They live in memory only. |
| Registry entry, tasks | new entry for the destination only | |
| Commit history and dataset id ([CI](CI-commit-identity.md)) | no | The destination gets a **new** dataset id and a root commit `seq 0` (CI kind `create`). The source's id and `seq` survive only as `forkedFrom` in `origin.json`. |
| Future: text / vector indexes ([F03](F03-full-text-search.md), [F04](F04-vector-search.md)) | no | The clone should copy their *configuration* and rebuild the indexes in the destination. Those specs decide. |

The clone writes `origin.json` (format version 1) into the destination directory:

```json
{ "originFormat": 1, "clonedAt": "2026-09-30T12:00:00Z",
  "source": { "name": "prod", "path": "/data/databases/prod", "version": 812,
              "generation": "gen-0007", "quads": 1048576 },
  "forkedFrom": { "id": "3f1c9a2e-…", "seq": 57 },
  "inferences": "copy" }
```

`forkedFrom` is `null` until CI exists. After that it holds `Store::dataset_id()` and
the captured snapshot's `Snapshot::commit`.

### 2.2 CLI

```
sparkles clone --loc SRC --to DST [--inferences copy|drop]
```

- `SRC` must be a Sparkles database directory, which means it contains `CURRENT`.
  Otherwise the command fails with `SRC is not a Sparkles database (no CURRENT)` and
  creates nothing. Without this check, `Store::open` would create an empty database.
- `DST` must not exist or must be an empty directory. Otherwise the error is
  `DST exists and is not empty`.
- Opening `SRC` takes its exclusive lock, as every CLI command does. If a server has it
  open, the usual message appears (`… is in use by another process (pid N); stop it or
  talk to it over HTTP`), and the user runs the HTTP clone instead.
- On success the command prints `cloned SRC (version V, N quads, G graphs) to DST in 3.21s`.
- The exit status is 0 on success and 1 on any error. After an error, `DST` is absent
  or empty.

### 2.3 HTTP

`POST /$/datasets/{ds}/clone` takes its parameters from the query string, a form body,
or a JSON body `{ "name": "...", "inferences": "copy"|"drop" }`:

| Param | Required | Meaning |
|---|---|---|
| `name` | yes | Name of the new dataset. The `valid_name` rules apply. |
| `inferences` | no, default `copy` | `copy` or `drop` |

Responses:

| Status | When | Body |
|---|---|---|
| 202 | The clone started. | `Task` with `kind: "clone"`, `dataset: "{ds}"`, `target: "{name}"`; header `Location: /$/datasets/{name}` |
| 400 | `name` is missing or invalid, or `inferences` is not `copy` or `drop`. | `{"error": "invalid dataset name '…'"}` |
| 403 | The server runs with `--read-only`. | `{"error": "server is read-only"}` |
| 404 | The source dataset does not exist. | |
| 409 | `name` is registered, is reserved by a running clone, or `<data>/databases/{name}` already exists on disk. | `{"error": "dataset /x already exists"}`, `"… is being created by task 12"`, or `"directory databases/x exists but is not a registered dataset; remove it first"` |

While the task runs, `GET /$/datasets/{name}` returns 404 and `POST /$/datasets` with
that name returns 409. A successful task ends `done` with the message
`"cloned /prod at version 812 (1048576 quads) into /prod-sandbox"`. The new dataset is
then registered and persisted in `config.json`. A `failed` task leaves no directory and
no registry entry, and releases the name.

`Task` gains an optional `target: string`, and the TS type `TaskKind` gains `"clone"`.

### 2.4 Rust API

```rust
// crates/sparkles-core/src/store.rs
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

`clone_to` writes only the data files. The caller writes `origin.json` and
`reasoning.json`. That caller is the server or CLI layer, which already owns
`reasoning.json`.

### 2.5 UI

The dataset page header gets a **Clone** button next to Query and Delete. The button is
disabled when the server is read-only. The dialog asks for:

- the new name, pre-filled with `{ds}-sandbox` and checked with the dataset-name
  validation that `DatasetDialogs.svelte` already uses;
- an "Include inferences" checkbox. It is checked by default and shown only when the
  source has reasoning.

On submit, the page tracks the task in the task list, as it does for compaction or
backup. When the task is `done`, a toast says "Cloned into /x" and has an **Open** link to
`/datasets/x`. A 409 shows its message inline under the name field. The new dataset's
page shows a short line such as "cloned from /prod at version 812, 2026-09-30 12:00".
For that, `DatasetInfo` gets an optional `origin` field, read from `origin.json`.

## 3. Standards basis

- **RDF 1.1 / 1.2 Concepts.** Blank nodes are local to a graph or dataset. A copy that
  keeps the same blank-node identities, rather than an isomorphic copy, preserves the
  most. It also matches Sparkles' promise that `_:b<hex>` labels round-trip through the
  protocol (README decisions table). Triple terms are terms, and the clone copies them as
  terms.
- **RDF Dataset Canonicalization (RDFC-1.0).** The clone uses it only as a *test
  oracle*: the canonical N-Quads of the source and the clone must be equal. Sparkles
  already enables `oxrdf`'s `rdfc-10` feature.
- **Fuseki admin protocol conventions.** These are `/$/datasets`, a `Task` for long
  operations, and `202`. Sparkles already follows them for compaction and backup.

## 4. Semantics

### 4.1 Consistency

The clone contains exactly the quads of one `Snapshot`. The capture holds the source
writer lock just long enough to read `(snapshot(), next_bnode)` together, so no commit
can land between the two reads. It then releases the lock. Source updates continue
during the clone and are not included in it. The report and `origin.json` carry the
snapshot's `version` and generation. Once CI exists, they also carry `forkedFrom.seq`
(`Snapshot::commit`).

Concurrent operations on the source are safe:

- **Compaction and bulk commits** switch `CURRENT` and unlink the old generation. The
  captured `Arc<Generation>` keeps its mmaps valid, because Unix keeps an unlinked
  file's data while it is mapped. The store already relies on this for readers.
- **Deleting the source dataset** during a clone is the same case on Unix. On Windows
  the delete fails while mmaps are open. That is acceptable, and the delete error is
  reported.
- **Cloning a clone** is allowed.

### 4.2 Source untouched

`clone_to` never writes under the source root. The CLI has one caveat. `Store::open`
runs its normal crash recovery, which truncates a torn WAL tail and creates `wal.log`
if it is missing. That does not change any data.

### 4.3 Atomic creation and cleanup

- The build happens in a hidden sibling directory:
  - server: `<data>/databases/.clone-{name}-{task}`;
  - CLI: `{DST}.clone-tmp-{pid}`.

  The server's name cannot collide with a dataset, because `valid_name` rejects a
  leading `.`.
- When the build finishes, `Builder::finish` syncs the files and `sync_dir` syncs the
  tree. The directory is then renamed to the final path, and the parent directory is
  synced. On any error, cancellation or panic, a drop guard removes the temporary
  directory.
- **Server startup** removes leftover `<data>/databases/.clone-*` directories. They are
  never registered, so removing them is always safe.
- **Registration** happens after the rename, under `AppState::manage`. The server opens
  the store, inserts it and calls `save_registry_locked`. If that fails, it removes the
  renamed directory and the task fails.
- **A crash between the rename and the registry save** leaves a directory that exists
  but is not registered. A later clone or create with that name returns 409 with the
  "exists but is not a registered dataset" message. Nothing is adopted silently. See
  open question 2.

### 4.4 Reasoning status

- **With `inferences=copy`:**
  - The inferred graph is copied.
  - `reasoning.json` is written into the destination.
  - The freshness position ([C08](C08-inference-freshness.md)) is rebased. If the
    source's inferences were fresh at the captured snapshot, the clone's status records
    `commit` as the clone's initial position (root commit `0` under CI) and `datasetId`
    as the clone's new id, so the clone reads as fresh.
  - If the source's status was stale, the clone's status records
    `"inheritedStale": true`. C08 reports that as `stale: true`,
    `commitsSince: null` and `staleReason: "inherited from source at clone time"`.
  - If the source's status was unknown, the clone's stays unknown and has no `commit`.

  Until C08 lands, `profile`, `inferred` and `at` are copied unchanged.
- **With `inferences=drop`**, the clone leaves out the inferred graph and writes no
  `reasoning.json`.
- **If the source has no `reasoning.json`**, `copy` still copies an
  `urn:x-sparkles:inferred` graph as ordinary data. The clone mirrors the source
  exactly.

### 4.5 Limits and configuration

- **Destination.** The destination is always persistent and lives in the server's data
  directory. Phase 1 has no `type=mem` destination and answers `400 "clone into an
  in-memory dataset is not supported yet"`.
- **Sources.** Any dataset can be a source: persistent, attached with `--loc`, or
  `--mem`.
- **Disk.** A clone needs about the source's compacted size, plus the builder's
  temporary sort files (`BuildOptions::sort_mem_quads` spills). Phase 1 has no
  preflight check. An out-of-space error fails the task, which then cleans up.
- **Concurrency.** Clones run as tasks in the existing task thread model. Phase 1 does
  not limit concurrent clones (open question 3).
- **Cancellation.** When this was written, tasks could not be cancelled. `DELETE /$/tasks/{id}` shipped later (see the Outcome).
  The `cancel` flag exists for the CLI's Ctrl-C handler and for future task
  cancellation.

## 5. Design sketch

**`crates/sparkles-core/src/store.rs`**

1. **Refactor.** `rebuild_locked` contains a loop that pushes every quad of a snapshot
   into a `Builder` encoder. It translates `Vocab` and `Delta` ids to keys (`Slot::Key`)
   and passes inline and blank-node ids through (`Slot::Id`). Extract that loop as
   `fn write_snapshot(builder: &Builder, snap: &Snapshot, skip_graph: impl Fn(u64) -> bool,
   extra: &[[Id;4]]) -> Result<()>`, and call it from both `rebuild_locked` and
   `clone_to`.

   Passing blank-node ids through preserves `_:b<hex>` identity. Triple-term keys
   already embed blank-node payloads (`id::write_term_key_with`), so copying the key
   bytes preserves those payloads as well.
2. **`clone_to(dir)`** builds directly into `dir`, which must be absent or empty. The
   caller creates `dir` as the hidden temporary sibling from §4.3, adds `origin.json`
   and `reasoning.json`, and does the final rename. `clone_to` does the following:
   - capture the state with
     `let w = self.writer.lock(); let snap = self.snapshot(); let nb = w.next_bnode; drop(w);`
   - build `dir/gen-0001` with `BuildOptions { first_bnode: nb, ..self.opts.build }`;
   - call `builder.add_prefixes(self.prefixes())`, then `finish()`;
   - create an empty `gen-0001/wal.log`;
   - `write_atomic(dir/CURRENT, "gen-0001")`, then write `dir/prefixes.json`;
   - `sync_dir(dir)`.

   A drop guard in `clone_to` empties `dir` on every early return. If anything fails
   later, the caller's guard removes the temporary directory itself. The clone does not
   need to create `delta.vocab`, because `DeltaVocab::open` treats a missing file as
   empty.
3. **In-memory sources** work without changes. `write_snapshot` reads through
   `Snapshot::key` and `scan`, whatever the backing store is.

**`crates/sparkles-server/src/state.rs`**

- Add `reserved: Mutex<BTreeMap<String, String /* task id */>>` and
  `fn reserve(&self, name) -> Result<Reservation>`. `Reservation` is an RAII guard that
  releases the name when dropped. `reserve` takes `manage` and fails when:
  - the name is registered;
  - the name is reserved;
  - `databases/{name}` exists.
- `create` and `attach` also check `reserved`.
- Add `fn adopt(&self, name, dir, reservation) -> Result<Arc<Dataset>>`. Under `manage`
  it calls `open_dataset(name, Persistent, None)`, inserts the dataset, saves the
  registry and drops the reservation.
- `AppState::new` removes `databases/.clone-*`.
- `Task` gains `target: Option<String>`.

**`crates/sparkles-server/src/http.rs`**

- Add the route `.route("/$/datasets/{ds}/clone", post(clone_dataset))`.
- The handler checks the parameters and calls `st.reserve(name)`, so the 409s come back
  synchronously. It then runs `start_task("clone", ds, …)`. The task:
  - calls `ds.store.clone_to(tmp, …)`, with the inferred graph in `exclude_graphs` for
    `drop`;
  - writes `origin.json`, and for `copy` also `reasoning.json`, into `tmp` before the
    rename;
  - renames `tmp` to `databases/{name}` and calls `st.adopt(…)`.
- `dataset_info` adds `origin` when `origin.json` exists.

**`crates/sparkles-server/src/main.rs`** gets `Cmd::Clone { loc, to, inferences }`. It
checks for `CURRENT`, opens the source and runs `clone_to(tmp)`. It then writes
`origin.json` and `reasoning.json` with `state::read_reasoning_file` and
`write_reasoning_file`, and renames the directory.

**Persistence format.** The destination is an ordinary Sparkles database with a
`FORMAT_VERSION` 2 generation. `origin.json` is new, versioned (`originFormat: 1`),
optional and informational.

**Crash safety.** A temporary directory is either removed on error or swept at startup.
The rename is the commit point for the files. The registry save is the commit point for
the server.

### Why Phase 1 rebuilds instead of copying files

There are three ways to produce the destination:

- **(a) `dump` → N-Quads → `load`.** This is simple, but it relabels every blank node,
  because the bulk loader scopes labels per source document and assigns fresh ids. It
  also serializes and parses every term and needs a temporary text file. The result
  matches the source only up to isomorphism.
- **(b) Rebuild from the snapshot with the builder.** This is the chosen approach. It
  makes one ordered scan and parses nothing. It preserves blank-node ids, compacts the
  delta away and handles in-memory sources. It supports `inferences=drop` by skipping a
  graph. It reuses the proven compaction path, so it costs the same as `sparkles
  compact` on the source: an external sort per permutation.
- **(c) Copy or link the immutable generation files** when the captured delta is empty.
  With hard links or reflinks this is nearly instant. A plain copy is O(bytes). It
  applies only when the delta is empty and `inferences=copy` is chosen. Links also need
  the source and destination on the same filesystem. It needs care in three places:
  - `delta.vocab` and `wal.log` in the generation directory are mutable, so they must be
    recreated, not linked.
  - `meta.next_bnode` may be lower than the writer's counter. That is safe only because
    an empty delta references no newer blank nodes.
  - With hard links, a future in-place repair tool would change both datasets.

Phase 1 ships only (b). It is one code path, it handles every case, and it adds no new
invariants. (c) is a Phase 2 optimization behind the same API. It uses `FICLONE`
reflinks and falls back to plain copies. It uses hard links only with an explicit
`--link` flag. Tests check that it produces the same result as (b).

## 6. Phasing

**Phase 1 (MVP, about one day):**

- the `write_snapshot` refactor and `Store::clone_to`;
- `sparkles clone`;
- `POST /$/datasets/{ds}/clone` with reservation, adoption and the startup sweep;
- `origin.json` and `inferences=copy|drop`;
- the `Task.target` field;
- the UI's Clone dialog and the "cloned from" line;
- tests for §7 A–G.

**Phase 2:**

- the file-copy and reflink fast path for an empty delta;
- a disk-space preflight that checks `statvfs` against the source's `disk_bytes()` × 1.2
  and answers 507 on a shortfall;
- in-memory destinations (`type=mem`);
- task cancellation;
- a limit on concurrent clones;
- a `--graph` filter for partial clones.

**Phase 3:**

- cloning at a past commit (`?at=` from [F06](F06-snapshots-and-point-in-time.md));
- cloning across servers through archives ([F05](F05-snapshot-repositories.md));
- branch and merge semantics.

## 7. Acceptance examples

**A. Round-trip fidelity** (library test). Source TriG:

```trig
@prefix ex: <http://ex.org/> .
ex:s ex:p _:x . _:x ex:q "v"@en--ltr .
GRAPH ex:g { ex:s ex:r <<( _:x ex:p 1 )>> }
GRAPH _:gb { _:x ex:in _:gb }
```

Bulk-load it, then run `INSERT DATA { ex:t ex:p ex:u }` so that one quad is in the
delta. Run `clone_to` and open the destination. Expected:

- `dump_nquads(src)` and `dump_nquads(dst)`, sorted, are byte-identical. Because the
  `_:b<hex>` labels are identical, their RDFC-1.0 canonical forms are equal too.
- `SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } }` returns the same count on both.
- `ASK { GRAPH ?g { ?s ?p <<( ?x ?q ?y )>> } }` is true on the destination.
- The destination has an empty delta and exactly one generation, `gen-0001`.

**B. Stable blank-node labels.** Suppose
`SELECT ?o { <http://ex.org/s> <http://ex.org/p> ?o }` returns `_:b1f` on the source. It
returns the same label on the clone. Because the counter is carried over,
`INSERT DATA { <http://ex.org/n> <http://ex.org/p> [] }` on the clone then creates a
blank node with a label the source does not use.

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

- A clone into an existing dataset name returns 409.
- Of two back-to-back clone requests with the same `name`, the second gets 409 `… is
  being created by task N`.
- `POST /$/datasets` with `dbName` set to the reserved name during the clone returns
  409.
- After `mkdir <data>/databases/ghost`, a clone with `name=ghost` returns 409 "exists
  but is not a registered dataset".
- `name=.hidden` or `name=a/b` returns 400.

**E. Failure cleanup.** Inject a failure after the build and before the rename, with a
test hook or a read-only temporary parent. The task is `failed`, neither
`databases/.clone-*` nor `databases/{name}` remains, and the name can be reused at once.
Separately, create `databases/.clone-x-9` by hand and restart the server. The directory
is gone and nothing is registered.

**F. Consistency under concurrent writes.** Start a clone of a 1M-quad dataset. While it
runs, POST 100 updates to the source, each inserting one quad. The clone's quad count
equals the source's count at the captured version (`origin.source.quads`) and is at
most the final source count. The source ends with all 100 inserts.

**G. Inferences:**

- On a source with rdfs reasoning, `inferences=copy` gives a clone whose DatasetInfo has
  `reasoning.profile "rdfs"`, and a query for an inferred type returns rows.
- With `inferences=drop`, `reasoning` is null, `GRAPH <urn:x-sparkles:inferred> {…}` is
  empty, and the quad count equals the source's count minus its inferred triples.

**H. CLI:**

- `sparkles clone --loc db --to db2` exits 0, and `sparkles stats --loc db2` shows the
  same quad count.
- Running it again exits 1 with `DST exists and is not empty`.
- `sparkles clone --loc nope --to x` exits 1 with `is not a Sparkles database`, and
  creates neither `nope` nor `x`.

## 8. Rejected alternatives

- **Dump and load as the Phase 1 mechanism.** It relabels blank nodes and is slower
  (§5). It remains the documented manual route for moving data between machines.
- **File copy as the Phase 1 mechanism.** It covers only the empty-delta case and needs
  new invariants about the mutable files in a generation directory (§5).
- **Holding the source writer lock for the whole clone.** This gives the same
  consistency, but it blocks source updates for the whole clone. The snapshot is
  already immutable, so the lock buys nothing.
- **Registering the destination first and filling it afterwards.** Clients could query
  a half-built dataset. Reserving the name without registering it avoids that.
- **Copying the WAL and history.** This exposes internal ids and ties the clone to the
  source's generation numbering. Under CI it would also give two lineages the same
  commit history. A fresh id plus `forkedFrom` states the relationship accurately.

## 9. Open questions

1. Should the default be `inferences=drop`, so that sandboxes start without inherited
   inferences and re-run reasoning? The spec defaults to `copy`, so the clone behaves
   exactly like the source.
2. Should startup offer `--adopt-orphans` to register `databases/{name}` directories
   that contain `origin.json` but have no registry entry? The spec leaves orphans alone
   and reports 409.
3. Should the number of concurrent clones be limited to protect disk and CPU? Phase 1
   does not limit it.
4. Should `POST /$/datasets` accept `{"cloneFrom": "prod"}` as an alias? It would suit
   Fuseki-style clients, but it duplicates the route.
5. Should the clone copy the source's per-dataset result-cache and block-cache settings?
   These settings are server-wide today, so the question does not arise yet.

## 10. Sources

- Sparkles repository:
  - [CI-commit-identity.md](CI-commit-identity.md) and [C08-inference-freshness.md](C08-inference-freshness.md),
    the sibling specs, for the dataset id, root commit, `forkedFrom` and the
    reasoning status fields.
  - `crates/sparkles-core/src/store.rs`: `Store::{open, write, rebuild_locked, dump_nquads,
    backup}`, `bnode_for`/`parse_bnode_label`, `WriteTxn::{intern_scoped, encode_quad}`,
    the WAL format, `write_atomic`/`sync_dir`.
  - `crates/sparkles-core/src/builder.rs`: per-document `LabelScope`, `Slot`,
    `BuildOptions::first_bnode`, `IndexMeta`.
  - `crates/sparkles-core/src/id.rs`: the triple-term key encoding with blank-node payloads.
  - `crates/sparkles-core/src/index.rs` and `vocab.rs`: generation file names.
  - `crates/sparkles-server/src/state.rs`: the registry, the `manage` lock,
    `create`/`attach`/`delete`, `valid_name`, the reasoning file and tasks.
  - `crates/sparkles-server/src/http.rs`: dataset admin handlers and status codes.
  - `crates/sparkles-server/src/main.rs`: the CLI structure, `dump`/`backup`/`infer`.
  - `ui/src/routes/datasets/[name]/+page.svelte` and `ui/src/lib/api.ts`.
  - `README.md` (the decisions table on blank-node labels and triple terms),
    `docs/API.md`, and the project's internal planning notes on the feature
    order.
- W3C RDF 1.1 / RDF 1.2 Concepts, for blank-node scope and triple terms, and RDF Dataset
  Canonicalization (RDFC-1.0) as a test oracle. These are cited from working knowledge
  and were not re-fetched.
- The Linux `ioctl_ficlone(2)` and `copy_file_range(2)` man pages, for the Phase 2
  reflink fallback. Cited from working knowledge.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30, after durable commit identity. The clone
was therefore a new lineage from the start, with a new dataset id and root commit `0`.
The source's id and the copied commit are recorded as `forkedFrom` in `dataset.json`,
which `Store::forked_from` reads, and in `origin.json`. There was no "before CI" stage
with `forkedFrom: null`.

- `Store::clone_to` builds the copy through the rebuild path that compaction uses, now
  factored into a shared `write_snapshot`. It copies every quad of every graph, triple
  terms, prefixes and identical blank-node ids, and carries over the blank-node
  counter. It holds the writer lock only to capture the snapshot.
- `POST /$/datasets/{ds}/clone?name=NEW[&inferences=copy|drop]` runs as a task. The
  name is reserved up front. The server returns `409` if the name is registered, is
  being created by another task, or names an unregistered directory that exists. The
  copy is built in `databases/.clone-NAME-TASK` and renamed into place. A failure leaves
  nothing behind, and startup sweeps any leftovers.
- With `inferences=copy`, the reasoning status is rebased as §4.4 describes. Fresh
  inferences stay fresh at the clone's root, and stale ones are inherited as stale
  ([C08](C08-inference-freshness.md)).
- `sparkles clone --loc SRC --to DST [--inferences copy|drop]` works offline. The
  dataset page gained a Clone action, a "cloned from" line and an Open link.

**Follow-ups.**
- At first, a clone of a dataset with full-text search lost its index configuration,
  so `text:query` silently stopped working on the clone. The clone now copies the text
  configuration and rebuilds the index on first open. The §2.1 table had left this to
  the text spec.
- When text indexes began to recover in the background at open, a new clone answered
  text queries with `503` until that recovery finished. The clone now builds its text
  index before it completes, under the clone's cancel flag and deadline, so the clone
  serves text queries as soon as it exists.
- Later work made clone tasks cancellable until the copy is in place
  (`DELETE /$/tasks/{id}`).
- A clone that would leave less than `--min-free-disk-mb` free is refused with `507`
  and leaves nothing behind. This serves the purpose of the planned disk-space
  preflight.

**Phase 2.** The rest of Phase 2 landed on 2026-10-02.

- **The file path.** A clone shares the source generation's index files instead of
  rebuilding them when three things hold. The captured snapshot is the generation's base,
  so the delta is empty and the source has had no change since its last compaction or
  bulk load. The clone is of the head. Every graph that has quads is kept, so
  `inferences=drop` on a source without inferences still qualifies. Any other clone is
  rebuilt as Phase 1 did, and the report names the reason.
- **How files are shared.** `mode=auto` (the default) clones each file with the `FICLONE`
  ioctl, which btrfs, XFS and ZFS with block cloning support, and copies it otherwise.
  The copy uses `copy_file_range` on Linux, which some file systems turn into block
  clones on their own. `mode=link` makes hard links first and falls back the same way
  when the source is on another file system. `mode=rebuild` always rebuilds. The spec
  had planned a `--link` flag. A `mode` parameter replaced it, so that the HTTP endpoint
  has the same choice. The report, the task's `detail` and `origin.json` say which
  method was used.
- **Which files.** The clone shares the immutable files of the generation directory:
  the permutation indexes, the vocabulary and `stats.json`. It writes its own
  `commit.json`, an empty `wal.log` and its own `meta.json`, which is the source's with
  the captured prefixes and blank-node counter. The counter can be ahead of the base's
  when blank nodes were made and deleted again since the base was built, and the clone
  never hands those ids out again. Subdirectories hold derived indexes (spatial,
  vector), which the clone rebuilds when it opens, as before.
- **Independence.** Index files are never written once their generation is published,
  and every rewrite of a small file goes through a rename. A reflinked or copied file is
  therefore independent of the source, and a hard-linked one is never written through
  either name. Tests write to and compact both sides of a linked clone, then delete the
  source's directory, and the clone still checks clean.
- **Compaction during a clone.** The capture takes a lease on the source generation
  under the writer lock, the same lease a backup takes. A compaction (C13) or bulk commit
  that switches the source's generation while the files are copied keeps the old
  directory until the lease is dropped, and then collects it. The history status shows
  the lease as `clone:<name>`. Tests run a bulk load and a compaction at that point.
- **Crash safety.** The copy is built in the same temporary directory as a rebuilt one,
  and `CURRENT` is written last, so a half-made clone is never a database. The server
  removes `databases/.clone-*` at startup as before. `sparkles clone` now removes the
  `DST.clone-tmp-PID` directories of runs whose process is gone.
- **History.** A clone carries no history. It is a new lineage, and the source's commit
  records belong to the source's dataset id. Named snapshots, retention settings, older
  generations retained by F06 and the commit catalog stay with the source. A clone that
  should start from a past state uses `at=`, which always rebuilds.
- **In-memory destinations.** `type=mem` (and `Store::clone_to_memory`) makes the clone
  a new in-memory dataset. Its base is a generation in a temporary directory, as an
  in-memory dataset's is after a bulk load, so the file path applies to it too. Like
  every in-memory dataset it is registered again after a restart, empty. Its origin is
  kept in memory, and the full-text, spatial and vector indexes are configured and
  built as on the source. Write-time validation and stored queries live in files and do
  not carry over. In-memory datasets could already be cloned into persistent ones.
- **Partial clones.** `graph=` (repeatable, or a JSON `graphs` array) and
  `sparkles clone --graph` copy only the graphs named. Names and patterns follow graph
  grants, through the access module's `GraphRule`. Patterns never match the inferred
  graph, and graphs named by blank nodes are never selected. The rebuild reads only the
  selected graphs from the GSPO index, so a small graph of a large dataset copies
  quickly. A partial clone keeps the reasoning status only when it names the inferred
  graph, and then marks it stale, because the inferences may come from graphs it left
  out.
- **Concurrency.** `serve --max-clones` (default 2) limits the clones that run at once.
  Clones were already counted in the `--max-tasks` slots. A clone over its limit waits
  as `queued`, and a freed slot goes to the first waiting task that may run, so waiting
  clones never hold back other tasks. The NixOS module sets both limits with its
  `maxClones` and `maxTasks` options.

**Measurements.** These are clone times with the release build through `sparkles clone`,
on datasets loaded from the benchmark data (`scripts/gen-data.py`) with `sparkles load`.
Each figure is the median of three runs, with the range in parentheses. The machine was
heavily loaded by other work, with a load average between 45 and 70 on 16 cores, so the
rebuild times vary by more than a factor of two. The file system was ext4, which has no
reflinks, so `auto` copied the files. The rebuild is the Phase 1 path, unchanged, and is
the time before this work.

| Dataset | On disk | Rebuild | Copy (`auto`) | Hard links (`link`) |
|---|---|---|---|---|
| 1.05M triples | 28 MB | 3.69 s (2.89–3.82) | 0.34 s (0.32–0.40) | 0.04 s (0.04–0.13) |
| 10.5M triples | 286 MB | 38.5 s (16.7–43.9) | 1.22 s (0.50–6.87) | 0.08 s (0.07–0.09) |

The rebuild's peak RSS was 1.44 GB at 10.5M triples, because of its sort buffers. The
copy's and the link's were 35 MB.

**Open questions.** The defaults stayed as specified. `inferences=copy` is the default,
orphaned directories are reported with `409` rather than adopted, and `POST /$/datasets`
has no `cloneFrom` alias. Question 3 is answered by `--max-clones`, which defaults to 2.

**Not built.** Cloning across servers through archives is not built. Branching and
merging shipped separately as [F09](F09-branches-and-merges.md).

**UI.** The dataset page's Clone dialog offers the type (persistent by default), all
graphs or a chosen few (the source's graphs as checkboxes, plus typed IRIs and patterns),
and for a persistent clone the index mode (`auto` by default). A partial clone that keeps
inferences names the inferred graph for the user. When the task is done, the task list,
the toast and the "Cloned into" banner say how the index was made, from the task's
detail. The clone's own page shows the selection and the method from `origin.json`.
