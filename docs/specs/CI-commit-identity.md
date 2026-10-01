# CI: Durable commit identity and commit receipts

> **Status:** implemented in part (Phases 1 and 2; Phase 3 not built)
>
> **Phases:** Phase 1 (dataset ids, commit sequence, WAL commit records, catalog, receipts,
> headers, `/$/commits`, `sparkles log`); Phase 2 (UI history and receipts, headers on
> explain and SHACL, `meta.commit`, `id`/`head`/`modified` on datasets, `sparkles log --at`,
> the in-memory ring); Phase 3 (change digest, `ETag`, commit messages) not built.
>
> **User docs:** [API: Commits](../API.md#commits) · [Features](../FEATURES.md#storage-tdb2-equivalent)
>
> This is the design as written before implementation; the [Outcome](#outcome) section at the end
> records how it landed.

This is a clean-room spec. It is the prerequisite for full-text and vector search
watermarks ([F03](F03-full-text-search.md), [F04](F04-vector-search.md)), point-in-time
queries (`?at=commit:<n>`, [F06](F06-snapshots-and-point-in-time.md)), change events and
remote storage ([F05](F05-snapshot-repositories.md)).

## 1. Summary, goals, non-goals

Sparkles has in-process snapshot versions (`Snapshot::version`, reset on every restart and
bumped by compaction), a per-generation WAL (`gen-NNNN/wal.log`) and immutable generations
switched through `CURRENT`. Nothing identifies a *data state* durably. This feature adds:

* a **dataset id**: a UUID created with the database and stored in it;
* a **commit sequence number** (`seq`): a gap-free, strictly increasing `u64` per dataset.
  Every committed write that changes data gets the next `seq`. `seq` survives restart,
  WAL replay, compaction and bulk-write generations. Commit `0` is the root. It is created
  with the database, or when an existing database is first opened by a version with this
  feature;
* a **commit record** per `seq`: server timestamp, parent, effective quad counts, dataset
  size after the commit, operation kind, generation and flags;
* **receipts** from every write API (library, SPARQL Update, GSP, upload, CLI), a
  `Sparkles-Commit` response header on writes *and* reads, a **commit catalog** listable
  over HTTP (`GET /$/commits/{ds}`) and CLI (`sparkles log`), and UI surfaces.

Goals: crash-safe (a `seq` is durable exactly when its data is), deterministic replay (a
reopened store reproduces the same `seq`, timestamp and counts), and near-zero overhead
(no extra fsync on the WAL path). The default HTTP responses stay Fuseki-compatible.

Non-goals (later features): keeping old *data* for time travel (only commit *metadata* is
retained here), `?at=` query selectors, branches/forks, signed or content-addressed
commits, commit messages/authors, replication.

## 2. User-visible behavior

### 2.1 Identifiers

* **Dataset id**: a UUID (RFC 9562, version 4), lowercase hyphenated text,
  e.g. `3f1c9a2e-7b4d-4c1e-9a55-0c2b8e61d7aa`. It names a *lineage* of commits. A new
  database gets a new id. So does a dataset re-created under the same name, and a dump
  reloaded into a new directory. Copying the directory byte-for-byte keeps the id (see
  Open questions).
* **Commit id**: the decimal `seq`, scoped by the dataset id. A commit is fully
  identified by the pair (dataset id, seq). The textual reference form is
  `commit:<seq>` (e.g. `commit:42`). It is reserved for the future `?at=commit:42` selector
  and is accepted by `sparkles log --at`/`GET /$/commits/{ds}/{ref}` as a synonym for
  `42`.
* **Parent**: `seq − 1`. The root commit (`seq 0`) has parent `null`. History is linear
  within a dataset id.
* **Timestamp**: server wall clock, RFC 3339 in UTC with millisecond precision and `Z`
  (`2026-09-30T14:03:11.482Z`). It is clamped to be non-decreasing:
  `ts(n) = max(now, ts(n−1))`. That makes a later `?at=time:` selector a well-defined
  binary search.

### 2.2 What creates a commit

A commit is created **iff** a write transaction has a non-zero *net* effect or staged a
bulk batch. Examples of a net-zero transaction: `INSERT DATA` of quads already present,
`DELETE DATA` of absent quads, or an insert and a delete of the same quad in one request.
A net-zero transaction creates no commit. Its receipt says `"committed": false` and
carries the unchanged head. Compaction creates no commit: data is unchanged, the head
keeps its `seq`, and only the snapshot `version` and generation change. Prefix changes
(`prefixes.json`) are not data and create no commit.

Operation kinds (JSON name / on-disk code):

| kind | code | produced by |
|---|---|---|
| `create` | 0 | root commit of a new database |
| `baseline` | 1 | root commit written when a pre-existing database is first opened (§5.6) |
| `update` | 2 | SPARQL Update (HTTP, CLI `update`, `Dataset::update`), including `LOAD` |
| `gsp-put` | 3 | GSP `PUT` (`/{ds}/data`, `/{ds}`) |
| `gsp-post` | 4 | GSP `POST` |
| `gsp-delete` | 5 | GSP `DELETE` |
| `upload` | 6 | `POST /{ds}/upload` |
| `load` | 7 | CLI `sparkles load`, `Dataset::load_*`, `Store::load` |
| `reason` | 8 | inference materialization (`/$/reason`, `sparkles infer`) |
| `reason-clear` | 9 | removal of inferences (`DELETE /$/reason`, `infer --clear`) |
| `transaction` | 10 | library `Dataset::transaction/insert/remove/extend`, `WriteTxn` default |
| `unknown` | 255 | reconstructed legacy records (§5.6) |

### 2.3 Counts

* `inserted` / `deleted` are **net effective** changes relative to the parent commit. A
  quad is counted once. Re-inserting a present quad or deleting an absent one counts 0.
  An insert and a delete of the same quad in one transaction count 0 each. Invariant:
  `quads(n) = quads(n−1) + inserted − deleted`.
* `exact` is `true` for every transaction on the WAL path and for bulk loads that only
  insert. It is `false` for a bulk-path commit that also deleted quads (GSP `PUT` of a
  large graph, a large `reason` run after deletes). There a quad deleted earlier in the
  same transaction and re-added by the bulk batch counts as both deleted and inserted. The
  invariant above still holds.
* The existing `UpdateStats.inserted/deleted` keep their current meaning: the sum of
  per-operation effective changes. These can differ from the receipt counts for
  multi-operation requests.

### 2.4 HTTP

**Headers.** These appear on every successful response of the write endpoints (`/{ds}/update`,
GSP `PUT/POST/DELETE`, `/{ds}/upload`, and the same operations dispatched through
`/{ds}`) and of the read endpoints (`/{ds}/sparql|query`, `/{ds}` queries, GSP `GET/HEAD`
on `/{ds}/data|get`, `/{ds}/explain`, `/{ds}/shacl`):

```
Sparkles-Commit: 42
Sparkles-Dataset-Id: 3f1c9a2e-7b4d-4c1e-9a55-0c2b8e61d7aa
```

On reads the value is the `seq` of the snapshot the request was evaluated against. On
writes it is the new commit, or the unchanged head when `committed` is false.
`Sparkles-Commit` is a valid RFC 9651 Integer. Both headers are added to CORS
`Access-Control-Expose-Headers`.

**Receipt negotiation.** The body of a successful write is unchanged unless the client asks
for a receipt. A client asks with `Accept: application/x-sparkles+json` (the existing
Sparkles media type) or `receipt=true` in the query string or form. The receipt is a
**superset** of today's body, so code that reads `inserted` or `count` keeps working:

| endpoint | default status / body (unchanged) | with receipt |
|---|---|---|
| `POST /{ds}/update` | `200`, `application/json` `{inserted,deleted,operations,timing}` | `200`, same members + receipt members |
| GSP `PUT` | `200` `{count,tripleCount,quadCount}` | `200`, same + receipt members |
| GSP `POST`, `/upload` | `201` / `200` `{count,tripleCount,quadCount}` | same status, same + receipt members |
| GSP `DELETE` | `204`, no body | `200`, receipt members only |

Receipt members (`Content-Type: application/x-sparkles+json`):

```json
{
  "dataset": "ds",
  "datasetId": "3f1c9a2e-7b4d-4c1e-9a55-0c2b8e61d7aa",
  "committed": true,
  "commit": {
    "seq": 42, "parent": 41, "ref": "commit:42",
    "timestamp": "2026-09-30T14:03:11.482Z",
    "kind": "update",
    "inserted": 3, "deleted": 1, "quads": 1204,
    "generation": "gen-0007", "bulk": false, "exact": true
  }
}
```

The Sparkles JSON query-result format (`application/x-sparkles+json`) also gains
`meta.commit` (the seq) and `meta.datasetId`.

**Commit catalog.**

* `GET /$/commits/{ds}?limit=&before=&after=`. By default it lists the newest commits
  first. `before=<seq>` pages backwards (exclusive). `after=<seq>` lists in *ascending*
  order starting after `seq`, which suits index consumers that follow the log. `limit`
  defaults to 50 and is clamped to 1000.
  ```json
  { "dataset": "ds", "datasetId": "…", "head": 42, "firstRetained": 0,
    "complete": true,
    "commits": [ {commit}, … ],
    "next": "/$/commits/ds?before=40&limit=2" }
  ```
  `next` is `null` on the last page. `complete` is `false` while the catalog lags the WAL
  after a catalog write error (§4.3).
* `GET /$/commits/{ds}/{ref}`, where `ref` is `42`, `commit:42` or `head`. It returns
  `{ "dataset", "datasetId", "commit": {commit} }`.
* `GET /$/datasets[/{ds}]` and `GET /$/server` dataset entries gain `"id"`, `"head"` (seq)
  and `"modified"` (head timestamp). `GET /$/stats/{ds}` gains `"commit"`.

All of these work on read-only servers.

### 2.5 CLI

```
sparkles log --loc DB [--limit N] [--before SEQ | --after SEQ] [--format text|json]
sparkles log --loc DB --at commit:42        # one commit
```

`log` reads `dataset.json` and `commits.bin` **without taking the database lock**, so it
works next to a running server. Text output:

```
dataset 3f1c9a2e-7b4d-4c1e-9a55-0c2b8e61d7aa  head 2
 seq  timestamp                 kind      +inserted  -deleted    quads  generation
   2  2026-09-30T14:05:00.020Z  update            0         1        1  gen-0001
   1  2026-09-30T14:04:59.910Z  update            2         0        2  gen-0001
   0  2026-09-30T14:04:59.800Z  create            0         0        0  gen-0001
```

`--format json` prints the §2.4 list document without `next`. Inexact counts are shown
with a trailing `~`. The write commands print the commit: `sparkles update` prints
`inserted 3 · deleted 1 · commit 42 · 1.20 ms` (or `no change · head 41`). `load`,
`infer` and `compact` print `commit N` (compact prints `head N (unchanged)`).

### 2.6 Rust library API

```rust
// sparkles::commit (new module)
pub enum CommitKind { Create, Baseline, Update, GspPut, GspPost, GspDelete, Upload,
                      Load, Reason, ReasonClear, Transaction, Unknown }
pub struct CommitInfo {
    pub seq: u64, pub timestamp_ms: i64, pub kind: CommitKind,
    pub inserted: u64, pub deleted: u64, pub quads: u64,
    pub generation: u32, pub bulk: bool, pub exact: bool, pub reconstructed: bool,
}
impl CommitInfo { pub fn parent(&self) -> Option<u64>; pub fn timestamp(&self) -> String /* RFC 3339 */ }
pub struct Receipt { pub dataset_id: uuid::Uuid, pub committed: bool, pub commit: CommitInfo }
pub struct CommitPage { pub commits: Vec<CommitInfo>, pub first_retained: u64, pub complete: bool }
pub enum CommitRange { Latest, Before(u64), After(u64) }

// Store
impl Store {
    pub fn dataset_id(&self) -> uuid::Uuid;
    pub fn head_commit(&self) -> CommitInfo;
    pub fn commit(&self, seq: u64) -> Result<Option<CommitInfo>>;
    pub fn commits(&self, range: CommitRange, limit: usize) -> Result<CommitPage>;
    pub fn load_as(&self, sources: &[Source], kind: CommitKind) -> Result<Receipt>;
    pub fn replace_as(&self, t: ReplaceTarget, s: &[Source], kind: CommitKind) -> Result<Receipt>;
    pub fn write_as(&self, kind: CommitKind) -> WriteTxn<'_>;   // write() == write_as(Transaction)
}
impl WriteTxn<'_> { pub fn commit(self) -> Result<Receipt>; /* was Result<u64> */ }
impl Snapshot { pub commit: u64 /* new field: seq this snapshot reflects */ }

// Dataset
impl Dataset {
    pub fn dataset_id(&self) -> uuid::Uuid;
    pub fn head_commit(&self) -> CommitInfo;
    pub fn commits(&self, range: CommitRange, limit: usize) -> Result<CommitPage>;
    pub fn transaction_receipt<R>(&self, f: impl FnOnce(&mut Transaction<'_>) -> Result<R>)
        -> Result<(R, Receipt)>;
    pub fn load_sources(&self, sources: Vec<Source>) -> Result<Receipt>;   // now public
}
// UpdateStats gains `pub commit: Receipt` (serialized as the receipt members);
// Dataset::update / update_with keep returning UpdateStats.
// sparql::update::update_as(store, text, opts, kind) — update() == update_as(.., Update).
```

`Store::load`/`replace` and the `Dataset::load_*` methods keep their `u64` return types as
thin wrappers.

### 2.7 UI (SvelteKit, `ui/`)

* **Dataset page** (`routes/datasets/[name]`): the header `<dl>` gains *Commit* (`42`, with
  relative time and the absolute RFC 3339 time as tooltip) and *Dataset id* (with a copy
  button). A new **History** panel lists the latest 20 commits in a table: seq, time,
  kind badge, `+inserted` / `−deleted` (with `~` and a tooltip when inexact), quads after,
  generation. A generation change between rows is marked as a thin divider labelled
  "compacted into gen-0008" or "rebuilt (bulk)". An *Older* button follows `next`. The
  panel refreshes after uploads, reasoning tasks, compaction and updates run from the
  query page (the same `app.refreshDatasets()` path). It shows an "incomplete catalog"
  warning when `complete` is false.
* **Query page** (`routes/query`): `api.update` sends
  `Accept: application/x-sparkles+json, application/json;q=0.9`. The update outcome and
  toast read "+3 / −1 quads · commit 42", or "No change · head 41" when `committed` is
  false. The commit links to the dataset's History panel. Query results show "at commit
  42" next to the timing, taken from the header or `meta.commit`. Against a Fuseki
  server (no header) this is omitted.
* **Upload panel**: after an upload, show "Loaded N quads · commit 42".
* **Datasets list** (`routes/datasets`): a *Modified* column (head timestamp, relative).
* `lib/api.ts`: `commits(ds, {before, after, limit})`, `commit(ds, ref)`, a `Receipt` type,
  and `UpdateResult` extended with the optional receipt members.

## 3. Standards basis

* **SPARQL 1.1 Protocol §2.2.4**: a successful update SHOULD use 2XX. "The response body
  of a successful update request is implementation defined. Implementations may use HTTP
  content negotiation to provide both human-readable and machine-processable
  information". That permits a receipt behind conneg.
* **SPARQL 1.1 Graph Store HTTP Protocol**: `PUT` that creates content → `201`, else `200`
  or `204`; `DELETE` → `200`/`204`. Receipt bodies ride on `200` for `DELETE` only when
  requested.
* **Fuseki behavior** (from the local Jena checkout): `SPARQL_Update` answers a
  `application/sparql-update` body with `204 No Content`. A form update gets a
  `200` "Update succeeded" page (HTML, text, or `{"statusCode":200,"message":…}` by
  `Accept`). GSP and upload return `ServletOps.uploadResponse`: `201` if the target was
  absent, else `200`, with body `{count, tripleCount, quadCount}`. Sparkles already
  returns `200` + JSON stats for updates, and every Fuseki/Jena client accepts any 2XX.
  Keeping today's defaults and adding only headers therefore stays compatible (see Open
  questions for byte-exact Fuseki status codes).
* **RFC 3339** timestamps; **RFC 9562** UUIDs; **RFC 9110** (status codes, header field
  semantics); **RFC 6648** (no `X-` prefix for new headers); **RFC 9651** (header value is
  an Integer item).
* **RFC 7089 (Memento)** is noted as the model for the future `?at=time:` /
  `Accept-Datetime` work. It is out of scope here but motivates non-decreasing timestamps.

## 4. Semantics

### 4.1 Admission and consistency

* `seq` is assigned under the single writer mutex, at commit time, as `head + 1`. Aborted
  transactions (error, timeout, cancel, parse error) consume nothing, and the next commit
  reuses the same `seq`.
* A commit is **acknowledged** (receipt returned, snapshot published) only after it is
  durable: WAL fsync on the delta path, or the `CURRENT` switch on the bulk path.
  Unacknowledged commits lost to a crash may have their `seq` reused. No client ever saw
  them.
* Readers see `snapshot.commit` of the snapshot they loaded. Because publication follows
  durability, a client that got receipt `n` sees `Sparkles-Commit ≥ n` on every later
  request to the same server (read-your-writes).
* In-memory datasets (`--mem`, `Dataset::memory()`) have commit ids too. The dataset id is
  random per instance, and the catalog is an in-memory ring of the last 65 536 commits
  (`firstRetained` advances). Nothing is durable.

### 4.2 Errors

| condition | status | message |
|---|---|---|
| unknown dataset | 404 | `no such dataset: /ds` (existing) |
| `before`/`after`/`limit` not a non-negative integer, `limit=0`, both `before` and `after` | 400 | `invalid commit range: …` |
| `ref` not `N`, `commit:N` or `head` | 400 | `invalid commit reference 'x'` |
| `seq > head` | 404 | `no commit 99 in dataset ds (head is 42)` |
| `seq < firstRetained` | 410 | `commit metadata before 1000 is no longer retained` |
| writer poisoned (§4.3) | 503 | `write-ahead log failed; restart the server to recover` |
| store open: seq discontinuity, CRC failure before the last WAL transaction, catalog dataset-id mismatch | open fails, `Error::Corrupt` | names file and offset |

### 4.3 Failure handling

* **Writer poisoning.** Any I/O error *after the first WAL byte of a commit is written*
  poisons the writer: a failed `write_all`/`flush`/`sync_data`. So does any error after a
  bulk `CURRENT` switch. The store's `WriterState.poisoned` is set. All later write
  transactions fail with a new `Error::Poisoned` (HTTP 503), and reads continue. Without
  this, a failed fsync followed by another commit could leave two WAL commit records with
  the same `seq`. Reopening the store replays whatever is durable.
* **Catalog lag.** The catalog is appended *after* the commit is durable and before the
  snapshot is published. An append error does not fail the commit (it is already durable)
  and does not poison. The record is kept in `Catalog::pending`, `complete` becomes
  `false`, the error is logged, and every later commit retries the pending appends first.
  Rebuilds refuse to switch `CURRENT` until the catalog is complete and fsynced (§5.4).
  Otherwise the old WAL, the only other copy, would be deleted.

### 4.4 Limits and configuration

* Catalog: 64 bytes per commit, kept forever (10 M commits ≈ 640 MB). No pruning in this
  feature. `firstRetained` exists for recovery (§5.5), in-memory rings and future pruning.
* No new CLI flags for `serve`. `StoreOptions` gains `memory_commit_ring: usize`
  (default 65 536). There is also a hidden test hook,
  `Store::set_clock(Arc<dyn Fn() -> i64 + Send + Sync>)`.
* Overhead per WAL commit: one 64-byte buffered `write` to `commits.bin` (no fsync). CRC
  over the transaction's WAL bytes. Two `OrdSet` lookups per effective insert/delete for
  net counting.

## 5. Design sketch

### 5.1 New and changed modules

* `crates/sparkles/src/commit.rs` (new): `CommitKind` (u8 codes, JSON names),
  `CommitInfo`, `Receipt`, `CommitPage`, RFC 3339 millisecond formatting (extends
  `builder::now_rfc3339`), `DatasetFile` (`dataset.json`), `GenCommitFile`
  (`gen-NNNN/commit.json`), `Catalog` (binary file, in-memory ring variant).
* `store.rs`: `Store` gains `dataset_id`, `catalog: Mutex<Catalog>` (appends happen under
  the writer mutex, and reads use a separate `pread` handle so listing never blocks
  writers). `WriterState` gains `head: CommitInfo`, `poisoned: bool`. `Snapshot` gains
  `commit: u64`. `WriteTxn` gains `kind`, `net_ins: u64`, `net_del: u64`. The WAL commit
  record changes (§5.3). Replay, `rebuild_locked`, `publish_log` and `open` change as below.
* `sparql/update.rs`: `update_as(.., kind)`. `UpdateStats.commit`.
* `dataset.rs`: API of §2.6.
* `sparkles-reasoner`: `materialize` uses `write_as(Reason)`, `clear` uses
  `write_as(ReasonClear)`.
* `sparkles-server/http.rs`: headers, receipt negotiation, `/$/commits` routes, CORS
  expose list. Every read handler takes the snapshot once and uses its `commit` for the
  header (`query_endpoint` today calls `ds.store.snapshot()` inside `sparql::query`; pass
  the captured snapshot instead). `main.rs`: `Log` subcommand and the printed commits.
* `ui/`: §2.7.

### 5.2 Net counting (exact on the WAL path)

`WriteTxn::insert` and `delete` already log only effective changes (`contains` checks the
view). For an effective change of quad `q` with key `k`, the transaction computes
`present_at_start = base.delta.ins[SPO].contains(k) || (in_base(q) && !base.delta.del[SPO].contains(k))`
(`in_base` is already computed there). Then:

* insert: `present_at_start ? net_del -= 1 : net_ins += 1`
* delete: `present_at_start ? net_ins -= 1 : net_del += 1`

(The delete case above is swapped; the implementation uses
`present_at_start ? net_del += 1 : net_ins -= 1`. See [Outcome](#outcome).)

`commit()` with `net_ins == net_del == 0` and an empty bulk batch creates no commit. It
writes no WAL bytes and publishes nothing. The terms it interned stay in the append-only
delta vocabulary, as they do for aborted transactions today. Replay recomputes the same
numbers per transaction, using the replayed delta so far as "start".

### 5.3 WAL commit record, version 2

Records stay 33 bytes. Data records (`1` insert, `2` delete) are unchanged. The commit
record (`op = 3`) uses its 24 spare bytes, which are always zero today:

| bytes | field |
|---|---|
| 0 | `3` |
| 1..9 | `next_bnode` u64 LE (unchanged) |
| 9..17 | `seq` u64 LE (≥ 1) |
| 17..25 | `timestamp_ms` i64 LE (Unix epoch, already clamped) |
| 25 | `kind` code |
| 26 | record version `2` (`0` = legacy record) |
| 27..29 | reserved, zero |
| 29..33 | CRC-32 (IEEE, `flate2::Crc`) over this transaction's data records followed by bytes 0..29 of this record |

Replay (in `Store::open`) groups records into transactions as today and checks each
commit record:

* CRC mismatch or truncated transaction **in the last transaction**: torn tail. Truncate
  to the previous commit as today.
* CRC mismatch **before** the last transaction: `Error::Corrupt`. Silently dropping
  acknowledged commits would reuse their ids.
* version 2: `seq` must equal the previous seq + 1. The first one must equal the
  generation's `base_seq + 1`. Anything else is `Error::Corrupt`.
* version 0 (legacy): see §5.6.

The id, timestamp and kind live inside the record that already defines durability. So a
commit id is durable exactly when its WAL entry is, and replay reproduces it
byte-for-byte.

### 5.4 Generations: `gen-NNNN/commit.json` (format 1)

Each generation records the commit its base index holds:

```json
{ "format": 1, "datasetId": "3f1c…", "baseSeq": 17, "origin": "bulk",
  "commit": { "seq": 17, "timestamp": "2026-09-30T14:03:11.482Z", "kind": "load",
              "inserted": 5000000, "deleted": 0, "quads": 5000120,
              "generation": "gen-0004", "bulk": true, "exact": true } }
```

`origin` is `create`, `baseline`, `bulk` (the rebuild *is* commit `baseSeq`) or
`compaction` (`commit` is a copy of the head record, whose `generation` names the
generation where it was made). `rebuild_locked` changes:

1. Before building: if the catalog is incomplete, flush `pending`. Then fsync
   `commits.bin`. On failure, abort (nothing switched, no seq consumed).
2. Bulk commit (`WriteTxn::commit` with a bulk batch, `load` into an empty or large store):
   `seq = head + 1`, timestamp clamped. `inserted = meta.quads − start.len() + net_del`,
   where `start` is the committed snapshot the transaction began from, and
   `deleted = net_del`.
   `exact = (net_del == 0)`. Compaction keeps `seq = head`.
3. After `Builder::finish`, `write_synced(dir/commit.json)`, then the existing
   `sync_dir(dir)`, `sync_dir(root)`, `write_atomic(CURRENT)`. **The `CURRENT` switch is
   the commit point**, as today. The new generation's WAL starts empty. Its first commit
   will carry `baseSeq + 1`.
4. After the switch: append the catalog record (bulk only) and publish a snapshot with
   `commit = seq`. Errors here poison the writer (§4.3). This covers the existing
   `add_prefixes(...)?` between the switch and `current.store`.

`Store::open` reads `commit.json` of the `CURRENT` generation and sets
`head = baseSeq`, then replays the WAL.

### 5.5 Catalog: `<root>/commits.bin` (format 1)

Fixed-size little-endian records, so commit `s` is at offset `64 + (s − first_seq)·64`.
That gives O(1) lookups and paging, and later a binary search on timestamps.

Header (64 bytes): `0..8` magic `SPKCMTS\0` · `8..12` format `1` · `12..16` record size
`64` · `16..32` dataset UUID bytes · `32..40` `first_seq` · `40..60` zero · `60..64`
CRC-32 of 0..60.

Record (64 bytes): `0..8` seq · `8..16` timestamp_ms · `16..24` inserted · `24..32`
deleted · `32..40` quads · `40..44` generation number · `44` kind · `45` flags (bit0 exact,
bit1 bulk, bit2 reconstructed) · `46..60` zero · `60..64` CRC-32 of 0..60.

It is appended with `write_all` + `flush`, with no fsync per commit (the WAL or
`commit.json` is the durable source). It is fsynced before every `CURRENT` switch and on
`Drop` of the store. **Repair on open**, after WAL replay has established `head`:

* Drop trailing records that are partial, fail their CRC, or have `seq > head`, with a
  warning. Any of these means lost page-cache writes or a hand-edited directory.
* If `last < baseSeq`, append from `commit.json`. That record is always present. If
  `last < baseSeq − 1`, the history between is unrecoverable. This only happens if the
  file was deleted or corrupted. Start a new catalog at `first_seq = baseSeq` and rename
  the old file to `commits.bin.corrupt-<ts>`. `firstRetained` then reports the gap.
* Append the replayed WAL commits with `seq > last`. Their counts come from §5.2.
* Header dataset UUID ≠ `dataset.json`: `Error::Corrupt`.

`sparkles log` (lockless) reads only complete records with valid CRCs, up to the last one.

### 5.6 Dataset file, creation, migration

`<root>/dataset.json` (format 1):
`{ "format": 1, "id": "3f1c…", "created": "2026-09-30T14:04:59.800Z", "origin": "create" | "baseline" }`.

* **New database** (`CURRENT` absent): write `dataset.json` (reusing it if present from
  an interrupted create). Build `gen-0001`, then `gen-0001/commit.json` (`baseSeq 0`,
  kind `create`, quads 0). Write `commits.bin` with record 0. Write `CURRENT` last.
* **Pre-existing database** (`CURRENT` present, no `dataset.json`): open and replay as
  today, treating version-0 commit records as legacy. Then write the baseline root commit:
  `seq 0`, kind `baseline`, `inserted = quads` (everything present), `deleted 0`,
  timestamp = now. Write it to `gen-NNNN/commit.json` (`baseSeq 0`, origin `baseline`),
  then `commits.bin`, then `dataset.json` last. The presence of `dataset.json` marks the
  migration complete, so an interrupted migration is redone. Legacy records
  *before* the first version-2 record are folded into `baseSeq` (commit 0).
* **Legacy records after a version-2 record** (an older binary appended to an upgraded
  database): each gets `seq = prev + 1`, kind `unknown`, timestamp = the parent's
  timestamp, and flag `reconstructed`. This is deterministic, so every replay assigns the
  same ids. Downgrading remains unsupported, but it does not corrupt anything.

### 5.7 Server plumbing

* `update_endpoint` passes `CommitKind::Update`. `gsp` uses `replace_as(.., GspPut)`,
  `load_as(.., GspPost)` and `update_as("CLEAR …", .., GspDelete)`. `upload` uses
  `load_as(.., Upload)`. GSP/upload `count` stays "new quads" (= `receipt.commit.inserted`
  when committed, else 0). It no longer needs the racy `snapshot().len()` before/after
  subtraction.
* `fn receipt_wanted(params, headers) -> bool`: `receipt=true`, or an `Accept` that
  explicitly lists `application/x-sparkles+json` (`*/*` does not count).
* Headers are set by one helper, `commit_headers(ds_id, seq)`. `CorsLayer` gets
  `.expose_headers([SPARKLES_COMMIT, SPARKLES_DATASET_ID])`.
* New routes: `/$/commits/{ds}` and `/$/commits/{ds}/{ref}` (GET only).

## 6. Phasing

**Phase 1 (MVP, ~1 day)**: `commit.rs` types. `dataset.json` + root commit (create and
baseline migration). WAL commit record v2 with seq, timestamp, kind and CRC, plus replay
checks. Net counting. `gen/commit.json` on rebuild/compaction. `Snapshot.commit`,
`head_commit`, `dataset_id`. `commits.bin` append + repair on open. Writer poisoning.
`CommitKind` plumbing through update/GSP/upload/load/reasoner. `Sparkles-Commit` /
`Sparkles-Dataset-Id` on write and query responses. Receipt negotiation.
`GET /$/commits/{ds}` (`limit`, `before`, `after`) and `/{ref}`. `sparkles log`. Tests
from §7 A1–A12.

**Phase 2**: UI (§2.7). Headers on `explain`/`shacl`/GSP `HEAD`. `meta.commit` in
Sparkles JSON. `/$/datasets` `id/head/modified`. In-memory ring bound. `sparkles log --at`.

**Phase 3**: optional per-commit **change digest** (§9, off by default).
`ETag: W/"<datasetId>:<seq>"` + `If-None-Match` on GSP `GET`. A commit annotations
side-file (client message via a `Sparkles-Commit-Message` request header).

**Later (separate specs)**: retained generations and `?at=commit:N|time:…`
([F06](F06-snapshots-and-point-in-time.md)). Search indexes that record their `seq`
watermark ([F03](F03-full-text-search.md), [F04](F04-vector-search.md)). Change feed `GET /$/commits/{ds}?after=`
long-poll. Catalog pruning.

## 7. Acceptance examples

Setup: `sparkles serve --data DIR` with a fresh persistent dataset `ds`.

**A1. Root commit.** `GET /$/commits/ds` → `200`,
`{"head":0,"firstRetained":0,"complete":true,"commits":[{"seq":0,"parent":null,"kind":"create","inserted":0,"deleted":0,"quads":0,"generation":"gen-0001","bulk":false,"exact":true,…}],"next":null}`.

**A2. Update receipt.**
```
POST /ds/update   Content-Type: application/sparql-update
Accept: application/x-sparkles+json
INSERT DATA { <urn:a> <urn:p> 1 . <urn:b> <urn:p> 2 }
```
→ `200`, `Sparkles-Commit: 1`, body has `"inserted":2,"deleted":0,"operations":1`,
`"committed":true`, `"commit":{"seq":1,"parent":0,"kind":"update","inserted":2,"deleted":0,"quads":2,"exact":true}`.

**A3. Default body unchanged.** The same request with `Accept: */*` and different data
(`<urn:c> <urn:p> 3`) → `200` `application/json`, body keys exactly
`inserted, deleted, operations, timing`, header `Sparkles-Commit: 2`.

**A4. No-op.** Re-send A2 with receipt → `"committed":false`, `"commit":{"seq":2,…}`,
`Sparkles-Commit: 2`. `GET /$/commits/ds` still has head 2.

**A5. Net counting.**
`INSERT DATA { <urn:x> <urn:p> 9 } ; DELETE DATA { <urn:x> <urn:p> 9 }` with receipt →
`"inserted":1,"deleted":1` (UpdateStats, per operation), `"committed":false`, head
unchanged. Then `DELETE DATA { <urn:a> <urn:p> 1 } ; INSERT DATA { <urn:a> <urn:p> 1 }` →
`committed:false`.

**A6. GSP.** `PUT /ds/data?graph=urn:g` (Turtle, 3 triples) with receipt → `200`,
`count 3` + `"kind":"gsp-put","inserted":3,"deleted":0,"seq":3`. `DELETE /ds/data?graph=urn:g`
without receipt → `204`, `Sparkles-Commit: 4`. (Variant test: the same DELETE with
receipt → `200`, `"kind":"gsp-delete","deleted":3,"seq":4`.)

**A7. Query header.** `GET /ds/sparql?query=ASK{}` → `Sparkles-Commit: 4`,
`Sparkles-Dataset-Id` equal to A1's `datasetId`. `Access-Control-Expose-Headers` contains
both names for a cross-origin request.

**A8. Restart and replay.** Stop the server (kill -9). Restart. `GET /$/commits/ds`
lists seqs 4..0 with the same timestamps, kinds and counts as before. The next update gets
seq 5.

**A9. Lost catalog tail.** Unit test: commit 3 times, drop the store, truncate
`commits.bin` by 100 bytes, reopen. `commits(Latest, 10)` equals the pre-truncation list
(records rebuilt from the WAL).

**A10. Torn WAL tail.** Unit test: append a partial transaction (data records, no
commit record) to `wal.log`. Reopen: head unchanged, WAL truncated, next commit = head+1.
Flip one byte in the *first* of two transactions' data records → open fails with
`Error::Corrupt` naming `wal.log`.

**A11. Compaction and bulk.** `POST /$/compact/ds` → after the task: head unchanged (5),
`generation` of the head's catalog entry still `gen-0001`, `/$/stats/ds` shows
`gen-0002`. Next update → seq 6, `"generation":"gen-0002"`. With
`StoreOptions { bulk_threshold: 10, .. }`, loading 100 new triples → seq 7,
`"kind":"load","bulk":true,"exact":true,"inserted":100`. Reopen → head 7 (from
`gen-0003/commit.json`), next update seq 8.

**A12. Paging and errors.** With head 8: `?limit=3` → seqs `[8,7,6]`,
`next=/$/commits/ds?before=6&limit=3`. `?before=6&limit=3` → `[5,4,3]`. `?after=5&limit=2`
→ `[6,7]`. `?before=x` → 400. `?before=3&after=1` → 400. `?limit=0` → 400. `/$/commits/nope`
→ 404. `/$/commits/ds/99` → 404 `no commit 99 in dataset ds (head is 8)`.
`/$/commits/ds/commit:2` → seq 2. `/$/commits/ds/head` → seq 8.

**A13. Migration.** Unit test: build a store with the pre-feature code path (write a
legacy WAL with version-0 commit records over 3 quads), remove `dataset.json`, reopen →
head 0, kind `baseline`, `inserted 3`, `quads 3`. The next update gets seq 1.

**A14. Monotone clock.** With a test clock returning 1000, then 500: commit 1 has
timestamp 1000 ms and commit 2 also 1000 ms.

**A15. Library.**
```rust
let ds = Dataset::memory();
let s = ds.update("INSERT DATA { <urn:a> <urn:p> <urn:o> }")?;
assert!(s.commit.committed && s.commit.commit.seq == 1);
assert_eq!(ds.head_commit().seq, 1);
let (_, r) = ds.transaction_receipt(|tx| tx.insert(quad_ref))?;   // existing quad
assert!(!r.committed && r.commit.seq == 1);
```

**A16. CLI.** `sparkles update --loc db 'INSERT DATA { <urn:a> <urn:p> 1 }'` →
stderr `inserted 1 · deleted 0 · commit 1 · …`. `sparkles log --loc db --format json`
while `sparkles serve` holds the lock → succeeds and lists seqs 1, 0.

## 8. Rejected alternatives

* **Using `Snapshot::version` as the commit id**: it is in-process, resets on restart, and
  compaction bumps it without changing data.
* **Random/UUID commit ids or content hashes as the primary id**: they are not ordered.
  Watermarks, paging and `?at=` all need `a < b` comparisons and O(1) lookup. A digest can
  be added as an attribute (Phase 3).
* **A separate WAL record type for commit metadata**: an older binary stops at the unknown
  opcode and *truncates* the WAL on open, losing data on a downgrade. The spare bytes of
  the existing commit record are ignored by old binaries.
* **fsyncing the catalog on every commit**: it doubles the fsyncs per write. The catalog is
  derivable from the WAL and `commit.json`. It only has to be durable before a
  generation's WAL is discarded.
* **A JSON-lines catalog**: it needs a scan or a side index for paging and seq lookup.
  Fixed records are simpler and faster. `sparkles log --format json` covers human
  inspection.
* **Counting raw WAL records as inserted/deleted**: it double-counts
  insert-then-delete within one transaction. Diffing delta sets at commit costs more than
  the O(1) bookkeeping of §5.2.
* **A commit for compaction**: two ids would name identical content, and search
  watermarks would re-index for nothing. Generation changes stay visible through the
  `generation` field.
* **Empty commits for no-op writes**: they add catalog noise and break "each commit
  changed something". The receipt returns the head instead.
* **Changing default update responses to Fuseki's exact `204`/HTML page**: that would
  break existing Sparkles clients (the UI parses the JSON). The receipt is opt-in instead
  (see Open questions).
* **Putting the commit in `ETag` only**: ETag has cache-validation semantics per
  representation (RFC 9110 §8.8.3), and a query result also depends on parameters and
  non-deterministic functions. It is a Phase 3 addition for GSP `GET`, not the carrier.
* **An `X-Sparkles-Commit` header name**: RFC 6648 deprecates the `X-` prefix.
* **Adding the base seq to `CURRENT`**: older binaries read `CURRENT` verbatim as a
  directory name. `gen-NNNN/commit.json` is invisible to them.

## 9. Open questions

1. **Directory copies share an id.** Should `Store::open` detect a copy (e.g. record the
   absolute path or inode in `dataset.json`) and mint a new id? Clone-to-sandbox
   ([C06](C06-clone-to-sandbox.md))
   should mint a new id and record `"forkedFrom": {"id", "seq"}`. Default: copies keep the
   id.
2. **Fuseki-exact defaults.** Should `Accept: */*` / no `Accept` update requests get
   `204` (like Fuseki) instead of today's `200` JSON? Also, GSP `POST` currently always
   returns `201` and `PUT` always `200`. Fuseki uses `201` iff the target was absent. That
   is worth aligning in a separate fix. Default: unchanged.
3. **Change digest (Phase 3).** Proposed, not canonical RDF. `digest(n) = SHA-256(
   "sparkles-commit-digest-v1\n" ‖ datasetId ‖ "\n" ‖ seq ‖ "\n" ‖ hex(digest(n−1)) ‖ "\n"
   ‖ timestamp ‖ "\n" ‖ kind ‖ "\n" ‖ sorted lines "-" + L(q) for deleted q ‖ sorted lines
   "+" + L(q) for inserted q)`. `L(q)` is the quad in canonical N-Quads form (RDF 1.2
   N-Quads §3) with blank nodes written as their store-internal labels (`bnode_for(id)`).
   Lines are sorted by UTF-8 bytes, and `digest(−1)` is 32 zero bytes. Blank-node labels
   are store-internal, so equal digests across different databases mean nothing. It is
   not an RDF Dataset Canonicalization (RDFC-1.0) hash. Bulk commits would need the change
   set materialized, and whether they get `digest = null` or pay the cost is open. Default:
   not implemented.
4. **Timestamp source**: wall clock with a non-decreasing clamp. A large backwards clock
   jump freezes timestamps until the wall clock catches up. Should the server warn?
5. **Catalog pruning** by age or count once point-in-time retention exists. Default:
   keep everything.
6. **Commit messages/authors**: need variable-length storage (annotation side-file keyed
   by seq). Deferred to Phase 3 so the fixed record stays 64 bytes.
7. **UUID version**: v4 (random, already enabled in the workspace `uuid`). v7 would embed
   creation time. Default: v4, with the creation time in `dataset.json`.

## 10. Sources

* Sparkles repository (read): `README.md`, `docs/API.md`, `docs/AUDIT.md`, the
  project's feature-ordering notes (this feature's scope and dependants),
  `crates/sparkles/src/store.rs` (generations, WAL format, replay, `publish_log`,
  `rebuild_locked` publication order, `write_atomic`/`sync_dir`),
  `crates/sparkles/src/dataset.rs`, `crates/sparkles/src/sparql/update.rs`
  (`UpdateStats`, per-op counting), `crates/sparkles/src/builder.rs` (`IndexMeta`,
  `now_rfc3339`), `crates/sparkles-reasoner/src/lib.rs` (reason/clear transactions),
  `crates/sparkles-server/src/{http.rs,main.rs,state.rs}` (current update/GSP/upload
  responses, CORS, CLI), `ui/src/lib/api.ts`, `ui/src/routes/query/+page.svelte`,
  `ui/src/routes/datasets/[name]/+page.svelte`, workspace `Cargo.toml` (existing `uuid`
  v4, `sha2`, `flate2`).
* Apache Jena sources (Apache-2.0):
  `jena-fuseki2/jena-fuseki-core/.../servlets/SPARQL_Update.java` (204 / success page),
  `ServletOps.java` (`successPage`, `uploadResponse` 201/200 + JSON), `GSP_RW.java`,
  `UploadDetails.java` (`count`/`tripleCount`/`quadCount`).
* tower-http 0.6.8 `src/cors/mod.rs` (MIT; crate source): `very_permissive()`
  exposes no headers.
* flate2 1.1.9 `src/crc.rs` (MIT/Apache-2.0): public `Crc` (CRC-32).
* W3C SPARQL 1.1 Protocol §2.2.4, https://www.w3.org/TR/sparql11-protocol/ (update
  success responses, conneg for update bodies).
* W3C SPARQL 1.1 Graph Store HTTP Protocol, https://www.w3.org/TR/sparql11-http-rdf-update/
  (PUT/POST/DELETE status codes).
* W3C RDF 1.2 N-Quads §3 "A Canonical form of N-Quads",
  https://www.w3.org/TR/rdf12-n-quads/ (digest line form in Open question 3).
* Cited from general knowledge, not fetched: RFC 3339, RFC 9562 (UUID), RFC 9110 (HTTP
  semantics, ETag), RFC 6648 (`X-` prefix), RFC 9651 (Structured Field Values), RFC 7089
  (Memento), W3C RDF Dataset Canonicalization (RDFC-1.0).
* **Not consulted**: Fluree, in any form (code, docs, tests, site, talks). Also not read:
  the project's earlier implementation review and feature plan.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30 as `0374949` (engine: dataset ids, WAL commit
record version 2, net counting, `gen-NNNN/commit.json`, `commits.bin` with repair on open,
writer poisoning) and `f128404` (server and CLI: headers, receipt negotiation,
`/$/commits` routes, `sparkles log`). Phase 2 followed the same day: the UI's commit
history panel and write receipts (`9b8cacb`), and commit headers on SHACL validation
(`cb598c8`); `id`, `head` and `modified` on dataset entries, `meta.commit` in the Sparkles
JSON format, `sparkles log --at` and the in-memory ring (`StoreOptions::memory_commit_ring`,
65,536) are all in place. The receipts became the base that the later features build on:
search watermarks ([F03](F03-full-text-search.md)), `?at=` reads and named snapshots
([F06](F06-snapshots-and-point-in-time.md)), clones that record `forkedFrom`
([C06](C06-clone-to-sandbox.md)), backups and restores ([F05](F05-snapshot-repositories.md))
and write-time validation summaries ([C10](C10-write-time-validation.md)).

**Deviations from the spec.**

- §5.2 swapped the delete case. The implementation counts deleting a quad present at
  the start of the transaction as `net_del += 1`, and deleting one the same transaction
  added as `net_ins -= 1`.
- The catalog is kept in memory (a ring of 64-byte records) rather than read with
  `pread`; a million commits take about 64 MiB. Reading from the file remains the option
  if histories grow long.
- Legacy WAL records are folded into the base only when the generation's base is a
  baseline commit (or during the upgrade itself); otherwise they are reconstructed with
  kind `unknown`, as §5.6 describes for records after a version-2 record.
- Graph Store PUT still reports the parsed count as `count`, as before (the spec said "new
  quads"). POST and upload report the commit's net inserted count, which equals the old
  number.
- `sparkles log` lists 20 commits unless `--limit` says otherwise.

**Open questions.** Directory copies still share an id (question 1), but clones and
restores into a new lineage mint a new id and record `forkedFrom`. Default response bodies
stay as they were (question 2). The non-decreasing clock clamp and v4 UUIDs are as
proposed; the catalog is not pruned.

**Tests at landing.** `crates/sparkles/tests/commits.rs` covers the root commit, net
counting, restart and replay, a lost catalog tail, a torn WAL tail, compaction and bulk
commits, migration, the monotone clock and the library API; router tests cover receipts,
headers, the catalog routes, paging and errors. The W3C SPARQL results did not change.

**Not built.** Phase 3: the per-commit change digest, `ETag`/`If-None-Match` on Graph
Store GET, and commit messages through a `Sparkles-Commit-Message` header. Catalog pruning
and the long-poll change feed remain later work.
