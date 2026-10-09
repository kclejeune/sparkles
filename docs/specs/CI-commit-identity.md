# CI: Durable commit identity and commit receipts

> **Status:** implemented (Phases 1, 2 and 3).
>
> **Phases:** Phase 1 added dataset ids, the commit sequence, WAL commit records, the
> catalog, receipts, headers, `/$/commits` and `sparkles log`. Phase 2 added the UI history
> and receipts, headers on explain and SHACL, `meta.commit`, `id`/`head`/`modified` on
> datasets, `sparkles log --at` and the in-memory ring. Phase 3 added commit messages,
> optional change digests, and entity tags with conditional Graph Store requests. The
> catalog horizon and the change feed, which §6 left to later work, landed after Phase 3.
>
> **User docs:** [API: Commits](../API.md#commits) ·
> [API: Entity tags and conditional requests](../API.md#entity-tags-and-conditional-requests) ·
> [API: Change feed](../API.md#change-feed) ·
> [Features](../FEATURES.md#storage-tdb2-equivalent)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

Several later features build on this spec: full-text and vector search
watermarks ([F03](F03-full-text-search.md), [F04](F04-vector-search.md)), point-in-time
queries (`?at=commit:<n>`, [F06](F06-snapshots-and-point-in-time.md)), change events and
remote storage ([F05](F05-snapshot-repositories.md)).

## 1. Summary, goals, non-goals

Sparkles has in-process snapshot versions (`Snapshot::version`), which reset on every
restart and are incremented by compaction. It also has a WAL per generation (`gen-NNNN/wal.log`) and
immutable generations switched through `CURRENT`. None of these identifies a *data state*
durably. This feature adds four things:

* A **dataset id**. This is a UUID created with the database and stored in it.
* A **commit sequence number** (`seq`). This is a gap-free, strictly increasing `u64` per
  dataset. Every committed write that changes data gets the next `seq`. The `seq` survives
  restart, WAL replay, compaction and bulk-write generations. Commit `0` is the root. It is
  created with the database, or when a version with this feature first opens an existing
  database.
* A **commit record** for each `seq`. It holds the server timestamp, the parent, the
  effective quad counts, the dataset size after the commit, the operation kind, the
  generation and some flags.
* **Receipts** and a **commit catalog**. Every write API (library, SPARQL Update, GSP,
  upload, CLI) returns a receipt. Writes *and* reads carry a `Sparkles-Commit` response
  header. The catalog can be listed over HTTP (`GET /$/commits/{ds}`), from the CLI
  (`sparkles log`) and in the UI.

The feature has three goals. It must be crash-safe: a `seq` is durable exactly when its
data is. Replay must be deterministic: a reopened store reproduces the same `seq`,
timestamp and counts. The overhead must be close to zero, with no extra fsync on the WAL
path. The default HTTP responses stay Fuseki-compatible.

Several things are left to later features. This feature keeps only commit *metadata*, not
old *data* for time travel. It has no `?at=` query selectors, branches or forks, signed or
content-addressed commits, commit messages or authors, or replication.

## 2. User-visible behavior

### 2.1 Identifiers

* **Dataset id.** A UUID (RFC 9562, version 4) written as lowercase hyphenated text, for
  example `3f1c9a2e-7b4d-4c1e-9a55-0c2b8e61d7aa`. It names a *lineage* of commits. A new
  database gets a new id. So does a dataset re-created under the same name, and a dump
  reloaded into a new directory. A byte-for-byte copy of the directory keeps the id (see
  Open questions).
* **Commit id.** The decimal `seq`, scoped by the dataset id. The pair (dataset id, seq)
  identifies a commit fully. The textual reference form is `commit:<seq>`, for example
  `commit:42`. It is reserved for the future `?at=commit:42` selector. `sparkles log --at`
  and `GET /$/commits/{ds}/{ref}` accept it as a synonym for `42`.
* **Parent.** The parent is `seq − 1`. The root commit (`seq 0`) has parent `null`. History
  is linear within a dataset id.
* **Timestamp.** The server's wall clock, in RFC 3339 UTC with millisecond precision and a
  `Z` suffix (`2026-09-30T14:03:11.482Z`). It is clamped so it never decreases:
  `ts(n) = max(now, ts(n−1))`. A later `?at=time:` selector can then use a binary search.

### 2.2 What creates a commit

A write transaction creates a commit **iff** it has a non-zero *net* effect or staged a
bulk batch. `INSERT DATA` of quads already present is net-zero. So is `DELETE DATA` of
absent quads, and so is an insert and a delete of the same quad in one request. A net-zero
transaction creates no commit. Its receipt says `"committed": false` and carries the
unchanged head.

Compaction creates no commit. The data is unchanged and the head keeps its `seq`. Only the
snapshot `version` and the generation change. Prefix changes (`prefixes.json`) are not
data and create no commit either.

Each commit has an operation kind, with a JSON name and an on-disk code:

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

* `inserted` and `deleted` are the **net effective** changes relative to the parent
  commit. Each quad counts once. Re-inserting a present quad or deleting an absent one
  counts 0. An insert and a delete of the same quad in one transaction count 0 each. The
  invariant is `quads(n) = quads(n−1) + inserted − deleted`.
* `exact` is `true` for every transaction on the WAL path and for bulk loads that only
  insert. It is `false` for a bulk-path commit that also deleted quads, such as a GSP `PUT`
  of a large graph or a large `reason` run after deletes. In that case, a quad deleted
  earlier in the transaction and re-added by the bulk batch counts as both deleted and
  inserted. The invariant still holds.
* The existing `UpdateStats.inserted/deleted` keep their meaning: the sum of the effective
  changes of each operation. For requests with several operations they can differ from
  the receipt counts.

### 2.4 HTTP

**Headers.** Every successful response from a write or read endpoint carries two headers.
The write endpoints are `/{ds}/update`, GSP `PUT/POST/DELETE`, `/{ds}/upload`, and the
same operations sent to `/{ds}`. The read endpoints are `/{ds}/sparql|query`, queries on
`/{ds}`, GSP `GET/HEAD` on `/{ds}/data|get`, `/{ds}/explain` and `/{ds}/shacl`.

```
Sparkles-Commit: 42
Sparkles-Dataset-Id: 3f1c9a2e-7b4d-4c1e-9a55-0c2b8e61d7aa
```

On a read, the value is the `seq` of the snapshot the request ran against. On a write, it
is the new commit, or the unchanged head when `committed` is false. `Sparkles-Commit` is a
valid RFC 9651 Integer. Both headers are listed in CORS `Access-Control-Expose-Headers`.

**Receipt negotiation.** The body of a successful write is unchanged unless the client asks
for a receipt. To ask, a client sends `Accept: application/x-sparkles+json`, the existing
Sparkles media type, or puts `receipt=true` in the query string or form. The receipt is a
**superset** of today's body, so code that reads `inserted` or `count` keeps working.

| endpoint | default status / body (unchanged) | with receipt |
|---|---|---|
| `POST /{ds}/update` | `200`, `application/json` `{inserted,deleted,operations,timing}` | `200`, same members + receipt members |
| GSP `PUT` | `200` `{count,tripleCount,quadCount}` | `200`, same + receipt members |
| GSP `POST`, `/upload` | `201` / `200` `{count,tripleCount,quadCount}` | same status, same + receipt members |
| GSP `DELETE` | `204`, no body | `200`, receipt members only |

The receipt members look like this (`Content-Type: application/x-sparkles+json`):

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

The Sparkles JSON query-result format (`application/x-sparkles+json`) gains
`meta.commit`, which holds the seq, and `meta.datasetId`.

**Commit catalog.**

* `GET /$/commits/{ds}?limit=&before=&after=` lists the newest commits first by default.
  `before=<seq>` pages backwards and excludes `seq`. `after=<seq>` lists in *ascending*
  order, starting after `seq`. That order suits index consumers that follow the log.
  `limit` defaults to 50 and is clamped to 1000.
  ```json
  { "dataset": "ds", "datasetId": "…", "head": 42, "firstRetained": 0,
    "complete": true,
    "commits": [ {commit}, … ],
    "next": "/$/commits/ds?before=40&limit=2" }
  ```
  `next` is `null` on the last page. `complete` is `false` while the catalog lags behind
  the WAL after a catalog write error (§4.3).
* `GET /$/commits/{ds}/{ref}` returns `{ "dataset", "datasetId", "commit": {commit} }`.
  `ref` is `42`, `commit:42` or `head`.
* The dataset entries of `GET /$/datasets[/{ds}]` and `GET /$/server` gain `"id"`,
  `"head"` and `"modified"`. `head` is the head seq and `modified` its timestamp.
  `GET /$/stats/{ds}` gains `"commit"`.

All of these work on read-only servers.

### 2.5 CLI

```
sparkles log --loc DB [--limit N] [--before SEQ | --after SEQ] [--format text|json]
sparkles log --loc DB --at commit:42        # one commit
```

`log` reads `dataset.json` and `commits.bin` **without taking the database lock**, so it
works next to a running server. The text output looks like this:

```
dataset 3f1c9a2e-7b4d-4c1e-9a55-0c2b8e61d7aa  head 2
 seq  timestamp                 kind      +inserted  -deleted    quads  generation
   2  2026-09-30T14:05:00.020Z  update            0         1        1  gen-0001
   1  2026-09-30T14:04:59.910Z  update            2         0        2  gen-0001
   0  2026-09-30T14:04:59.800Z  create            0         0        0  gen-0001
```

`--format json` prints the §2.4 list document without `next`. Inexact counts get a
trailing `~`. The write commands print the commit. `sparkles update` prints
`inserted 3 · deleted 1 · commit 42 · 1.20 ms`, or `no change · head 41`. `load` and
`infer` print `commit N`, and `compact` prints `head N (unchanged)`.

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

`Store::load`/`replace` and the `Dataset::load_*` methods become thin wrappers and keep
their `u64` return types.

### 2.7 UI (SvelteKit, `ui/`)

* **Dataset page** (`routes/datasets/[name]`). The header `<dl>` gains *Commit* and
  *Dataset id*. *Commit* shows `42` with the relative time, and the absolute RFC 3339 time
  as a tooltip. *Dataset id* has a copy button.

  A new **History** panel lists the latest 20 commits in a table. The columns are seq,
  time, a kind badge, `+inserted` / `−deleted`, the quads after the commit, and the
  generation. Inexact counts get a `~` and a tooltip. A thin divider between rows marks a
  generation change, labelled "compacted into gen-0008" or "rebuilt (bulk)". An *Older*
  button follows `next`. The panel refreshes after uploads, reasoning tasks, compaction,
  and updates run from the query page, through the same `app.refreshDatasets()` path.
  When `complete` is false it shows an "incomplete catalog" warning.
* **Query page** (`routes/query`). `api.update` sends
  `Accept: application/x-sparkles+json, application/json;q=0.9`. The update outcome and
  the toast read "+3 / −1 quads · commit 42", or "No change · head 41" when `committed` is
  false. The commit links to the dataset's History panel. Query results show "at commit
  42" next to the timing, taken from the header or `meta.commit`. A Fuseki server sends no
  header, so the note is left out there.
* **Upload panel.** After an upload, the panel shows "Loaded N quads · commit 42".
* **Datasets list** (`routes/datasets`). A *Modified* column shows the head timestamp as a
  relative time.
* **`lib/api.ts`.** It gains `commits(ds, {before, after, limit})`, `commit(ds, ref)` and a
  `Receipt` type. `UpdateResult` gains the optional receipt members.

## 3. Standards basis

* **SPARQL 1.1 Protocol §2.2.4.** A successful update SHOULD use 2XX. The spec says: "The
  response body of a successful update request is implementation defined. Implementations
  may use HTTP content negotiation to provide both human-readable and machine-processable
  information". That permits a receipt behind content negotiation.
* **SPARQL 1.1 Graph Store HTTP Protocol.** A `PUT` that creates content answers `201`,
  otherwise `200` or `204`. A `DELETE` answers `200` or `204`. A `DELETE` returns a receipt
  body, with `200`, only when the client asks for one.
* **Fuseki behavior**, read from the local Jena checkout. `SPARQL_Update` answers an
  `application/sparql-update` body with `204 No Content`. A form update gets a `200`
  "Update succeeded" page. Depending on `Accept`, the page is HTML, text, or
  `{"statusCode":200,"message":…}`. GSP and upload return `ServletOps.uploadResponse`:
  `201` if the target was absent, otherwise `200`, with the body
  `{count, tripleCount, quadCount}`. Sparkles already returns `200` with JSON stats for
  updates, and every Fuseki or Jena client accepts any 2XX. Keeping today's defaults and
  adding only headers therefore stays compatible. Open questions covers byte-exact Fuseki
  status codes.
* **RFCs.** Timestamps follow RFC 3339 and UUIDs follow RFC 9562. RFC 9110 defines the
  status codes and header field semantics. RFC 6648 says new headers take no `X-` prefix.
  Under RFC 9651 the header value is an Integer item.
* **RFC 7089 (Memento)** is the model for the future `?at=time:` and `Accept-Datetime`
  work. That work is out of scope here, but it is the reason timestamps never decrease.

## 4. Semantics

### 4.1 Admission and consistency

* The writer assigns `seq = head + 1` at commit time, under the single writer mutex. An
  aborted transaction consumes nothing, whether it failed on an error, a timeout, a cancel
  or a parse error. The next commit reuses the same `seq`.
* A commit is **acknowledged**, meaning its receipt is returned and its snapshot
  published, only after it is durable. On the delta path that is the WAL fsync. On the
  bulk path it is the `CURRENT` switch. A crash can lose unacknowledged commits, and their
  `seq` may then be reused. No client ever saw them.
* Readers see the `snapshot.commit` of the snapshot they loaded. Publication follows
  durability, so a client that got receipt `n` sees `Sparkles-Commit ≥ n` on every later
  request to the same server. This gives read-your-writes.
* In-memory datasets (`--mem`, `Dataset::memory()`) have commit ids too. Each instance
  gets a random dataset id. The catalog is an in-memory ring of the last 65,536 commits,
  and `firstRetained` advances as it wraps. Nothing is durable.

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

* **Writer poisoning.** An I/O error *after the first WAL byte of a commit is written*
  poisons the writer. This covers a failed `write_all`, `flush` or `sync_data`, and any
  error after a bulk `CURRENT` switch. The store sets `WriterState.poisoned`. Every later
  write transaction fails with a new `Error::Poisoned` (HTTP 503), and reads continue.
  Without poisoning, a failed fsync followed by another commit could leave two WAL commit
  records with the same `seq`. Reopening the store replays whatever is durable.
* **Catalog lag.** The catalog record is appended *after* the commit is durable and before
  the snapshot is published. An append error neither fails the commit, which is already
  durable, nor poisons the writer. The store keeps the record in `Catalog::pending`, sets
  `complete` to `false` and logs the error. Every later commit retries the pending appends
  first. A rebuild refuses to switch `CURRENT` until the catalog is complete and fsynced
  (§5.4). After the switch the old WAL, the only other copy, is deleted.

### 4.4 Limits and configuration

* The catalog takes 64 bytes per commit and keeps every commit, so 10 M commits take about
  640 MB. This feature does no pruning. `firstRetained` exists for recovery (§5.5), for
  in-memory rings and for future pruning.
* `serve` gets no new CLI flags. `StoreOptions` gains `memory_commit_ring: usize`, which
  defaults to 65,536. A hidden test hook,
  `Store::set_clock(Arc<dyn Fn() -> i64 + Send + Sync>)`, replaces the clock.
* Each WAL commit costs one buffered 64-byte `write` to `commits.bin`, with no fsync, and
  a CRC over the transaction's WAL bytes. Net counting adds two `OrdSet` lookups per
  effective insert or delete.

## 5. Design sketch

### 5.1 New and changed modules

* **`crates/sparkles-core/src/commit.rs`** is new. It holds `CommitKind` with its u8 codes and
  JSON names, `CommitInfo`, `Receipt` and `CommitPage`. It formats RFC 3339 timestamps
  with milliseconds, extending `builder::now_rfc3339`. It also holds `DatasetFile` for
  `dataset.json`, `GenCommitFile` for `gen-NNNN/commit.json`, and `Catalog`, which has a
  binary-file variant and an in-memory ring variant.
* **`store.rs`.** `Store` gains `dataset_id` and `catalog: Mutex<Catalog>`. Appends happen
  under the writer mutex. Reads use a separate `pread` handle, so listing never blocks
  writers. `WriterState` gains `head: CommitInfo` and `poisoned: bool`. `Snapshot` gains
  `commit: u64`. `WriteTxn` gains `kind`, `net_ins: u64` and `net_del: u64`. The WAL commit
  record changes as described in §5.3. Replay, `rebuild_locked`, `publish_log` and `open`
  change as described below.
* **`sparql/update.rs`** gains `update_as(.., kind)` and `UpdateStats.commit`.
* **`dataset.rs`** gains the API of §2.6.
* **`sparkles-reasoner`.** `materialize` uses `write_as(Reason)` and `clear` uses
  `write_as(ReasonClear)`.
* **`sparkles-server/http.rs`** gains the headers, receipt negotiation, the `/$/commits`
  routes and the CORS expose list. Every read handler takes the snapshot once and uses its
  `commit` for the header. Today `query_endpoint` calls `ds.store.snapshot()` inside
  `sparql::query`. It should pass the captured snapshot instead. **`main.rs`** gains the
  `Log` subcommand and prints commits.
* **`ui/`** changes as described in §2.7.

### 5.2 Net counting (exact on the WAL path)

`WriteTxn::insert` and `delete` already log only effective changes, because `contains`
checks the view. For an effective change of quad `q` with key `k`, the transaction
computes the following. `in_base` is already computed at that point.

`present_at_start = base.delta.ins[SPO].contains(k) || (in_base(q) && !base.delta.del[SPO].contains(k))`

Then:

* insert: `present_at_start ? net_del -= 1 : net_ins += 1`
* delete: `present_at_start ? net_ins -= 1 : net_del += 1`

The delete case above is swapped. The implementation uses
`present_at_start ? net_del += 1 : net_ins -= 1` (see [Outcome](#outcome)).

When `net_ins == net_del == 0` and the bulk batch is empty, `commit()` creates no commit.
It writes no WAL bytes and publishes nothing. The terms it interned stay in the
append-only delta vocabulary, as they do today for aborted transactions. Replay recomputes
the same numbers for each transaction, with the delta replayed so far as the "start".

### 5.3 WAL commit record, version 2

Records stay 33 bytes. Data records (`1` insert, `2` delete) are unchanged. The commit
record (`op = 3`) puts its 24 spare bytes to use. Today they are always zero.

| bytes | field |
|---|---|
| 0 | `3` |
| 1..9 | `next_bnode` u64 LE (unchanged) |
| 9..17 | `seq` u64 LE (≥ 1) |
| 17..25 | `timestamp_ms` i64 LE (Unix epoch, already clamped) |
| 25 | `kind` code |
| 26 | record version `2` (`0` = legacy record) |
| 27 | flags (`1` = the write bypassed write-time validation) |
| 28 | how many commits just before this one may not have been durable when it was written |
| 29..33 | CRC-32 (IEEE, `flate2::Crc`) over this transaction's data records followed by bytes 0..29 of this record |

Replay in `Store::open` groups records into transactions as it does today, and checks each
commit record:

* Damage to a transaction is a torn tail when no later commit record shows that the
  transaction was durable. Damage here means a CRC mismatch, zero or unknown records, or
  delta ids beyond `delta.vocab`. Replay truncates to the previous commit. Damage in the
  last transaction is always a torn tail.
* Damage that a later commit record shows was durable is `Error::Corrupt`. Dropping
  acknowledged commits silently would reuse their ids. A commit record with byte 28 set
  to `n` shows that commits up to its own seq minus `n + 1` were durable. Ordinary
  commits write zero there, because each is synced before the next is written, so damage
  before the last transaction stays corruption for them.
* In a version 2 record, `seq` must equal the previous seq + 1. The first one must equal
  the generation's `base_seq + 1`. Anything else is `Error::Corrupt`.
* Version 0 records are legacy records, handled as in §5.6.

The id, timestamp and kind live inside the record that already defines durability. A
commit id is therefore durable exactly when its WAL entry is, and replay reproduces it
byte-for-byte.

### 5.4 Generations: `gen-NNNN/commit.json` (format 1)

Each generation records the commit its base index holds:

```json
{ "format": 1, "datasetId": "3f1c…", "baseSeq": 17, "origin": "bulk",
  "commit": { "seq": 17, "timestamp": "2026-09-30T14:03:11.482Z", "kind": "load",
              "inserted": 5000000, "deleted": 0, "quads": 5000120,
              "generation": "gen-0004", "bulk": true, "exact": true } }
```

`origin` is one of four values. `create` and `baseline` mark a root commit. `bulk` means
the rebuild *is* commit `baseSeq`. `compaction` means `commit` is a copy of the head
record, whose `generation` names the generation where that commit was made.
`rebuild_locked` changes in four steps:

1. Before building, it flushes `pending` if the catalog is incomplete, then fsyncs
   `commits.bin`. If either fails, it aborts. Nothing is switched and no seq is consumed.
2. A bulk commit is `WriteTxn::commit` with a bulk batch, or a `load` into an empty or
   large store. It gets `seq = head + 1` and a clamped timestamp. `start` is the committed
   snapshot the transaction began from, and the counts are
   `inserted = meta.quads − start.len() + net_del` and `deleted = net_del`.
   `exact = (net_del == 0)`. Compaction keeps `seq = head`.
3. After `Builder::finish`, it calls `write_synced(dir/commit.json)`, then the existing
   `sync_dir(dir)`, `sync_dir(root)` and `write_atomic(CURRENT)`. **The `CURRENT` switch
   is the commit point**, as today. The new generation's WAL starts empty, and its first
   commit will carry `baseSeq + 1`.
4. After the switch, it appends the catalog record (bulk only) and publishes a snapshot
   with `commit = seq`. Errors in this step poison the writer (§4.3). That includes the
   existing `add_prefixes(...)?` between the switch and `current.store`.

`Store::open` reads the `commit.json` of the `CURRENT` generation, sets `head = baseSeq`,
and then replays the WAL.

### 5.5 Catalog: `<root>/commits.bin` (format 1)

The catalog holds fixed-size little-endian records, so commit `s` is at offset
`64 + (s − first_seq)·64`. Lookups and paging are O(1), and a later binary search on
timestamps is possible.

Header (64 bytes): `0..8` magic `SPKCMTS\0` · `8..12` format `1` · `12..16` record size
`64` · `16..32` dataset UUID bytes · `32..40` `first_seq` · `40..60` zero · `60..64`
CRC-32 of 0..60.

Record (64 bytes): `0..8` seq · `8..16` timestamp_ms · `16..24` inserted · `24..32`
deleted · `32..40` quads · `40..44` generation number · `44` kind · `45` flags (bit0 exact,
bit1 bulk, bit2 reconstructed) · `46..60` zero · `60..64` CRC-32 of 0..60.

Records are appended with `write_all` and `flush`, with no fsync per commit. The WAL or
`commit.json` is the durable source. The file is fsynced before every `CURRENT` switch and
when the store is dropped.

**Repair on open** runs after WAL replay has established `head`:

* Trailing records that are partial, fail their CRC, or have `seq > head` are dropped with
  a warning. Any of these means lost page-cache writes or a hand-edited directory.
* If `last < baseSeq`, the record from `commit.json` is appended. That record is always
  present. If `last < baseSeq − 1`, the history in between is lost, which happens only if
  the file was deleted or corrupted. In that case a new catalog starts at
  `first_seq = baseSeq`, the old file is renamed to `commits.bin.corrupt-<ts>`, and
  `firstRetained` reports the gap.
* The replayed WAL commits with `seq > last` are appended, with counts from §5.2.
* If the dataset UUID in the header differs from `dataset.json`, open fails with
  `Error::Corrupt`.

`sparkles log` takes no lock. It reads only complete records with valid CRCs, up to the
last one.

### 5.6 Dataset file, creation, migration

`<root>/dataset.json` (format 1):
`{ "format": 1, "id": "3f1c…", "created": "2026-09-30T14:04:59.800Z", "origin": "create" | "baseline" }`.

* **New database** (`CURRENT` absent). Write `dataset.json`, or reuse it if an
  interrupted create left one. Build `gen-0001`, then write `gen-0001/commit.json` with
  `baseSeq 0`, kind `create` and quads 0. Write `commits.bin` with record 0. Write
  `CURRENT` last.
* **Pre-existing database** (`CURRENT` present, no `dataset.json`). Open and replay as
  today, treating version-0 commit records as legacy. Then write the baseline root commit.
  It has `seq 0`, kind `baseline`, `inserted = quads` (everything present), `deleted 0`
  and the current time as its timestamp. Write it to `gen-NNNN/commit.json` (`baseSeq 0`,
  origin `baseline`), then to `commits.bin`, and write `dataset.json` last. Once
  `dataset.json` exists the migration is complete, so an interrupted migration is redone.
  Legacy records *before* the first version-2 record are folded into `baseSeq`
  (commit 0).
* **Legacy records after a version-2 record.** These appear when an older binary appends
  to an upgraded database. Each gets `seq = prev + 1`, kind `unknown`, the parent's
  timestamp and the `reconstructed` flag. The rule is deterministic, so every replay
  assigns the same ids. Downgrading is still unsupported, but it corrupts nothing.

### 5.7 Server plumbing

* `update_endpoint` passes `CommitKind::Update`. `gsp` uses `replace_as(.., GspPut)`,
  `load_as(.., GspPost)` and `update_as("CLEAR …", .., GspDelete)`. `upload` uses
  `load_as(.., Upload)`. The GSP and upload `count` still means "new quads". It equals
  `receipt.commit.inserted` when the write committed, and 0 otherwise. The racy
  subtraction of `snapshot().len()` before and after the write is no longer needed.
* `fn receipt_wanted(params, headers) -> bool` returns true for `receipt=true`, or for an
  `Accept` that lists `application/x-sparkles+json` explicitly. `*/*` does not count.
* One helper, `commit_headers(ds_id, seq)`, sets the headers. `CorsLayer` gets
  `.expose_headers([SPARKLES_COMMIT, SPARKLES_DATASET_ID])`.
* There are two new routes, `/$/commits/{ds}` and `/$/commits/{ds}/{ref}`, both GET only.

## 6. Phasing

**Phase 1 (MVP, ~1 day)** contains:

* the `commit.rs` types;
* `dataset.json` and the root commit, for both create and the baseline migration;
* WAL commit record v2 with seq, timestamp, kind and CRC, and the replay checks;
* net counting;
* `gen/commit.json` on rebuild and compaction;
* `Snapshot.commit`, `head_commit` and `dataset_id`;
* appending to `commits.bin` and repairing it on open;
* writer poisoning;
* `CommitKind` passed through update, GSP, upload, load and the reasoner;
* `Sparkles-Commit` and `Sparkles-Dataset-Id` on write and query responses;
* receipt negotiation;
* `GET /$/commits/{ds}` (with `limit`, `before` and `after`) and `/{ref}`;
* `sparkles log`;
* the tests of §7 A1–A12.

**Phase 2** adds the UI (§2.7) and the headers on `explain`, `shacl` and GSP `HEAD`. It
also adds `meta.commit` in Sparkles JSON, `id`, `head` and `modified` on `/$/datasets`,
the bound on the in-memory ring, and `sparkles log --at`.

**Phase 3** adds an optional per-commit **change digest** (§9), off by default. It also
adds `ETag: W/"<datasetId>:<seq>"` and `If-None-Match` on GSP `GET`, and a side-file of
commit annotations. Clients send a message in a `Sparkles-Commit-Message` request header.

**Later work** has its own specs. Retained generations and `?at=commit:N|time:…` belong to
[F06](F06-snapshots-and-point-in-time.md). Search indexes that record their `seq`
watermark belong to [F03](F03-full-text-search.md) and [F04](F04-vector-search.md). A
long-poll change feed on `GET /$/commits/{ds}?after=` and catalog pruning are also later
work.

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

* **`Snapshot::version` as the commit id.** It lives in one process and resets on restart,
  and compaction increments it without changing data.
* **Random or UUID commit ids, or content hashes, as the primary id.** They are not
  ordered. Watermarks, paging and `?at=` all need `a < b` comparisons and O(1) lookup. A
  digest can be added as an attribute in Phase 3.
* **A separate WAL record type for commit metadata.** An older binary stops at the unknown
  opcode and *truncates* the WAL on open, so a downgrade would lose data. Old binaries
  ignore the spare bytes of the existing commit record.
* **An fsync of the catalog on every commit.** It doubles the fsyncs per write. The catalog
  can be rebuilt from the WAL and `commit.json`. It only has to be durable before a
  generation's WAL is discarded.
* **A JSON-lines catalog.** Paging and seq lookup would need a scan or a side index. Fixed
  records are simpler and faster, and `sparkles log --format json` covers inspection by
  people.
* **Counting raw WAL records as inserted and deleted.** That double-counts an insert
  followed by a delete in one transaction. Diffing the delta sets at commit costs more
  than the O(1) bookkeeping of §5.2.
* **A commit for compaction.** Two ids would name identical content, and search watermarks
  would re-index for nothing. The `generation` field still shows generation changes.
* **Empty commits for no-op writes.** They add noise to the catalog and break the rule that
  each commit changed something. The receipt returns the head instead.
* **Fuseki's exact `204` or HTML page as the default update response.** That would break
  existing Sparkles clients, because the UI parses the JSON. The receipt is opt-in instead
  (see Open questions).
* **The commit in `ETag` only.** An ETag has cache-validation semantics for each
  representation (RFC 9110 §8.8.3). A query result also depends on parameters and
  non-deterministic functions. ETag is a Phase 3 addition for GSP `GET`, not the main
  carrier.
* **An `X-Sparkles-Commit` header name.** RFC 6648 deprecates the `X-` prefix.
* **The base seq in `CURRENT`.** Older binaries read `CURRENT` verbatim as a directory
  name. They never see `gen-NNNN/commit.json`.

## 9. Open questions

1. **Directory copies share an id.** Should `Store::open` detect a copy and mint a new id?
   It could record the absolute path or inode in `dataset.json`, for example.
   Clone-to-sandbox ([C06](C06-clone-to-sandbox.md)) should mint a new id and record
   `"forkedFrom": {"id", "seq"}`. Default: copies keep the id.
2. **Fuseki-exact defaults.** Should update requests with `Accept: */*` or no `Accept` get
   `204`, as in Fuseki, instead of today's `200` JSON? Separately, GSP `POST` currently
   always returns `201` and `PUT` always `200`, while Fuseki returns `201` iff the target
   was absent. That is worth aligning in a separate fix. Default: unchanged.
3. **Change digest (Phase 3).** This is a proposal, and it is not canonical RDF:
   `digest(n) = SHA-256(
   "sparkles-commit-digest-v1\n" ‖ datasetId ‖ "\n" ‖ seq ‖ "\n" ‖ hex(digest(n−1)) ‖ "\n"
   ‖ timestamp ‖ "\n" ‖ kind ‖ "\n" ‖ sorted lines "-" + L(q) for deleted q ‖ sorted lines
   "+" + L(q) for inserted q)`. `L(q)` is the quad in canonical N-Quads form (RDF 1.2
   N-Quads §3), with blank nodes written as their store-internal labels (`bnode_for(id)`).
   Lines are sorted by UTF-8 bytes, and `digest(−1)` is 32 zero bytes. Because blank-node
   labels are internal to the store, equal digests from different databases mean nothing.
   It is not an RDF Dataset Canonicalization (RDFC-1.0) hash. Bulk commits would need
   their change set materialized. It is open whether they get `digest = null` or pay that
   cost. Default: not implemented.
4. **Timestamp source.** The wall clock, clamped so it never decreases. A large backwards
   jump of the clock freezes timestamps until the wall clock catches up. Should the server
   warn?
5. **Catalog pruning** by age or count, once point-in-time retention exists. Default: keep
   everything.
6. **Commit messages and authors** need variable-length storage, such as an annotation
   side-file keyed by seq. They are deferred to Phase 3 so the fixed record stays 64
   bytes.
7. **UUID version.** v4 is random and already enabled in the workspace `uuid` crate. v7
   would embed the creation time. Default: v4, with the creation time in `dataset.json`.

## 10. Sources

* Sparkles repository (read): `README.md`, `docs/API.md`, `docs/AUDIT.md`, the
  project's feature-ordering notes (this feature's scope and dependants),
  `crates/sparkles-core/src/store.rs` (generations, WAL format, replay, `publish_log`,
  `rebuild_locked` publication order, `write_atomic`/`sync_dir`),
  `crates/sparkles/src/dataset.rs`, `crates/sparkles-core/src/sparql/update.rs`
  (`UpdateStats`, per-op counting), `crates/sparkles-core/src/builder.rs` (`IndexMeta`,
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

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30 in two commits. `0374949` changed the engine.
It added dataset ids, WAL commit record version 2, net counting, `gen-NNNN/commit.json`,
`commits.bin` with repair on open, and writer poisoning. `f128404` changed the server and
CLI. It added the headers, receipt negotiation, the `/$/commits` routes and
`sparkles log`.

Phase 2 followed the same day. `9b8cacb` added the UI's commit history panel and write
receipts, and `cb598c8` added commit headers on SHACL validation. Dataset entries have
`id`, `head` and `modified`. The Sparkles JSON format has `meta.commit`.
`sparkles log --at` works, and in-memory datasets keep a ring of 65,536 commits
(`StoreOptions::memory_commit_ring`).

Later features build on the receipts. Search watermarks ([F03](F03-full-text-search.md)),
`?at=` reads and named snapshots ([F06](F06-snapshots-and-point-in-time.md)), clones that
record `forkedFrom` ([C06](C06-clone-to-sandbox.md)), backups and restores
([F05](F05-snapshot-repositories.md)) and write-time validation summaries
([C10](C10-write-time-validation.md)) all use them.

**Deviations from the spec.**

- §5.2 swapped the delete case. The implementation counts deleting a quad that was present
  at the start of the transaction as `net_del += 1`. Deleting a quad that the same
  transaction added counts as `net_ins -= 1`.
- The catalog is kept in memory as a ring of 64-byte records, not read with `pread`. A
  million commits take about 64 MiB. If histories grow long, reading from the file is
  still an option.
- Legacy WAL records are folded into the base only when the generation's base is a
  baseline commit, or during the upgrade itself. Otherwise they are reconstructed with
  kind `unknown`, as §5.6 describes for records after a version-2 record.
- Graph Store PUT still reports the parsed count as `count`, as before. The spec said "new
  quads". POST and upload report the commit's net inserted count, which equals the old
  number.
- `sparkles log` lists 20 commits unless `--limit` says otherwise.
- Byte 28 of the commit record, reserved in §5.3 as first written, counts the commits
  before it that may not have been durable yet. Experimental group commit writes up to
  64 commits before their shared sync, so a crash can damage any of them, not just the
  last. Replay treats such damage as a torn tail unless a later commit record shows the
  damaged commit was durable. The count reaches the disk with the next sync, so the
  records of the most recent group cannot prove each other durable. Damage to them after
  they were acknowledged, which only a failing disk causes, is truncated like a torn last
  transaction.

**Open questions.** Directory copies still share an id (question 1). Clones, and restores
into a new lineage, mint a new id and record `forkedFrom`. Default response bodies are
unchanged (question 2). The non-decreasing clock clamp and v4 UUIDs are as proposed. The
catalog is not pruned.

**Tests at landing.** `crates/sparkles-core/tests/commits.rs` covers the root commit, net
counting, restart and replay, a lost catalog tail, a torn WAL tail, compaction and bulk
commits, migration, the monotone clock and the library API. Router tests cover receipts,
headers, the catalog routes, paging and errors. The W3C SPARQL results did not change.

**Phase 3** landed on 2026-10-02 in two commits. `8f9fb00` changed the engine. It
added the annotation side-file, commit messages, change digests and write preconditions.
`37961d8` changed the server and CLI. It added entity tags, conditional Graph Store
requests, the `Sparkles-Commit-Message` header and `--message`.

* **Annotations.** `<root>/annotations.bin` holds one record per annotated commit: the
  seq, a flags byte, an optional 32-byte digest, an optional message and a CRC-32. A
  32-byte header holds a magic, the format, a flag that turns digests on, and the dataset
  id. The record is appended and synced before the commit point, which is the WAL fsync
  or the `CURRENT` switch of a bulk commit. A record whose commit never became durable
  has a seq above the head at the next open, which truncates it with any torn tail. A
  record left by a commit that failed in the same process is cut off at once. Backups
  carry the file, and a restore into a new lineage rewrites its dataset id.
* **Messages.** `WriteOptions::message` carries a message to the commit.
  `annotations::validate_message` trims it and accepts at most 1024 bytes of UTF-8 with
  no control characters. A message that is empty after trimming is no message. The HTTP
  header also accepts an RFC 8187 value (`UTF-8''…`), because browsers cannot send other
  non-ASCII header values. Receipts, `/$/commits`, `/$/commits/{ds}/{ref}` and
  `sparkles log` show it as `message`. A receipt without a commit shows the head's
  message.
* **Digests.** `StoreOptions::commit_digests`, or the CLI's global `--commit-digests`,
  turns digests on. The flag in the side-file keeps them on for later openings. The
  input to SHA-256 is the line `sparkles-commit-digest-v1`, then one line each for the
  dataset id, the seq, the parent's digest in hex, the RFC 3339 timestamp and the kind
  name. Then come `-` and the canonical N-Quads line of each deleted quad, sorted by
  UTF-8 bytes, and `+` and the line of each inserted quad, sorted the same way. Every
  line ends with `\n`, and a quad line ends with ` .`. Only net changes count, so a quad
  inserted and deleted in one transaction is left out. The empty `create` root gets a
  digest with a zero parent.
* **Entity tags.** Graph Store `GET` and `HEAD` send `ETag: W/"<datasetId>:<seq>:<format>"`
  and, when the format was negotiated, `Vary: Accept`. `If-None-Match` answers `304`.
  `If-Match` and `If-None-Match` on `PUT`, `POST` and `DELETE` become a
  `guard::Precondition` that the store checks on the head snapshot right after it takes
  the writer lock. A failure is `Error::PreconditionFailed`, which the server sends as
  `412` with `code: "precondition-failed"`. CORS allows the two request headers and the
  message header, and exposes `ETag`.

**Phase 3 deviations.**

- The tag has a third part, the serialization, which §6 did not have. Turtle and
  N-Triples of one commit then have different tags, so a cache that keeps both variants
  cannot confuse them on revalidation (RFC 9111 §4.3.4).
- The tag is weak, as §6 proposed, but `If-Match` still works with it. RFC 9110 asks
  `If-Match` to use the strong comparison, under which a weak tag never matches. Sparkles
  matches a tag when it names the current head in any serialization. A tag identifies
  the data exactly, which is what a concurrency check needs. It cannot promise identical
  bytes, because a compaction reorders the output and a prefix change rewrites Turtle.
- Tags are per dataset, not per graph. The catalog does not record which graphs a commit
  touched, and bulk commits do not know it cheaply, so any commit changes every graph's
  tag.
- Query responses get no tag, as §8 decided.
- Bulk commits get no digest. This answers open question 3. A commit after one without
  a digest chains from 32 zero bytes.
- Messages are single lines. The rule against control characters rejects newlines in the
  library as well as over HTTP, where a header cannot carry them.

**Phase 3 tests.** `crates/sparkles-core/tests/annotations.rs` covers messages through
updates, replaces and bulk loads, their survival across reopen, lost and torn
annotations, digest values and chaining, the sticky setting, failed preconditions and
concurrent writers with one precondition. `http/conditional_tests.rs` covers tags per
commit and format, `304` on `GET` and `HEAD`, tags at `?at=`, `If-Match` on reads and
writes, `If-None-Match: *` creation, concurrent `PUT`s with one tag, messages on updates,
Graph Store writes and uploads, their rejection, and CORS.

**Not built in Phase 3.** Catalog pruning and the long-poll change feed were left to
later work, and both landed afterwards, as the next paragraph describes. The UI showed
no messages at first. Its commit graph now shows each commit's message in the list and
in the commit's details. `/{ds}/update` takes no `If-Match`, because a tag names a
representation of a graph and an update has none.

**Pruning and the change feed** landed on 2026-10-02, after Phase 3. This answers open
question 5.

* **Catalog horizon.** `history.json` holds an optional horizon,
  `catalog: {keepCommits, keepAgeMs}`, which is off by default. The catalog then drops
  the records, messages and digests of commits that are older than both the oldest
  readable commit and the horizon. A commit is kept if either limit keeps it, and pins
  keep their commits because they keep them readable. Pruning holds the writer lock. It
  writes a new `commits.bin` whose header names the new first commit, and a new
  `annotations.bin`, and each replaces the old file with `write_atomic`. Readers without
  the lock, such as `sparkles log`, see either file whole. The head is never pruned, so
  commit numbers stay monotonic, and the next digest chains from the head's. The history
  upkeep prunes once at least 1,024 records, and an eighth of the catalog, can go, so
  the file is not rewritten every minute. Setting the horizon, `Store::prune_commits` and
  `sparkles snapshot gc` prune at once. A backup opens `commits.bin` under the writer
  lock, so a rewrite cannot change the file between capture and upload. A pruned
  catalog is not append-only, so the next backup stores it whole. `sparkles check`
  accepts a catalog that starts after commit 0, and the quota measures the directory
  again after a prune.
* **Change feed.** `Store::changes` and `GET /{ds}/changes` list the commits after a
  commit with their changes, and `Store::subscribe_commits` announces each published
  commit. See [F06 Outcome](F06-snapshots-and-point-in-time.md#outcome) for the feed,
  its formats and its budgets.

**Tests.** `crates/sparkles-core/tests/annotations.rs` prunes a catalog with messages and
digests on, through a pin, a reopen, an offline read, `sparkles check` and a backup
restore. `http/diff_tests.rs` covers the horizon over HTTP.

**Write-through WAL commits** landed on 2026-10-09 in `09d03a69`. The WAL record format
did not change. On Linux, the writer opens a second handle on `wal.log` with `O_DIRECT`
and `O_DSYNC`. A commit of up to 64 KiB that fits inside the preallocated zero space is
written through that handle at the log's logical end, and the write returns once the
bytes are durable, in place of the buffered write and `fdatasync`. The blocks are already
allocated and written, so the write changes no file metadata. On ext4 and XFS the kernel
can then complete it as a forced-unit-access write of those blocks rather than a flush of
the device's whole cache, when the device supports FUA.

The write covers whole 4 KiB blocks. The writer keeps the last partial block in memory,
or reads it back through the direct handle when it does not, and rewrites it with the
same durable prefix followed by the new records and zeros. A torn write can therefore
damage only records that were never acknowledged, which replay already treats as a torn
tail. Dirty update-vocabulary terms are synced on the vocabulary worker while the WAL
write runs, and the commit is acknowledged only after both finish, as on the buffered
path. A commit larger than 64 KiB, or one that needs more preallocated space, takes the
buffered path. If the file system rejects direct I/O with `EINVAL`, the writer falls
back to the buffered path for that log. `StoreOptions::wal_direct_writes` turns the path
off, and so does the hidden server flag `--no-wal-direct-writes`. Compaction and rebuild
reopen the handle for the new log.

Directional measurements compared the same binary with and without the flag in paired,
interleaved runs. On a busy laptop with NVMe, the median single-triple HTTP update took
0.93 times as long with direct writes on the 1.05M-triple store, and 0.82 times as long
on the 10.5M-triple store. On a Namespace 8x16 instance at 1.05M, whose virtual disk is
write through and reports no FUA, the pooled median was 0.53 ms against 0.56 ms, but
the two rounds disagreed on the direction. Serial churn with literal
inserts did not change measurably there. Those commits wait on the update vocabulary's
sync, which appends to `delta.vocab` and needs a file-system journal commit, and that
cost now dominates them.

**Tests.** A unit test in `store/wal.rs` mixes direct and buffered commits and compares
the file bytes, the zero tail and the size limits. `tests/commits.rs` runs the same
commits with and without direct writes, including an 80 KiB commit that takes the
buffered path. It restores a crash image of the log with its zero tail and checks that
the head, the counts and the data survive a reopen. Another test checks that the handle
follows compaction to the new log.
