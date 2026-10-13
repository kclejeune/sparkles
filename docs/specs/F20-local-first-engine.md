# F20: Local-first engine in WebAssembly

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 is a storage layer without memory-mapped files or
> temporary directories, the engine compiled to `wasm32-unknown-unknown` with an
> in-memory store, and a TypeScript package that runs it in a dedicated worker. Phase 2
> adds persistence in the Origin Private File System, local branches, and sync with a
> server through RDF Patch and F09 merges, with conflict resolution, partial replicas by
> graph and offline writes. Phase 3 adds the Sparkles UI's local mode, several tabs on one
> replica, and the variant for Node.js, Deno and edge runtimes.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

This design was written from the WebAssembly core specification, the MDN and WHATWG
documentation of the File System Standard (the Origin Private File System and
`FileSystemSyncAccessHandle`), IndexedDB, the Storage Standard (`navigator.storage`
estimates and persistence), the Web Locks API and `BroadcastChannel`, the wasm-bindgen
guide, M. Kleppmann, A. Wiggins, P. van Hardenberg and M. McGranaghan, "Local-first
software: You own your data, in spite of the cloud" (Onward! 2019), the RDF Patch
documentation, and the Sparkles code and specs. §17 lists the sources. It builds on the
browser section of [P05 §12](P05-node-bindings.md#12-in-the-browser), which listed what a
browser build would take, applying RDF Patch ([F10](F10-replication.md) Phase 1), branches
and merges ([F09](F09-branches-and-merges.md)), the change feed and diffs
([F06](F06-snapshots-and-point-in-time.md)), commit identity
([CI](CI-commit-identity.md)), access control ([C09](C09-dataset-access-control.md),
[C12](C12-graph-access-control.md)) and the compression codecs
([X01](X01-compression-codecs.md)).

## 1. Summary, goals, non-goals

An application that keeps its data in a Sparkles dataset reaches it over HTTP today. It
stops working without a network, every read pays a round trip, and a user cannot keep
their own copy. The local-first approach that Kleppmann et al. describe keeps the
primary copy of a user's working data on the user's device, makes reads and writes local
and instant, and syncs with a server when one is reachable.

Sparkles has most of what this needs on the server: commits with stable identity, diffs
and a change feed in RDF Patch, an endpoint that applies RDF Patch with `H prev` as a
precondition, and branches with three-way merges and conflict reports. It lacks an
engine that runs where the application runs. This spec compiles the engine to
WebAssembly, gives it storage in the browser, and defines the sync protocol between a
local replica and a server on top of the existing endpoints.

**Goals**

1. The engine compiles to `wasm32-unknown-unknown` with SPARQL 1.1 and 1.2 queries and
   updates, the RDF parsers and serializers, GeoSPARQL, the reasoner and SHACL and ShEx
   validation, and passes the W3C SPARQL suites in a headless browser.
2. A replica of a dataset, or of some of its graphs, persists in the Origin Private File
   System across reloads, with commits that survive a tab crash.
3. Writes made offline are commits on a local branch. When the server is reachable they
   are pushed as RDF Patch, and when the server has moved they are merged with the
   server's changes by F09's three-way merge, locally, with conflicts shown to the user.
4. Sync is correct under lost responses, retries, reloads and several open tabs: no
   commit is applied twice, and none is lost.
5. Size targets: the module is at most 8 MB uncompressed and 3 MB with brotli, as P05
   §12.4 estimated, and a replica of 1 million triples opens in under one second from
   the Origin Private File System on a current laptop.
6. The same module runs in Node.js, Deno and edge runtimes that have WebAssembly, with
   storage supplied by the host.

**Non-goals**

- Peer-to-peer sync between devices without a server. Every replica syncs with one
  server branch. CRDT-based merging is rejected in §15.
- Full-text search in the browser. Tantivy relies on mapped files and threads, so the
  `text` feature stays off, and `text:query` fails with `UnsupportedError`.
- SERVICE and `LOAD <http…>` in the engine. The evaluator is synchronous and the
  browser's `fetch` is not (P05 §12.2).
- Replicas of datasets larger than the WebAssembly heap allows (§8).
- Encrypting the replica at rest. Browsers' storage is per origin, and open question 4
  covers a passphrase-derived key.

## 2. User-visible behaviour

### 2.1 The package

A new package, `@sparkles-rdf/local`, holds the WebAssembly module, a worker script and a
TypeScript layer. Its API follows the engine package of P05 §3, so that code written
against `@sparkles-rdf/engine` runs against it, and it shares the term classes, errors
and the `SparqlDataset` interface of `@sparkles-rdf/common`.

```ts
import { openLocal } from '@sparkles-rdf/local';

const db = await openLocal('notes', {
  storage: 'opfs',                                  // or 'memory'
  remote: { url: 'https://kg.example.org/notes', branch: 'main',
            graphs: ['https://kg.example.org/g/alice'],   // optional partial replica
            auth: () => getAccessToken() },               // bearer token or cookie session
});

for await (const row of db.query('SELECT ?s ?title WHERE { ?s ex:title ?title }')) { … }
await db.update('INSERT DATA { ex:n1 ex:title "Draft" }');   // a local commit, at once
const status = await db.sync();     // { pulled, pushed, merged, conflicts, head, remoteHead }
db.on('sync', s => …); db.on('conflict', report => …);
```

Every method returns a promise or an async iterator, because the engine runs in a
dedicated worker (P05 §12.4 step 4). The page never blocks on a query.

### 2.2 Local branches

A replica has three branches in the F09 sense.

| Branch | Holds |
|---|---|
| `remote` | The server branch's state as of the last pull. It is written only by sync. |
| `main` | The user's working state: `remote` plus local commits not yet pushed. Queries and writes go here by default. |
| other names | Branches the application creates with `db.branches.create(…)`, which never sync unless merged into `main`. |

The local `main` is a branch of `remote`, so `db.branches.get('main')` reports `ahead`
(local commits not pushed) and `behind` (pulled commits not yet merged), as F09's
`Branch` does.

### 2.3 Sync states

`db.status()` returns `offline`, `syncing`, `synced`, `ahead`, `conflict`, `rejected` or
`error`, with the counts above, the last sync time, and for `rejected` the server's
answer to the push (§6.6). An application shows it, and the UI does (§9).

## 3. The engine on wasm32

### 3.1 What blocks the build today

The browser build of the formatter, `crates/sparkles-fmt-wasm`, shows that the
formatter crate compiles for `wasm32-unknown-unknown` with wasm-bindgen, without its
`parallel` feature, and with getrandom's `wasm_js` backend. `mise run ui:wasm` builds it
into `ui/src/lib/wasm`, and the UI falls back to `POST /$/format` when the module does
not load. The engine is a larger case. These dependencies of `sparkles-core` stand in
the way, checked against its `Cargo.toml`:

| Dependency | Where | Change |
|---|---|---|
| `memmap2` | `index.rs` (`PermIndex` holds `Option<Arc<Mmap>>`), `vocab.rs` (`Bytes::Map`), `io.rs`, `geo/persist.rs`, `vector/persist.rs`, `text/lazydir.rs` | §3.2. `vocab.rs` already has a `Bytes::Vec` variant that the storage layer generalizes. |
| `tempfile` | The in-memory store builds generations in a temporary directory (`Generation::_tmp` in `store.rs`, and the bulk path), and compaction writes into a directory | §3.2. |
| `rayon` | Parallel sorts in the builder and parallel operators in 24 modules | A `parallel` feature, on by default, as `sparkles-fmt` has. Without it the loops run sequentially. |
| `stacker` | `sparql/depth.rs` grows the stack for deep queries | Off on wasm. The depth limits that `depth.rs` enforces apply with a fixed stack, set by the linker's stack size. |
| `reqwest` with its `blocking` feature, and `tokio` with `rt` | SERVICE, remote `LOAD`, the outbound policy's DNS lookups | A `remote` feature, off on wasm. SERVICE and remote LOAD fail with `Error::Unsupported`. `tokio`'s `sync` parts used by the commit watch channel stay. |
| `liblzma` (static, a C build) | xz inputs in `codec.rs` | An `xz` feature, off on wasm. P05 §12.2 did not list it. |
| `zstd` (optional, a C build) | zstd inputs and dumps | Off on wasm. The `zstd-decode-pure` feature that X01 designed, with `ruzstd`, is not built yet. F20 Phase 1 builds it so that a replica can read zstd dumps from a server. |
| `tantivy` (optional) | Full-text search | Off on wasm. |
| `std::time::{Instant, SystemTime}` | Commit times, deadlines and budgets, in about 45 modules | `std` panics on these calls on `wasm32-unknown-unknown`. A `clock` module with `now()` and `monotonic()` replaces direct calls, backed by `Date.now()` and `performance.now()` through `js-sys` on wasm. A lint rule (`clippy::disallowed_methods`) keeps new direct calls out. |
| `libc` | `statvfs` for free-disk checks | Already limited to Unix. The free-disk reserve uses the storage estimate on wasm (§8). |
| `getrandom` (through `rand` and `uuid`) | Blank node labels, dataset and branch ids | The `wasm_js` backend, as in `sparkles-fmt-wasm`. |

`bzip2` 0.6 builds on `libbz2-rs-sys`, which is pure Rust, and `flate2`, `lz4_flex`,
`brotli`, the `ox*` parsers, `spargebra`, `icu_normalizer`, the geospatial crates, the
reasoner and the validators are pure Rust. CI gains a job that builds `sparkles-core`
with `--no-default-features --features wasm` for `wasm32-unknown-unknown`, so a new
dependency that cannot build there fails the change that adds it.

### 3.2 A storage layer

The engine reads generations through mapped files and writes them as files. A new trait
separates the two concerns from the file system.

```rust
// crates/sparkles-core/src/storage/mod.rs
pub trait Storage: Send + Sync {
    fn open_read(&self, path: &RelPath) -> Result<Arc<dyn ReadAt>>;  // immutable files
    fn create(&self, path: &RelPath) -> Result<Box<dyn WriteFile>>;   // write once, then sync
    fn open_append(&self, path: &RelPath) -> Result<Box<dyn AppendFile>>; // the WAL
    fn rename(&self, from: &RelPath, to: &RelPath) -> Result<()>;    // atomic within the store
    fn remove(&self, path: &RelPath) -> Result<()>;
    fn list(&self, dir: &RelPath) -> Result<Vec<String>>;
    fn available_bytes(&self) -> Option<u64>;
}
pub trait ReadAt: Send + Sync { fn len(&self) -> u64; fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<()>;
                                fn as_slice(&self) -> Option<&[u8]>; }
pub trait AppendFile: Send { fn append(&mut self, b: &[u8]) -> Result<()>; fn sync(&mut self) -> Result<()>; }
```

| Implementation | Where | `as_slice` |
|---|---|---|
| `FsStorage` | Native targets, today's behaviour | the mapped file, so the current read path keeps its speed |
| `MemStorage` | In-memory stores on every target | the heap buffer |
| `OpfsStorage` | wasm, in a dedicated worker | none for large files: blocks are read through `read_at` into the block cache |

Index blocks are already read through `PermIndex` and cached in `BlockCache` (`index.rs`),
so the reader needs only `read_at` where it now slices the map. Vocabularies and the
vector and spatial index files are read the same way. `MemStorage` replaces the
temporary directory of in-memory stores on every target, which P05 §12.4 noted is useful
beyond the browser: an in-memory dataset then never touches the disk. The builder's
external sort keeps its runs in the storage too, so on `MemStorage` a bulk load is
bounded by memory, and on `OpfsStorage` it spills to the Origin Private File System.

### 3.3 Threads and the worker

The engine runs single-threaded in one dedicated worker. A build with wasm threads
needs `SharedArrayBuffer`, and so cross-origin isolation headers on every page that uses
it (P05 §12.4 step 3). That is open question 1, to decide by measurement once Phase 1
exists. The worker owns the store. The TypeScript layer sends requests by `postMessage`
and receives result batches as transferable `ArrayBuffer`s in the batch encoding of the
Node package (P05 §5.2), so the page decodes terms lazily, and a slow consumer stops the
worker from producing more batches.

A long query yields between batches, so cancellation (`AbortSignal`) and the query's
deadline are checked as they are in the Node package. A query cannot be interrupted
inside one batch's computation. The batch size bounds that delay.

## 4. Storage in the browser

### 4.1 The Origin Private File System

The Origin Private File System gives each origin a private directory tree.
`FileSystemSyncAccessHandle`, available in dedicated workers, reads and writes a file
synchronously at an offset and has `flush()`, which is the equivalent of `fsync`. The
synchronous engine needs exactly that. The replica's directory has the layout of a
dataset directory, with `branches/` for the local branches of F09, and a `sync.json`
file (§6.2).

A sync access handle is exclusive: while one worker holds it, no other context can open
the file. §7 builds the multi-tab design on that.

### 4.2 IndexedDB

IndexedDB is asynchronous only, so it is not used for the store. It holds the small
records that the page needs before the worker starts: the list of replicas on the
origin, each one's last known status, and the pending-sync flag for a service worker's
Background Sync (open question 3). Where the Origin Private File System is not
available, `openLocal` with `storage: 'opfs'` fails with `UnsupportedError`, and the
application can choose `memory` and persist with `dump()` into IndexedDB itself, which
is the route P05 §12.4 step 6 described as the simplest first version.

### 4.3 Durability

A commit on `OpfsStorage` appends its WAL record and calls `flush()` on the WAL's handle
before the write resolves, as `wal.rs` calls `sync_data` on native targets. Compaction
writes a new generation's files, flushes them, and switches by an atomic rename of the
generation pointer, as on native targets. A tab closed during a commit leaves a torn WAL
tail that the next open drops, which the existing recovery already handles.

The browser can evict an origin's storage under pressure unless the origin holds
persistent storage. `openLocal` calls `navigator.storage.persist()` when the replica has
unpushed commits, and `db.status()` reports whether persistence was granted, so an
application can warn the user that unsynced work may be evicted.

## 5. Replicas

### 5.1 Creating a replica

`openLocal` with a `remote` and no local data creates the replica.

1. It reads the server branch's head with `GET /$/branches/{ds}/{branch}` and records
   the dataset id, the branch id and the head seq `H`.
2. It downloads the state at `H` with a Graph Store read of the whole dataset,
   `GET /{ds}/data?at=commit:H&branch=B` as N-Quads, compressed with brotli or zstd. The
   read already applies the caller's view (C12) and writes stored blank nodes with their
   stable `_:b<hex>` labels ([API: Blank nodes](../API.md#blank-nodes)). A partial
   replica reads each of its graphs with `graph=`.
3. It bulk-loads the dump into the local `remote` branch and creates `main` from it.
4. It records `H` as the synced commit in `sync.json`.

Blank nodes must keep their identity across replica and server, because later patches
name them. The dump carries the stored labels, but the bulk loader allocates new ids for
every blank node it reads. Phase 2 adds a load option, `keep_bnode_labels`, that gives a
`_:b<hex>` label exactly that id, which the local clones of F09 already do internally
with the stored ids. The cross-server export of F09 §2.11 (`GET /$/export/{ds}`), which
is not built, is not needed for this.

### 5.2 Partial replicas

A replica can hold some graphs of the dataset: the graphs listed in `remote.graphs`,
intersected with the caller's read view. The server already filters by view where it
matters here: `GET /{ds}/changes` builds its pages from the changes of the graphs the
caller may read (`http/changes.rs`), and `/{ds}/diff` filters the net change the same
way (C12 §5.6). Sync adds a `graphs` parameter to `/{ds}/changes` that narrows the view
further, so a replica of two graphs does not receive the changes of every graph the user
can read.

C12b's protections of triples already shape the feed: with protections by predicate and
graph only, each change is filtered by its quad, and a caller whose protections depend on
the data (by class or pattern) gets `403`, because the feed would need the view of every
commit ([C12b](C12b-triple-access-control.md), `store/changes.rs`). A replica must never
receive a triple the user may not read, so sync inherits that rule: such a principal
cannot create a replica, and gets `403` when it tries. The initial Graph Store read is
served from the masked snapshot in every case, so the refusal comes from the feed. Open
question 5 asks whether to lift it.

Writes on a partial replica are limited to its graphs, and the server checks the
caller's write grants again on push.

## 6. Sync

### 6.1 The protocol

Sync uses existing endpoints with three additions (§6.4, §6.5, §10). It has two halves.

**Pull.** `GET /{ds}/changes?after=S&branch=B&format=patch-binary&graphs=…`, where `S`
is the synced commit, returns one RDF Patch per commit after `S`, each with `id` naming
its commit and `prev` its parent. The client applies them in order to the local `remote`
branch with `Dataset::apply_patch`, in one local commit per server commit, and records
each server seq in `sync.json` as it commits. A page that ends early continues from
`next`. With `wait=30` the request long-polls while the page is open, and with
`Accept: text/event-stream` it streams (F06's change feed).

**Push.** When `main` is ahead of `remote`, the client computes the net change of its
unpushed commits as one RDF Patch, with the local diff, and sends it:

```
POST /{ds}/patch?branch=B
Content-Type: application/rdf-patch+thrift
H prev <urn:uuid:<branch id>#commit:S> .
H id <urn:uuid:<replica id>#push:<n>> .
H message "…" .
A … / D … rows
```

`H prev` makes the push a precondition on the server's head: if the server has not moved
since `S`, the patch applies as one commit, and the server's history stays linear.
`Store::own_commit` (`store/patch_apply.rs`) compares the commit IRI's id with the store's
id, which on a branch store is the branch id, so the check already works for pushes to a
branch other than `main`.

### 6.2 State kept by the replica

`sync.json` holds, per replica: the server URL, dataset id, branch name and id, the
synced server seq `S`, the map from local `remote` commits to server seqs, the replica
id (a UUID made at creation), the push counter `n`, and the push in flight, if any, with
its patch id and the local head it covers. It is written with an atomic rename after
each step, so a reload resumes at the step it stopped.

### 6.3 When the server has moved

A push answered `412 prev-mismatch` means the server has commits after `S`.

1. Pull them into `remote` (§6.1).
2. Merge `remote` into `main` locally with F09's three-way merge, base = the old `S`
   state of `remote`, ours = local `main`, theirs = the new `remote` head, with the
   conflict scope `cell` and `onConflict: "fail"` by default. The merge runs in the
   worker with the same code as on the server. In-memory branches of F09 Phase 2 and the
   branch storage on `OpfsStorage` both support it.
3. Without conflicts the merge commits on `main`, and the client pushes again with the
   new `prev`. With conflicts, the status becomes `conflict` and the `conflict` event
   carries F09's conflict report. The application, or the UI's merge page (§9), resolves
   it with F09's `resolutions` (`ours`, `theirs`, `base`, `union`, `objects`) and calls
   `db.resolve(resolutions)`, after which the push repeats.

An application can choose another policy per replica: `onConflict: "theirs"` (server
wins), `"ours"` (local wins), or `"union"`, as F09's merge options define them.

### 6.4 Blank nodes

F10's blank-node rule for patches is: a label in the stored form `b<hex>` names that
stored node when `prev` names this lineage and the dataset has allocated it, and every
other label is local to the request. A replica allocates new blank nodes itself, so its
labels for new nodes would either collide with server nodes or become new, unrelated
nodes on the server, and the replica's next pull would then bring them back as a second
copy.

F09 already partitions blank-node ids by a 16-bit branch ordinal (F09 §4.8). A replica
therefore registers with the server once, at creation, and the server reserves an
ordinal for it, recorded in the dataset's branch table without a branch store
(`POST /$/replicas/{ds}`, §10). The replica allocates new blank nodes in that ordinal's
range. The patch rule gains a third case, the one F10 open question 13 anticipated: a
`b<hex>` label in a range reserved for the pushing replica names a node with exactly
that id, allocated on first use. Blank nodes then keep their identity in both directions.

### 6.5 Retries and idempotence

A push whose response is lost leaves the client unsure whether it applied. Retrying the
same patch would fail `412` if it applied, and the client would then pull its own change
back and merge it with itself, which F09's rule makes a no-op, but at the cost of a
merge commit and a confusing history.

F10's open question 4 proposed idempotent patches by `H id`. F20 adopts it: the server
records the `id` header of an applied patch in the commit record, and a patch whose `id`
is already recorded on the branch answers `200` with the original receipt and
`"duplicate": true`, without applying anything. The record needs only the most recent
ids per replica, kept for the change log's retention. The client sends the same `id` on
every retry of one push, and a new one after a merge.

### 6.6 Rejected pushes

A push can fail for reasons a merge cannot fix: the caller lost write access to a graph
(`403`), write-time validation rejects the result (`422`, with the C10 report), a quota
is full (`507`), or the branch is protected (`403 branch-protected`). The status becomes
`rejected`, the local commits stay on `main`, and the application can move them to a
side branch (`db.branches.create('rejected-1', { from: 'main' })`) and reset `main` to
`remote`. Nothing is dropped without the user's action.

A replica whose server branch was deleted, or whose synced commit is older than the
change log's retention (`410 history-gone`), is re-seeded: the client downloads the
current state into `remote` as in §5.1 and merges `main` into it with the old `remote`
as base, which still exists locally.

### 6.7 Pushing to a review branch

Some deployments do not let devices write to `main`. A replica configured with
`remote.pushTo: "branch"` pushes to a server branch of its own,
`replica-<replica id>`, created on first push, and the server-side merge into `main`
happens through F09's merge endpoint by a reviewer. Pulls still read `main`.

## 7. Several tabs

A sync access handle is exclusive per file, so only one worker can own a replica. Each
tab starts its worker, and the workers elect one owner with the Web Locks API
(`navigator.locks.request('sparkles:<replica>', …)`). The owner opens the store. Other
tabs' workers forward requests to the owner through a `BroadcastChannel` and receive
results the same way, and their pages see no difference except latency. When the
owner's tab closes, its lock is released, the next waiting worker takes the lock and
opens the store, and requests in flight to the old owner fail with a retryable error
that the TypeScript layer retries once. A `SharedWorker` would avoid the election, but
it cannot create sync access handles, which are limited to dedicated workers.

Writes from every tab go through the owner's single writer, as the engine's writer is
single on every target. A tab learns about new commits through a `commit` message on the
channel, which the TypeScript layer turns into the `change` event.

## 8. Limits

| Limit | Value | Notes |
|---|---|---|
| WebAssembly heap | 4 GiB at most, often less | 32-bit memories. The block cache is sized to a quarter of the heap. Memory64 is open question 2. |
| Replica size | about 20 million triples | The vocabulary and the delta live in the heap. The indexes are on the Origin Private File System and are read in blocks. The number will be measured in Phase 2. |
| Storage quota | the origin's quota | `navigator.storage.estimate()` is the free-space check, in place of `statvfs`. A write that would leave less than the reserve fails with `507`, as on the server. |
| Module size | 8 MB, 3 MB with brotli | §1 goal 5. Optional features (`geo`, `shacl`, `shex`, `reasoning`) can be left out of a custom build. |
| First sync | the dump's size | A replica of a large dataset should be partial. |

## 9. The Sparkles UI in local mode

The UI's requests go through `request` in `ui/src/lib/api.ts`, which calls `fetch`. Phase
3 puts a transport interface behind that function with two implementations: HTTP, as
today, and local, which serves the same routes from a replica. The local transport
covers the routes the query page, the explorer, the commit history, the branch panel and
the merge page use: SPARQL queries and updates, Graph Store reads, `/{ds}/diff`,
`/$/commits/{ds}`, `/$/branches/{ds}` and `/$/merge/{ds}`. Other routes answer `501`
locally, and the UI hides their pages in local mode.

The dataset menu gains "Work offline", which creates a replica of the current dataset
(or of selected graphs) and switches the UI to it. A status chip shows §2.3's state, a
Sync button runs `db.sync()`, and a conflict opens the existing merge page of F09 Phase 2
with the local conflict report. The formatter's module (`sparkles-fmt-wasm`) stays
separate, because the formatter is useful without a replica and is much smaller.

## 10. Server changes

| Change | Where | Why |
|---|---|---|
| A bulk-load option that keeps `_:b<hex>` labels as ids | `store.rs` (`load_with`), the builder | §5.1. Also used by the replica itself. |
| `graphs` on `GET /{ds}/changes` | `http/changes.rs` | §5.2. |
| `POST /$/replicas/{ds}`, `GET`, `DELETE` | `store/branching.rs`, `http/branches.rs` | §6.4. Reserves a blank-node ordinal and records the replica id, the principal and the last push. `DELETE` releases nothing, because ordinals are never reused. |
| The reserved-range blank-node rule for patches | `store/patch_apply.rs` | §6.4. |
| Idempotent patches by `H id` | `store/patch_apply.rs`, the commit record | §6.5. |

Idempotent patches are useful without F20, to any client that retries a patch.

## 11. Node.js, Deno and edge runtimes

The same module runs wherever WebAssembly does. The TypeScript layer takes a `storage`
option with three forms: `'memory'`, `'opfs'` in browsers, and a host adapter.

```ts
interface HostStorage {            // synchronous, because the engine is
  readAt(path: string, off: number, buf: Uint8Array): void; size(path: string): number;
  write(path: string, bytes: Uint8Array): void; append(path: string, bytes: Uint8Array): void;
  sync(path: string): void; rename(from: string, to: string): void;
  remove(path: string): void; list(dir: string): string[];
}
```

- **Node.js and Deno.** An adapter over `node:fs`'s synchronous calls. The native engine
  of P05 is faster and should be used where its prebuilt binaries install. The wasm
  build serves platforms without them, such as browser-based development environments.
- **Edge runtimes** with no synchronous storage use `'memory'` and persist by `dump()`
  to the host's object or key-value store after commits, then `load()` at start. A
  worker that serves reads from a replica and forwards writes to the server is the
  intended use, with pull on a schedule.

The package is tested in Node.js 22 and 24, Deno and a headless browser.

## 12. Architecture

| Place | Contents |
|---|---|
| `crates/sparkles-core/src/storage/` | The `Storage` trait, `FsStorage`, `MemStorage`. |
| `crates/sparkles-core/src/clock.rs` | `now()` and `monotonic()`. |
| `crates/sparkles-core/Cargo.toml` | Features `parallel`, `remote`, `xz`, `mmap` (default on), `zstd-decode-pure`, and `wasm`, which selects the wasm set. |
| `crates/sparkles-wasm/` | A new cdylib, out of the root workspace with its own lock as `sparkles-node` is: wasm-bindgen exports, `OpfsStorage` over `web-sys`'s `FileSystemSyncAccessHandle`, the host-storage adapter, the batch encoder, and the sync engine (`sync.rs`), which drives pulls, pushes and merges against the server with `fetch` from the worker's event loop between engine calls. |
| `js/local/` | `@sparkles-rdf/local`: the worker script, the TypeScript layer, the tab election and the transport for the UI. |
| `crates/sparkles-server/src/http/` | §10's routes and changes. |
| `ui/src/lib/api.ts`, `ui/src/lib/local/` | The transport interface and local mode. |
| `scripts/build-local-wasm.sh`, `mise.toml` | A `js:local` task, next to `ui:wasm`, with the wasm-bindgen CLI pinned to the crate's version and `wasm-opt` for size. |

The sync engine is in Rust, not TypeScript, so that its state machine is tested with the
store's crash-safety harness and reused by the Node and Python bindings later. The
network calls are made by the TypeScript layer on the sync engine's behalf: the engine
returns "send this request" steps and receives the responses, so it stays synchronous
and has no HTTP client of its own.

## 13. Phases

**Phase 1.** `Storage` with `FsStorage` and `MemStorage`, in-memory stores without
temporary directories on every target, the `clock` module, the features of §12, the
pure-Rust zstd decoder, the wasm build in CI, `crates/sparkles-wasm` with `memory`
storage, `@sparkles-rdf/local` with queries, updates, loads, dumps and transactions in a
worker, and the W3C suites in a headless browser.

**Phase 2.** `OpfsStorage`, local branches, the sync engine, §10's server changes,
replica creation, pull, push, local merges, conflict events and resolutions, idempotent
retries, rejected pushes, re-seeding, partial replicas by graph, review-branch pushes,
and authentication with bearer tokens and cookie sessions.

**Phase 3.** The UI's local mode, several tabs, the host-storage adapter and the Node.js,
Deno and edge variants, and the measurements of §14.2.

## 14. Acceptance examples and testing

### 14.1 Acceptance examples

- **A1. Build.** `cargo build -p sparkles-core --no-default-features --features wasm
  --target wasm32-unknown-unknown` succeeds in CI, and the module after `wasm-opt -Oz`
  is at most 8 MB, and 3 MB with brotli.
- **A2. Queries.** In a headless browser, the W3C SPARQL 1.1 query and update suites and
  the SPARQL 1.2 suite pass with the counts of the native build, except tests that need
  SERVICE or remote LOAD, which are listed.
- **A3. No temporary directory.** A native in-memory store that bulk-loads 1 million
  triples and compacts creates no file, checked with a `Storage` that counts calls and
  a process with no writable temporary directory.
- **A4. Persistence.** A replica with 100 commits on OPFS, whose tab is killed during the
  101st commit, reopens with 100 commits and passes `check`.
- **A5. First sync.** A replica of a server dataset of 1 million triples with blank nodes
  has the same canonical N-Quads as the server's state at the synced commit, with the
  same blank-node labels.
- **A6. Fast push.** Offline, two `INSERT DATA` commits. Back online, `sync()` pushes one
  patch with `H prev` naming the synced commit, the server makes one commit, and the next
  pull brings nothing new except that commit, which the replica recognizes by its `id`.
- **A7. Concurrent edits, no conflict.** The server changes `ex:a ex:title`, the replica
  changes `ex:b ex:title`. The push gets `412`, the client pulls, merges, pushes, and both
  sides end with both changes.
- **A8. Conflict.** Both change `ex:a ex:age` from 30, the server to 31 and the replica to
  32. `sync()` ends in `conflict` with F09's report naming the cell. `resolve([{ graph:
  null, subject: '<ex:a>', predicate: '<ex:age>', take: 'ours' }])` pushes 32.
- **A9. Lost response.** A push that applied on the server, whose response is dropped by
  a test proxy, is retried with the same `id` and answered `"duplicate": true`. The server
  has one commit.
- **A10. Blank nodes.** An offline `INSERT DATA { ex:a ex:addr [ ex:city "Oslo" ] }`
  pushed and pulled back leaves one blank node on both sides, with the same label.
- **A11. Partial replica.** A replica of graph `g1` of a dataset with `g1` and `g2`
  receives only `g1`'s changes. An offline write to `g2` fails locally. A principal whose
  grants exclude `g1` cannot create the replica.
- **A12. Rejected push.** A server whose SHACL guard rejects the pushed state answers
  `422`. The replica's status is `rejected`, its commits remain, and moving them to a side
  branch and resetting `main` brings the replica back to `synced`.
- **A13. Two tabs.** Two tabs on one replica: a write in one is visible to a query in the
  other after the `change` event, and closing the owner tab moves ownership without
  losing a commit.
- **A14. Node.js.** The package runs A2's query suite and A6 in Node.js 22 with the
  `node:fs` adapter.

### 14.2 Measurement

- Query times of the BSBM explore mix at 100,000 products in Chrome, Firefox and Safari,
  against the native in-memory engine and Oxigraph's WebAssembly package, cold and warm.
- Opening time and memory for replicas of 100,000, 1 million and 10 million triples.
- Pull throughput in commits and quads per second, and push latency, on a simulated slow
  network.
- A randomized sync test: several replicas and the server make random writes, go offline
  and online, lose responses and reload, and at the end every replica and the server
  hold the same state, which equals the result of applying every accepted change.

## 15. Rejected alternatives

- **CRDTs for RDF.** A CRDT of quad sets merges without conflicts by deciding, per quad,
  whether additions or removals win. That loses the cell-level conflicts that F09 reports
  and users can act on, and RDF data with functional properties needs them. F09's
  three-way merge, run on the client, keeps the server's semantics.
- **napi-rs's WASI target.** It reuses the Node binding, but needs `SharedArrayBuffer`,
  and so cross-origin isolation on every page. A wasm-bindgen build works on any page.
  Threads are open question 1.
- **Sync through SPARQL Update.** Updates carry no `prev` precondition and no stable
  blank-node identity, and they are larger than patches.
- **Replicating with F10 Phase 2's replica protocol.** A replica in F10 is read-only and
  keeps the primary's commit ids. A local-first replica writes, so it needs branches and
  merges, not a follower's log.
- **IndexedDB as the store.** It is asynchronous, and the engine is synchronous. A
  store over IndexedDB would hold whole files in memory and write them back, as a dump
  does.
- **The sync logic in TypeScript.** It would be duplicated for every binding and could
  not use the store's crash-safety tests.

## 16. Open questions

1. **Threads.** Is a threaded build behind cross-origin isolation worth a second module
   for parallel queries and loads?
2. **Memory64.** WebAssembly 3.0 standardizes 64-bit memories. When every target
   browser runs them at an acceptable cost, should the heap limit of §8 rise?
3. **Background sync.** Should a service worker push pending commits when the page is
   closed, where the browser supports Background Sync?
4. **Encryption at rest.** Should a replica encrypt its files with a key derived from a
   passphrase, in the manner of F11's backups?
5. **Protections by class or pattern.** Should sync send such a principal a diff of the
   masked states per pull, as `/{ds}/diff` computes it, instead of refusing the replica?
6. **Granularity of pushes.** One patch per sync (the design) or one per local commit,
   which keeps local commit messages on the server at the cost of more requests?
7. **Replicas of views.** Should a replica be defined by a SPARQL CONSTRUCT view rather
   than graphs? It would need incremental view maintenance (F13).

## 17. Sources

- WebAssembly Core Specification (W3C), and the wasm-bindgen guide, cited from working
  knowledge.
- The File System Standard (WHATWG) and MDN's documentation of the Origin Private File
  System and `FileSystemSyncAccessHandle`, the Storage Standard (WHATWG) for
  `navigator.storage.estimate()` and `persist()`, the Indexed Database API, the Web Locks
  API and `BroadcastChannel`, cited from working knowledge.
- M. Kleppmann, A. Wiggins, P. van Hardenberg and M. McGranaghan, "Local-first software:
  You own your data, in spite of the cloud", Onward! 2019.
- The RDF Patch documentation (Apache Jena / RDF Delta), as used by F10.
- The Sparkles code: `crates/sparkles-fmt-wasm`, `scripts/build-fmt-wasm.sh` and the
  `ui:wasm` task, `sparkles-core`'s `Cargo.toml`, `index.rs`, `vocab.rs`, `io.rs`,
  `store.rs`, `store/wal.rs`, `store/patch_apply.rs`, `store/branching.rs`,
  `sparql/depth.rs`, `codec.rs`, the server's `http/changes.rs`, and `ui/src/lib/api.ts`.
  The specs P05 §12, F06, F09, F10, C09, C12, C12b and X01.

## Outcome

Not built. See [ROADMAP](../ROADMAP.md) item 44.
