# F06: Named snapshots and point-in-time queries

> **Status:** implemented in part
>
> **Phases:** Phase 1 shipped (point-in-time reads with `?at=`, named snapshots, the
> `keepCommits`/`keepAge` retention window, `/$/snapshots`, `/$/history`, the
> `sparkles snapshot` CLI, `query --at` and `dump --at`). Phase 2 (diffs,
> `Accept-Datetime`, `maxBytes` retention and scheduled pins, history for in-memory
> datasets, the UI's snapshot panel and `at` selector) and Phase 3 are not built.
>
> **User docs:** [API: Point-in-time reads and snapshots](../API.md#point-in-time-reads-and-snapshots) ·
> [Features](../FEATURES.md#storage-tdb2-equivalent) ·
> [Benchmarks: Point-in-time reads](../BENCHMARKS.md#point-in-time-reads-105m-triples)
>
> This is the design as written before implementation; the [Outcome](#outcome) section at
> the end records how it landed.

A clean-room design. It builds on durable commit identity
([CI](CI-commit-identity.md)), which reserved `?at=commit:<n>` and made commit timestamps
non-decreasing for a `time:` selector, and it delivers the "retained generations and
`?at=`" follow-up named in [CI §6](CI-commit-identity.md).

## 1. Summary, goals, non-goals

Sparkles already keeps commit *metadata* for every commit (`commits.bin`). It does not
keep old *data*:

* compaction and bulk commits build `gen-N+1`, switch `CURRENT`, and then
  `remove_dir_all` the old generation (`Store::rebuild_locked`);
* in-memory `Snapshot`s are dropped as soon as no reader holds them.

Yet the data for every commit since the current generation's base is still on disk. It
is the base index plus a prefix of `gen-NNNN/wal.log`. Commits in older generations are
recoverable exactly while their generation directory exists.

This feature makes that history addressable and lets users keep it:

* **Point-in-time reads.** `?at=<ref>` on `/{ds}/sparql|query`, `/{ds}` queries,
  `/{ds}/explain` and GSP `GET/HEAD`. A ref is `head`, `42`, `commit:42`,
  `time:<RFC 3339>` or `snapshot:<name>`. Every commit that is still *reconstructable*
  can be read.
* **Named snapshots.** A named snapshot is a durable name for a commit (a *pin*) that
  keeps its data reconstructable across compaction and bulk commits. They can be
  created, listed and deleted over HTTP, the CLI and the library.
* **Retention.** An optional per-dataset window that keeps every commit of the last
  N commits or of the last D duration reconstructable. It is off by default.
* **Precise failure.** A commit that is no longer reconstructable returns `410` with a
  machine-readable body naming what is available.
* **Accounting.** Retained generations are visible with their bytes and the pins that
  hold them. Garbage collection is crash-safe.

**Goals**

* No change to the write path or its fsync count.
* Zero extra disk when nothing is pinned (the current generation already gives free
  history).
* Bounded memory for materialized history.
* Crash-safe interplay of pins, GC and rebuilds.

**Non-goals (Phase 1)**

* Diffs between commits (Phase 2).
* History queries across commits (for example "when did this triple change").
* Branches, or writes at a past commit.
* Full-text search at past commits.
* History for in-memory datasets.
* Remote or tiered storage of old generations.

## 2. User-visible behavior

### 2.1 The `at` selector

```
at       = "head" | seq | "commit:" seq | "time:" datetime | "snapshot:" name
seq      = 1*20DIGIT                     ; a u64
datetime = RFC 3339 §5.6 date-time       ; any offset, e.g. 2026-09-30T14:03:11.482Z, …+02:00
name     = [A-Za-z0-9][A-Za-z0-9._-]{0,63}
```

* `head` (or an absent `at`) is today's behavior: the live snapshot.
* `42` and `commit:42` name the state right after commit 42 ([CI §2.1](CI-commit-identity.md)).
* `time:T` names the state as of instant T. That is the **last commit whose timestamp
  is ≤ T**, found by binary search in the catalog, where timestamps are non-decreasing.
  Fractional seconds beyond milliseconds are truncated. A `+` in a query string decodes
  to a space, so the parser also accepts a single space where the offset sign belongs
  (`…14:03:11 02:00` means `+02:00`). `-` offsets and `Z` need no care.
* `snapshot:NAME` names the commit a named snapshot pins.
* The selector is read from the query string, or from the form body of a
  `application/x-www-form-urlencoded` POST, like `timeout`. If it appears twice with
  different values, the request is rejected with `400`.
* If `at` resolves to the current head, the **live snapshot** is used: the full-text
  index works, and there are no Memento headers (§2.3). Otherwise the request reads a
  **historical snapshot** (§5.3).

Canonical form, used in headers and JSON: `head`, `commit:N`, `time:<rfc3339 ms Z>`,
`snapshot:NAME`.

### 2.2 Where `at` applies

| Endpoint | `at` |
|---|---|
| `GET/POST /{ds}/sparql`, `/{ds}/query`, `/{ds}?query=` | Phase 1 |
| `GET/POST /{ds}/explain` | Phase 1 |
| GSP `GET/HEAD /{ds}/data`, `/{ds}/get`, `/{ds}` | Phase 1. `404 no such graph` is evaluated at the selected commit |
| `/{ds}/update`, GSP `PUT/POST/DELETE`, `/{ds}/upload` | `400 writes cannot target a past state (at=commit:3)`. This also applies to `at=head`, so the mistake stays visible |
| `/$/schema/{ds}…`, `/$/stats/{ds}`, `/{ds}/shacl`, `/$/datasets/{ds}/clone`, `/$/backup/{ds}` | Phase 2 |

### 2.3 Response headers on reads with `at`

```
Sparkles-Commit: 42                        (the commit evaluated; as today)
Sparkles-Dataset-Id: 3f1c9a2e-…            (as today)
Sparkles-At: snapshot:release-1            (canonical selector as requested)
Sparkles-Head: 57                          (head when the request was evaluated)
Memento-Datetime: Wed, 30 Sep 2026 14:03:11 GMT     (GET/HEAD, and only when 42 < head)
Link: </ds/data?graph=urn:g>; rel="original"        (sent with Memento-Datetime)
```

* `Memento-Datetime` is commit 42's timestamp in RFC 1123 form, truncated to seconds
  (RFC 7089 §2.1.1). RFC 7089 requires `Link rel="original"` whenever it is sent. Its
  value is the request path and query with `at` removed.
* All four `Sparkles-*` names are added to CORS `Access-Control-Expose-Headers`.
* The Sparkles JSON result format gains
  `meta.at = { selector, commit, timestamp, head, historical }`.
* `Sparkles-Inferences` at a past commit is described in §2.8.
* Phase 2 adds caching headers: for GSP GET with a `commit:` selector,
  `ETag: W/"<datasetId>:<seq>"` and `Cache-Control: max-age=31536000, immutable`
  (RFC 8246). The ETag is weak because serialization order and blank-node allocation
  follow generation-local ids, so the same commit read from two generations is
  semantically, not byte-for-byte, identical (RFC 9110 §8.8.1). Query responses get no
  such headers, because `NOW()`, `RAND()` and `SERVICE` make them non-deterministic.

### 2.4 Errors

Bodies keep the `{ "error": … }` shape and add a stable `code`.

| Condition | Status | `code` | Example message |
|---|---|---|---|
| `at` malformed, repeated with different values, name invalid | 400 | `invalid-at` | `invalid at 'x': expected head, N, commit:N, time:<RFC 3339> or snapshot:<name>` |
| `at` on a write | 400 | `at-on-write` | `writes cannot target a past state (at=commit:3)` |
| `seq > head` | 404 | `no-such-commit` | `no commit 99 in dataset ds (head is 57)` |
| `time:` before the root commit | 404 | `before-history` | `no commit at or before 2020-01-01T00:00:00.000Z (dataset created 2026-09-30T14:04:59.800Z)` |
| unknown snapshot name | 404 | `no-such-snapshot` | `no snapshot 'v9' in dataset ds` |
| commit exists but its data is gone, or its metadata is gone (`< firstRetained`) | **410** | `history-gone` | see below |
| in-memory dataset, `at` ≠ head | 501 | `history-unsupported` | `point-in-time reads need a persistent dataset` |
| `text:query` at a historical snapshot | 501 | `text-not-at-commit` | `full-text search is only available at the head (commit 57); this query reads commit 42` |
| materialization exceeds the history memory budget | 507 | `budget: "history-memory"` | as other budgets (`limit`, `requested`) |
| timeout or cancel during materialization | 408 / 503 | — | as today |
| sealed WAL fails its CRC before the target commit | 500 | — | `corrupt index: gen-0003/wal.log: checksum mismatch …` |

The `410` body:

```json
{ "error": "commit 12 of dataset ds is no longer reconstructable; the oldest reconstructable commit is 40",
  "code": "history-gone", "at": "commit:12", "commit": 12, "head": 57,
  "oldestReconstructable": 40,
  "reconstructable": [ { "from": 40, "to": 57 } ],
  "metadata": { "seq": 12, "timestamp": "…", "kind": "update", … } }
```

`metadata` is the commit's catalog record, or `null` if that is gone too. For
`snapshot:NAME`, the body adds `"snapshot": "NAME"`, and the pin is kept (§4.4).
`410` fits because the loss is permanent (RFC 9110 §15.5.11).

### 2.5 Named snapshots and history over HTTP

| Method | Path | Result |
|---|---|---|
| GET | `/$/snapshots/{ds}` | `200 { dataset, datasetId, head, snapshots: NamedSnapshot[] }`, sorted by commit, then name |
| POST | `/$/snapshots/{ds}` | Body JSON `{ "name", "at"?: ref (default "head"), "note"?: string ≤ 1024 bytes }`, or the same as query/form params. `201` + `Location: /$/snapshots/{ds}/{name}` + body `NamedSnapshot`. `200` if the name already pins the same commit (idempotent retry) |
| GET | `/$/snapshots/{ds}/{name}` | `200 NamedSnapshot` / `404` |
| DELETE | `/$/snapshots/{ds}/{name}` | `204`. Unneeded generations are collected before the response |
| GET | `/$/history/{ds}` | `200 HistoryStatus` |
| PUT | `/$/history/{ds}` | Body `Retention`. `200 HistoryStatus` after GC |

```ts
type NamedSnapshot = {
  name: string; ref: string;              // "snapshot:release-1"
  commit: Commit;                         // CI §2.4 Commit object
  created: string;                        // RFC 3339 ms, when the pin was made
  note: string | null;
  generation: string;                     // generation that would serve it now
  reconstructable: boolean;               // false only after external damage (§4.4)
};
type Retention = {
  keepCommits: number | null;             // keep the last N commits reconstructable
  keepAge: string | number | null;        // "7d", "12h", "30m", "90s", "2w", or seconds
};
type HistoryStatus = {
  dataset: string; datasetId: string; head: number;
  oldestReconstructable: number;
  reconstructable: { from: number; to: number }[];     // ascending, disjoint
  bytes: number;                                         // retained non-current generations
  generations: {
    name: string; baseSeq: number; endSeq: number; bytes: number; current: boolean;
    heldBy: string[];                     // "head", "snapshot:<name>", "retention"
  }[];
  retention: Retention;
  snapshots: number;
  limits: { maxSnapshots: number; maxGenerations: number; cacheBytes: number };
  cache: { entries: number; bytes: number; hits: number; misses: number };
};
```

Snapshot errors:

* `400` for a bad name or note, or a malformed `at`.
* `404` for an unknown dataset or snapshot.
* `409` when the name already pins a different commit (the body includes the existing
  `NamedSnapshot`).
* `409` with code `history-limit` when `maxSnapshots` is reached, or when the pin would
  make more than `maxGenerations` generations pinned (§4.5).
* `410` when `at` is not reconstructable.
* `403 server is read-only` for POST, DELETE and PUT.

The existing commit endpoints change as follows:

* `GET /$/commits/{ds}` gains `oldestReconstructable` and `reconstructable` (ranges).
* Each `Commit` in the list gains `"reconstructable": bool`, and `"snapshots": [names]`
  when the commit is pinned.
* `GET /$/stats/{ds}` gains
  `history: { bytes, generations, snapshots, oldestReconstructable }`.

### 2.6 CLI

```
sparkles snapshot create  --loc DB NAME [--at REF] [--note TEXT]
sparkles snapshot list    --loc DB [--format text|json]          # lockless, like `log`
sparkles snapshot delete  --loc DB NAME
sparkles snapshot history --loc DB [--format text|json]          # lockless HistoryStatus
sparkles snapshot retain  --loc DB [--keep-commits N] [--keep-age DUR] [--off]
sparkles query --loc DB --at REF …          # historical query
sparkles dump  --loc DB --at REF            # N-Quads of that state
sparkles serve … --history-cache-mb 1024 --history-max-generations 8 --max-snapshots 256
```

* `create`, `delete` and `retain` take the database lock. If a server holds it, they
  print the existing "in use by another process … talk to it over HTTP" message.
* `list` and `history` read `history.json`, `commits.bin` and the `gen-*/commit.json`
  files without the lock. Their text output:

```
snapshot     commit  timestamp                 generation  note
release-1        42  2026-09-30T14:03:11.482Z  gen-0003    before migration
nightly          57  2026-09-30T23:00:00.000Z  gen-0005
reconstructable: 40..57   retained: gen-0003 (1.2 GiB, snapshot:release-1)
```

* `query --at` and `dump --at` print `at commit 42 (2026-09-30T14:03:11.482Z)` on stderr.
* `sparkles log` marks reconstructable commits with `*` in Phase 2.

### 2.7 Rust library API

```rust
// crates/sparkles/src/history.rs (new)
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum At { Head, Commit(u64), Time(i64 /* ms */), Snapshot(String) }
impl std::str::FromStr for At { type Err = Error; }          // §2.1 grammar
impl std::fmt::Display for At { /* canonical form */ }

pub struct Resolved { pub at: At, pub commit: CommitInfo, pub head: u64, pub historical: bool }
pub struct NamedSnapshot { pub name: String, pub commit: CommitInfo, pub created_ms: i64,
                           pub note: Option<String>, pub reconstructable: bool }
#[derive(Clone, Copy, Default)]
pub struct Retention { pub keep_commits: Option<u64>, pub keep_age_ms: Option<u64> }
pub enum Hold { Head, Snapshot(String), Retention }
pub struct HistoryGeneration { pub name: String, pub base_seq: u64, pub end_seq: u64,
                               pub bytes: u64, pub current: bool, pub held_by: Vec<Hold> }
pub struct HistoryStatus { pub head: u64, pub reconstructable: Vec<RangeInclusive<u64>>,
                           pub generations: Vec<HistoryGeneration>, pub bytes: u64,
                           pub retention: Retention, pub snapshots: usize, pub cache: CacheStats }
#[derive(Clone, Default)]
pub struct HistoryOptions { pub cancel: Option<Arc<AtomicBool>>, pub deadline: Option<Instant> }
pub fn read_offline(root: &Path) -> Result<(Vec<NamedSnapshot>, HistoryStatus)>; // lockless

impl Store {
    pub fn resolve(&self, at: &At) -> Result<Resolved>;       // metadata only; 404/410 errors
    pub fn snapshot_at(&self, at: &At, o: &HistoryOptions) -> Result<(Arc<Snapshot>, Resolved)>;
    pub fn snapshots(&self) -> Vec<NamedSnapshot>;
    pub fn named_snapshot(&self, name: &str) -> Option<NamedSnapshot>;
    /// Ok((snapshot, true)) if created, (existing, false) if it already pinned that commit.
    pub fn create_snapshot(&self, name: &str, at: &At, note: Option<String>)
        -> Result<(NamedSnapshot, bool)>;
    pub fn delete_snapshot(&self, name: &str) -> Result<bool>;
    pub fn retention(&self) -> Retention;
    pub fn set_retention(&self, r: Retention) -> Result<HistoryStatus>;
    pub fn history(&self) -> HistoryStatus;
    pub fn dump_nquads_at(&self, at: &At, w: impl Write) -> Result<u64>;
}
impl Snapshot { pub historical: bool }                        // new field
impl Dataset {
    pub fn snapshot_at(&self, at: &At) -> Result<Arc<Snapshot>>;
    pub fn query_at(&self, at: &At, q: &str, opts: &QueryOptions) -> Result<QueryResult>;
    // plus snapshots / create_snapshot / delete_snapshot / history forwarding to Store
}
// error.rs
Error::NotFound(String)             // 404 (commit, snapshot, time before root)
Error::HistoryGone(HistoryGone)     // 410; HistoryGone { seq, snapshot, oldest, reconstructable, metadata }
Error::HistoryUnsupported(String)   // 501 (in-memory, text at a past commit)
BudgetKind::HistoryMemory           // "history-memory"
// StoreOptions
pub history_cache_bytes: u64        // 1 GiB
pub history_max_generations: usize  // 8 non-current generations held by pins
pub max_snapshots: usize            // 256
pub history_open_generations: usize // 2 sealed generations open at once
```

`sparql::query(snap, …)` already takes an `Arc<Snapshot>`, so the engine needs no
change beyond the cache key (§5.5).

### 2.8 Interactions

* **Result cache.** Historical and live snapshots share the store's result cache. The
  key becomes `(generation uid, commit)` instead of `(version, generation name)`
  (§5.5), so entries for different commits or generations never collide. Two reads of
  the same commit through the same generation instance share entries.
  `POST /$/cache/clear/{ds}` clears all of them.
* **Full-text search.** The Tantivy index reflects only the head (`TextView.seq`). A
  historical snapshot has `text: None`, and `text:query` fails with `501
  text-not-at-commit`. Queries without `text:query` work normally at any commit, and
  `at=head` keeps full-text search. Phase 3 may build per-pin indexes (§6).
* **Vector search.** It works at any commit with no special code. `GenerationVectors`
  lives on the `Generation` instance, and each query overlays its snapshot's delta, so a
  historical snapshot sees exactly its own vectors. A retained generation packs its
  vectors lazily under the same per-generation budget. At most
  `history_open_generations` (2) historical generations are open at once, which bounds
  total vector memory at 3× the budget (Open question 4).
* **Reasoning.** A historical read includes the inferred graph as it was at that commit
  (`reasoning=true|false` works as today). `Sparkles-Inferences` is computed *at* the
  commit from the catalog:
  * let r be the latest commit ≤ s whose kind is `reason` or `reason-clear`;
  * r of kind `reason-clear`, or r of kind `reason` with r = s: no header;
  * r of kind `reason` with r < s: `stale; commits-since=<s−r>`;
  * no such r in the retained metadata: `unknown`.

  A materialization run that changed nothing creates no commit, so the result may
  overstate staleness (the conservative direction). The current `reasoning.json` is not
  used for historical reads.
* **Read-only servers** (`--read-only`). All `at` reads and `GET /$/snapshots|history`
  work. POST, DELETE and PUT return `403`. Opening the store still does its recovery:
  WAL tail truncation today, plus history GC (§4.3). Open question 6 covers the
  alternative.
* **Compaction and bulk commits** keep their current semantics and cost. The only change
  is that the outgoing generation is kept when a pin or the retention window needs it
  (§4.2). The compaction task message then says
  `kept gen-0003 (1.2 GiB) for snapshot:release-1`.
* **Clone** (C06) copies no history and no pins. Phase 2 adds `clone?at=REF`, which
  records `forkedFrom.seq = the resolved commit`.

## 3. Standards basis

* **SPARQL 1.1 Protocol §2.1** defines `query`, `default-graph-uri` and
  `named-graph-uri`, and says nothing about other parameters. `at` is an extension
  parameter like the existing `timeout`, `reasoning` and `nocache`. §2.1.5 allows
  "other 4XX or 5XX HTTP response codes for other failure conditions", which covers
  404, 410, 501 and 507.
* **SPARQL 1.1 Graph Store HTTP Protocol.** `?graph=` / `?default` identify the graph,
  and `at` only chooses the state. "404 Not Found MUST be provided" for missing graph
  content applies at the selected commit. The protocol says responses "SHOULD be made
  cacheable wherever possible", which motivates the Phase 2 ETag.
* **RFC 3339 §5.6** gives the `time:` date-time syntax. **[CI §2.1](CI-commit-identity.md)** gives non-decreasing
  millisecond timestamps, which make "last commit ≤ T" well defined.
* **RFC 7089 (Memento).** `Memento-Datetime` "indicate[s] that a response reflects a
  prior state of an Original Resource". Its value is an RFC 1123 date in GMT, and when
  it is sent, `Link` MUST include `rel="original"`. `Accept-Datetime` and TimeGate
  negotiation (`Vary: accept-datetime`) are Phase 2. RFC 7089 has a TimeGate clamp a
  datetime before the first memento to the first memento. `?at=time:` deliberately
  returns 404 `before-history` instead, because the dataset had no state then. The
  Phase 2 `Accept-Datetime` support follows RFC 7089 and clamps to the oldest
  reconstructable commit.
* **RFC 9110.** §15.5.11 410 Gone ("likely to be permanent") covers GC'd history.
  §15.5.5 404 covers what never existed, §15.5.10 409 covers pin conflicts, and §8.8.1
  makes weak validators the right choice for semantically equivalent but not
  byte-identical representations.
* **RFC 8246** `immutable` (Phase 2) and **RFC 6648** (no `X-` prefix) apply.

## 4. Semantics

### 4.1 Reconstructable commits

Write `range(G) = [base(G), end(G)]` for a generation G, where:

* `base(G)` is the `baseSeq` in `G/commit.json`;
* `end(G)` is the last commit in G's WAL (or `base(G)` if the WAL is empty). For the
  current generation it is the head.

Ranges of consecutive generations touch in one of two ways:

* **compaction**: `base(G+1) = end(G)`, with the same data and new ids;
* **bulk commit**: `base(G+1) = end(G) + 1`.

A commit s is **reconstructable** iff some *retained* generation G (the current one or a
kept one) has s ∈ range(G). Its **owner** is the newest such G. When s = base(owner),
the state is the base index with an empty delta. Otherwise it is base(owner) plus
owner's WAL replayed through commit s.

With nothing pinned and retention off, the reconstructable set is exactly
`range(current)`: every commit since the last compaction or bulk commit, at zero extra
disk.

### 4.2 What keeps a generation

`Protected` is the union of:

* the head;
* the commit of every named snapshot;
* the **retention window**: every commit s whose state was the head at some instant in
  the window. With `keepCommits: N`, that is `s > head − N`. With `keepAge: D`, it is
  s = head, or `ts(s+1) > now − D`, so the state as of *any* instant in the last D is
  covered. With both set, the union applies.

A non-current generation G is **needed** iff G owns some s ∈ Protected. That is:

* s ∈ range(G);
* no newer retained generation covers s.

At a compaction the outgoing G covers the head only at `end(G)`, which the new
generation also covers. So a pin *at the head* never retains G ("pin, then compact" is
free). A pin inside G retains all of G, and every other commit of G becomes
reconstructable as a side effect. Listing shows this through `heldBy`.

### 4.3 Garbage collection (GC)

GC runs, under the writer mutex and the history mutex:

* after every rebuild (compaction or bulk commit), in place of today's unconditional
  `remove_dir_all(old)`;
* after a pin is deleted and after the retention is changed;
* at `Store::open`.

The window's time edge moves without events, so age-expired history is collected at the
next of these points (Phase 2 adds an hourly server tick). GC:

1. Removes `gen-*.deleting` directories left by an interrupted GC.
2. Removes `gen-N` directories with N > the current generation number. These are
   interrupted rebuilds; `rebuild_locked` already overwrites such a directory today.
3. For each non-current `gen-N` with a readable `commit.json` whose dataset id matches:
   if it is not needed, it renames it to `gen-N.deleting`, fsyncs the root, then runs
   `remove_dir_all`.
4. Leaves alone any directory it cannot identify (no `commit.json`, a foreign dataset
   id, an unparsable name) and logs a warning. Sparkles never deletes what it cannot
   attribute.

Readers that already hold a historical snapshot keep working, because their files
(mmaps, `.dat` handles, the WAL handle) stay open after the unlink on Unix. On Windows
the rename fails while a file is open. GC logs the failure, and the directory stays
listed as "pending deletion" until the next GC point.

### 4.4 Pins

* A pin names a commit, not a generation. The generation mapping is always derived.
* A pin can only be created at a reconstructable commit (otherwise 410). Creation and
  deletion are durable (`write_atomic`) before the response.
* Pins are immutable: re-creating the same name at the same commit is idempotent (200),
  and at another commit it is 409. To move a pin, delete it and create it again.
* A pin whose data is gone is **kept** and shown with `reconstructable: false`, and reads
  of it return 410. This only happens after external damage, such as a hand-deleted
  directory or a failed disk. Deleting the pin is the user's decision.
* Deleting a pin never deletes data that another pin or the window still needs.

### 4.5 Limits and budgets

| Limit | Default | Flag / option | Enforcement |
|---|---|---|---|
| Named snapshots per dataset | 256 (hard cap 10 000) | `--max-snapshots`, `StoreOptions::max_snapshots` | 409 `history-limit` on create |
| Non-current generations held by **pins** | 8 | `--history-max-generations` | 409 `history-limit` on create. A pin in the current generation counts as one more held generation, because it will retain one at the next rebuild |
| Generations held by the **window** | the same limit, shared | | The window is best effort: the oldest window-only generations are dropped first to stay within the limit. Pins always win |
| Historical generations open at once | 2 | `StoreOptions::history_open_generations` | LRU of `Arc<Generation>` |
| Memory of materialized history | 1 GiB | `--history-cache-mb` | LRU eviction. A single materialization that would exceed it fails with 507 `history-memory` (checked every 64 Ki records) |
| Time to materialize | the query's timeout | `timeout=` | Checked every 64 Ki records together with the cancel flag. A materialization that times out is not cached |

Disk use has no hard cap in Phase 1. Every retained generation costs about one full index
(vocabulary + 7 permutations + its WAL and delta vocabulary). `HistoryStatus.bytes`, the
`/$/stats` `history.bytes` and the compaction task message make that cost visible.
Phase 2 adds `Retention.maxBytes`, which trims window-only generations.

### 4.6 Materialization targets

These are estimates to confirm in Phase 1. Release build, warm page cache, 10 M-quad
base.

* Opening a sealed generation: < 50 ms. It mmaps the vocabulary and reads block
  metadata of 100 B per 32 Ki-row block per permutation, ≈ 200 KB at 10 M quads. It
  also loads the generation's `delta.vocab` into memory.
* WAL replay: ≥ 1 M records/s. Each record is one SPO membership probe plus 7
  ordered-set updates. For example, 100 k single-quad commits (200 k records) take
  ≈ 0.2 s.
* A cached historical snapshot adds < 1 ms to a query.
* Memory ≈ 300 B per quad in the replayed delta (7 × a 32-byte key plus tree overhead,
  to be measured and fixed in `Delta::approx_bytes`), plus 2 × `delta.vocab` bytes per
  open generation. Decoded blocks go through the shared block cache (`--cache-mb`), so
  they may evict live blocks.

If a target is missed, the README status records the measured numbers. They are not
blockers.

## 5. Design

### 5.1 Recommended: retained generations plus sealed WALs (alternative A)

Keep whole generation directories that own a protected commit, and materialize a past
state by replaying a prefix of that generation's WAL. Every piece already exists and is
durable:

* the base index;
* `commit.json` (`baseSeq`);
* a WAL whose records carry `seq` and a CRC ([CI §5.3](CI-commit-identity.md));
* the catalog, whose `generation` field says which generation's WAL holds each WAL
  commit.

Nothing is added to the write path. The only behavioral change to rebuilds is to *not
delete* a needed generation.

A generation stops being written the moment `CURRENT` moves away from it: it becomes
**sealed**. Its WAL was fsynced per commit, and every `delta.vocab` entry that a WAL
record references was synced before that commit (`dvocab.sync()` in `publish_log`).
A torn tail can only come from an aborted, unreferenced intern and is ignored. So a
sealed generation is safe to open read-only at any later time.

### 5.2 Alternatives compared

| | A. Retained generations + sealed WALs | B. Checkpoints + generation-independent change log | C. Materialized copy per pin |
|---|---|---|---|
| Idea | Keep `gen-N` dirs that own protected commits; replay a WAL prefix | Log every commit's net changes as term keys to `history/changes-*.log`, plus periodic full checkpoints; replay forward from a checkpoint (or backward from the head) across generations | On pin, build a dedicated frozen generation from that snapshot (`clone_to`-style) under `snapshots/<name>/` |
| Arbitrary commit / time | yes, within retained ranges | yes, anywhere after the oldest checkpoint | no, named points only |
| Disk | ≈ 1 full index per retained generation; 0 when nothing is pinned | changes (≈ 100–200 B per changed quad as keys) + checkpoints | 1 full index per pin, with no sharing |
| Write-path cost | none | an extra log write per commit; bulk commits must compute and log their change set (expensive) | none |
| Pin cost | O(1), paid at the next rebuild | O(1) | a full rebuild (minutes at 10⁸ quads) |
| Read cost | open (ms) + replay ≤ one WAL | replay across many commits, interning keys into a temporary vocabulary | ~0 |
| Diffs | cheap within and across compaction boundaries; full compare across bulk | natural | full compare |
| New formats | `history.json` only | change log, checkpoint manifest | pin directory layout |
| Effort | 2–4 days | 1.5–3 weeks | 2–3 days, but narrower |

**Recommendation: A.** It reuses the durable state Sparkles already has. It is free when
unused, keeps the write path untouched, and fits the MVP budget. B is the right
evolution if retained full copies become the dominant disk cost, for example with
frequent compactions under a long window. It is kept as a Phase 3 option (Open
question 2).

**Rejected** (see also §8):

* C as the primary design: no time/commit selectors, and a full rebuild per pin.
* Keeping `Arc<Snapshot>`s in memory for persistent datasets: lost on restart, and memory
  is unbounded. It is right only for in-memory datasets (Phase 2).
* Filesystem snapshots or reflinks (btrfs, ZFS, APFS clones): platform-specific and
  outside the store's crash model.
* Temporal permutations (valid-from/valid-to columns on all 7 permutations): an index
  format change, with cost on every scan.

### 5.3 Materializing a historical snapshot

`Store::snapshot_at(at)`:

1. **Resolve** (no locks beyond the catalog): commit → seq; time → catalog binary search
   (`Catalog::at_time(ms)`); snapshot → pin. Check `seq ≤ head` (404) and
   `seq ≥ firstRetained` (410 with `metadata: null`). If `seq == live.commit`, return the
   live snapshot.
2. **Locate and lease** under the history mutex:
   * find `owner(seq)` from the in-memory generation table (§5.4), or 410;
   * look up `(owner, seq)` in the snapshot LRU; on a hit, return it;
   * otherwise get `Arc<Generation>` for owner. For the current generation that is the
     live one (its `dvocab` is shared; ids below its length are stable). Otherwise the
     open-generations LRU has it, or `Generation::open_sealed(dir)` opens it: the same
     as `open`, but with `DeltaVocab::open_read_only` (no truncation, no append handle,
     a torn tail ignored);
   * open the WAL with a read-only handle.

   Holding the mutex for these opens (milliseconds) is what makes GC safe: GC takes the
   same mutex, and once the files are open, an unlink cannot hurt the reader.
3. **Single-flight.** Concurrent requests for the same `(owner, seq)` wait on one
   `OnceLock`-style slot. Only one replay runs.
4. **Replay** without any lock: stream the WAL in 1 MiB chunks through the replay
   function (§5.6), stopping after the commit record with `seq`. Every 64 Ki records,
   check cancel, deadline and the memory estimate. If the base is exactly `seq`, skip
   replay.
5. **Publish.** Build `Snapshot { generation: owner, delta, version: 0, historical: true,
   commit: seq, text: None, dvocab_len: owner.dvocab.len(), cache: store block cache,
   results: store result cache, union_default_graph: store option, … }`. Insert it into
   the LRU, weighted by `delta.approx_bytes()`, plus the generation's `delta.vocab`
   bytes when it is the only entry holding that generation.

`dvocab_len` is the generation's full length, not its length at `seq` (that is not
recorded). Terms interned after `seq` resolve to ids that match no quad, which is
harmless.

Materialization is **on demand**. Pinned snapshots are not kept in memory. Phase 2 may
add `warm: true` on a pin to keep it materialized under the same budget.

**Phase 2 speedups:**

* reuse a cached `(owner, s1)` delta and its WAL offset to reach `s2 > s1`;
* for the current generation, replay **backwards** from the live head delta, applying
  inverse ops. Every logged op was effective, so the inverse is exact. The cheaper
  direction is chosen by record count;
* a sparse offset index (seq → WAL offset every 1024 commits).

### 5.4 History state and files

`<root>/history.json` (format 1), written with `write_atomic`:

```json
{ "format": 1, "datasetId": "3f1c9a2e-…",
  "retention": { "keepCommits": null, "keepAgeMs": null },
  "snapshots": [
    { "name": "release-1", "seq": 42, "created": "2026-09-30T15:00:00.000Z", "note": "before migration" } ] }
```

It stores commits only, never generation names. A missing file means no pins and no
retention. A `datasetId` mismatch (a copied directory) fails the open with
`Error::Corrupt`. An older binary ignores the file. At its next compaction it deletes
only the generation it compacts from, and leaves the retained ones as unmanaged
directories.

In memory, `Store` gains `history: Mutex<HistoryState>`:

* `pins: BTreeMap<String, Pin>`;
* `retention`;
* `gens: BTreeMap<u32, GenEntry { name, base_seq, end_seq, bytes, dir }>`;
* the LRUs and hit/miss counters.

The table is built at open:

* `base_seq` comes from `commit.json`;
* `end_seq` is the largest catalog seq with `generation == N`, or `base_seq`. If the
  catalog cannot supply it (metadata before `firstRetained`), a scan of the WAL's commit
  records supplies it;
* `bytes` is `dir_size` once per sealed generation, since sealed generations are
  immutable.

The current entry's `end_seq` tracks the head.

### 5.5 Changes to existing code

* `store.rs`:
  * `rebuild_locked` calls `history.after_rebuild(old, new)` in place of
    `remove_dir_all(old)`;
  * `open` builds the history state and runs GC after WAL replay and catalog repair;
  * the WAL replay loop in `open` moves into a reusable function (§5.6);
  * `Snapshot` gains `historical`;
  * `Generation` gains `uid: u64`, process-unique like `PermIndex::uid`, and
    `open_sealed`;
  * new `Store` methods (§2.7), and `dump_nquads_at`.
* `vocab.rs`: `DeltaVocab::open_read_only`.
* `commit.rs`: `Catalog::at_time(ms) -> Option<u64>` (binary search) and
  `Catalog::last_of_kinds_at_or_before(seq, &[Reason, ReasonClear])`. RFC 3339 parsing
  gains offsets (today only `Z`).
* `sparql/cache.rs::raw_key`: replace `v{version}|{generation.name}` with
  `g{generation.uid}|c{commit}`. For live snapshots this is exactly as precise: data is
  a function of (generation instance, commit), and compaction creates a new instance.
* `schema.rs::snapshot_identity`: return the commit, as its comment already plans.
  Cursors then survive restarts.
* `text.rs::search`: when `snap.historical`, return
  `Error::HistoryUnsupported(text-not-at-commit)` before the "no index" check.
* `error.rs`, and `http.rs` `From<Error>`: the new variants, plus a `code` member for
  them.
* `sparkles-server/http.rs`:
  * `fn at_param(&Params) -> ApiResult<Option<At>>`;
  * `query_endpoint`, `explain` and `gsp` GET/HEAD call `store.snapshot_at` inside
    `blocking(…)` when `at` is set, then add `history_headers(resolved, uri)`;
  * `update_endpoint`, GSP writes and `upload` reject `at`;
  * new routes `/$/snapshots/{ds}[/{name}]` and `/$/history/{ds}`;
  * CORS expose list;
  * `with_inferences` takes the resolved commit and uses the catalog rule of §2.8 when
    it is historical.
* `sparkles-server/main.rs`: `Snapshot` subcommand group, `--at` on `Query`/`Dump`, and
  the `serve` flags.
* `docs/API.md`: a "Point-in-time reads and snapshots" section. `README.md`: the history
  row.

### 5.6 Replay function

```rust
pub(crate) struct Replay { pub delta: Delta, pub commits: Vec<CommitInfo>, pub next_bnode: u64,
                           pub good_bytes: u64, pub records: u64 }
pub(crate) enum Stop { End, AfterSeq(u64) }
pub(crate) fn replay_wal(gen_: &Arc<Generation>, cache: &Arc<BlockCache>, base: CommitInfo,
                         fold_legacy: bool, wal: impl Read, stop: Stop,
                         check: &mut dyn FnMut(u64 /*records*/, &Delta) -> Result<()>) -> Result<Replay>;
```

* `Store::open` calls it with `Stop::End` and keeps today's behavior: torn-tail truncation
  by `good_bytes`, and `Error::Corrupt` for a CRC mismatch before the last transaction.
* History calls it with `Stop::AfterSeq(s)` on a sealed or live WAL and never truncates.
  A CRC mismatch at or before `s` is `Error::Corrupt`, and anything after `s` is never
  read.
* Legacy (version-0) records get the same seqs as at open, because the same `base` and
  `fold_legacy` inputs produce them deterministically.

### 5.7 Crash safety

| Step | What is written | fsync |
|---|---|---|
| WAL commit | unchanged | `sync_data` per commit (CI) |
| Rebuild | unchanged order: catalog sync → new gen files and `commit.json` synced → dirs synced → `CURRENT` via `write_atomic` | as today |
| After the `CURRENT` switch | keep the old generation (no writes), or GC it | see GC |
| Pin create/delete, retention change | `history.json` via `write_atomic` (tmp write + `sync_all`, rename, root dir fsync) | **before** the response and before any GC that depends on it |
| GC of a generation | rename `gen-N` → `gen-N.deleting`, fsync root, `remove_dir_all` | the rename only |
| Open | remove `*.deleting` and `gen-N` with N > current; GC per policy; ignore a stale `history.json.tmp` | the renames |

Invariants:

1. A directory named `gen-N` (not `.deleting`) with a valid `commit.json` is complete. The
   rebuild writes `commit.json` and syncs before `CURRENT` names the generation, and GC
   renames before it deletes.
2. A generation needed by the durable pins and the window is never renamed. Pins are
   durable before they are relied on, and a pin removal is durable before any
   generation it held is renamed.
3. A crash at any point leaves every durably pinned commit reconstructable, unless files
   were damaged externally.

## 6. Phasing

**Phase 1 (MVP, ~3 days).**

* Day 1: `history.rs` (`At` parsing, `history.json`, the generation table, needed-set and
  GC with `.deleting`); the rebuild hook; open-time recovery;
  `DeltaVocab::open_read_only` and `Generation::open_sealed`; the extraction of
  `replay_wal`.
* Day 2: `resolve` and `snapshot_at` (live-generation and sealed-generation paths,
  single-flight, LRUs, budget, cancel and deadline); `Catalog::at_time`; the cache-key
  switch; `snapshot_identity`; the text guard; errors.
* Day 3: HTTP (`at` on query/explain/GSP GET/HEAD, write rejection, headers including
  Memento, `/$/snapshots`, `/$/history` GET/PUT, commit-list `reconstructable`, stats,
  historical `Sparkles-Inferences`); CLI (`snapshot create|list|delete|history|retain`,
  `query --at`, `dump --at`); the `Dataset` forwarding API; `docs/API.md` and the
  README; tests A1–A22.
* Retention in Phase 1: `keepCommits` and `keepAge`, sharing the pin generation limit.

**Phase 2.**

* **Diffs** between two reconstructable commits:
  * `GET /{ds}/diff?from=REF&to=REF`, `sparkles diff --loc DB FROM TO`, and
    `Store::diff(from, to, sink)`;
  * output is `application/json`
    (`{from, to, added, removed, quads?: [{op:"+"|"-", …}]}`, quads only with
    `quads=true`) or N-Quads lines prefixed `+ `/`- ` (`text/x-sparkles-diff`). RDF Patch
    output is an open question;
  * semantics: net set difference (added = in `to` and not in `from`; removed =
    vice versa). Blank nodes keep their `_:b<hex>` labels, so diffs are meaningful
    within a lineage;
  * algorithm: walk the WAL segments between the two commits, decoding each op to term
    keys and keeping `key → (present at from, present now)`. Memory is O(changed quads).
    This crosses compaction boundaries for free, because the data is unchanged there. A
    bulk commit between the two falls back to a sorted key-stream comparison of both
    states under a `--diff-max-quads` budget (507 beyond it). Phase 3 logs bulk change
    sets so this fallback disappears.
* `Retention.maxBytes`; per-pin `expires`; scheduled pins
  (`{every: "1d", keepLast: 7, prefix: "daily-"}`); an hourly GC tick in the server.
* `Accept-Datetime` on GSP GET (RFC 7089 TimeGate, `Vary: accept-datetime`); ETag and
  `immutable` for `commit:` reads; `If-None-Match`.
* In-memory datasets: pins and a window kept as `Arc<Snapshot>` (structural sharing makes
  this cheap).
* The replay speedups of §5.3, and `warm` pins.
* `at` on schema, stats, SHACL, clone and backup; `sparkles log` reconstructable markers;
  metrics (`sparkles_history_bytes`, `…_materialize_seconds`, cache hits and misses).
* UI:
  * the History panel gets "query at this commit" and snapshot badges;
  * a Snapshots panel supports create and delete;
  * the query page gets an `at` selector showing "at commit 42 · 3 h ago".

**Phase 3 and later.**

* A generation-independent change log (alternative B), for cheap long history and exact
  bulk diffs.
* Full-text search at pins: build a Tantivy index per pin at pin time, or a RAM index on
  demand under a budget.
* History queries (changes to a subject over time).
* Pin rebasing: rebuild a pinned state into its own compact generation to free a large,
  otherwise unneeded generation.
* Moving sealed generations to cheaper or remote storage.

## 7. Acceptance examples

Setup: `sparkles serve --data /tmp/s`, a fresh persistent dataset `ds`, and
`Store::set_clock` returning 1000, 2000, 3000 … ms for successive commits. Commits:
1 `INSERT DATA { <urn:a> <urn:p> 1 }`, 2 `INSERT DATA { <urn:b> <urn:p> 2 }`,
3 `DELETE DATA { <urn:a> <urn:p> 1 }`. Q = `SELECT ?s { ?s ?p ?o } ORDER BY ?s`.

**A1. Free history in the current generation.**
* `GET /ds/sparql?query=Q&at=commit:1` → `200`, `[urn:a]`, with headers
  `Sparkles-Commit: 1`, `Sparkles-Head: 3`, `Sparkles-At: commit:1`,
  `Memento-Datetime: Thu, 01 Jan 1970 00:00:01 GMT`, and
  `Link: </ds/sparql?query=…>; rel="original"`.
* `at=2` → `[urn:a, urn:b]`. `at=0` → `[]`.
* `at=3` and `at=head` → `[urn:b]`, with no `Memento-Datetime`.
* `GET /$/history/ds` → `bytes: 0`, `reconstructable: [{from:0,to:3}]`.

**A2. Time selector.**
* `at=time:1970-01-01T00:00:02.500Z` → commit 2. `…00:00:02.000Z` → 2. `…00:00:01.999Z`
  → 1.
* `at=time:1970-01-01T01:00:02%2B01:00` → 2. The same with a literal `+` (decoded to a
  space) → 2.
* `at=time:1969-12-31T00:00:00Z` → `404 before-history`.
* `at=time:2099-01-01T00:00:00Z` → head (3) and no Memento headers.

**A3. Graph Store GET.**
* `GET /ds/data?default&at=1` → Turtle with only `<urn:a>`, `Sparkles-Commit: 1`.
* Commit 4 `PUT /ds/data?graph=urn:g` (1 triple). `GET /ds/data?graph=urn:g&at=3` →
  `404 no such graph`; `&at=4` → 200. `HEAD` with `at=1` → headers only.

**A4. Selector and write errors.**
* `at=abc` and `at=commit:-1` → `400 invalid-at`.
* `at=99` → `404 no-such-commit` naming head 4.
* `at=snapshot:nope` → `404 no-such-snapshot`.
* `POST /ds/update?at=1` → `400 at-on-write`, with no commit created.
* `PUT /ds/data?default&at=head` → `400`.
* A form POST query with `at=1` in the body and `at=2` in the URL → `400`.

**A5. Compaction ends unpinned history.**
* `POST /$/compact/ds` (head 4, `gen-0001` → `gen-0002`), then:
  * `at=1` → `410`, body `code: "history-gone"`, `oldestReconstructable: 4`,
    `reconstructable: [{from:4,to:4}]`, `metadata.seq: 1`;
  * `at=4` → 200;
  * `GET /$/commits/ds/1` → still 200 (metadata);
  * `GET /$/commits/ds` marks seq 4 `reconstructable: true` and 1 `false`.
* `gen-0001` is gone from disk.

**A6. A pin protects its generation.**
* Fresh `ds`, commits 1–3 as in the setup. `POST /$/snapshots/ds {"name":"v1","at":"commit:1"}`
  → `201`, `Location: /$/snapshots/ds/v1`, `commit.seq: 1`.
* Compact. `GET /$/history/ds` shows `gen-0001` with `heldBy: ["snapshot:v1"]` and
  `bytes > 0`.
* `at=snapshot:v1` → `[urn:a]` with `Sparkles-At: snapshot:v1` and `Sparkles-Commit: 1`.
  `at=2` → 200 too (the same generation).
* `DELETE /$/snapshots/ds/v1` → `204`. `gen-0001` is removed, and `at=1` → `410`.

**A7. Pinning the head before compaction is free.**
* With head 3, pin `rel` (no `at`), then compact.
* `gen-0001` is deleted, and `history.bytes == 0`.
* `at=snapshot:rel` → 200 from `gen-0002`'s base (no replay).

**A8. Pin conflicts and limits.**
* Re-POST `rel` at head 3 → `200` with the same body.
* POST `rel` at `commit:2` → `409`, body includes the existing pin.
* Name `a/b` → `400`. `at` = a GC'd commit → `410`.
* With `--max-snapshots 1`, a second name → `409 history-limit`.
* With `--history-max-generations 1`:
  * pin at commit 1, compact;
  * add commits, pin one in the current generation → `409 history-limit` (it would hold
    a second generation).

**A9. Restart.**
* After A6's compaction, before the delete: `kill -9` the server and restart it.
* `GET /$/snapshots/ds` lists `v1`, `at=snapshot:v1` gives the same result, and history
  lists `gen-0001`.
* `sparkles snapshot list --loc /tmp/s/databases/ds --format json` works while the server
  runs.

**A10. Bulk-commit boundary.**
* `StoreOptions { bulk_threshold: 10 }`, `keepCommits: 10`.
* Two small commits (1, 2), then load 100 triples (bulk, commit 3, `gen-0002`).
* `at=2` → 2 quads (replayed from the kept `gen-0001`). `at=3` → 102 (`gen-0002` base).
  History shows `gen-0001` `heldBy: ["retention"]`.

**A11. KeepCommits window.**
* `PUT /$/history/ds {"keepCommits":2}`. Head 5 in `gen-0001`; compact → `gen-0001`
  is kept (commit 4 is in the window and only `gen-0001` has it).
* Commits 6 and 7, then compact → `gen-0001` is deleted (window {6,7}), and `gen-0002`
  (range 5..7) is kept for 6.
* `at=4` → `410`, and `at=6` → 200.

**A12. KeepAge window.**
* Clock at 1000·s for commit s. `keepAge: "5s"` and head 10 in `gen-0001`, clock now
  10 000.
* Compact → `gen-0001` is kept (commits 5–9 were the head within the last 5 s).
* Set the clock to 100 000, make a commit (seq 11 in `gen-0002`), and compact again →
  `gen-0001` is collected. `gen-0002` (range 10..11) is kept: commit 10 was the head
  until 100 000, which is inside the window, and `gen-0002` is its newest owner.
  `at=9` → `410`, and `at=10` → 200.

**A13. Crash during GC (unit).**
* Create `gen-0001.deleting/` with half its files, plus current `gen-0002`. Reopen: the
  `.deleting` directory is removed and no error is raised.
* Copy a valid sealed `gen-0001` back in place (simulating a crash after the `CURRENT`
  switch and before GC). Reopen with no pins → it is removed. With `history.json`
  pinning commit 1 → it is kept, and `snapshot_at(Commit(1))` works.
* A directory `gen-0007` with no `commit.json` → left alone, with a warning.

**A14. Crash while creating a pin (unit).**
* Leave a truncated `history.json.tmp` next to a valid `history.json`. Reopen: the pins
  equal the valid file.
* A hook that aborts after the `write_atomic` of a create and before the response →
  after reopen, the pin exists, and a retried POST returns `200` (idempotent).

**A15. Crash in the middle of compaction with a pin (unit).**
* A failpoint returns an error after the new generation's files are written and before
  `CURRENT` switches. Reopen:
  * `CURRENT` is still `gen-0001`;
  * the partial `gen-0002` is removed;
  * pinned and unpinned `at` reads of commits in `gen-0001` work.
* The next compaction produces `gen-0002` again.
* A second failpoint panics right after the switch. Reopen → `gen-0001` is kept iff
  pinned, otherwise it is removed.

**A16. Torn WAL tail and history (unit).**
* Append a partial transaction to the current WAL and reopen: the head is unchanged, and
  `at` reads of every commit up to the head work.
* Flip a byte in commit 2's data records in a *sealed* generation: `at=1` still works
  (replay stops before the damage), while `at=3` → `500` corrupt, naming the file.

**A17. Full-text search.**
* A dataset with text enabled. `text:query` with `at=head` → 200.
* With `at=commit:1` → `501 text-not-at-commit`.
* A plain query with `at=commit:1` → 200.

**A18. Vector search.**
* Commit 1 sets `ex:x ex:emb "[1,0]"^^spk:vector`; commit 2 replaces it with `"[0,1]"`.
* `spk:vectorSearch (ex:emb "[1,0]"^^spk:vector 1)` at `at=1` scores 1.0 for `ex:x`, and
  at the head scores 0.0.

**A19. Reasoning header.**
* Materialize at commit 4 (kind `reason`), then commits 5 and 6.
* A query with `reasoning=true`:
  * `at=4` → no `Sparkles-Inferences`;
  * `at=5` → `stale; commits-since=1`;
  * `at=3` → `unknown`.
* The inferred graph at `at=3` is empty.

**A20. Result-cache isolation.**
* Run Q with `at=1`, then `at=2`, then head, and repeat all three. The results equal the
  first round, and the second round shows `cached` in `/ds/explain` plans.
* After a restart, the same holds.
* A unit test: an in-memory compaction yields a new generation uid, so no stale entry is
  hit.

**A21. Budgets and cancellation.**
* `--history-cache-mb 1`, and a generation with 100 k single-quad commits:
  `at=<last−1>` → `507`, `budget: "history-memory"`.
* `timeout=0.001` → `408`, and the next request re-materializes (nothing cached).
* Eight concurrent `at=50000` requests → one replay (debug counter
  `history.materializations == 1`).

**A22. CLI and library.**
* `sparkles snapshot create --loc db v1 --at commit:1` → prints `v1 → commit 1`.
* `sparkles query --loc db --at snapshot:v1 'SELECT …'` → `urn:a`.
* `sparkles dump --loc db --at 2` → two N-Quads lines.
* `sparkles snapshot delete --loc db v1`.
* Library:
  `ds.query_at(&"commit:1".parse()?, "ASK { <urn:a> ?p ?o }", &Default::default())?`
  is true. `Dataset::memory().snapshot_at(&At::Commit(0))` on a store with head 1 →
  `Error::HistoryUnsupported`.

**A23. Performance (bench, not CI).**
* 10 M-quad base plus 200 k single-quad commits in one generation.
* The first `at=100000` query completes in < 1 s over the plain query time, and the
  second adds < 1 ms.
* `at=<base of a sealed generation>` adds < 50 ms.
* Measured numbers go to `docs/BENCHMARKS.md`.

## 8. Rejected alternatives

* **B or C as the MVP** (§5.2): B changes the write path and needs new formats; C only
  supports named points and costs a rebuild per pin.
* **In-memory `Arc<Snapshot>` retention for persistent datasets**: it does not survive a
  restart, and memory is unbounded. It is kept for in-memory datasets (Phase 2).
* **Storing generation names in pins**: they would go stale across compaction. Pins store
  commits, and the mapping is derived.
* **Deleting a pin's data when its generation is missing**: silent loss. The pin stays,
  flagged.
* **`404` for GC'd commits**: it hides the difference between "never existed" and "no
  longer kept". RFC 9110 410 exists for the latter.
* **RFC 7089 clamping for `?at=time:` before the root**: that would answer with a state
  the dataset never had at T. The `Accept-Datetime` negotiation in Phase 2 follows the
  RFC.
* **A strong ETag per commit**: GSP serialization order and blank-node allocation follow
  generation-local ids, so the bytes differ after compaction.
* **Refusing compaction when retention would exceed a limit**: compaction reduces memory
  and must stay available. The window trims instead, and pins are checked at creation.
* **A `sealed.json` per generation**: `end_seq` is derivable from the catalog's
  `generation` field (with a WAL-scan fallback), so one less file needs crash handling.
* **Using `Snapshot::version` in the cache key for historical snapshots**: versions are
  in-process counters and could collide with a live version of the same generation.
  `(generation uid, commit)` is exact.
* **`X-Sparkles-*` or product-specific selector syntaxes**: RFC 6648. The selector
  follows the `commit:N` form CI already reserved.

## 9. Open questions

1. **Default retention.** It is off (only the current generation plus pins). Should new
   datasets default to `keepCommits: 1000` or `keepAge: "24h"`? The cost is up to one
   extra full index per compaction. Default: off.
2. **Alternative B** as the long-term store for history if retained generations dominate
   disk. That needs a decision on logging bulk change sets (cost scales with the bulk
   batch). Default: revisit after Phase 1 measurements.
3. **Diff output format.** Sparkles `+`/`-` N-Quads lines and JSON only, or also Jena RDF
   Patch (`application/rdf-patch`, `A`/`D` rows) for Jena ecosystem compatibility? The
   RDF Patch syntax must be checked against Jena's documentation before adoption.
   Default: Sparkles formats only.
4. **Vector budget.** Should the vector budget become process-wide rather than
   per-generation, so historical generations cannot multiply it? Default: per generation,
   plus the open-generation cap of 2.
5. **`dvocab_len` at a commit.** Should WAL commit records carry the delta-vocabulary
   length, so a historical snapshot hides later terms exactly? That needs a WAL record
   change. Default: no; it is harmless (§5.3).
6. **Open-time GC on a `--read-only` server.** It currently runs, like WAL tail
   truncation. The alternative is to defer policy deletions to the first write-capable
   open. Default: run.
7. **Pins on metadata-only commits** (below `oldestReconstructable` but in the catalog):
   allow them as "bookmarks" that 410 on read? Default: no, 410 at creation.
8. **Relative selectors** (`head~3`, `snapshot:v1~2`). Default: not in Phase 1.

## 10. Sources

* **Sparkles repository, read for this spec:**
  * `crates/sparkles/src/store.rs`: generations, `Generation::open`, `Delta`, `Snapshot`,
    `Store::open` WAL replay and catalog repair, `rebuild_locked` publication order and
    old-generation removal, `publish_log`, `write_atomic` / `write_synced` / `sync_dir`,
    `clone_to`, `dump_nquads`, `wal_bytes`;
  * `crates/sparkles/src/commit.rs`: `CommitInfo`, `Catalog` (open, append, sync, get,
    page), `commit.json`, `dataset.json`, `rfc3339_ms` / `parse_rfc3339_ms`, WAL v2
    record sealing;
  * `crates/sparkles/src/text.rs`: `TextView.seq`, the `search` staleness check,
    `unavailable`;
  * `crates/sparkles/src/vector.rs`: `GenerationVectors`, the per-generation budget;
  * `crates/sparkles/src/index.rs`: `BlockCache` keyed by `PermIndex::uid`,
    `BLOCK_ROWS`, `META_BYTES`, `PermIndex::open`;
  * `crates/sparkles/src/vocab.rs`: `Vocab::open`, `DeltaVocab::open` (tail truncation,
    append handle);
  * `crates/sparkles/src/sparql/cache.rs`: `raw_key`;
  * `crates/sparkles/src/sparql/mod.rs`: `query` signature;
  * `crates/sparkles/src/schema.rs`: `snapshot_identity`;
  * `crates/sparkles/src/error.rs`;
  * `crates/sparkles/src/dataset.rs`: the public API list;
  * `crates/sparkles-server/src/http.rs`: routes, `ApiError` mapping, `query_endpoint`,
    `gsp` GET, `with_commit`, `list_commits` / `get_commit`, `query_options`,
    `with_inferences`, read-only checks;
  * `crates/sparkles-server/src/reasoning.rs`: `freshness`, `inferences_header`;
  * `crates/sparkles-server/src/main.rs`: CLI subcommands and `serve` flags;
  * `docs/API.md`: the protocol, Commits, Full-text search, Vector similarity, Errors
    and Budgets sections;
  * `README.md`: the status and comparison table and the optimizations list. The
    comparison table names another product's history syntax. It was not used, and the
    selector here extends CI's own `commit:N` form;
  * `docs/BENCHMARKS.md`: the update-heavy workload notes;
  * the [CI](CI-commit-identity.md) spec, in full;
  * the other specs, for format and style only ([C06](C06-clone-to-sandbox.md) in part,
    the others' headings, [PROVENANCE.md](PROVENANCE.md)). No other planning document
    was read.
* **Fetched:**
  * RFC 7089 (Memento), https://www.rfc-editor.org/rfc/rfc7089.html: `Memento-Datetime`
    and `Accept-Datetime` syntax, the `Link rel="original"` MUST, TimeGate `Vary`, and
    clamping before the first memento;
  * RFC 9110, https://www.rfc-editor.org/rfc/rfc9110.html: 410, 404, 409, weak and
    strong validators;
  * W3C SPARQL 1.1 Protocol, https://www.w3.org/TR/sparql11-protocol/: parameters,
    and "other 4XX or 5XX" codes;
  * W3C SPARQL 1.1 Graph Store HTTP Protocol, https://www.w3.org/TR/sparql11-http-rdf-update/:
    graph identification, 404 on GET, cacheability.
* **Cited from general knowledge, not fetched:** RFC 3339 (§5.6 date-time), RFC 8246
  (`immutable`), RFC 6648 (`X-` prefix), RFC 9651 (Integer header items, via CI), and
  Apache Jena's RDF Patch format (Open question 3 only; to be checked against the Jena
  documentation, Apache-2.0).
* **Not consulted:** Fluree, in any form. No checkout of Fluree's source repository and
  none of Fluree's documentation, site, tests or talks was opened, searched or fetched.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30 in two commits: the engine (`d866c0c`) and the
server and CLI (`812dc3e`).

* **Engine:** the new `sparkles::history` module (the `At` selectors, `history.json` with
  pins and the retention window, the generation table, the needed set and crash-safe
  collection through `gen-NNNN.deleting`), and the `Store` methods `resolve`,
  `snapshot_at`, `create_snapshot`, `set_retention`, `history` and `dump_nquads_at`.
* **Server:** `?at=` on queries, explain and Graph Store `GET`/`HEAD`, with
  `Sparkles-At`, `Sparkles-Head`, `Memento-Datetime` and `Link rel="original"`;
  `/$/snapshots/{ds}` and `/$/history/{ds}`; and the commit listing's `reconstructable`
  markers.
* **CLI:** `sparkles snapshot create|list|delete|history|retain`, `query --at` and
  `dump --at`.

The limits and their defaults are the spec's: `--history-cache-mb 1024`,
`--history-max-generations 8`, `--max-snapshots 256`.

`Store::open` and history reads share one WAL replay (`replay_wal`), so both number
commits alike, and result-cache keys use the generation and commit, so past and live
states never share entries.

**Deviations.**
* `text:query` at a past commit answers `501` with the general `history-unsupported`
  code, not a separate `text-not-at-commit` code.
* A materialization larger than `--history-cache-mb` fails with the shared budget error
  of [C01](C01-observability-and-budgets.md) (`507`, `budget: "memory"`), not a
  `history-memory` code.

Open questions kept their defaults (retention off unless set; no relative selectors).
Backup repositories ([F05](F05-snapshot-repositories.md)) later added a `backup:<name>`
hold on the generation a backup uploads; a restored dataset starts without history.

**Performance** (acceptance example A23, measured at 10.5M triples with 200,000
single-quad commits in one generation; see
[Benchmarks: Point-in-time reads](../BENCHMARKS.md#point-in-time-reads-105m-triples)):
the first read of a cold commit takes 0.6–0.65 s (target < 1 s); repeat reads reuse the
cached state (0.7–3.6 ms); the base of a sealed generation takes 43 ms (target < 50 ms),
or 89–93 ms right after a restart, which opens that generation's files.

**Not built.** All of Phase 2 (diffs, `Accept-Datetime` and ETags, `maxBytes` retention,
per-pin expiry and scheduled pins, history for in-memory datasets, `at` on schema, stats,
SHACL, clone and backup, history metrics, the UI's snapshot panel and `at` selector) and
Phase 3 (a generation-independent change log, full-text search at pins, history queries,
pin rebasing).
