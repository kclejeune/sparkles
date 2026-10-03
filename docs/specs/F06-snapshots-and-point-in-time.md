# F06: Named snapshots and point-in-time queries

> **Status:** implemented in part
>
> **Phases:** Phase 1 shipped. It covers point-in-time reads with `?at=`, named
> snapshots, the `keepCommits`/`keepAge` retention window, `/$/snapshots`, `/$/history`,
> the `sparkles snapshot` CLI, `query --at` and `dump --at`. Phase 2 shipped. It covers
> diffs, `Accept-Datetime`, `maxBytes` retention, pin expiry and schedules, history for
> in-memory datasets, `at` on more endpoints, history metrics, and the UI's snapshot
> panel and `at` selector. Its replay speedups and warm pins came after the rest,
> together with RDF Patch output for diffs and a change feed. Phase 3 shipped the change
> log and history queries (§11). Full-text search at pins and pin rebasing are deferred.
>
> **User docs:** [API: Point-in-time reads and snapshots](../API.md#point-in-time-reads-and-snapshots) ·
> [API: History queries](../API.md#history-queries) ·
> [Usage: History, snapshots and clones](../USAGE.md#history-snapshots-and-clones) ·
> [Features](../FEATURES.md#storage-tdb2-equivalent) ·
> [Benchmarks: Point-in-time reads](../BENCHMARKS.md#point-in-time-reads-105m-triples)
>
> This is the design as written before implementation. The [Outcome](#outcome) section
> at the end records how it landed.

This design builds on durable commit identity
([CI](CI-commit-identity.md)), which reserved `?at=commit:<n>` and made commit timestamps
non-decreasing so that a `time:` selector is well defined. It delivers the "retained
generations and `?at=`" follow-up that [CI §6](CI-commit-identity.md) names.

## 1. Summary, goals, non-goals

Sparkles already keeps metadata for every commit in `commits.bin`, but it does not keep
old data. Compaction and bulk commits build `gen-N+1`, switch `CURRENT`, and then delete
the old generation with `remove_dir_all` (`Store::rebuild_locked`). In-memory
`Snapshot`s are dropped as soon as no reader holds them.

The data for every commit since the current generation's base is still on disk, as the
base index plus a prefix of `gen-NNNN/wal.log`. A commit in an older generation can be
recovered for as long as that generation's directory exists.

This feature lets users read that history and keep it:

* **Point-in-time reads.** `?at=<ref>` works on `/{ds}/sparql|query`, `/{ds}` queries,
  `/{ds}/explain` and GSP `GET/HEAD`. A ref is `head`, `42`, `commit:42`,
  `time:<RFC 3339>` or `snapshot:<name>`. Any commit that is still *reconstructable*
  can be read.
* **Named snapshots.** A named snapshot is a durable name for a commit, called a *pin*.
  It keeps that commit's data reconstructable across compaction and bulk commits.
  Snapshots can be created, listed and deleted over HTTP, from the CLI and through the
  library.
* **Retention.** An optional per-dataset window keeps the last N commits, or every
  commit from the last D duration, reconstructable. It is off by default.
* **Precise failure.** A read of a commit that is no longer reconstructable returns
  `410`, with a machine-readable body that names what is still available.
* **Accounting.** The history status lists retained generations with their size in
  bytes and the pins that hold them. Garbage collection is crash-safe.

**Goals**

* No change to the write path or its fsync count.
* No extra disk when nothing is pinned. The current generation already provides some
  history for free.
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

* `head`, or no `at` at all, reads the live snapshot, as requests do today.
* `42` and `commit:42` name the state right after commit 42 ([CI §2.1](CI-commit-identity.md)).
* `time:T` names the state as of instant T, which is the **last commit whose timestamp
  is ≤ T**. Catalog timestamps are non-decreasing, so a binary search finds it.
  Fractional seconds beyond milliseconds are truncated. A `+` in a query string decodes
  to a space, so the parser also accepts a single space in place of the offset sign
  (`…14:03:11 02:00` means `+02:00`). `-` offsets and `Z` need no special handling.
* `snapshot:NAME` names the commit that a named snapshot pins.
* The server reads `at` from the query string, or from the form body of an
  `application/x-www-form-urlencoded` POST, as it does `timeout`. A request that gives
  `at` twice with different values gets `400`.
* If `at` resolves to the current head, the request uses the **live snapshot**.
  Full-text search works and no Memento headers are sent (§2.3). Any other commit is
  read from a **historical snapshot** (§5.3).

Headers and JSON use the canonical forms `head`, `commit:N`, `time:<rfc3339 ms Z>` and
`snapshot:NAME`.

### 2.2 Where `at` applies

| Endpoint | `at` |
|---|---|
| `GET/POST /{ds}/sparql`, `/{ds}/query`, `/{ds}?query=` | Phase 1 |
| `GET/POST /{ds}/explain` | Phase 1 |
| GSP `GET/HEAD /{ds}/data`, `/{ds}/get`, `/{ds}` | Phase 1. Whether the graph exists (`404 no such graph`) is decided at the selected commit. |
| `/{ds}/update`, GSP `PUT/POST/DELETE`, `/{ds}/upload` | Rejected with `400 writes cannot target a past state (at=commit:3)`. Even `at=head` is rejected, so the mistake stays visible. |
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
  (RFC 7089 §2.1.1). RFC 7089 requires a `Link rel="original"` header alongside it. The
  link points to the request path and query with `at` removed.
* CORS `Access-Control-Expose-Headers` lists all four `Sparkles-*` headers.
* The Sparkles JSON result format gains
  `meta.at = { selector, commit, timestamp, head, historical }`.
* §2.8 describes `Sparkles-Inferences` at a past commit.
* Phase 2 adds caching headers. A GSP GET with a `commit:` selector gets
  `ETag: W/"<datasetId>:<seq>"` and `Cache-Control: max-age=31536000, immutable`
  (RFC 8246). The ETag is weak because serialization order and blank-node allocation
  follow generation-local ids. The same commit read from two generations is
  semantically identical but not byte-for-byte identical (RFC 9110 §8.8.1). Query
  responses get no caching headers, because `NOW()`, `RAND()` and `SERVICE` make them
  non-deterministic.

### 2.4 Errors

Error bodies keep the `{ "error": … }` shape and gain a stable `code`.

| Condition | Status | `code` | Example message |
|---|---|---|---|
| `at` is malformed, repeated with different values, or has an invalid name | 400 | `invalid-at` | `invalid at 'x': expected head, N, commit:N, time:<RFC 3339> or snapshot:<name>` |
| A write has `at` | 400 | `at-on-write` | `writes cannot target a past state (at=commit:3)` |
| `seq > head` | 404 | `no-such-commit` | `no commit 99 in dataset ds (head is 57)` |
| `time:` is before the root commit | 404 | `before-history` | `no commit at or before 2020-01-01T00:00:00.000Z (dataset created 2026-09-30T14:04:59.800Z)` |
| Unknown snapshot name | 404 | `no-such-snapshot` | `no snapshot 'v9' in dataset ds` |
| The commit existed but its data is gone, or its metadata is gone (`< firstRetained`) | **410** | `history-gone` | See below. |
| In-memory dataset with `at` ≠ head | 501 | `history-unsupported` | `point-in-time reads need a persistent dataset` |
| `text:query` at a historical snapshot | 501 | `text-not-at-commit` | `full-text search is only available at the head (commit 57); this query reads commit 42` |
| Materialization exceeds the history memory budget | 507 | `budget: "history-memory"` | Same as other budgets (`limit`, `requested`). |
| Timeout or cancellation during materialization | 408 / 503 | — | Unchanged. |
| A sealed WAL fails its CRC check before the target commit | 500 | — | `corrupt index: gen-0003/wal.log: checksum mismatch …` |

The `410` body:

```json
{ "error": "commit 12 of dataset ds is no longer reconstructable; the oldest reconstructable commit is 40",
  "code": "history-gone", "at": "commit:12", "commit": 12, "head": 57,
  "oldestReconstructable": 40,
  "reconstructable": [ { "from": 40, "to": 57 } ],
  "metadata": { "seq": 12, "timestamp": "…", "kind": "update", … } }
```

`metadata` is the commit's catalog record, or `null` if the record is gone as well. When
the selector was `snapshot:NAME`, the body adds `"snapshot": "NAME"` and the pin is kept
(§4.4). The status is `410` because the loss is permanent (RFC 9110 §15.5.11).

### 2.5 Named snapshots and history over HTTP

| Method | Path | Result |
|---|---|---|
| GET | `/$/snapshots/{ds}` | `200 { dataset, datasetId, head, snapshots: NamedSnapshot[] }`. Snapshots are sorted by commit, then by name. |
| POST | `/$/snapshots/{ds}` | Takes JSON `{ "name", "at"?: ref (default "head"), "note"?: string ≤ 1024 bytes }`, or the same fields as query or form parameters. Returns `201` with `Location: /$/snapshots/{ds}/{name}` and a `NamedSnapshot` body. Returns `200` if the name already pins the same commit, so a retry is idempotent. |
| GET | `/$/snapshots/{ds}/{name}` | `200 NamedSnapshot`, or `404` |
| DELETE | `/$/snapshots/{ds}/{name}` | `204`. Generations that are no longer needed are collected before the response. |
| GET | `/$/history/{ds}` | `200 HistoryStatus` |
| PUT | `/$/history/{ds}` | Takes a `Retention` body. Returns `200 HistoryStatus` after GC. |

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
  hold more than `maxGenerations` generations (§4.5).
* `410` when `at` is not reconstructable.
* `403 server is read-only` for POST, DELETE and PUT.

The existing commit and stats endpoints gain fields:

* `GET /$/commits/{ds}` gains `oldestReconstructable` and the `reconstructable` ranges.
* Each `Commit` in the list gains `"reconstructable": bool`. A pinned commit also gains
  `"snapshots": [names]`.
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
  files without taking the lock. Their text output looks like this:

```
snapshot     commit  timestamp                 generation  note
release-1        42  2026-09-30T14:03:11.482Z  gen-0003    before migration
nightly          57  2026-09-30T23:00:00.000Z  gen-0005
reconstructable: 40..57   retained: gen-0003 (1.2 GiB, snapshot:release-1)
```

* `query --at` and `dump --at` print `at commit 42 (2026-09-30T14:03:11.482Z)` on stderr.
* In Phase 2, `sparkles log` marks reconstructable commits with `*`.

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
  key changes from `(version, generation name)` to `(generation uid, commit)` (§5.5),
  so entries for different commits or generations never collide. Two reads of the same
  commit through the same generation instance share entries.
  `POST /$/cache/clear/{ds}` clears all of them.
* **Full-text search.** The Tantivy index reflects only the head (`TextView.seq`). A
  historical snapshot has `text: None`, and `text:query` on it fails with `501
  text-not-at-commit`. Queries without `text:query` work at any commit, and `at=head`
  keeps full-text search. Phase 3 may build per-pin indexes (§6).
* **Vector search** works at any commit with no special code. `GenerationVectors` lives
  on the `Generation` instance, and each query overlays its snapshot's delta, so a
  historical snapshot sees exactly its own vectors. A retained generation packs its
  vectors lazily under the same per-generation budget. At most
  `history_open_generations` (2) historical generations are open at once, so total
  vector memory stays within 3× the budget (Open question 4).
* **Reasoning.** A historical read includes the inferred graph as it was at that commit,
  and `reasoning=true|false` works as today. `Sparkles-Inferences` for commit s is
  computed from the catalog. Let r be the latest commit ≤ s whose kind is `reason` or
  `reason-clear`. Then:
  * if r is a `reason-clear`, or r is a `reason` and r = s, there is no header;
  * if r is a `reason` and r < s, the header is `stale; commits-since=<s−r>`;
  * if the retained metadata has no such r, the header is `unknown`.

  A materialization run that changed nothing creates no commit, so the header may
  overstate staleness. That errs in the conservative direction. Historical reads do not
  use the current `reasoning.json`.
* **Read-only servers** (`--read-only`) serve every `at` read and
  `GET /$/snapshots|history`, and answer POST, DELETE and PUT with `403`. Opening the
  store still runs recovery: WAL tail truncation as today, plus history GC (§4.3). Open
  question 6 covers the alternative.
* **Compaction and bulk commits** keep their current semantics and cost. The one change
  is that the outgoing generation is kept when a pin or the retention window needs it
  (§4.2). The compaction task message then says
  `kept gen-0003 (1.2 GiB) for snapshot:release-1`.
* **Clone** (C06) copies no history and no pins. Phase 2 adds `clone?at=REF`, which
  records the resolved commit as `forkedFrom.seq`.

## 3. Standards basis

* **SPARQL 1.1 Protocol §2.1** defines `query`, `default-graph-uri` and
  `named-graph-uri` and says nothing about other parameters. `at` is an extension
  parameter, like the existing `timeout`, `reasoning` and `nocache`. §2.1.5 allows
  "other 4XX or 5XX HTTP response codes for other failure conditions", which covers
  404, 410, 501 and 507.
* **SPARQL 1.1 Graph Store HTTP Protocol.** `?graph=` and `?default` identify the graph,
  and `at` only chooses its state. The rule "404 Not Found MUST be provided" for missing
  graph content applies at the selected commit. The protocol says responses "SHOULD be
  made cacheable wherever possible", which motivates the Phase 2 ETag.
* **RFC 3339 §5.6** defines the `time:` date-time syntax.
  **[CI §2.1](CI-commit-identity.md)** makes millisecond timestamps non-decreasing,
  so "last commit ≤ T" is well defined.
* **RFC 7089 (Memento).** `Memento-Datetime` "indicate[s] that a response reflects a
  prior state of an Original Resource". Its value is an RFC 1123 date in GMT, and a
  response that sends it MUST include a `Link` with `rel="original"`.
  `Accept-Datetime` and TimeGate negotiation (`Vary: accept-datetime`) are Phase 2.
  Under RFC 7089, a TimeGate clamps a datetime before the first memento to the first
  memento. `?at=time:` returns 404 `before-history` instead, because the dataset had no
  state at that time. Phase 2's `Accept-Datetime` support follows RFC 7089 and clamps to
  the oldest reconstructable commit.
* **RFC 9110.** §15.5.11 (410 Gone, "likely to be permanent") covers collected history.
  §15.5.5 (404) covers what never existed, and §15.5.10 (409) covers pin conflicts.
  §8.8.1 makes weak validators the right choice for representations that are
  semantically equivalent but not byte-identical.
* Phase 2 uses the `immutable` directive of **RFC 8246**. Header names follow
  **RFC 6648** and carry no `X-` prefix.

## 4. Semantics

### 4.1 Reconstructable commits

For a generation G, write `range(G) = [base(G), end(G)]`, where:

* `base(G)` is the `baseSeq` in `G/commit.json`;
* `end(G)` is the last commit in G's WAL, or `base(G)` if the WAL is empty. For the
  current generation it is the head.

Consecutive generations meet in one of two ways:

* **compaction**: `base(G+1) = end(G)`. The data is the same, with new ids.
* **bulk commit**: `base(G+1) = end(G) + 1`.

A commit s is **reconstructable** iff s ∈ range(G) for some *retained* generation G,
meaning the current generation or a kept one. Its **owner** is the newest such G. When
s = base(owner), the state is the base index with an empty delta. Otherwise it is the
owner's base index plus its WAL replayed through commit s.

With nothing pinned and retention off, the reconstructable set is exactly
`range(current)`. That is every commit since the last compaction or bulk commit, and it
costs no extra disk.

### 4.2 What keeps a generation

`Protected` is the union of:

* the head;
* the commit of every named snapshot;
* the **retention window**, which is every commit s that was the head at some instant
  inside the window. With `keepCommits: N`, that is `s > head − N`. With `keepAge: D`,
  it is s = head or `ts(s+1) > now − D`, which covers the state as of *any* instant in
  the last D. With both set, the window is their union.

A non-current generation G is **needed** iff it owns some s ∈ Protected: s ∈ range(G)
and no newer retained generation covers s.

At a compaction, the outgoing G covers the head only at `end(G)`, and the new
generation covers that commit as well. A pin *at the head* therefore never retains G,
so pinning and then compacting costs nothing. A pin anywhere else in G retains all of G,
which makes every other commit of G reconstructable as a side effect. The `heldBy`
field in the history listing shows what holds each generation.

### 4.3 Garbage collection (GC)

GC runs under the writer mutex and the history mutex, at three points:

* after every rebuild (compaction or bulk commit), in place of today's unconditional
  `remove_dir_all(old)`;
* after a pin is deleted or the retention changes;
* at `Store::open`.

The window's time edge moves without any event, so history that ages out is collected
at the next of these points. Phase 2 adds an hourly server tick. Each GC run:

1. Removes `gen-*.deleting` directories left by an interrupted GC.
2. Removes `gen-N` directories with N greater than the current generation number.
   These are interrupted rebuilds, and `rebuild_locked` already overwrites such a
   directory today.
3. Checks each non-current `gen-N` that has a readable `commit.json` with a matching
   dataset id. If the generation is not needed, GC renames it to `gen-N.deleting`,
   fsyncs the root, then runs `remove_dir_all`.
4. Leaves alone, with a logged warning, any directory it cannot identify: one with no
   `commit.json`, a foreign dataset id or an unparsable name. Sparkles never deletes
   what it cannot attribute.

Readers that already hold a historical snapshot keep working. On Unix their open files
(mmaps, `.dat` handles, the WAL handle) stay valid after the unlink. On Windows the
rename fails while a file is open. GC logs the failure, and the directory stays listed
as "pending deletion" until the next GC point.

### 4.4 Pins

* A pin names a commit, not a generation. Sparkles always derives which generation
  holds it.
* A pin can only be created at a reconstructable commit. Otherwise the request gets
  410. Creation and deletion are durable (`write_atomic`) before the response is sent.
* Pins are immutable. Re-creating a name at the same commit is idempotent (200), and
  re-creating it at another commit is 409. To move a pin, delete it and create it
  again.
* A pin whose data is gone is **kept** and listed with `reconstructable: false`, and
  reads of it return 410. This happens only after external damage, such as a
  hand-deleted directory or a failed disk. Whether to delete the pin is the user's
  decision.
* Deleting a pin never deletes data that another pin or the window still needs.

### 4.5 Limits and budgets

| Limit | Default | Flag / option | Enforcement |
|---|---|---|---|
| Named snapshots per dataset | 256 (hard cap 10 000) | `--max-snapshots`, `StoreOptions::max_snapshots` | 409 `history-limit` on create |
| Non-current generations held by **pins** | 8 | `--history-max-generations` | 409 `history-limit` on create. A pin in the current generation counts as one more held generation, because it will retain one at the next rebuild. |
| Generations held by the **window** | Shares the pin limit | | Best effort. The oldest generations held only by the window are dropped first to stay within the limit. Pins always win. |
| Historical generations open at once | 2 | `StoreOptions::history_open_generations` | An LRU of `Arc<Generation>` |
| Memory of materialized history | 1 GiB | `--history-cache-mb` | LRU eviction. A single materialization that would exceed the limit fails with 507 `history-memory`. The check runs every 64 Ki records. |
| Time to materialize | The query's timeout | `timeout=` | Checked every 64 Ki records, along with the cancel flag. A materialization that times out is not cached. |

Phase 1 has no hard cap on disk use. Each retained generation costs about one full
index: the vocabulary, 7 permutations, its WAL and its delta vocabulary.
`HistoryStatus.bytes`, `history.bytes` in `/$/stats` and the compaction task message
report that cost. Phase 2 adds `Retention.maxBytes`, which trims generations held only
by the window.

### 4.6 Materialization targets

These targets are estimates for Phase 1 to confirm. They assume a release build, a warm
page cache and a 10 M-quad base.

* Opening a sealed generation takes < 50 ms. It mmaps the vocabulary and reads the
  block metadata, which is 100 B per 32 Ki-row block per permutation, or ≈ 200 KB at
  10 M quads. It also loads the generation's `delta.vocab` into memory.
* WAL replay runs at ≥ 1 M records/s. Each record costs one SPO membership probe plus 7
  ordered-set updates. For example, 100 k single-quad commits (200 k records) take
  ≈ 0.2 s.
* A cached historical snapshot adds < 1 ms to a query.
* Memory is ≈ 300 B per quad in the replayed delta, plus 2 × the `delta.vocab` size
  per open generation. The 300 B is 7 × a 32-byte key plus tree overhead, to be
  measured and set in `Delta::approx_bytes`. Decoded blocks go through the shared block
  cache (`--cache-mb`), so they may evict live blocks.

A missed target is not a blocker. The README status records the measured numbers.

## 5. Design

### 5.1 Recommended: retained generations plus sealed WALs (alternative A)

Sparkles keeps every generation directory that owns a protected commit, and
materializes a past state by replaying a prefix of that generation's WAL. Every piece
this needs already exists and is durable:

* the base index;
* `commit.json` (`baseSeq`);
* a WAL whose records carry `seq` and a CRC ([CI §5.3](CI-commit-identity.md));
* the catalog, whose `generation` field records which generation's WAL holds each WAL
  commit.

Nothing is added to the write path. The only change to rebuilds is that they *do not
delete* a needed generation.

Once `CURRENT` moves away from a generation, nothing writes to it again, and it is
**sealed**. Its WAL was fsynced per commit. Every `delta.vocab` entry that a WAL record
references was synced before that commit (`dvocab.sync()` in `publish_log`). A torn
tail can only come from an aborted intern that nothing references, so it is ignored. A
sealed generation is therefore safe to open read-only at any later time.

### 5.2 Alternatives compared

| | A. Retained generations + sealed WALs | B. Checkpoints + generation-independent change log | C. Materialized copy per pin |
|---|---|---|---|
| Idea | Keep the `gen-N` directories that own protected commits, and replay a WAL prefix. | Log every commit's net changes as term keys to `history/changes-*.log`, with periodic full checkpoints. Replay forward from a checkpoint, or backward from the head, across generations. | On pin, build a dedicated frozen generation from that snapshot under `snapshots/<name>/`, as `clone_to` does. |
| Arbitrary commit / time | Yes, within retained ranges | Yes, anywhere after the oldest checkpoint | No. Named points only. |
| Disk | About 1 full index per retained generation. Nothing when nothing is pinned. | The changes (≈ 100–200 B per changed quad as keys) plus checkpoints | 1 full index per pin, with no sharing |
| Write-path cost | None | An extra log write per commit. Bulk commits must compute and log their change set, which is expensive. | None |
| Pin cost | O(1), paid at the next rebuild | O(1) | A full rebuild (minutes at 10⁸ quads) |
| Read cost | An open (ms) plus a replay of at most one WAL | A replay across many commits, interning keys into a temporary vocabulary | ~0 |
| Diffs | Cheap within and across compaction boundaries. A full compare across a bulk commit. | Natural | Full compare |
| New formats | `history.json` only | A change log and a checkpoint manifest | A pin directory layout |
| Effort | 2–4 days | 1.5–3 weeks | 2–3 days, but narrower in scope |

**Recommendation: A.** It reuses the durable state Sparkles already has. It costs
nothing when unused, leaves the write path untouched, and fits the MVP budget. B is the
better design if retained full copies come to dominate disk use, for example with
frequent compactions under a long window. It stays a Phase 3 option (Open question 2).

**Rejected** (see also §8):

* C as the primary design. It has no time or commit selectors and needs a full rebuild
  per pin.
* Keeping `Arc<Snapshot>`s in memory for persistent datasets. They are lost on restart,
  and their memory is unbounded. This fits only in-memory datasets (Phase 2).
* Filesystem snapshots or reflinks (btrfs, ZFS, APFS clones). They are
  platform-specific and outside the store's crash model.
* Temporal permutations, with valid-from and valid-to columns on all 7 permutations.
  That changes the index format and adds cost to every scan.

### 5.3 Materializing a historical snapshot

`Store::snapshot_at(at)` runs in five steps:

1. **Resolve**, with no locks beyond the catalog. A commit selector gives the seq
   directly, a time goes through a catalog binary search (`Catalog::at_time(ms)`), and a
   snapshot name gives its pin. Check `seq ≤ head` (else 404) and `seq ≥ firstRetained`
   (else 410 with `metadata: null`). If `seq == live.commit`, return the live snapshot.
2. **Locate and lease** under the history mutex:
   * Find `owner(seq)` in the in-memory generation table (§5.4). If there is none,
     return 410.
   * Look up `(owner, seq)` in the snapshot LRU, and return it on a hit.
   * Otherwise get the owner's `Arc<Generation>`. For the current generation that is
     the live one. Its `dvocab` is shared, and ids below its length are stable. For an
     older generation, the open-generations LRU has it, or `Generation::open_sealed(dir)`
     opens it. `open_sealed` is `open` with `DeltaVocab::open_read_only`, which does not
     truncate, has no append handle and ignores a torn tail.
   * Open the WAL with a read-only handle.

   These opens take milliseconds, and holding the mutex during them is what makes GC
   safe. GC takes the same mutex, and once the files are open, an unlink cannot hurt
   the reader.
3. **Single-flight.** Concurrent requests for the same `(owner, seq)` wait on one
   `OnceLock`-style slot, so only one replay runs.
4. **Replay** without holding any lock. Stream the WAL in 1 MiB chunks through the
   replay function (§5.6), and stop after the commit record for `seq`. Every 64 Ki
   records, check for cancellation, the deadline and the memory estimate. If the base
   is exactly `seq`, skip the replay.
5. **Publish.** Build `Snapshot { generation: owner, delta, version: 0, historical: true,
   commit: seq, text: None, dvocab_len: owner.dvocab.len(), cache: store block cache,
   results: store result cache, union_default_graph: store option, … }` and insert it
   into the LRU. Its weight is `delta.approx_bytes()`, plus the generation's
   `delta.vocab` bytes when it is the only entry holding that generation.

`dvocab_len` is the generation's full length, because its length at `seq` is not
recorded. Terms interned after `seq` resolve to ids that match no quad, which is
harmless.

Materialization happens **on demand**. Pinned snapshots are not kept in memory. Phase 2
may add `warm: true` on a pin to keep it materialized under the same budget.

**Phase 2 speedups:**

* Reuse a cached `(owner, s1)` delta and its WAL offset to reach `s2 > s1`.
* For the current generation, replay **backwards** from the live head delta by applying
  inverse ops. Every logged op took effect, so the inverse is exact. Sparkles picks the
  cheaper direction by record count.
* Keep a sparse offset index (seq → WAL offset every 1024 commits).

### 5.4 History state and files

`<root>/history.json` (format 1) is written with `write_atomic`:

```json
{ "format": 1, "datasetId": "3f1c9a2e-…",
  "retention": { "keepCommits": null, "keepAgeMs": null },
  "snapshots": [
    { "name": "release-1", "seq": 42, "created": "2026-09-30T15:00:00.000Z", "note": "before migration" } ] }
```

The file stores commits, never generation names. A missing file means no pins and no
retention. If `datasetId` does not match, as in a copied directory, the open fails with
`Error::Corrupt`. An older binary ignores the file. At its next compaction it deletes
only the generation it compacts from, and leaves the retained ones behind as unmanaged
directories.

In memory, `Store` gains `history: Mutex<HistoryState>`, which holds:

* `pins: BTreeMap<String, Pin>`;
* `retention`;
* `gens: BTreeMap<u32, GenEntry { name, base_seq, end_seq, bytes, dir }>`;
* the LRUs and hit/miss counters.

Sparkles builds the generation table at open:

* `base_seq` comes from `commit.json`.
* `end_seq` is the largest catalog seq with `generation == N`, or `base_seq` if there
  is none. When the catalog cannot supply it, because the metadata predates
  `firstRetained`, a scan of the WAL's commit records does.
* `bytes` comes from one `dir_size` call per sealed generation, since sealed
  generations never change.

The current generation's `end_seq` tracks the head.

### 5.5 Changes to existing code

* `store.rs`:
  * `rebuild_locked` calls `history.after_rebuild(old, new)` in place of
    `remove_dir_all(old)`;
  * `open` builds the history state and runs GC after WAL replay and catalog repair;
  * the WAL replay loop in `open` moves into a reusable function (§5.6);
  * `Snapshot` gains `historical`;
  * `Generation` gains `open_sealed` and a `uid: u64` that is unique within the process,
    like `PermIndex::uid`;
  * the new `Store` methods (§2.7), including `dump_nquads_at`.
* `vocab.rs`: `DeltaVocab::open_read_only`.
* `commit.rs`: `Catalog::at_time(ms) -> Option<u64>` (a binary search) and
  `Catalog::last_of_kinds_at_or_before(seq, &[Reason, ReasonClear])`. RFC 3339 parsing
  gains offsets; today it accepts only `Z`.
* `sparql/cache.rs::raw_key`: replace `v{version}|{generation.name}` with
  `g{generation.uid}|c{commit}`. This is exactly as precise for live snapshots, because
  the data is a function of the generation instance and the commit, and compaction
  creates a new instance.
* `schema.rs::snapshot_identity`: return the commit, as its comment already plans, so
  cursors survive restarts.
* `text.rs::search`: when `snap.historical`, return
  `Error::HistoryUnsupported(text-not-at-commit)` before the "no index" check.
* `error.rs` and the `From<Error>` mapping in `http.rs`: the new variants, with a
  `code` member in their error bodies.
* `sparkles-server/http.rs`:
  * `fn at_param(&Params) -> ApiResult<Option<At>>`;
  * when `at` is set, `query_endpoint`, `explain` and `gsp` GET/HEAD call
    `store.snapshot_at` inside `blocking(…)`, then add
    `history_headers(resolved, uri)`;
  * `update_endpoint`, GSP writes and `upload` reject `at`;
  * new routes `/$/snapshots/{ds}[/{name}]` and `/$/history/{ds}`;
  * the new headers in the CORS expose list;
  * `with_inferences` takes the resolved commit and, for a historical read, applies the
    catalog rule of §2.8.
* `sparkles-server/main.rs`: the `Snapshot` subcommand group, `--at` on `Query` and
  `Dump`, and the `serve` flags.
* `docs/API.md` gets a "Point-in-time reads and snapshots" section, and `README.md` a
  history row.

### 5.6 Replay function

```rust
pub(crate) struct Replay { pub delta: Delta, pub commits: Vec<CommitInfo>, pub next_bnode: u64,
                           pub good_bytes: u64, pub records: u64 }
pub(crate) enum Stop { End, AfterSeq(u64) }
pub(crate) fn replay_wal(gen_: &Arc<Generation>, cache: &Arc<BlockCache>, base: CommitInfo,
                         fold_legacy: bool, wal: impl Read, stop: Stop,
                         check: &mut dyn FnMut(u64 /*records*/, &Delta) -> Result<()>) -> Result<Replay>;
```

* `Store::open` calls it with `Stop::End` and keeps today's behavior. It truncates a torn
  tail at `good_bytes`, and returns `Error::Corrupt` for a CRC mismatch before the last
  transaction.
* History reads call it with `Stop::AfterSeq(s)` on a sealed or live WAL and never
  truncate. A CRC mismatch at or before `s` is `Error::Corrupt`. Records after `s` are
  never read.
* Legacy (version-0) records get the same seqs as at open, because the same `base` and
  `fold_legacy` inputs number them deterministically.

### 5.7 Crash safety

| Step | What is written | fsync |
|---|---|---|
| WAL commit | Unchanged | `sync_data` per commit (CI) |
| Rebuild | Unchanged order: sync the catalog, write and sync the new generation's files and `commit.json`, sync the directories, then switch `CURRENT` with `write_atomic` | As today |
| After the `CURRENT` switch | Keep the old generation (no writes), or GC it | See GC |
| Pin create or delete, retention change | `history.json` via `write_atomic`: write and `sync_all` a temporary file, rename it, fsync the root directory | **Before** the response, and before any GC that depends on it |
| GC of a generation | Rename `gen-N` to `gen-N.deleting`, fsync the root, then `remove_dir_all` | The rename only |
| Open | Remove `*.deleting` and any `gen-N` with N > current, GC per policy, and ignore a stale `history.json.tmp` | The renames |

Invariants:

1. A directory named `gen-N` (not `.deleting`) with a valid `commit.json` is complete. A
   rebuild writes and syncs `commit.json` before `CURRENT` names the generation, and GC
   renames a directory before deleting it.
2. A generation needed by the durable pins and the window is never renamed. Pins are
   durable before anything relies on them, and a pin's removal is durable before any
   generation it held is renamed.
3. A crash at any point leaves every durably pinned commit reconstructable, unless files
   were damaged externally.

## 6. Phasing

**Phase 1 (MVP, ~3 days).**

* Day 1: `history.rs`, with `At` parsing, `history.json`, the generation table, the
  needed set, and GC through `.deleting`. Also the rebuild hook, open-time recovery,
  `DeltaVocab::open_read_only`, `Generation::open_sealed`, and moving the WAL replay
  into `replay_wal`.
* Day 2: `resolve` and `snapshot_at`, with the live-generation and sealed-generation
  paths, single-flight, the LRUs, the budget, cancellation and the deadline. Also
  `Catalog::at_time`, the cache-key switch, `snapshot_identity`, the text guard and the
  errors.
* Day 3:
  * HTTP: `at` on query, explain and GSP GET/HEAD, write rejection, the headers
    including Memento, `/$/snapshots`, `/$/history` GET and PUT, `reconstructable` in
    the commit list, stats, and historical `Sparkles-Inferences`.
  * CLI: `snapshot create|list|delete|history|retain`, `query --at` and `dump --at`.
  * The `Dataset` forwarding API, `docs/API.md`, the README, and tests A1–A22.
* Phase 1 retention covers `keepCommits` and `keepAge`, which share the pin generation
  limit.

**Phase 2.**

* **Diffs** between two reconstructable commits:
  * The interfaces are `GET /{ds}/diff?from=REF&to=REF`,
    `sparkles diff --loc DB FROM TO` and `Store::diff(from, to, sink)`.
  * Output is `application/json`
    (`{from, to, added, removed, quads?: [{op:"+"|"-", …}]}`, with quads only when
    `quads=true`), or N-Quads lines prefixed with `+ ` or `- ` (`text/x-sparkles-diff`).
    RDF Patch output is an open question.
  * A diff is the net set difference. `added` holds what is in `to` and not in `from`,
    and `removed` the reverse. Blank nodes keep their `_:b<hex>` labels, so diffs are
    meaningful within one lineage.
  * The algorithm walks the WAL segments between the two commits, decodes each op to
    term keys, and tracks `key → (present at from, present now)`. Memory is O(changed
    quads). This crosses compaction boundaries at no cost, because compaction does not
    change the data. A bulk commit between the two commits forces a fallback: a sorted
    key-stream comparison of both states under a `--diff-max-quads` budget, with 507
    beyond it. Phase 3 logs bulk change sets, which removes the fallback.
* `Retention.maxBytes`, per-pin `expires`, scheduled pins
  (`{every: "1d", keepLast: 7, prefix: "daily-"}`) and an hourly GC tick in the server.
* `Accept-Datetime` on GSP GET (an RFC 7089 TimeGate with `Vary: accept-datetime`),
  ETag and `immutable` for `commit:` reads, and `If-None-Match`.
* History for in-memory datasets: pins and a window kept as `Arc<Snapshot>`s, which
  structural sharing makes cheap.
* The replay speedups of §5.3, and `warm` pins.
* `at` on schema, stats, SHACL, clone and backup, reconstructable markers in
  `sparkles log`, and metrics (`sparkles_history_bytes`, `…_materialize_seconds`, cache
  hits and misses).
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

* **B or C as the MVP** (§5.2). B changes the write path and needs new formats. C
  supports only named points and costs a rebuild per pin.
* **In-memory `Arc<Snapshot>` retention for persistent datasets.** It does not survive a
  restart, and its memory is unbounded. It remains the plan for in-memory datasets
  (Phase 2).
* **Storing generation names in pins.** The names would go stale across compaction.
  Pins store commits, and the generation is derived.
* **Deleting a pin's data when its generation is missing.** That would lose data
  silently. The pin stays and is flagged instead.
* **`404` for collected commits.** It hides the difference between "never existed" and
  "no longer kept". RFC 9110 defines 410 for the second case.
* **RFC 7089 clamping for `?at=time:` before the root.** That would answer with a state
  the dataset never had at T. Phase 2's `Accept-Datetime` negotiation does follow the
  RFC.
* **A strong ETag per commit.** GSP serialization order and blank-node allocation follow
  generation-local ids, so the bytes differ after compaction.
* **Refusing compaction when retention would exceed a limit.** Compaction reduces memory
  and must stay available. The window trims itself instead, and pins are checked when
  they are created.
* **A `sealed.json` per generation.** `end_seq` can be derived from the catalog's
  `generation` field, with a WAL scan as the fallback, so there is one less file to
  make crash-safe.
* **`Snapshot::version` in the cache key for historical snapshots.** Versions are
  in-process counters and could collide with a live version of the same generation.
  `(generation uid, commit)` is exact.
* **`X-Sparkles-*` headers or product-specific selector syntaxes.** RFC 6648 retires the
  `X-` prefix, and the selector follows the `commit:N` form that CI already reserved.

## 9. Open questions

1. **Default retention.** Retention is off, so only the current generation and pins are
   kept. Should new datasets default to `keepCommits: 1000` or `keepAge: "24h"`? The
   cost is up to one extra full index per compaction. Default: off.
2. **Alternative B** as the long-term history store, if retained generations come to
   dominate disk use. That needs a decision on logging bulk change sets, whose cost
   scales with the bulk batch. Default: revisit after Phase 1 measurements.
3. **Diff output format.** Should diffs offer only the Sparkles `+`/`-` N-Quads lines and
   JSON, or also Jena's RDF Patch (`application/rdf-patch`, `A`/`D` rows) for
   compatibility with the Jena ecosystem? The RDF Patch syntax must be checked against
   Jena's documentation before adoption. Default: Sparkles formats only.
4. **Vector budget.** Should the vector budget become process-wide rather than
   per-generation, so historical generations cannot multiply it? Default: per generation,
   plus the open-generation cap of 2.
5. **`dvocab_len` at a commit.** Should WAL commit records carry the delta-vocabulary
   length, so a historical snapshot hides later terms exactly? That needs a change to
   the WAL record. Default: no, because the extra terms are harmless (§5.3).
6. **Open-time GC on a `--read-only` server.** In this design it runs, as WAL tail
   truncation does. The alternative is to defer policy deletions until the first
   write-capable open. Default: run.
7. **Pins on metadata-only commits**, which are below `oldestReconstructable` but still
   in the catalog. Should they be allowed as bookmarks that return 410 on read?
   Default: no, creating one returns 410.
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
    comparison table names another product's history syntax. This spec does not use
    it; the selector extends the `commit:N` form that CI defined;
  * `docs/BENCHMARKS.md`: the update-heavy workload notes;
  * the [CI](CI-commit-identity.md) spec, in full;
  * the other specs, for format and style only: part of [C06](C06-clone-to-sandbox.md),
    the headings of the rest, and [PROVENANCE.md](PROVENANCE.md). No other planning
    document was read.
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
  Apache Jena's RDF Patch format (Apache-2.0). The RDF Patch format matters only for
  Open question 3 and is still to be checked against the Jena documentation.

## 11. Phase 3: history queries and the change log

This section was written on 2026-10-03, after Phase 2 and its follow-ups had shipped. It
designs the three Phase 3 items of §6: history queries across commits, a change log that
does not depend on the retained generations, and full-text search at pins.

### 11.1 Goals

* **History queries.** Users ask how the data changed, such as when a triple was added
  or removed, which commit last changed a subject, or which values a predicate took over
  time. The answer is a list of change events, each with its commit's number, time,
  kind, author and message.
* **Reach.** History must not end at the last compaction. With nothing pinned, Phase 1
  and 2 can only see the current generation's commits, because compaction deletes the
  older write-ahead logs.
* **Cost.** Single-triple commit latency must not regress noticeably. The write-ahead
  log is synced once per commit (CI), and that sync dominates the latency of a small
  commit, so the new design must not add a second sync to the commit path.
* **Standard surface.** Queries stay in SPARQL 1.1 syntax with RDF 1.2 triple terms,
  which the parser already supports. There is no new keyword.
* **Access control.** A caller sees the history only of what it may read. Graph views
  ([C12](C12-graph-access-control.md)) and triple protections
  ([C12b](C12b-triple-access-control.md)) apply to history rows, so the history of a
  hidden triple is hidden too.

### 11.2 Prior art

These systems and standards were studied for their public, documented behavior:

* **SQL:2011 system-versioned tables.** Every row version carries a system-time period,
  and `FOR SYSTEM_TIME AS OF`, `FROM … TO`, `BETWEEN … AND` and `ALL` select versions.
  Sparkles' `?at=` already covers `AS OF`. A history query is the `ALL` case reduced to
  the moments where versions start and end.
* **Datomic.** A database value can be read as of a time, since a time, or as a history
  database that holds every assertion and retraction. Each datom carries its entity,
  attribute, value, transaction and an `added` flag, and a transaction is itself an
  entity with its time. A Sparkles change row has the same parts, with the quad as the
  fact and `op` as the flag.
* **Dolt.** Tables such as `dolt_diff_<table>` list row changes with the commit, its
  author and date and a `diff_type` of added, modified or removed. `dolt_history_<table>`
  lists every row at every commit. History is queried in plain SQL against these
  tables. Sparkles exposes its change rows the same way, as a source that ordinary query
  operators (joins, aggregates, filters) consume.
* **TerminusDB.** Its history API lists the commits that changed a document. That is a
  history query restricted to one subject.
* **R43ples.** A revision is stored as an added set and a deleted set of triples per
  graph, and queries name a revision with a SPARQL extension keyword. The change log
  stores the same add and delete sets per commit. The keyword is not adopted, because a
  standard parser rejects it.
* **Quit Store.** It keeps an RDF dataset in Git and describes commits with PROV-O,
  including a blame view of which commit added each statement. Its provenance graph is
  data a query can join, which is the model here, but Sparkles does not materialize it
  as a named graph.
* **RDF 1.2 and SPARQL 1.2 reification.** `<< s p o >>` is a reifier: a resource that
  `rdf:reifies` the triple term `<<( s p o )>>`. A change event is a natural reifier: it
  is an occurrence of a triple, with its own properties.

### 11.3 The change log

**What it holds.** For every commit, the net quad changes relative to its parent, as
vocabulary keys. Keys are the same in every generation, so the log is independent of
compactions and bulk commits. A record also holds the commit's number, timestamp, kind,
counts, author and message, so it stays readable after the catalog horizon prunes the
commit's metadata.

**Files.** A persistent dataset keeps `<root>/changes/`. It is a sequence of segments
named by their first commit. Each segment starts with a 32-byte header naming the
dataset, then holds framed records (length, CRC-32, body). A record is one of three
kinds: the changes of a commit, a summary of a commit whose changes were not recorded,
or a gap covering commits the log could not record. A segment that reaches
`change_log_segment_bytes` (16 MiB) is sealed. Sealing writes an index file next to it
and syncs both. The index lists the offset and timestamp of every record and a sorted
array of (term hash, record number) pairs for the subject, predicate, object and graph
of every change. The open segment keeps the same index in memory, and an open rebuilds
it by reading the segment once. A dataset's own settings are in `changelog.json`.

**Write path.** A commit through the write-ahead log appends its records and syncs as
before. It then pushes its change list, still as the generation's ids, onto an
in-memory queue, which costs a vector copy and a mutex. One background thread for the
whole process takes queued commits after a 5 ms pause, turns the ids into keys, nets out
a quad changed twice in one transaction, and appends the records without syncing. It
syncs at most once a second. Like the write-ahead log, the open segment grows by zeros
written ahead of its records, so a sync overwrites allocated blocks and needs no journal
commit on file systems that update blocks in place. A history query appends whatever is queued before it
reads, so it always sees every commit up to the state it reads.

**Durability.** The write-ahead log stays the durable copy of each commit. The change
log must be synced only where that copy could disappear:

* before a compaction switches `CURRENT`, because the old generation's log goes then (the
  sync runs before the final writer lock, since the new base is fixed at the start);
* before a bulk commit switches `CURRENT`, for the same reason;
* when the store closes.

After a crash, an open reads the log, truncates a torn or damaged tail, and rebuilds a
missing sealed index. It then appends the commits after the last recorded one from the
current generation's write-ahead log. That log starts at or before the last synced
record, because of the sync before every switch. Commits recovered this way have no
author, because the write-ahead log does not hold one. Their messages come from the
annotations file. A commit that no retained log holds is recorded as a gap.

**Bulk commits** have no write-ahead log records. Their changes are computed when the
two states are at hand, while the bulk commit holds the writer lock and before it is
published. If the dataset was empty, every quad of the new generation is an addition. If
both states together hold at most `change_log_bulk_max_quads` (1,000,000) quads, the two
states are compared. Otherwise the commit is recorded as a summary with its counts. The
first load of a large dataset is therefore not copied into the log. A crash after the
switch and before the background writer records the bulk commit leaves that commit
unrecorded, unless a retained generation still holds the state before it.

**Retention.** Whole sealed segments are dropped, oldest first. `keepCommits` and
`keepAge` keep the segments that hold any of the last N commits or any commit of the
last D. `maxBytes` caps the total, and the size limit wins over the window. The default
is on, with no window and 1 GiB. The open segment is never dropped. Retention runs when a
segment is sealed and in the minute history upkeep, which applies the age limit.

**Settings.** `StoreOptions::change_log` turns the log on for new and existing datasets
(on by default). `change_log_max_bytes`, `change_log_segment_bytes` and
`change_log_bulk_max_quads` set the limits. A dataset's `changelog.json` can turn it off
or on and set `keepCommits`, `keepAge` and `maxBytes`. Turning it off removes the
directory. Turning it on starts it at the next commit and records the commits before it
as a gap. An in-memory dataset keeps the same records in memory, at most 64 MiB.

**What the log is not.** It is not a second write-ahead log. Point-in-time reads still
materialize states from the retained generations, and a state the generations no longer
hold cannot be read even when the log has its changes. Rebuilding a state from the log
would need a checkpoint to start from, which is alternative B of §5.2 in full, and it
stays out of scope.

### 11.4 History queries

**SPARQL.** A history query is a `SERVICE` call to `urn:x-sparkles:history#changes`
whose block holds one reified triple pattern. The reifier stands for a change event.

```sparql
PREFIX hist: <urn:x-sparkles:history#>
SELECT ?value ?op ?commit ?time ?author WHERE {
  SERVICE hist:changes {
    << <http://example.org/alice> <http://example.org/name> ?value >>
        hist:op ?op ; hist:commit ?commit ; hist:time ?time ; hist:author ?author .
  }
}
```

* The triple's constants are looked up in the log's index. Its variables are bound per
  change. A blank node in the triple matches anything.
* `hist:op` is `"add"` or `"remove"`, as an output or as a filter.
* `hist:graph` binds the graph, and is unbound for the default graph. As a constant it
  filters, and the IRI `hist:defaultGraph` selects the default graph. Without it, the
  changes in every graph the caller may read are listed.
* `hist:commit` (`xsd:integer`), `hist:time` (`xsd:dateTime`), `hist:kind`,
  `hist:author` and `hist:message` describe the commit and are outputs only.
* `hist:from` and `hist:to` bound the commits read, inclusively. Each takes a commit
  number, an `xsd:dateTime` (the first commit at or after it, or the last at or before
  it) or a selector string such as `"commit:42"`. `hist:to` defaults to the commit the
  query reads, so a query with `?at=commit:42` sees the history up to commit 42, as an
  `AS OF` read would.
* `hist:limit` caps the changes, and `hist:order hist:descending` lists the newest
  commits first. With both, "the last change to X" reads one record.
* `hist:subject`, `hist:predicate` and `hist:object` give the triple without SPARQL 1.2
  syntax.

The call reads the change log only. It does not take bindings from the rest of its
group, so a join with the current state happens after it (see the examples in
[API](../API.md#history-queries)). Its results are never cached, because the log grows
and retention trims it.

The common questions are then ordinary SPARQL:

* when a triple was added or removed: the triple with constants, and `hist:op` and
  `hist:commit`;
* which commit last changed a subject: `GROUP BY ?s` with `MAX(?commit)`, or
  `hist:order hist:descending ; hist:limit 1` for one subject;
* the values of a predicate over time: the predicate as a constant, the object as a
  variable, ordered by commit.

**HTTP.** `GET /{ds}/history` takes `subject`, `predicate`, `object` and `graph`
(repeatable), `from` and `to` (the selectors of `at`), `op`, `order` and `limit`
(default 1,000, at most `--max-rows`). It answers JSON with the changes and the commits
whose changes are not recorded. It needs the `diff` grant, as the change feed does.
`GET /$/history/{ds}` reports the log's state, and `PUT /$/history/{ds}` takes a
`changeLog` object of settings.

**CLI.** `sparkles history --loc DB` takes the same filters as flags and prints one line
per change, or JSON lines.

**Library.** `Store::history_changes(&HistoryQuery)` returns a `HistoryResult` with the
changes, `unrecorded` ranges and a `truncated` flag.

**Unrecorded commits.** A result names the commits in its range whose changes the log
does not hold, with a reason: older than the log (`before-log`), a large bulk commit
(`bulk`), or a gap (`gap`). SPARQL has no channel for that note, so a SPARQL history
query skips such commits. The HTTP endpoint, the CLI and the library report them.

**Diffs.** `Store::diff` reads the change log when the write-ahead logs cannot give the
answer, either because an end is no longer readable or because a bulk commit or a
collected generation lies between the two commits, and the log records every commit in
between. Diffs therefore reach back past compactions.

### 11.5 Access control

History rows pass through the caller's view like diff and change feed rows. A change in a
graph the caller may not read is left out, and so is a change that a protection depending
only on the predicate and graph hides. A protection that depends on the data (classes or
patterns) would need the state of every commit to decide, so a caller with such a
protection gets `403` for history queries. The change feed does the same. Callers
without such protections are unaffected.

### 11.6 Full-text search at pins

This stays deferred. The two options of §6 are a Tantivy index per pin, built when the pin
is made, and a RAM index built on demand under a budget. Either costs a full text index
build per pinned state, in time and in disk or memory. The common need behind it, finding
past values of a property, is met by history queries with exact terms. `text:query` at a
past commit keeps answering `501`.

### 11.7 Rejected alternatives

* **Syncing the change log in the commit path**, in its own file. It would add a second
  `fdatasync` to every commit. On the development machine a small append and sync costs
  about 0.3 ms, against a median single-triple commit of about 0.45 ms, so small commits
  would be over half again as slow. The asynchronous writer with recovery from the WAL
  gives the same durability at no cost to the commit.
* **Writing keys into the write-ahead log.** The WAL is per generation and replays ids. A
  global log of keys in it would change the WAL format, its replay and compaction's
  catch-up, for no gain over a separate log.
* **History as a materialized named graph** of reified statements (Quit Store's
  provenance graph, made visible to every query). It would double storage and every
  `GRAPH ?g` query would see it. A `SERVICE` call keeps history explicit.
* **New query syntax**, such as R43ples' revision keyword or SQL's `FOR SYSTEM_TIME` as
  a SPARQL clause. Standard parsers and tools would reject such queries.
* **Answering history by replaying states** across the retained generations. It costs a
  materialization per commit and ends where the generations do.
* **Valid-time periods per quad** (one row per version with a start and an end) as the
  primary output. Periods follow from the change rows by pairing each addition with the
  next removal of the same quad, and their open ends need the state at the range's
  edges. They are left to queries over the change rows.

### 11.8 Acceptance tests

* Every commit's recorded changes equal the difference between the states before and
  after it, as WAL replay materializes them, for random histories with deletes of base
  quads, re-inserts, changes undone within a transaction, blank nodes, graphs,
  compactions and bulk commits. The same holds after a restart, in persistent and
  in-memory stores.
* A lookup by subject, predicate, object or graph returns exactly the changes a full scan
  would, in commit order or newest first, with limits.
* A crash at any point (the queue unwritten, a torn last record, a sealed segment without
  its index, each compaction failpoint) loses no change.
* Retention by count, age and size drops whole segments, and the result reports the
  commits before the log.
* Graph views and protections hide the history of what they hide, and data-dependent
  protections are refused.
* Diffs across collected generations equal the state differences.
* Single-triple commit latency with the log on and off, and the log's disk use per
  commit, are measured and recorded in the Outcome.

### 11.9 Sources of this section

* The Sparkles code and this spec, CI, C12, C12b and C13.
* The W3C SPARQL 1.1 Query Language (`SERVICE`) and the RDF 1.2 Concepts and SPARQL 1.2
  Query drafts (triple terms, reifiers and `rdf:reifies`), cited from general knowledge.
* ISO/IEC 9075:2011 system-versioned tables, Datomic's documentation of `as-of`, `since`
  and history databases, Dolt's documentation of its `dolt_diff_<table>`,
  `dolt_history_<table>` and `dolt_log` system tables, TerminusDB's documentation of
  document history, the R43ples paper (Graube, Hensel and Urbas, 2014) and the Quit Store
  papers (Arndt, Naumann and Marx, 2017–2019), all cited from general knowledge for their
  documented behavior. No code of these systems was read.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30 in two commits: the engine (`d866c0c`) and the
server and CLI (`812dc3e`).

* **Engine.** A new `sparkles::history` module holds the `At` selectors, `history.json`
  with pins and the retention window, the generation table, the needed set, and
  crash-safe collection through `gen-NNNN.deleting`. `Store` gained `resolve`,
  `snapshot_at`, `create_snapshot`, `set_retention`, `history` and `dump_nquads_at`.
* **Server.** `?at=` works on queries, explain and Graph Store `GET`/`HEAD`, and those
  responses carry `Sparkles-At`, `Sparkles-Head`, `Memento-Datetime` and
  `Link rel="original"`. The server adds `/$/snapshots/{ds}` and `/$/history/{ds}`, and
  the commit listing marks commits as `reconstructable`.
* **CLI.** `sparkles snapshot create|list|delete|history|retain`, `query --at` and
  `dump --at`.

The limits have the defaults this spec gives: `--history-cache-mb 1024`,
`--history-max-generations 8` and `--max-snapshots 256`.

`Store::open` and history reads share one WAL replay function (`replay_wal`), so both
number commits the same way. Result-cache keys use the generation and the commit, so
past and live states never share entries.

**Deviations.**
* `text:query` at a past commit answers `501` with the general `history-unsupported`
  code, not a separate `text-not-at-commit` code.
* A materialization larger than `--history-cache-mb` fails with the shared budget error
  of [C01](C01-observability-and-budgets.md) (`507`, `budget: "memory"`), not a
  `history-memory` code.

The open questions kept their defaults: retention is off unless set, and there are no
relative selectors. Backup repositories ([F05](F05-snapshot-repositories.md)) later added
a `backup:<name>` hold on the generation a backup uploads. A restored dataset starts
without history.

**Performance.** Acceptance example A23 was measured at 10.5M triples, with 200,000
single-quad commits in one generation
([Benchmarks: Point-in-time reads](../BENCHMARKS.md#point-in-time-reads-105m-triples)).
The first read of a cold commit takes 0.6–0.65 s, against a target of < 1 s. Repeat
reads reuse the cached state and take 0.7–3.6 ms. Reading the base of a sealed
generation takes 43 ms (target < 50 ms), or 89–93 ms right after a restart, when that
generation's files must be opened first.

**Entity tags.** The Graph Store tags of Phase 2 came with
[CI Phase 3](CI-commit-identity.md#outcome). A read with `at` gets the tag of the commit
it read, `W/"<datasetId>:<seq>:<format>"`, and `If-None-Match` answers `304`. The format
part keeps the tags of different serializations apart. `Cache-Control: immutable` is not
sent. A dataset deleted and re-created under the same name starts its commits again at 0,
so the URL of `at=commit:N` can later name a different state.

### Phase 2

Phase 2 landed on 2026-10-02 in four commits. `8ec0c57` changed the engine, `601c2cf`
added the diff endpoint, `Accept-Datetime` and the history settings to the server and
CLI, `b09f623` added `at` to more endpoints, and `6282681` changed the UI.

**Diffs.** `Store::diff`, `GET /{ds}/diff?from=&to=` and `sparkles diff` return the net
quads added and removed between two readable commits, in either order. The design rests
on one property of the write-ahead log. Every change it records took effect, because an
insert is logged only for an absent quad and a delete only for a present one. The net
difference between two commits is then the symmetric difference of the changes between
them. Each change toggles its quad in a map, and a quad changed back cancels out. A
quad's first change tells its state at `from`, and its last change tells its state at
`to`, so no base index is probed.

A diff plans its way from the older commit to the newer one through the retained
generations. Where a generation's base is at or before the current position, its log
covers the next stretch, and the planner takes the log that reaches furthest. A
compaction does not change the data, so the walk continues in the next generation's log
from its base. A bulk commit has no log records, and a collected generation leaves a gap.
Those stretches compare the two states. The older state's quads are translated into the
newer generation's ids through the vocabulary, sorted, and merged with a sorted GSPO scan
of the newer state. A quad with a term the newer generation lacks is removed outright.
Two states of one generation, which is the in-memory case, differ only in their deltas,
so only the delta keys are compared. Generation ids are local to a generation, so each
stretch's changes are turned into vocabulary keys before they are combined.

The JSON format has counts, `method` (`log`, `compare` or `same`) and, with `quads=true`,
the changes as `{op, subject, predicate, object, graph}`. The diff format
(`text/x-sparkles-diff`) has one N-Quads line per change, marked `+ ` or `- `. Removals
come first, then additions, each ordered by graph, subject, predicate and object. `to`
defaults to the head and `from` to the commit before `to`. `graph=` limits a diff to one
graph. Large bodies stream. A diff between two `commit:` selectors gets a weak entity tag.

**Diff performance.** A release build measured these times on one run, with other builds
running on the machine. The dataset had a bulk-built base of 1,000,000 quads, then 20,000
commits of 5 inserts each (`diff_performance` in `crates/sparkles/tests/diff.rs`).

| Diff | Path | Time |
|---|---|---|
| All 20,000 commits (100,000 changes) | log | 84–152 ms |
| The last 100 commits | log | 2.0–2.3 ms |
| One commit | log | 1.6–1.8 ms |
| 1,003 commits across a compaction | log | 22–41 ms |
| A bulk commit of 50,000 quads, two states of 1.1 M quads | compare | 0.75 s |

A diff of one commit at the end of a long log spends most of its time finding the commit,
because the walk reads the log from the generation's base. The sparse offset index of §5.3
would remove that cost.

**Memento.** A Graph Store `GET` or `HEAD` without `at` is its own TimeGate, with
200-style negotiation. `Accept-Datetime` selects the last readable commit at or before
the end of that second, clamped to the oldest readable commit. The response carries
`Memento-Datetime`, `Content-Location` with the memento's `at=commit:N` URL,
`Link: <…>; rel="original timegate"` and `Vary: accept-datetime`. Graph Store reads
without the header send `Vary: accept-datetime` as well.

**Retention and pins.** `Retention.maxBytes` caps the disk of the generations that only
the window keeps, removing the oldest first. Pins take an `expires` time. Schedules
(`{prefix, every, keepLast}`, stored in `history.json`) pin the head as
`<prefix><UTC time>`, skip a pin when the newest one already holds the head, and rotate
the oldest out. `Store::history_tick` expires pins, runs the schedules and collects, and
a server runs it every minute. `sparkles snapshot schedule` and `snapshot gc` are the
CLI for them.

**In-memory datasets.** Pins and the retention window keep `Arc<Snapshot>`s, which share
their structure with the live state. A commit that no pin or window holds is `410
history-gone`. Diffs between them compare snapshots.

**Other endpoints.** `at` works on `/$/stats`, `/$/schema`, `/{ds}/shacl`,
`/$/datasets/{ds}/clone` (recorded as `forkedFrom.seq`) and `/$/backup` (named after the
commit). Stats report `commit`, `at` and the `history` summary of §2.5. `sparkles clone`
takes `--at`. `sparkles log` marks the readable commits with `*`. `/$/metrics` reports
`sparkles_history_bytes`, `sparkles_history_snapshots`, `sparkles_history_cache_entries`,
the cache hit and miss counters, and `sparkles_history_materialize_seconds_count` and
`_sum`.

**UI.** The dataset page has a Snapshots panel with the readable ranges and retention,
creation (with an expiry) and deletion. Its figures can show a past state. The commit
history marks unreadable commits and snapshot pins, opens the query page at a commit, and
shows the changes of a commit, or between any two, as signed N-Quads lines. The query page
has an At field, offers the dataset's snapshots in it, and labels a result read at a past
state with "at commit 42 · 3 h ago".

**Phase 2 deviations.**

- There is no `--diff-max-quads`. The change set and the sort buffer of a comparison
  count against the existing rows budget (`--max-rows`), and the body against
  `--max-export-mb`.
- The log walk is not limited to one generation. It crosses compactions, and only bulk
  commits and gaps are compared, as §6 asked.
- RDF Patch output is not offered, the default of open question 3.
- History upkeep runs every minute, not every hour, so that schedules shorter than an
  hour keep time.
- An in-memory dataset answers `410` for a commit it does not keep, where Phase 1 answered
  `501`. `maxBytes` does not apply to it, and its history status lists no generations.
- The Graph Store sends no `Cache-Control: immutable`, as in Phase 1.

**Phase 2 tests.** `crates/sparkles/tests/diff.rs` checks diffs against the two
materialized states for random commit sequences with deletes and re-inserts, across
compactions, bulk commits and a collected generation, for one graph and all, in
persistent and in-memory stores. It also covers the rows budget, pin expiry, schedules
and `maxBytes`. `http/diff_tests.rs` covers both diff formats, defaults, errors, entity
tags, the budget, `Accept-Datetime`, the history settings and metrics, in-memory history,
and `at` on stats, schema, SHACL, clone and backup. `ui/src/lib/history.test.ts` covers
selector parsing, the result label and diff formatting.

**Not built.** The replay speedups of §5.3 (reusing a cached delta, replaying backwards
and the sparse offset index), `warm` pins, and clones into an in-memory dataset are not
built. Nor is Phase 3: a generation-independent change log, full-text search at pins,
history queries and pin rebasing.

### Replay speedups, RDF Patch and the change feed

These landed on 2026-10-02, after Phase 2. They build the three speedups of §5.3 and
`warm` pins, answer open question 3 with RDF Patch output, and add a change feed. The
catalog horizon that prunes old commit records is in the
[CI Outcome](CI-commit-identity.md#outcome).

**Sparse offset index.** Each generation keeps, in memory, where every 1,024th commit
ends in its write-ahead log, and also the first commit after each further MiB of log, so
large transactions do not spread the entries out. An entry holds the commit's number,
the byte offset after its last record, and whether legacy records still fold into the
base there. The current generation's index is filled by the replay at open and kept up
by each commit after its WAL write. A sealed generation's index is built the first time
a read or diff needs it, by reading its log's records once without probing the base
index. The index stops at damage, because it only says where to start reading, and the
reader that reaches the damage reports it. The index is not written to disk. Rebuilding
it costs one pass over a log, which is much cheaper than the replay that a read needed
before, so there is no new file for backups, restores, `sparkles check` or the quota to
handle.

**Materialization.** A past state starts from the known state of its generation that is
nearest in the log: the base, a cached past state on either side, or the live state of
the current generation. It replays the log forward from an earlier state, or undoes it
backward from a later one. Every logged change took effect, so applying the inverse
change to the delta, transaction by transaction in reverse, is exact. The backward path
reads the range from its end in 4 MiB chunks and verifies each transaction's checksum
before undoing it. A legacy transaction has no number in the log, so a range that holds
one is replayed forward from the base instead. Cached states remember where their commit
ends, so a later read can start from them. The replay at open no longer clones the delta
after each commit to count it. It notes, per transaction, whether each changed quad was
present when the transaction began.

**Diffs** read the log from the index entry nearest their first commit instead of from
the generation's base.

**Warm pins.** `SnapshotOptions::warm`, `"warm": true` in `POST /$/snapshots/{ds}` and
`sparkles snapshot create --warm` mark a pin warm. Its state is built when the pin is
made and by the history upkeep, so after a restart within a minute. The history cache
evicts warm states last, only when they alone pass `--history-cache-mb`. `history.json`
records the flag.

**RDF Patch.** `sparkles::patch` writes Apache Jena's RDF Patch as text
(`application/rdf-patch`, also offered as `text/rdf-patch`) and as RDF Thrift
(`application/rdf-patch+thrift`), the `RDF_Patch_Row` structs of Jena's
`BinaryRDF.thrift` in the Thrift compact protocol. `GET /{ds}/diff` negotiates them, and
`format=patch|patch-binary` and `sparkles diff --format patch|patch-binary` ask for
them. A patch has `H id` with the `to` commit's IRI and `H prev` with the `from`
commit's, then `TX`, the `D` rows, the `A` rows and `TC`. A commit's IRI is
`urn:uuid:<dataset id>#commit:<seq>`. Blank nodes are written `<_:label>`, because
Jena's text reader drops the first character of a `_:label` but keeps the bracketed
form. Jena 6.2's `rdfpatch` command read the text patches of a test database with blank
nodes, a blank graph name, triple terms, directional and tagged literals and escapes,
and its binary reader read the same changes. The binary reader drops the base direction
of a directional literal, which Sparkles writes in the IDL's `baseDirection` field. A
patch lists every change, so `limit` with a patch format is refused.

**Change feed.** `Store::changes(after, options)` lists the commits after `after`, each
with its net changes relative to its parent, in pages bounded by `max_commits` and
`max_quads`. Within the logs one walk reads every commit of a page, and a bulk commit's
changes come from comparing its two states. A page stays within one readable range, so
the commit after a gap answers `410`. `Store::subscribe_commits` is a watch channel that
each published commit updates. `GET /{ds}/changes?after=SEL&limit=&wait=` serves pages as
JSON or as one patch per commit, needs read permission, and caps a page at
`--max-rows` changes. A commit whose changes alone pass that is listed with its counts
and `complete: false` in JSON, and is `507 changes-too-large` at the start of a patch
page. `wait` long-polls for up to 60 seconds. `Accept: text/event-stream` streams
`commit` events whose ids are commit numbers, so `Last-Event-ID` resumes a stream. A
stream lasts at most five minutes and ends when the server drains.

**Performance.** Two measurements were taken on a machine that other builds were using,
so they vary by tens of percent. `history_performance` in
`crates/sparkles/tests/history.rs` repeats the setup of A23 in the engine with a
1,000,000-quad base and 50,000 single-quad commits in one generation, reading with
`snapshot_at`. A release build gave:

| Case | Before | After |
|---|---:|---:|
| First read in the middle (base + 25,000) | 212 ms | 91 ms |
| A neighbour of a cached state (+1) | 169 ms | 0.37 ms |
| 5,000 commits after a cached state | 231 ms | 18 ms |
| 100 commits before the head | 400 ms | 0.68 ms |
| One-commit diff at the head | 2.9 ms | 0.52 ms |
| Reopen (replays 50,000 commits) | 421 ms | 104 ms |

The benchmark of [Point-in-time reads](../BENCHMARKS.md#point-in-time-reads-105m-triples)
at the same smaller scale (1,000,000 triples, 50,000 single-quad commits over 4 HTTP
connections, timed by single `curl` requests) gave:

| Request | Before | After |
|---|---:|---:|
| `COUNT(*)` at base + 25,000, first | 213 ms | 67 ms |
| The same, second and later | 1.1–1.4 ms | 1.2–1.6 ms |
| `COUNT(*)` at base + 25,001 | 285 ms | 2.1 ms |
| `COUNT(*)` at head − 100 | 486 ms | 2.7 ms |
| `COUNT(*)` at base + 12,500, cold | 108 ms | 31 ms |
| Diff of one commit at the head | 2.3–3.9 ms | 0.6–2.0 ms |
| Diff of the last 100 commits | 3.9 ms | 1.6 ms |
| After a restart, at base + 25,000 | 177 ms | 69 ms |

`diff_performance` (1,000,000 quads, then 20,000 commits of 5 inserts) gave 2.0 ms →
0.08 ms for one commit, 4.6 ms → 0.70 ms for the last 100 commits, 224 ms → 153 ms for
all 20,000 and 47 ms → 24 ms across a compaction. The first diff in a sealed generation
builds its index by reading that log once.

**Tests.** `history.rs` checks 1,300 random commits with deletes of base quads,
re-inserts and changes undone within a transaction: every sampled past state, read in
an order that exercises the base, cached states on both sides, the live state and a
cache of one entry, equals the dump taken when the commit was made, in the current
generation and after it is sealed. It also covers warm pins across a restart. `diff.rs`
follows the change feed in pages of several sizes and budgets over random histories with
compactions and bulk commits, checks each commit against its one-commit diff, and checks
gaps and the commit watch. `http/diff_tests.rs` covers the patch formats, the feed as
JSON and patches, long polling, server-sent events with `Last-Event-ID`, the budget,
`410` and warm pins over HTTP. Unit tests cover the index and the Thrift encoding.

**Still not built.** The feed is per dataset, not per graph, and Phase 3 remains later
work. Applying RDF Patch came later with [F10](F10-replication.md) Phase 1, at
`/{ds}/patch`. Clones into an in-memory dataset came with
[C06](C06-clone-to-sandbox.md).

### Compaction during writes

[C13](C13-automatic-compaction.md) changed how compactions meet. A compaction now builds
from a snapshot while writes go on, and carries the commits made during the build into
the new generation's log. The new generation's base then holds the commit at which the
build started, which can be earlier than the end of the generation it replaces. §4.1's
rule `base(G+1) = end(G)` becomes `base(G+1) ≤ end(G)`, and the commits in between are in
both logs. The newest generation that covers a commit owns it, as before, so reads, pins,
the retention window and diffs need no other change. The catalog keeps the generation a
commit was made in. When a sealed generation has no commit of its own after its base,
its end is read from its log.

### Phase 3

Phase 3 landed on 2026-10-03 as designed in §11. The engine has a `store::changelog`
module (segments, the index, the background writer, recovery and retention) and a
`store::history_query` module (`Store::history_changes`). SPARQL has the
`hist:changes` service in `sparql::history_svc`. The server has `GET /{ds}/history`, the
`changeLog` member of `/$/history/{ds}`, `--no-change-log` and `--change-log-mb`, and the
CLI has `sparkles history`. The UI's history panel can list the changes of a subject or
predicate, newest first, with their authors and messages, and its **Diff** button now
works for commits whose state is gone when the change log has their changes.

**Authors.** `WriteOptions` gained `author`. The server sets it to the authenticated
caller (`user:bob`, `oidc:…`, or a minted token's owner) for SPARQL Update, Graph Store
writes, uploads, RDF Patch and the MCP update tool. Without authentication there is no
author. The author lives only in the change log, so commits recovered from the
write-ahead log after a crash have none.

**Measurements.** A release build ran `commit_latency_with_and_without_the_change_log`
in `crates/sparkles/tests/history_log.rs` on a real disk while other builds were using
the machine. Each run made 2,000 single-triple commits per store, six rounds per
setting, alternating which setting went first, on top of a 10,000-quad base.

| | Change log off | Change log on |
|---|---:|---:|
| Median commit | 0.50 ms | 0.45 ms |
| 90th percentile | 0.93 ms | 0.76 ms |
| 99th percentile | 4.3 ms | 3.8 ms |

The difference is within the noise of the machine: earlier runs had stalls of one to
five seconds in either setting, from other processes' disk traffic. A commit only queues
its change list, so no difference is expected. The background writer appended 2,000
queued commits in 3.9 ms, about 2 µs a commit.

| Per single-triple commit | Bytes |
|---|---:|
| Write-ahead log | 66 |
| Change log | 112 |

The change log is larger than the write-ahead log because it stores the terms' keys,
with each IRI written out once per record, and the commit's metadata. Unlike the
write-ahead log, it is not removed by compaction, so the 1 GiB default holds about nine
million such commits. A commit that changes many quads of a few subjects shares their
keys within its record.

An open that recovered 2,000 unwritten commits from the write-ahead log took 27 ms,
against 18 ms for the next open. Listing the 4 changes of one subject in a log of 6,001
commits took 2.4 ms. Most of that was decoding the 10,000-quad record of the bulk load
that also names the subject. Listing all 16,000 changes took 19 ms.

**Tests.** `crates/sparkles/tests/history_log.rs` checks every commit's recorded changes
against the difference of the states around it, as WAL replay materializes them, for
300 random commits with compactions, bulk loads, deletes of base quads, changes undone
in a transaction and blank nodes, in persistent and in-memory stores and after a
restart. It checks lookups by subject, predicate, object and graph against a brute-force
list of every change, crashes with the queue unwritten, a torn record and a sealed
segment without its index, retention by count, age and size, turning the log off and on,
bulk commits recorded and summarized, authors and messages across a restart, time
bounds, graph views and protections, diffs across collected generations, and the SPARQL
service with its parameters and errors. The compaction crash test now also checks that a
crash at each failpoint loses no recorded change. `http/diff_tests.rs` covers
`GET /{ds}/history`, its errors, SPARQL over the protocol, `at`, and the settings, and
an authentication test checks that the history follows graph grants, records the
caller as the author and needs the `diff` grant.

**Deviations.**

- The open segment's index stays in memory, and sealed indexes are read on demand into a
  cache of eight. Records are read one at a time through the index, so a query that
  matches most of a large log reads it record by record.
- SPARQL history queries skip unrecorded commits without saying so, as §11.4 allows.
- The change feed (`/{ds}/changes`) still reads the write-ahead logs and still ends where
  the readable history does. Diffs read the change log, but the feed does not.

**Not built.** Full-text search at pins (§11.6) and pin rebasing remain deferred, as do
periods per quad, a history query that takes bindings from the rest of its group, and
point-in-time reads rebuilt from the change log.

**Later additions (2026-10-03).** The MCP read tools take `at` and read past states as
`?at=` does, and the MCP tool `list_changes` runs the history query of
`GET /{ds}/history` ([C11](C11-mcp-server.md#outcome)).
