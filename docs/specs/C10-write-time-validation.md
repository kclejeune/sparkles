# C10: Write-time SHACL validation

> **Status:** implemented in part
>
> **Phases:** Phase 1 shipped. It covers the store's write guard, full SHACL validation of
> every write's post-state, `/$/validation/{ds}`, `sparkles validation`, metrics, logs and
> `bench:shacl-write`. Phase 2 shipped in most parts: the predicate relevance filter,
> incremental validation with its fallbacks, the persisted baseline, grandfather mode and
> a status panel on the dataset page. Phase 3 shipped: localizable SHACL-SPARQL,
> `sh:targetWhere`, the refinement of F3, the `unvalidated` catalog flag,
> `serve --validate` and mixed shapes sources. ShEx later became a second guard language
> ([G02](G02-shex.md)), and its guard is incremental too.
>
> **User docs:** [API: Write-time validation](../API.md#write-time-validation) ·
> [API: Metrics](../API.md#metrics) · [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This design depends on the existing SHACL validator
(`crates/sparkles-shacl`) and on durable commit identity ([CI](CI-commit-identity.md)),
which provides `Receipt`, `CommitKind`, `Snapshot::commit` and
`WriteTxn::commit() -> Receipt`.

## 1. Summary, goals, non-goals

SHACL is an on-demand check today. `POST /{ds}/shacl` and `sparkles shacl` validate a
snapshot against a shapes graph sent with the request, and nothing stops a write from
making the data non-conforming. This feature adds a per-dataset **write guard**. Before a
write transaction commits, the guard validates the transaction's **post-state**: the
uncommitted snapshot view, read under the writer lock. The mode decides what happens next.

* In `reject` mode, a write that would leave blocking results is not committed. The client
  gets `422` and a bounded validation report. No WAL bytes are written, no generation is
  switched, and no commit `seq` is used up.
* In `warn` mode, the write commits. A summary of the report goes on the receipt and in a
  response header.
* `off` is the default and behaves as Sparkles does today.

**Goals**

* One hook in the store's commit path covers every write path: SPARQL Update, GSP,
  upload, bulk load, the reasoner, CLI writes, and the Rust `Transaction`/`WriteTxn` API.
* The guard fails closed. A process that cannot validate refuses writes to a database
  that requires validation, unless that process explicitly bypasses validation.
* Each dataset has its own configuration. It is persisted in the database directory and
  can be set over HTTP and from the CLI.
* Phase 1 runs a full validation per write and comes with a measured cost model.
* Phase 2 validates incrementally and checks only the focus nodes the delta can affect.
  It names the cases where that is unsound and runs a full validation for them.

**Non-goals**

* Automatic repair and suggested fixes.
* Validating each operation of a SPARQL Update. A request is validated once, at its end.
* ShEx.
* SHACL Rules and inference driven by shapes.
* Authentication. Sparkles had none when this was written, so a server flag gates the
  HTTP bypass (§2.3.4).
* Constraints that span datasets.

## 2. User-visible behavior

### 2.1 Configuration

Each dataset has one `ValidationConfig` document, stored as `<db>/validation.json`
(format 1):

```json
{
  "format": 1,
  "mode": "reject",
  "shapes": { "graphs": ["urn:x-shapes:main"] },
  "dataGraph": "default",
  "includeInferences": false,
  "threshold": "violation",
  "timeoutSeconds": 10,
  "reportLimit": 100,
  "updated": "2026-09-30T14:03:11.482Z"
}
```

| field | values | default | meaning |
|---|---|---|---|
| `mode` | `reject` \| `warn` \| `off` | `off` | See §4.2. |
| `shapes` | `{ "graphs": [iri, …] }` or `{ "file": "validation-shapes.ttl", "source": "/abs/path.ttl", "sha256": "…" }` | required unless `off` | Either named graphs of this dataset or a file. Named graphs are merged into one shapes graph and read from the post-state view on every write, so changes to them are validated (§4.4). A file's content is copied into `<db>/validation-shapes.ttl` when the config is set. `source` and `sha256` are for information only. Editing the source file later has no effect until the config is set again. |
| `dataGraph` | `"default"` \| `"union"` \| `[iri, …]` | `"default"` | The data graph, chosen as in `/{ds}/shacl`. `default` is the default graph, or every graph when the store uses `--union-default-graph`. `union` is every graph. A list merges the listed graphs into one data graph. In a list, `urn:x-arq:DefaultGraph` names the default graph, and a missing graph counts as empty. The data graph never includes the shapes graphs, and includes `urn:x-sparkles:inferred` only when `includeInferences` is set. |
| `includeInferences` | bool | `false` | Merges `urn:x-sparkles:inferred` into the data graph. See the reasoner row of §4.3. |
| `threshold` | `violation` \| `warning` \| `info` | `violation` | A result is **blocking** iff its severity rank is at or above the threshold. `sh:Violation` ranks 3, `sh:Warning` 2, `sh:Info` 1, and SHACL 1.2's `sh:Debug` and `sh:Trace` 0. Any other severity IRI counts as `sh:Violation`, so the guard fails closed. |
| `timeoutSeconds` | number > 0 | `10` | The validation budget per write (§4.6). |
| `reportLimit` | 1 … 10 000 | `100` | How many results a rejection or receipt carries. The counts are always exact. |

A config with `mode: "off"` behaves as if there were no file. Clearing the config removes
`validation.json` and `validation-shapes.ttl`.

### 2.2 HTTP: configuration

| Method | Path | Behavior |
|---|---|---|
| GET | `/$/validation/{ds}` | Returns `200` `{config, status}`, or `{config: null}` when there is no config. Works on read-only servers. |
| PUT | `/$/validation/{ds}` | Sets the config. The JSON body is as in §2.1, without `format` and `updated`. For file shapes the body is `{"shapes": {"inline": "<turtle>", "format": "text/turtle"}, …}`, and the server writes `validation-shapes.ttl`. Returns `200` `{config, status}`. |
| DELETE | `/$/validation/{ds}` | Turns validation off and returns `204`. |

```ts
type ValidationStatus = {
  mode: "reject" | "warn" | "off";
  shapes: { shapeCount: number; graphs?: string[]; file?: string; sparql: boolean /* has SHACL-SPARQL constraints */ };
  baseline: { commit: number; conforms: boolean | null /* null = unknown */; blocking: number; total: number; millis: number } | null;
  lastFullMillis: number | null;     // most recent full validation
  counters: { passed: number; warned: number; rejected: number; skipped: number; bypassed: number };  // since start
  warnings: string[];                // e.g. "shapes graph <…> has no shapes", "full validation took 1.8 s; every write waits for it"
};
```

A PUT takes the writer lock by beginning a write transaction that changes nothing. It
parses the shapes, runs a **full validation of the head** with the new config, installs
the guard, writes `validation.json` and drops the transaction. All of this happens under
the lock, so no write can commit between the check and the switch.

* If the mode is `reject` and the head has blocking results, the PUT returns `409`
  `{"error":"dataset does not conform; fix the data or use mode 'warn' first","validation":{…§2.3.2…}}`.
  Nothing changes, and the invariant of §4.2 holds.
* The PUT fails with:
  * `400` for an invalid config, a shapes parse error, or a shapes graph IRI that is not
    a valid IRI;
  * `403` on a `--read-only` server;
  * `404` for an unknown dataset;
  * `408` when the enabling validation takes longer than `timeoutSeconds`;
  * `501` when the server was built without the `shacl` feature.
* An empty or missing shapes graph is accepted with a warning. Later writes can add
  shapes, and those writes are validated (§4.4).

### 2.3 HTTP: writes

#### 2.3.1 Header

On a dataset with a config, every response from a write endpoint carries a
`Sparkles-Validation` header. The header is an RFC 9651 Dictionary and is listed in
`Access-Control-Expose-Headers`:

```
Sparkles-Validation: status=warned, mode=warn, strategy=full, blocking=2, total=3, violations=2, warnings=1, infos=0, ms=14
```

`status` takes one of these values:

* `passed`: there are no blocking results.
* `warned`: the mode is `warn`, there are blocking results, and the write committed.
* `rejected`.
* `skipped`: the write touched nothing that is validated (§4.1), or the transaction was a
  no-op.
* `bypassed`.

`strategy` is `full`, `incremental` (Phase 2) or `none`. Datasets without a config get no
header.

#### 2.3.2 Rejection

A rejected write gets **`422 Unprocessable Content`**. This applies to `/{ds}/update`,
GSP `PUT`/`POST`/`DELETE`, `/{ds}/upload`, and the same operations through `/{ds}`. The
response also carries `Sparkles-Commit: <head>`, which is unchanged. The body format
depends on `Accept`.

* The default is `application/json`:
  ```json
  {
    "error": "SHACL validation failed: 2 blocking results (threshold violation); nothing was committed",
    "validation": {
      "status": "rejected", "mode": "reject", "strategy": "full",
      "threshold": "violation", "conforms": false,
      "blocking": 2, "total": 3,
      "bySeverity": { "violation": 2, "warning": 1, "info": 0 },
      "limit": 100, "truncated": false, "millis": 14,
      "head": 41, "kind": "update",
      "results": [ /* ShaclReport results (docs/API.md "SHACL validation"), blocking first, then by severity, source shape, focus node */ ]
    }
  }
  ```
* When `Accept` lists `text/turtle` explicitly, the body is a standard
  `sh:ValidationReport` in Turtle, with the first `limit` results and
  `sh:conforms false`. `*/*` does not count as listing it. The counts are only in the
  header.

`?validationLimit=N` overrides `reportLimit` for one request, up to 10 000.

#### 2.3.3 Warn mode and receipts

In `warn` mode the write commits and the default body is unchanged. A client can ask for
a receipt with `receipt=true` or `Accept: application/x-sparkles+json`
([CI §2.4](CI-commit-identity.md)). The receipt then gains the same `validation` object,
with `results` capped at `limit`:

```json
{ "committed": true, "commit": { "seq": 42, "kind": "update" },
  "validation": { "status": "warned", "blocking": 2, "total": 3, "results": [] } }
```

In `reject` mode, a successful write's receipt carries `validation` with status `passed`
and any non-blocking results.

#### 2.3.4 Bypass

A write request can ask to skip validation with `?validate=false` or the header
`Sparkles-Validate: off`. A server flag decides whether the request is honored.

* Without the flag, the server refuses the bypass with `403` `"validation bypass is
  disabled on this server"`.
* With `sparkles serve --allow-unvalidated-writes`, the write skips the guard. The server
  logs it at WARN, counts it as `bypassed` and answers with `Sparkles-Validation:
  status=bypassed`. The write marks the baseline unknown (§4.2).

The flag exists because Sparkles has no authentication. Without it, any client could
bypass validation with a request parameter.

### 2.4 CLI

```
sparkles validation --loc DB --status [--format text|json]
sparkles validation --loc DB --mode reject|warn \
    (--shapes-graph IRI … | --shapes FILE) [--data-graph default|union|IRI …] \
    [--include-inferences] [--threshold violation|warning|info] [--timeout S] [--report-limit N]
sparkles validation --loc DB --off
```

The command takes the database lock, like other CLI writes, and runs the same enable
check as PUT. It exits with:

* 0 when the config is set;
* 1 when `reject` was refused because the head does not conform. The report goes to
  stdout, as with `sparkles shacl`.

The commands that write data (`load`, `update`, `infer`) install the guard from
`validation.json`. On a rejection they print the summary and the first results to
stderr and exit with **3**. Every other error still exits with 1. `--no-validate`
bypasses the guard for that command. It prints a warning and counts as `bypassed`. The
CLI already has direct file access, so it needs no server flag. A binary built without
`shacl` refuses to write to a validated database unless `--no-validate` is given.

`sparkles stats` gains a line such as
`validation      reject · 2 shape graphs · 20 shapes · last full 164 ms`.

### 2.5 Rust library API

```rust
// sparkles::guard (new module; sparkles cannot depend on sparkles-shacl)
pub trait CommitGuard: Send + Sync {
    /// Called under the writer lock with the post-state; Err(Error::Rejected) aborts.
    fn check(&self, c: &Candidate<'_>) -> Result<ValidationSummary>;
    /// A short description for errors and status.
    fn describe(&self) -> String;
}
pub struct Candidate<'a> {
    pub base: &'a Arc<Snapshot>,          // committed head the transaction started from
    pub view: Arc<Snapshot>,              // post-state (uncommitted)
    pub kind: CommitKind,
    pub changes: Changes<'a>,
    pub opts: &'a WriteOptions,
}
pub enum Changes<'a> {
    Log(&'a [(u8, [Id; 4])]),                           // WAL path: effective inserts/deletes
    Rebuilt { log: &'a [(u8, [Id; 4])], bulk: &'a [[Id; 4]] }, // bulk txn path
    Unknown,                                            // rebuild from sources (load_as bulk)
}
#[derive(Clone, Debug, Default)]
pub struct WriteOptions {
    pub bypass_validation: bool,
    pub deadline: Option<Instant>,        // request deadline; min'd with timeoutSeconds
    pub cancel: Option<Arc<AtomicBool>>,
    pub report_limit: Option<usize>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ValidationSummary {
    pub status: GuardStatus,              // Passed | Warned | Rejected | Skipped | Bypassed
    pub mode: GuardMode, pub strategy: Strategy, pub threshold: Severity,
    pub blocking: u64, pub total: u64, pub by_severity: SeverityCounts,
    pub limit: usize, pub truncated: bool, pub millis: u64,
    pub results: Vec<serde_json::Value>,  // first `limit`, ShaclReport JSON form
    #[serde(skip)] pub report_turtle: Option<String>, // first `limit`, for 422 text/turtle
}
pub struct Rejection { pub summary: ValidationSummary, pub head: u64, pub kind: CommitKind }

impl Store {
    pub fn set_guard(&self, g: Option<Arc<dyn CommitGuard>>);   // ArcSwapOption, no writer lock
    pub fn guard_required(&self) -> bool;                         // validation.json mode != off
    pub fn write_with(&self, kind: CommitKind, o: WriteOptions) -> WriteTxn<'_>;
    pub fn load_with(&self, s: &[Source], kind: CommitKind, o: &WriteOptions) -> Result<Receipt>;
    pub fn replace_with(&self, t: ReplaceTarget, s: &[Source], kind: CommitKind, o: &WriteOptions)
        -> Result<(u64, Receipt)>;
}
// Receipt gains `pub validation: Option<Arc<ValidationSummary>>` (serialized when Some).
// Receipt loses `Copy` (stays Clone).
// Error gains `Rejected(Box<Rejection>)` (HTTP 422) and `GuardMissing(String)` (HTTP 501).
// QueryOptions gains `write: WriteOptions` (update_as passes it to write_with).
// StoreOptions gains `unvalidated_writes: bool` (default false): the library-level bypass.

// sparkles_shacl::guard (new)
pub struct ShaclGuard { /* config, cached Shapes, baseline, counters */ }
impl ShaclGuard {
    pub fn load(root: &Path) -> anyhow::Result<Option<Arc<ShaclGuard>>>;  // reads validation.json
    pub fn new(cfg: ValidationConfig, root: Option<&Path>) -> anyhow::Result<Arc<ShaclGuard>>;
    pub fn install(store: &Store) -> anyhow::Result<Option<Arc<ShaclGuard>>>; // load + set_guard
    pub fn status(&self) -> ValidationStatus;
}
pub fn set_config(store: &Store, cfg: Option<ValidationConfig>) -> anyhow::Result<SetOutcome>; // §2.2 check
```

A rejected library write returns `Err(Error::Rejected(r))` from `WriteTxn::commit`,
`Dataset::transaction`, `Dataset::update`, `Store::load` and the other write calls. If a
program opens a validated database and installs no guard, every non-empty commit fails
with `Error::GuardMissing("dataset requires write-time SHACL validation; call
sparkles_shacl::guard::ShaclGuard::install or set StoreOptions::unvalidated_writes")`.

### 2.6 Metrics and logs

| Name | Type | Labels |
|---|---|---|
| `sparkles_validation_total` | counter | `dataset`, `status` (`passed` `warned` `rejected` `skipped` `bypassed` `timeout` `error`) |
| `sparkles_validation_duration_seconds` | histogram (1 ms … 300 s, the request histogram's buckets) | `dataset`, `strategy` (`full` `incremental`) |
| `sparkles_validation_results_total` | counter of the results found on validated writes | `dataset`, `severity` |
| `sparkles_validation_fallbacks_total` (Phase 2) | counter | `dataset`, `reason` (§6.2) |
| `sparkles_validation_focus_nodes` (Phase 2) | histogram | `dataset`, `strategy` |

The logs change as well:

* The access log gains `validation` (the status) and `validation_ms` fields.
* `Outcome` gains `rejected`, so `sparkles_requests_total` does not count a 422 as a plain
  `client_error`.
* A rejection is logged at INFO under `sparkles::validation`. The line has the dataset,
  the kind, the counts, and the shape and focus node of the first result. Bypasses are
  logged at WARN.

## 3. Standards basis

* **SHACL (W3C Rec, 2017).**
  * §3.6.1.1 says `sh:conforms` is true iff validation produced no results, whatever their
    severity. Reports built by the guard keep that meaning. The commit decision uses the
    separate `threshold`.
  * §2.1.4 says severities "have no impact on the validation". That is why the threshold
    belongs to Sparkles and not to the report.
  * §3.3 says `sh:shapesGraph` in the data graph only suggests shapes. The guard uses its
    explicit config and ignores those triples.
  * §3.4.1 says failures are signaled outside the report. Here a failure fails the write
    (§4.6).
  * §3.4.3 leaves validation with recursive shapes implementation-defined. Sparkles keeps
    its current treatment, and Phase 2 always validates recursive shapes in full.
  * §2.1.6 says deactivated shapes are skipped.
  * §§2.3.1.2 and 2.3.1.4 define sequence and inverse paths. They drive the reverse
    evaluation of §6.2.
  * §4.8.1 defines `sh:closed`.
  * Appendix A defines pre-binding, which SHACL-SPARQL constraints use.
* **SHACL 1.2 Core (W3C Working Draft, September 2026).**
  * §6.6 *Conformance Checking* is validation that returns only a boolean, and a write
    guard is such a check. This spec still produces a bounded report, because clients
    need to know *why* a write failed.
  * §3.1.3.6 `sh:targetWhere` makes target membership depend on conformance to another
    shape. Phase 2 always validates such shapes in full, until Sparkles implements the
    feature.
  * The new severities `sh:Debug` and `sh:Trace` rank below `sh:Info` (§2.1).
* **SPARQL 1.1 Update §2.2.** Each request SHOULD be atomic: it has no effect or its
  complete effect, however many operations it holds. The operations must have the effect
  of running in sequence. Validating the state after the *last* operation, and aborting
  the whole request on rejection, satisfies both rules. Intermediate states are not
  constrained.
* **SPARQL 1.1 Protocol §2.2.5.** A failed update should get 400 (syntax) or 500, and
  "may use other 4XX or 5XX HTTP response codes for other failure conditions, as per
  HTTP". The failure body is implementation-defined and may be content-negotiated. That
  allows a 422 with a JSON or Turtle report.
* **RFC 9110.**
  * §15.5.21 `422 Unprocessable Content` means the request is well-formed but its
    instructions cannot be processed.
  * §15.5.10 `409 Conflict` means the request conflicts with the target resource's
    current state, and the user might resolve the conflict and resubmit.
  * §9.2.2 defines idempotency. A rejected PUT stays idempotent.
* **RFC 9651.** The `Sparkles-Validation` header value is a Dictionary.
* **Apache Jena SHACL documentation (Apache-2.0).** Its example
  `Shacl02_validateTransaction` updates a graph "only if, after the changes, the graph is
  validated". The semantics are the same post-state semantics. Jena does it in
  application code, and Sparkles does it in the store.
* **Incremental integrity checking.** Nicolas (1982) introduced *simplification*. If the
  database satisfied the constraints before an update, only the instances of the
  constraints that the update can affect need rechecking. Phase 2 applies that method to
  SHACL shape dependencies, and the assumption becomes the "conforming baseline" of
  §4.2. Blakeley, Larson and Tompa (1986) and Gupta and Mumick (1995) cover
  relevant-update detection for views. Corman, Reutter and Savković (2018) study the
  semantics of recursive SHACL, which is why recursive shapes are never validated
  incrementally.

## 4. Semantics

### 4.1 What is validated, when

* **The guard runs once per transaction, at commit.** It runs inside `WriteTxn::commit`
  (and in the bulk rebuild, §5.2) while the writer lock is held. It runs after the
  transaction's last change and before any WAL byte is written or `commit.json` or
  `CURRENT` is switched.
  * A SPARQL Update with several operations is validated once, on its final state.
  * A GSP `PUT` is validated on the replaced state, never on the cleared state in between.
* **The post-state** is `Arc::new(txn.view())`: the base generation plus the
  transaction's delta. It includes the terms the transaction interned (`dvocab_len`
  covers them). Its result cache has size zero, so SHACL-SPARQL queries neither read nor
  fill the shared cache. Staged bulk quads are invisible to `view()`, so on the bulk path
  the post-state is a snapshot over the newly built generation before it is switched in.
* **Some transactions are skipped.** They get status `skipped` and no validation runs.
  * A no-op transaction, with no net change and no bulk batch, creates no commit.
  * A transaction that touches neither a validated data graph nor a shapes graph leaves
    both unchanged, so the report is unchanged too. Rejecting such a write for results
    that were already there would block unrelated work, such as reasoner output when
    `includeInferences` is false.
  * `Changes::Unknown` is never skipped.
* Readers are unaffected and keep reading the head. Other writers wait on the writer lock
  while the validation runs (§6.1).

### 4.2 Decision rule and the baseline invariant

Let `R` be the results of validating the post-state, and `blocking(R)` the results at or
above `threshold`.

| mode | `blocking(R)` empty | `blocking(R)` non-empty |
|---|---|---|
| `reject` | commit, `passed` | abort, `Error::Rejected`, `rejected` |
| `warn` | commit, `passed` | commit, `warned` |

**Strict post-state semantics.** Phase 1 decides on the post-state alone. It does not
compare it with the pre-state. That is sound and simple *because of the enable
precondition*. `reject` can only be enabled on a head without blocking results (§2.2),
and every later commit is validated. In `reject` mode, every committed head therefore
has no blocking results. This is **the baseline invariant**. Under it, "no blocking
results after the write" means the same as "the write introduced none".

Two things leave the baseline unknown:

* a bypassed write (§2.3.4, `--no-validate`, `unvalidated_writes`);
* a clone (§5.4).

After either, `baseline.conforms` is `null`. The next validated write is judged strictly,
with a full validation. If the head has blocking results, every write that touches
validated graphs is rejected until the data is repaired. The repair can be one
transaction that fixes everything, a bypassed write, or a switch to `warn`. For existing
data that does not conform, the workflow is to run `warn` until the data is clean, then
switch to `reject`. Phase 2 adds a `baseline: "grandfather"` option that blocks only
*new* results (§6.2.5).

The guard keeps `baseline = {commit, conforms, blocking, total}` in memory and updates it
after every validated commit. A config change, or a commit the guard did not see (a gap
in seq), resets it to unknown. Phase 2 persists it (§5.4).

### 4.3 Coverage of write paths

| write | path through the store | validated | notes |
|---|---|---|---|
| SPARQL Update (HTTP, `sparkles update`, `Dataset::update`), including `LOAD` | `update_as` → one `WriteTxn` → `commit` | Yes, once per request. | `QueryOptions.write` carries the deadline, the cancel flag and the bypass. |
| GSP `PUT` | `replace_as` → `WriteTxn` (+ `insert_bulk`) | Yes. | Large bodies take the bulk path (§5.2). |
| GSP `POST`, `/upload`, `sparkles load`, `Dataset::load_*` | `load_as`: a `WriteTxn`, or `rebuild_locked` from sources | Yes. | A load into an empty store, or one above `bulk_threshold`, is validated on the built generation before `CURRENT` switches (`Changes::Unknown`). |
| GSP `DELETE` | `update_as("CLEAR …", GspDelete)` | Yes. | Deletions can cause violations, for example of `sh:minCount` or of `sh:class` on a node that refers to the deleted one. |
| Reasoner `materialize` / `clear` (`/$/reason`, `sparkles infer`, auto-reason) | `write_as(Reason/ReasonClear)` + `insert_bulk` | Yes, but skipped unless `includeInferences` is set. | A rejected run fails its task, with the summary as the task error. The inferred graph is unchanged. |
| `Dataset::transaction`, `insert`, `remove`, `extend`, `WriteTxn` | `WriteTxn::commit` | Yes. | The caller gets `Error::Rejected`. |
| Compaction | `rebuild_locked(bulk = None)` | No. | The data does not change and there is no commit. |
| Clone (`/$/datasets/{ds}/clone`, `sparkles clone`) | `clone_to` builds a new database from a committed snapshot | No. The source is untouched. | The clone gets a copy of the config, and its baseline is unknown (§5.4). |
| Prefix changes | `add_prefixes` | No. | Prefixes are not data. |
| Bypass: `?validate=false` with `--allow-unvalidated-writes`, CLI `--no-validate`, `StoreOptions::unvalidated_writes` | `WriteOptions.bypass_validation` | No. | These are the only bypasses. Each one is counted and logged, and makes the baseline unknown. |

### 4.4 Changes to the shapes graph

With `shapes.graphs`, a transaction can change the shapes themselves. When any change
falls in a shapes graph, the guard does the following:

1. It re-reads the shapes graphs from the **post-state view** with
   `Shapes::from_store_graphs`. Blank nodes stay store blank nodes.
2. If the shapes fail to parse, it rejects the write with `422` and
   `"validation":{"status":"rejected", "shapesError":"…"}`, in both modes. A write must
   not leave the dataset with shapes that cannot be read.
3. It validates the whole data graph against the **new** shapes. This is always a full
   validation. In `reject` mode, adding a constraint that the data violates is rejected.
   Removing constraints always passes.
4. After a commit, the newly parsed shapes replace the guard's cache. After a rejection,
   they are dropped.

Emptying the shapes graphs (`DROP GRAPH`) is an ordinary write. It passes, because nothing
can violate an empty shapes graph, and the status then shows a warning. Open question 1
asks whether the shapes graph should be protected. A config change, such as a new shapes
file, new graphs or a new data graph, goes through PUT (§2.2) and never through a data
write.

### 4.5 Errors

| condition | HTTP | CLI exit | library |
|---|---|---|---|
| Blocking results in `reject` mode, or a write that makes the shapes graph unparseable | 422 | 3 | `Error::Rejected` |
| Enabling `reject` on a head that does not conform | 409 | 1 | `SetOutcome::NotConforming(summary)` |
| Validation exceeded its budget (§4.6) | 408, `validation.status: "timeout"` | 1 | `Error::Timeout` |
| The request was cancelled during validation because the client went away | 503 (the existing `Cancelled`) | – | `Error::Cancelled` |
| Engine failure (SHACL §3.4.1), such as a failing SPARQL constraint or recursion that is too deep | 500, `validation.status: "error"` | 1 | `Error::Invalid` |
| A bypass requested without `--allow-unvalidated-writes` | 403 | – | – |
| A validated database with no guard installed and no bypass | 501 | 1 | `Error::GuardMissing` |

In every row, nothing is committed. No WAL record is written, no generation is switched
(a candidate bulk generation's directory is removed), and the head `seq` stays the same.
The next successful commit gets `head + 1`. Terms interned by the aborted transaction
stay in the append-only delta vocabulary, as they do for aborted transactions today.

### 4.6 Budgets and timeouts

* The validation deadline is the earlier of `now + timeoutSeconds` and the request's
  deadline (`update_timeout`). The validation engine checks it between shapes and every
  256 focus nodes (`Engine::check_limits`). A validation that runs past the deadline
  **fails the write** in both modes. `warn` never blocks a write because of its
  *results*, but committing without a finished validation would be a bypass. Open
  question 4 asks whether `warn` should commit instead.
* The validation runs on the rayon pool with `parallel: true`. SHACL-SPARQL constraints
  use the request's memory budget (`QueryOptions.max_memory_bytes`).
* When a bulk candidate is rejected or times out, the full generation build has already
  been paid for. Phase 1 accepts that cost (§6.1).

## 5. Design sketch

### 5.1 `sparkles` (store)

* A new `guard.rs` holds the types of §2.5.
* `Store` gains two fields:
  * `guard: ArcSwapOption<dyn CommitGuard>`;
  * `guard_required: AtomicBool`. `open` sets it when `validation.json` exists with
    `mode != "off"`. `open` parses only the `mode` member, so the store does not depend
    on SHACL.
* `WriteTxn` gains `opts: WriteOptions` and `validation: Option<Arc<ValidationSummary>>`.
* `Error` gains `Rejected` and `GuardMissing`.

### 5.2 Commit-path integration

`WriteTxn::commit`:

```text
if poisoned → Err(Poisoned)
if bulk.is_empty() && net_ins == 0 && net_del == 0 → no-op receipt (validation: skipped, not run)
if bulk.is_empty():
    summary = self.run_guard(|| Arc::new(self.view()), Changes::Log(&self.log))?   // may Err(Rejected)
    publish_log(summary)                        // seq = head+1 assigned here, after the guard
else:
    rebuild_locked(..., Some(BulkCommit{..}), GuardCall{ changes: Rebuilt{log, bulk}, opts })
```

`run_guard` works as follows:

* With a bypass, it returns `Bypassed`.
* With no guard, `guard_required` set and `store.opts.unvalidated_writes` unset, it
  returns `Err(GuardMissing)`.
* With no guard otherwise, it returns `None`.
* Otherwise it calls `guard.check(&Candidate{…})`.

When `rebuild_locked` runs with `bulk: Some(_)`, it calls the guard **after**
`Generation::open(&dir, …)` and **before** `write_synced(dir/commit.json)`. The candidate
is `Snapshot { generation: Arc::new(gen_), delta: empty, commit: w.head.seq, results:
zero-size cache, … }`. On `Err`, it drops the generation, calls `remove_dir_all(dir)`
and returns the error. For in-memory stores the tempdir is dropped instead.
`w.next_bnode` may have advanced, but gaps in blank-node ids do no harm. `load_as` from
sources passes `Changes::Unknown`. `add_prefixes` runs only after a successful switch, as
it does today.

`publish_log` and `rebuild_locked` store the summary on the receipt (`Receipt.validation`).

### 5.3 `sparkles-shacl`

* `ValidateOptions` gains `graphs: Option<GraphScope>`. `GraphScope::Only(Vec<String>)`
  treats missing graphs as empty, and `GraphScope::AllExcept(Vec<String>)` excludes the
  listed graphs. `data::GraphSel` gains the matching `AllExcept(Vec<u64>)`. With it, a
  write to a `union` data graph does not have to list every graph id, and the shapes
  graphs stay out.
* `Shapes::from_store_graphs(snap, &[iri])` merges the graphs. It sets `source_graph` to
  the first graph, which is what `$shapesGraph` binds to (Open question 6).
* `report::to_json` and `path_json` move from `sparkles-server/src/shacl.rs`, so the guard
  can build the bounded JSON results.
* A new `guard.rs` holds `ShaclGuard::check`, which runs these steps:
  1. **Relevance.** It maps `changes` to the set of graph ids they touch. If that set hits
     neither a data graph nor a shapes graph, it returns `Skipped`. `Unknown` always
     proceeds.
  2. **Shapes.** It uses the cached `Arc<Shapes>`, or re-parses the shapes from the view
     when a shapes graph was touched (§4.4).
  3. **Validate.** It calls `validate(&view, &shapes, &opts)` with `parallel: true` and
     the deadline and cancel flag from `Candidate.opts`.
  4. It counts the results by severity and sorts them: blocking first, then by severity,
     source shape and focus node. It keeps the first `limit` results as JSON and, in
     `reject` mode, as Turtle.
  5. It decides per §4.2 and updates the counters and the baseline. The baseline update
     stays pending until the commit, as described next.

  The baseline changes only after the store confirms the commit. The guard records a
  *pending* baseline keyed by `(base.commit + 1)`. A later `check` whose `base.commit`
  equals that seq promotes it. Any other seq resets the baseline to unknown.
* `set_config` takes the writer lock with `store.write_as(Transaction)` and changes
  nothing through it. It validates `txn.base()` and writes `validation.json` (and
  `validation-shapes.ttl`) through a temp file, an fsync and a rename, as `write_atomic`
  does. It then calls `set_guard` and drops the transaction.

### 5.4 Persistence, open, clone

* `<db>/validation.json` and `<db>/validation-shapes.ttl` sit next to `text.json` and
  `reasoning.json`.
  * The server (`AppState::attach`/`create`) and the CLI write commands call
    `ShaclGuard::install` right after `Store::open`.
  * If `validation.json` is malformed, `install` fails. The dataset then opens with
    `guard_required` set, so writes fail with `GuardMissing` while reads work. The server
    logs an error.
  * In-memory datasets keep their config in memory only.
* **Clone** copies both files, as it copies `text.json`. The clone's baseline is unknown,
  so its first validated write runs a full validation. If the source used
  `includeInferences` and the clone dropped its inferences (`inferences=drop`), the
  status shows a warning.
* **Phase 2** persists the baseline in `validation-status.json` as
  `{commit, configHash, conforms, blocking, total}`. The file is written after the commit
  is durable, and it is trusted only when `commit == head.seq` and `configHash` matches.
  A crash between the commit and the status write therefore gives "unknown", never a
  false "conforming".

### 5.5 Server plumbing

* In `http.rs`:
  * `update_endpoint`, `gsp` and `upload` build `WriteOptions` from `update_timeout`, the
    request's cancel flag, `validate=false` (checked against
    `st.allow_unvalidated_writes`) and `validationLimit`.
  * `From<Error> for ApiError` maps `Rejected` to 422 with the §2.3.2 body, choosing the
    Turtle form by `Accept`. It maps `GuardMissing` to 501.
  * `with_validation(resp, summary)` adds the header, and `write_response` puts
    `validation` into receipts.
  * The CORS expose list gains `Sparkles-Validation`.
* New routes serve `/$/validation/{ds}` (GET/PUT/DELETE).
* `DatasetInfo` gains `validation: {mode, baselineConforms} | null`.
* `obs.rs` gains the §2.6 metrics and `Outcome::Rejected`.
* `main.rs` gains the `Validation` subcommand, `--no-validate` on `load`, `update` and
  `infer`, and `serve --allow-unvalidated-writes`.
* In reasoning tasks, a `Rejected` error becomes the task error text
  `"inferences rejected by SHACL validation: 2 blocking results (first: <shape> at <node>)"`.

## 6. Performance

### 6.1 Phase 1: full validation per write

Validation over a snapshot uses the indexes. Focus nodes come from `PSO`/`POS` scans, and
paths are evaluated with `SPO`/`POS` prefix scans. The work runs in parallel over chunks
of 256 focus nodes. The current measurement (`docs/BENCHMARKS.md`,
`mise run bench:shacl 100000`, 20 shapes, a compacted base and an empty delta) is
**1.05M triples, 48,428 results: 164 ms parallel, 741 ms sequential.**

| validated data | full validation (parallel) | writes/s ceiling with `reject` (the lock is held for validation + commit) |
|---|---:|---:|
| 100k triples | ≈ 16 ms (extrapolated) | ≈ 45 |
| 1.05M triples | 164 ms (measured) | ≈ 6 |
| 10M triples | ≈ 1.6 s (extrapolated, unmeasured) | < 1 |

For comparison, a 1-triple `INSERT DATA` commit costs 5–7 ms today. The cost of
validation grows linearly with the number of focus nodes times the constraints
evaluated, plus the cost of building results. A conforming dataset builds no results, so
it should be cheaper than the benchmark. A large uncompacted delta makes scans merge
rows, so it costs more. Neither effect has been measured.

Phase 1 suits datasets of up to about 1M validated triples with modest write rates. It
also suits datasets of any size where writes are rare bulk loads. Three rules follow:

* **Measure the cost.** A new `mise run bench:shacl-write N` measures the latency of a
  1-triple `INSERT DATA` with `off`, `warn` and `reject`, at N = 100k and 1M, with 0 and
  with 10k pending delta quads. The results go in `docs/BENCHMARKS.md`. The acceptance
  targets are:
  * `reject` latency ≤ the standalone validation time + 10 ms;
  * `off` latency unchanged within noise.
* Every full validation records `ValidationStatus.lastFullMillis`. PUT adds a warning
  when it is over 1000 ms.
* The relevance skip (§4.1) makes writes to graphs outside the validated set free. That
  includes reasoner output.

### 6.2 Phase 2: incremental validation

#### 6.2.1 Predicate relevance filter (cheap, first)

For each active shape, the guard computes `reads(S)` statically. It is the set of
predicates the shape's evaluation can read:

* path predicates;
* target predicates;
* the predicates of `sh:equals`, `sh:disjoint`, `sh:lessThan` and `sh:lessThanOrEquals`;
* `rdf:type` and `rdfs:subClassOf`, for class targets and `sh:class`;
* `*` (every predicate) for `sh:closed` and for SHACL-SPARQL constraints and components;
* the predicates of every shape it references, transitively.

If no changed predicate is in the union of `reads` over all shapes, the report is
unchanged. As long as the baseline is known, the write is then `skipped`.

#### 6.2.2 Affected focus nodes

The inputs are the changed quads Δ, restricted to data graphs. Δ⁺ and Δ⁻ come from
`Changes::Log`/`Rebuilt`. G is the pre-state (`base`) and G′ the post-state (`view`).

A **dependency** is a tuple `(π, p, dir)`. It says that evaluating the shape at focus
node f reads edges with predicate p (or `*`) at the nodes that path π reaches from f,
with those nodes as subjects (`out`) or as objects (`in`). `deps(S)` is computed
statically from the shape:

| construct | dependencies |
|---|---|
| property shape with path P | One per predicate step `p` in P: `(prefix(P, step), p, out)`, or `in` under `sh:inversePath`. For `*`, `+` and `?`, the prefix includes the closure itself, so `p*` gives `(p*, p, out)`. |
| value-level constraints (`datatype`, `nodeKind`, `in`, `pattern`, lengths, ranges, `languageIn`, `hasValue`, counts, `uniqueLang`) | None beyond the path's. These constraints depend only on the value set. |
| `sh:class C` | `(P, rdf:type, out)`, plus a global dependency on `rdfs:subClassOf`. |
| `equals`/`disjoint`/`lessThan[OrEquals]` q | `(ε, q, out)` |
| `sh:closed` | `(ε, *, out)` at the value nodes. That is `(P, *, out)` on a property shape and `(ε, *, out)` on a node shape. |
| `sh:node`/`sh:property`/`and`/`or`/`not`/`xone`/`qualifiedValueShape` → S₂ | `{ (P·π₂, p₂, d₂) : (π₂, p₂, d₂) ∈ deps(S₂) }`, where P = ε on node shapes. |
| SHACL-SPARQL constraint or component, `sh:targetWhere` | ⊤. The whole shape is recomputed. |

The target delta is:

* for changed `rdf:type` triples, their subjects (for class targets; `is_target` filters
  them later);
* for `targetSubjectsOf p`, the subjects of changed p-triples;
* for `targetObjectsOf p`, the objects of changed p-triples;
* for `targetNode`, nothing. Those nodes are reached through deps.

`A(S)` is the target delta united with `reverse(π, x)` for each change `(s, p, o)` and
each dependency `(π, p′, d)` with `p′ ∈ {p, *}`. Here `x = s` for `out` and `x = o` for
`in`, and `reverse(π, x)` is the set of f with `x ∈ π(f)`. It is computed with the
reversed path, where inverse steps use `SPO` and forward steps use `POS`. It is evaluated
in **both G and G′**, and the two results are united. The engine then validates S on
`{f ∈ A(S) : is_target(S, f) in G′}`. This uses a new
`validate_nodes(snap, shapes, &[(ShapeId, Vec<Id>)], opts)`, built on
`Engine::run(Some(_))` generalized to lists of nodes.

**Soundness sketch.** Take a focus node f ∉ A(S), and suppose some edge read while
evaluating S at f differs between G and G′. Take the first such edge along the
evaluation's read path, reached from f via π. Every earlier edge on that path is
unchanged, so π reaches the edge's endpoint x in both states. Then f ∈ reverse(π, x) ⊆
A(S), which is a contradiction. So S reads the same edges at f in G and G′, and gives the
same result. With the baseline invariant (no blocking results at G), a blocking result in
G′ can only occur at a node in A. Induction on the path length gives the general case.
Composition through referenced shapes follows because `deps` composes paths.

#### 6.2.3 Where incremental is unsound: fall back to full

| # | case | fallback |
|---|---|---|
| F1 | The baseline is unknown or does not conform (strict mode). | Full. |
| F2 | The shapes graph or the config changed. | Full. |
| F3 | `rdfs:subClassOf` changed in a validated graph while some shape uses class targets or `sh:class`. | Full. A later refinement adds to A the instances of the changed subject's subclass tree, in G and G′. |
| F4 | The shape has a SHACL-SPARQL constraint or component, or `sh:targetWhere`. | Full *for that shape only*, over all its focus nodes. Other shapes stay incremental. |
| F5 | The shape is in a reference cycle (recursive, SHACL §3.4.3). | Full for the shapes in the cycle. |
| F6 | `Changes::Unknown` (a bulk load from sources). | Full. |
| F7 | Over budget: \|A\| > 50k focus nodes, \|A\| > 25 % of the shape's last full focus count, or a reverse traversal through `*`/`+` visits > 100k nodes. | Full. |
| F8 | `includeInferences` is set and the change is a reasoner commit. | No fallback. Incremental is sound, because the inferred graph is an ordinary graph. |

Every fallback increments `sparkles_validation_fallbacks_total{reason}`.

#### 6.2.4 Expected cost

The cost is O(|Δ| × reverse fan-in × constraints). Each reverse step is an index prefix
scan, so a 1-triple write should take well under 1 ms at 10M triples, against ≈ 1.6 s for
a full validation. The benchmark of §6.1 gains an `incremental` row. A test compares
incremental and full reports on random writes (A15).

#### 6.2.5 Grandfather mode

`baseline: "grandfather"` (Phase 2) allows `reject` on a head that does not conform. The
guard validates A in G *and* in G′. It blocks only the post-state results whose key
`(focus, path, value, sourceShape, component, sourceConstraint)` is not among the
pre-state results. A fallback to full validation does the same over the whole graph, at
the cost of two validations.

### 6.3 Uniqueness constraints (later item)

With a single writer, validating the exact post-state has no races. No concurrent
transaction can insert a duplicate between the check and the commit, so uniqueness needs
no extra locking. Two forms fit:

* **Core SHACL (recommended).** This shape says "a value of `ex:email` identifies at most
  one node":
  ```turtle
  ex:EmailUnique a sh:NodeShape ; sh:targetObjectsOf ex:email ;
      sh:property [ sh:path [ sh:inversePath ex:email ] ; sh:maxCount 1 ] .
  ```
  It works in Phase 1, where it costs a full validation. In Phase 2 it is incremental. An
  inserted `(s, ex:email, o)` gives `A = {o}` through the target delta, and the check is
  one `POS` prefix scan. That is O(log n) per changed triple, with no fallback.
* **SHACL-SPARQL**, for example `FILTER EXISTS { ?other ex:key ?k . FILTER(?other != $this) }`.
  In Phase 2 this falls under F4, so the shape is validated in full. Phase 3 adds a
  **localizable-query analysis**. It applies when every triple pattern in the
  constraint's WHERE clause connects to `$this` through variables, the query has no
  subqueries or `SERVICE`, and its property paths have no closures. The query's patterns
  then become `deps` in the same way as shape paths: a pattern `?a p ?b` reached from
  `$this` by a chain π gives `(π, p, out)`. The shape then no longer falls under F4.
  Negation (`FILTER NOT EXISTS`, `MINUS`) is allowed when it is anchored the same way.
* Composite keys over several properties need SHACL-SPARQL, because Core SHACL has no
  tuple uniqueness. The same Phase 3 analysis covers them.

## 7. Phasing

**Phase 1 (MVP, ~2–3 days)**

* `sparkles::guard`: the trait, `Candidate`, `Changes`, `WriteOptions` and
  `ValidationSummary`.
* The store:
  * the hooks in `WriteTxn::commit` and in bulk `rebuild_locked`, which builds a
    candidate generation and removes it on rejection;
  * `Error::Rejected` and `Error::GuardMissing`, and `Receipt.validation`;
  * the fail-closed `guard_required` check.
* `sparkles-shacl`:
  * `GraphScope`, `from_store_graphs`, `report::to_json`;
  * `ShaclGuard`, with full validation, the relevance skip, the shapes cache and its
    re-parse when a shapes graph changes, the threshold and limits, and the in-memory
    baseline;
  * `set_config` with the enable check.
* Server:
  * `/$/validation/{ds}` GET/PUT/DELETE;
  * the 422/409/501 mapping and the JSON and Turtle rejection bodies;
  * the `Sparkles-Validation` header and receipt member;
  * `WriteOptions` plumbing in update, GSP and upload;
  * `validate=false` behind `--allow-unvalidated-writes`;
  * `sparkles_validation_total` and `_duration_seconds`, the access-log fields and
    `Outcome::Rejected`;
  * reasoner task errors.
* CLI: `sparkles validation`, guard installation and `--no-validate` on `load`, `update`
  and `infer`, exit code 3, and the `stats` line.
* Clone copies the config.
* `docs/API.md` gains a "Write-time validation" section.
* Tests A1–A14 and `bench:shacl-write`.

**Phase 2**

* The predicate relevance filter (§6.2.1).
* Incremental focus nodes (§6.2.2), with fallbacks F1–F8 and the fallback and focus-node
  metrics.
* A persisted baseline (`validation-status.json`).
* Grandfather mode.
* UI:
  * a *Validation* panel on the dataset page, with the mode, shapes, a baseline badge,
    counters, the last full validation time and a config form;
  * the query page shows a 422 as a results table with an "N of M shown" footer.
* Tests A15–A16.

**Phase 3**

* The localizable SHACL-SPARQL analysis (§6.3) and the refinement of F3.
* `sh:targetWhere`, once Sparkles supports SHACL 1.2 targets.
* A catalog flag, bit3 `unvalidated`, on bypassed commits, so `sparkles log` shows them.
  This needs CI's reserved WAL commit byte 27.
* `serve --validate NAME=CONFIG.json`, like `--text`.
* Shapes from mixed sources (graphs plus a file).

## 8. Acceptance examples

Setup: `sparkles serve --data DIR`, with a persistent dataset `ds`. Prefix `ex:` is
`http://ex.org/`. These shapes are loaded with `PUT /ds/data?graph=urn:shapes` while
validation is off:

```turtle
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ; sh:datatype xsd:string ] ;
  sh:property [ sh:path ex:age ; sh:maxInclusive 150 ; sh:severity sh:Warning ] .
ex:EmailUnique a sh:NodeShape ; sh:targetObjectsOf ex:email ;
  sh:property [ sh:path [ sh:inversePath ex:email ] ; sh:maxCount 1 ] .
```

**A1. Enable.** `PUT /$/validation/ds` with
`{"mode":"reject","shapes":{"graphs":["urn:shapes"]}}` → `200`. The response has
`status.baseline.conforms: true` and `status.shapes.shapeCount ≥ 2`.
`<db>/validation.json` exists with `"format":1`. `GET /$/validation/ds` returns the same
config.

**A2. Conforming write.** `INSERT DATA { ex:a a ex:Person ; ex:name "A" }` with a
receipt → `200`. The header is `Sparkles-Validation: status=passed, mode=reject,
strategy=full, blocking=0, …`. The receipt has `"validation":{"status":"passed",…}` and
`commit.seq` = head+1. Call that seq n.

**A3. Rejection, nothing consumed.** `INSERT DATA { ex:b a ex:Person }` → `422`
`application/json`, with:

* `validation.blocking: 1`;
* `results[0].sourceConstraintComponent` = `sh:MinCountConstraintComponent`;
* `focusNode` = `ex:b`;
* `Sparkles-Commit: n`.

`ASK { ex:b ?p ?o }` → false. `GET /$/commits/ds` shows head n. The next conforming write
gets seq n+1.

**A4. One validation per request.**
`INSERT DATA { ex:c a ex:Person } ; INSERT DATA { ex:c ex:name "C" }` → `200`, because
the state between the two operations is not validated. `DELETE DATA { ex:a ex:name "A" }
; INSERT DATA { ex:x ex:p 1 }` → `422`, and neither operation is applied.

**A5. Turtle report and bounds.** Insert 250 `ex:Person` nodes without names, with
`Accept: text/turtle` → `422 text/turtle`. The body parses as an `sh:ValidationReport`
with `sh:conforms false` and exactly 100 `sh:result`. The header has `blocking=250,
total=250`. The same request as JSON with `validationLimit=300` → `results.length == 250`
and `truncated: false`. With the default limit, the body has `truncated: true` and
`limit: 100`.

**A6. Threshold.** `INSERT DATA { ex:a ex:age 200 }` → `200`, with status `passed`,
`warnings=1` and `blocking=0`. Then `PUT /$/validation/ds` with `"threshold":"warning"` →
`409`, because the head now has a warning-level result. With `"mode":"warn",
"threshold":"warning"` the PUT → `200`.

**A7. Warn mode.** With `mode: warn`, `INSERT DATA { ex:d a ex:Person }` → `200`, and the
write commits. The header is `status=warned, blocking=1`, and the receipt's `results`
has one entry. `GET /$/validation/ds` → `baseline.conforms: false` and
`counters.warned ≥ 1`.

**A8. GSP and deletions.** Switch back to `reject` on a conforming head. The config is
`"dataGraph":"union"`, which excludes the shapes graph. The data is `ex:o1 ex:email
"x@ex.org"` in graph `urn:g1`.

* `POST /ds/data?graph=urn:g2` with `ex:o2 ex:email "x@ex.org"` → `422`
  (`sh:MaxCountConstraintComponent`, focus `"x@ex.org"`).
* `PUT /ds/data?graph=urn:g1` with a body that renames the email → `200`.
* A rejected `PUT` leaves `urn:g1` byte-identical, and GSP `GET` returns the same triples.
* `DELETE /ds/data?graph=urn:g1`, where `urn:g1` holds a person's only `ex:name` →
  `422`, and the graph is still there.

**A9. Bulk path.** A unit test uses `StoreOptions { bulk_threshold: 10 }` and an installed
guard, and loads 100 triples in which one person has no name → `Err(Error::Rejected)`.
`CURRENT`, `head_commit()` and `snapshot().len()` are unchanged, and no new `gen-NNNN`
directory remains. The same load without the violation → a `bulk: true` receipt with
`validation.status == Passed`.

**A10. Shapes change.** In `reject` mode on a conforming head, where one person has no
`ex:email`:

* inserting `ex:PersonShape sh:property [ sh:path ex:email ; sh:minCount 1 ]` into
  `urn:shapes` → `422`;
* inserting a shape the data satisfies → `200`, and the next write is validated against
  it;
* inserting `ex:S sh:property [ sh:path ]`, which has a malformed path → `422` with
  `shapesError`;
* `DROP GRAPH <urn:shapes>` → `200`, and `GET /$/validation/ds` shows the warning
  "shapes graph <urn:shapes> has no shapes".

**A11. Relevance skip.** With `dataGraph: ["urn:g1"]`, `INSERT DATA { GRAPH <urn:other>
{ ex:z a ex:Person } }` → `200`, with `status=skipped` and `strategy=none`.
`sparkles_validation_total{status="skipped"}` goes up.

**A12. Bypass.**

* On a server without the flag, `POST /ds/update?validate=false` with a violating insert
  → `403`.
* After a restart with `--allow-unvalidated-writes`, the same request → `200`, with
  `Sparkles-Validation: status=bypassed`, a WARN log line and `baseline.conforms: null`.
* The next write that conforms on its own but does not fix the bypassed violation →
  `422`, because the post-state still contains the violation.

**A13. Timeout.** A unit test runs a guard over a store with 200k persons and
`timeoutSeconds: 0.001` → `Error::Timeout`, and the head is unchanged. Over HTTP the
result is `408` with `validation.status: "timeout"`, and
`sparkles_validation_total{status="timeout"}` goes up.

**A14. Library, CLI, reasoner and fail-closed.**

* `Dataset::open(db)` with no guard, on a database whose `validation.json` says `reject`
  → `ds.update("INSERT DATA {…}")` returns `Err(Error::GuardMissing(_))`. Reads work.
* With `StoreOptions { unvalidated_writes: true }` → `Ok`.
* After `ShaclGuard::install(ds.store())`, a violating `ds.transaction(|tx| …)` →
  `Err(Error::Rejected(r))` with `r.summary.blocking == 1`.
* `sparkles update --loc db 'INSERT DATA { ex:q a ex:Person }'` → exit 3, with the
  summary on stderr.
* `sparkles update --loc db --no-validate …` → exit 0 and a warning.
* Set `includeInferences: true` and add a shape `sh:targetClass ex:Agent` that requires
  `ex:name`. The rules infer `ex:q a ex:Agent` for a node without a name. Then
  `POST /$/reason/ds` → task state `failed`, with an error that starts with "inferences
  rejected by SHACL validation". The inferred graph is unchanged.

**A15. Incremental equals full (Phase 2).** A property test uses random shapes from the
bench set, random data (10k triples) and 500 random transactions of one or more quads.
For each transaction, the incremental decision and the blocking results restricted to A
equal those of a full validation. Every fallback case F1–F7 is hit at least once and
counted in the metric.

**A16. Uniqueness is local (Phase 2).** At 1M triples with `ex:EmailUnique`, inserting
one `ex:email` triple validates exactly 1 focus node (`sparkles_validation_focus_nodes`)
with strategy `incremental`, in < 5 ms.

## 9. Rejected alternatives

* **Validate after commit and revert with a compensating commit.** That uses two seqs.
  Readers can see the invalid state, and search indexes and watermarks index it.
* **Validate only the request's data**, meaning the inserted triples on their own. That
  misses deletions, cardinality across old and new data, `sh:class` of existing nodes,
  and uniqueness. SHACL defines the conformance of a data graph, not of a diff.
* **Validate each operation of an update with several operations.** That contradicts the
  request atomicity of SPARQL Update §2.2, because fixes made by later operations would
  be rejected.
* **Guard only the HTTP layer.** That misses CLI writes, the reasoner and library writes.
  The hook in the store covers all of them.
* **Validate outside the writer lock**, optimistically: validate, then commit if the head
  has not changed. With a single writer there is no concurrency to gain, and a retry loop
  complicates the bulk paths. Holding the lock also keeps uniqueness free of races.
* **Validate the bulk batch through `view()`.** Staged bulk quads are invisible to
  `view()`, so the guard would validate the wrong state. The candidate generation is the
  post-state.
* **`409 Conflict` for rejected writes.**
  * Sparkles already uses 409 for state conflicts that a retry can resolve, such as "task
    already running", "dataset exists" and the enable precondition of this feature.
  * A SHACL rejection means the request's instructions produce invalid content, which is
    what 422 means (RFC 9110 §15.5.21). The client must change the request, and retrying
    it will not help.
  * Clients need to tell the two cases apart without parsing bodies.
* **`400 Bad Request`.** 400 is reserved for syntax errors (SPARQL Protocol §2.2.5), and
  the UI and other tools read it as "fix your query".
* **Reference the shapes file by path.** The file could change without the data being
  validated again, which breaks the baseline invariant. The guard copies the content
  instead.
* **Reject on any result (`sh:conforms false`).** Warnings and infos would then block
  writes, and SHACL §2.1.4 says severities do not affect validation. A separate threshold
  keeps the report standard.
* **A reserved default name for the shapes graph.** Users choose the graph, and
  `urn:x-sparkles:shapes` gets no special treatment.
* **Honor `sh:shapesGraph` triples in the data (§3.3).** Any writer could then change the
  shapes by writing data. The explicit config is authoritative.
* **Diff semantics (block only new results) as the Phase 1 default.** That needs the
  pre-state report on every write, which doubles the cost. On a conforming baseline, the
  enable precondition plus strict semantics gives the same answer. Diff semantics arrive
  in Phase 2 as grandfather mode.

## 10. Open questions

1. **Should the shapes graph be protected?** Today a data write can edit or drop the
   shapes (§4.4), and dropping them always passes. One option is `protectShapes: true`.
   It would reject any change to a shapes graph unless the write is a bypass or goes
   through a new `PUT /$/validation/{ds}/shapes`. The proposed default is off in Phase 1,
   with a status warning when the shapes graph is empty.
2. **`includeInferences` and staleness ([C08](C08-inference-freshness.md)).** User writes
   are validated against inferences that stay stale until the next reasoning run. Should
   the guard refuse `includeInferences` without `--auto-reason`, or only warn? The
   default is a warning in the status.
3. **Unknown severity IRIs** count as `sh:Violation`, so the guard fails closed. The
   alternative is to ignore them. The SHACL 1.2 severity ordering may settle this once it
   is final.
4. **Timeouts in `warn` mode** fail the write today. Should `warn` commit with
   `status=incomplete` instead (`onTimeout: "commit"`)?
5. **Long bulk validations hold the writer lock**, for several seconds at 10M+ triples.
   Should the candidate generation be built and validated *before* the lock is taken, and
   committed only if the head is unchanged, or rebuilt otherwise? This would be an
   optimistic scheme for bulk writes only.
6. **`$shapesGraph` with several shapes graphs.** It binds to the first graph. Should it
   bind to a union view instead?
7. **Per-graph validation.** `dataGraph: [g1, g2]` merges the graphs into one data graph.
   Some users may want each graph validated on its own, for example with one graph per
   tenant. A later `perGraph: true` could provide that.
8. **Report ordering.** Results are sorted with blocking results first, then by shape and
   focus node. Is a deterministic order worth the cost of sorting 10k results?

## 11. Sources

* **Sparkles repository (read):**
  * `crates/sparkles-shacl/src/{lib.rs,data.rs,validate.rs,shapes.rs,path.rs,report.rs,sparql.rs}`
    for snapshot access, `GraphSel`, parallel chunking, deadlines, targets, constraints
    and the `$shapesGraph` binding;
  * `crates/sparkles-shacl/examples/bench.rs`;
  * `crates/sparkles-core/src/store.rs` for `WriteTxn`, `view`, `commit`, `publish_log`,
    `insert_bulk`, `load_as`, `replace_as`, `rebuild_locked`, `clone_to` and the
    persistence of the text config;
  * `crates/sparkles-core/src/sparql/update.rs`, `crates/sparkles/src/dataset.rs`,
    `crates/sparkles-core/src/error.rs`, `crates/sparkles-core/src/commit.rs` (`Receipt`);
  * `crates/sparkles-reasoner/src/lib.rs` for the `materialize`/`clear` commit kinds and
    the bulk insert;
  * `crates/sparkles-server/src/{http.rs,shacl.rs,state.rs,obs.rs,clone.rs,main.rs}` for
    the update, GSP, upload and shacl handlers, error mapping, receipts, metrics and the
    CLI;
  * `docs/API.md` (SHACL validation, metrics, access log);
  * `docs/BENCHMARKS.md` (164 ms / 741 ms SHACL, 5–7 ms update latency);
  * `README.md`.
* **Existing specs:** [CI](CI-commit-identity.md) for receipts, seq assignment and abort
  semantics; [C08](C08-inference-freshness.md) for style and the reasoner interaction;
  only the config-copy and budget sections of [C06](C06-clone-to-sandbox.md) and
  [C01](C01-observability-and-budgets.md); and [PROVENANCE](PROVENANCE.md). No other
  planning document was read.
* **W3C Shapes Constraint Language (SHACL)**, https://www.w3.org/TR/shacl/ (fetched):
  §§2.1.4, 2.1.6, 2.3.1.2, 2.3.1.4, 3.3, 3.4.1, 3.4.3, 3.6.1.1, 4.8.1, Appendix A.
* **W3C SHACL 1.2 Core, Working Draft**, https://www.w3.org/TR/shacl12-core/ (fetched):
  §3.1.3.6 `sh:targetWhere`, §6.6 conformance checking, the extended severities.
* **W3C SPARQL 1.1 Update**, https://www.w3.org/TR/sparql11-update/ (fetched): §2.2 on
  atomicity and sequential effect, §3 on aborting on failure.
* **W3C SPARQL 1.1 Protocol**, https://www.w3.org/TR/sparql11-protocol/ (fetched): §2.2.5
  on update failure responses.
* **Apache Jena SHACL documentation** (Apache-2.0), https://jena.apache.org/documentation/shacl/
  (fetched): the `Shacl02_validateTransaction` example and SHACL-SPARQL support.
* **Cited from general knowledge, not fetched:** RFC 9110 (§§9.2.2, 15.5.10, 15.5.21),
  RFC 9651 (Structured Field Values), and these papers:
  * J.-M. Nicolas, "Logic for improving integrity checking in relational data bases",
    *Acta Informatica* 18 (1982);
  * J. A. Blakeley, P.-Å. Larson, F. W. Tompa, "Efficiently updating materialized
    views", SIGMOD 1986;
  * A. Gupta, I. S. Mumick, "Maintenance of materialized views: problems, techniques,
    and applications", *IEEE Data Eng. Bull.* 18(2) (1995);
  * J. Corman, J. L. Reutter, O. Savković, "Semantics and validation of recursive SHACL",
    ISWC 2018.
* TopBraid documentation was not consulted.

## Outcome

**Phase 1 shipped on 2026-09-30** in three steps:

1. The store's write guard. It checks every commit's post-state before anything is
   written, on both the WAL path and the bulk path, and fails closed with `GuardMissing`
   and `Error::Rejected`. Receipts carry the summary, so `Receipt` is no longer `Copy`.
2. The SHACL guard in the library.
3. The server and CLI: `/$/validation/{ds}`, `422`/`409`/`501`, the `Sparkles-Validation`
   header, `--allow-unvalidated-writes`, `sparkles validation`, `--no-validate`, exit
   status 3, and clones that keep the configuration.

A follow-up added the `sparkles_validation_*` metrics and the `validation` and
`validation_ms` access-log fields. It also added the reasoner's "inferences rejected by
SHACL validation" task error, the validation status in CLI summaries and
`sparkles stats`, and `mise run bench:shacl-write`, which measures 1-triple
`INSERT DATA` latency with validation off, warn and reject. The W3C SPARQL suites and the
SHACL suite (98/98 Core, 20/20 SPARQL) stayed green.

**Deviations.**
- `sparkles stats` shows no "last full N ms" for SHACL. The baseline lives only in
  memory, because Phase 1 does not persist it.
- The library gained a public `GuardObserver` trait, which the server uses for its metrics
  and logs.
- When ShEx became a second guard language ([G02](G02-shex.md) Phase 2), the configuration
  gained `language`.
  - The maintainer decided that `validation.json` is always written as format 2 with
    `language`, SHACL included. Format 1 files are still read. The file is internal, so
    only older Sparkles binaries are affected.
  - The maintainer also decided that the `sparkles_validation_*` series gain a `language`
    label, which is `shacl` for SHACL.
  - During implementation, the SHACL `Sparkles-Validation` header was left unchanged, and
    ShEx adds `lang=shex`. Summaries and `GET /$/validation/{ds}` gain an additive
    `language` field.
  - The ShEx guard also skips writes whose predicates no shape reads. That relevance skip
    is finer than SHACL's, which works per graph.

**Cost at Phase 1.** Every validated write ran a full validation of the data graph. For
SHACL that took about 160 ms at 1M triples. Skipped writes were free. The standalone
validator's numbers are in
[BENCHMARKS: Other measurements](../BENCHMARKS.md#other-measurements).

**Phase 2 shipped on 2026-10-02.**

- **Exact counts instead of a conforming baseline.** The guard keeps the result counts
  of the head by severity. A full validation sets them, and every validated write moves
  them by the results of the affected focus nodes, validated in the states before and
  after the write. Counts, `blocking` and the decision therefore equal those of a full
  validation whether or not the head conforms. The summary lists the results of the
  focus nodes it validated, gains `focusNodes` and `fallback`, and gains `introduced` in
  grandfather mode. F1 applies in strict `reject` mode only, because `warn` stays exact
  on a head that does not conform.
- **Affected focus nodes (§6.2.2).** Dependencies come from the shapes as designed,
  with inverses pushed down to predicates. Target reads are dependencies at the focus
  node, so the target delta needs no separate rule. A node of `sh:targetNode` that a
  write adds to the store is matched by its term in the state before the write.
- **Fallbacks (§6.2.3).** F1 to F7 are built. F2 also covers bulk loads from sources,
  which may change the shapes graph. F4 and F5 validate only the shapes concerned in
  full, and their counts before the write are remembered per shape. F6 also covers the
  bulk path of a transaction (`Changes::Rebuilt`), because the rebuilt generation
  renumbers terms. The F7 share rule applies only once a shape has more than 512
  affected focus nodes. Each fallback sets `fallback` in the summary and counts in
  `sparkles_validation_fallbacks_total{reason}`.
- **The relevance filter (§6.2.1)** skips a write when no shape reads any predicate it
  changes and the state of the head is known.
- **The persisted baseline (§5.4)** is `validation-status.json`, with the counts by
  severity. It is written after the commit without an fsync, because only a file that
  names the head and the current `validation.json` is trusted. Clones and backups do not
  copy it.
- **Grandfather mode (§6.2.5)** is `"baseline": "grandfather"` in the configuration and
  `--grandfather` in the CLI. A shapes change compares with the results the old shapes
  gave.
- **UI.** The dataset page has a Write-time validation panel with the mode, the result
  counts of the head, the shapes validated in full, the last validated write, the
  counters and the last ten rejected writes. `GET /$/validation/{ds}` gained
  `lastCheck`, `recentRejections` and, for SHACL, `incremental`.
- **Tests.** A15 runs random shapes over every path form and the core components,
  including SHACL-SPARQL and recursive shapes, with random data and writes in `warn`,
  strict `reject` and grandfather `reject`. Each write's decision and counts are checked
  against full validations of the states before and after it, and every new result must
  be listed. At landing, 180 scenarios with 23,550 writes in all passed. They hit every
  fallback reason except `shapes` and `bulk`, which unit tests cover. CI runs a smaller
  set. A16 is a unit test that checks one focus node and the incremental strategy.
- **Cost.** `bench:shacl-write` gained a write that touches a person, which the shapes
  read through a sequence path and `sh:class`. It also gained `TIMEOUT`, because a full
  validation of 10.5M triples can exceed the default budget of 10 s. On a machine busy
  with other builds, `warn` and `reject` writes took 12 to 37 ms at 1.05M triples, about
  as long as with validation off, against 1.3 to 4.4 s before. At 10.5M triples they took
  9 to 20 ms, against 9.8 to 14.8 s before.

**Not built in Phase 2.** The `sparkles_validation_focus_nodes` histogram was not added.
Summaries carry `focusNodes` instead. The panel has no configuration form, and the query
page shows a `422` as an error rather than a results table. The open questions of §10
keep their Phase 1 defaults.

**Phase 3 shipped on 2026-10-02.**

- **Localizable SHACL-SPARQL (§6.3).** A constraint or SPARQL-based component is
  localized when every triple pattern of its query connects to `$this`, or to `$value`
  for an ASK validator, which the shape's path reaches. Each pattern reached at a node
  over a path π reads its predicate there, as a dependency `(π, p, out)` or
  `(π, p, in)`, and extends the path to its other end. Every edge a solution matches is
  reached from the focus node over edges of the same solution, so the soundness sketch of
  §6.2.2 carries over in the state the solution belongs to. The design asked for no
  closures in query paths. The implementation accepts them, because shape paths already
  take closures through the same reverse evaluation. `FILTER NOT EXISTS`, `OPTIONAL`,
  `UNION`, `BIND`, aggregates and `GROUP BY` are localized when their patterns are
  anchored by the enclosing ones. A variable bound in only one branch of a `UNION`, or
  only inside an `OPTIONAL`, anchors nothing after it, because a solution may leave it
  unbound. A variable predicate reads every predicate at its node and reaches no path,
  so patterns may continue from its object only through another anchor. Subqueries,
  `GRAPH`, `SERVICE`, `VALUES`, `MINUS`, triple terms and negated property sets inside a
  longer path keep F4. The SPARQL uniqueness constraint of §6.3 now validates the holders
  of the changed key.
- **`sh:targetWhere`.** The SHACL 1.2 Core Working Draft of 18 September 2026, §3.1.3.6,
  defines the target as the nodes of the data graph that conform to the given shape. Its
  note says an engine may have to test every node. Sparkles parses the target, and both
  the on-demand validator and the guard support it. Candidates are narrowed when the
  where shape has `sh:class`, `sh:hasValue`, `sh:in`, or a predicate or inverse predicate
  property with `sh:minCount` of at least 1, directly or through `sh:node`,
  `sh:property` and `sh:and`. Otherwise every subject and object of the data graph is
  tested. Membership reads what conformance to the where shape reads at the focus node,
  so the target delta of §6.2.2 needs no new rule. Without narrowing, being a node of the
  data graph also reads every edge of the node. A where shape that is recursive or has a
  query that is not anchored makes its shape fall back like any other. The draft's node
  expressions for `sh:targetNode` were left out, because their vocabulary is in a
  separate, less settled draft.
- **F3 refined.** A changed `(s, rdfs:subClassOf, o)` is replaced by changed `rdf:type`
  edges of the instances of `s` and of its subclasses, in both states. Those instances are
  the only nodes whose class membership the change can alter. The fallback remains when
  there are more than `max_visit` (100,000) of them.
- **The `unvalidated` flag.** A commit whose guard check was bypassed carries
  `unvalidated: true` in `/$/commits`, receipts and `sparkles log` (`[unvalidated]`). So
  does a write of a library store opened with `unvalidated_writes` on a database that
  requires validation, which is how `--no-validate` works. Such a write is now reported as
  `bypassed` like the HTTP bypass. The flag is bit 0 of byte 27 of the WAL commit record
  and is covered by its CRC. In the catalog it takes bit 4 of the flags byte, because bit
  3 had gone to the default-graph flag after this spec was written. `commit.json` gains an
  `unvalidated` member. Older records read as validated.
- **`serve --validate NAME[=CONFIG.json]`.** With a file, the dataset's configuration is
  set at startup as by `PUT /$/validation/{ds}`. Shapes or a schema without inline text
  are read from the path in `source`, relative to the file. With `NAME` alone, the
  dataset is validated with the configuration it has. Either way the data is validated
  in full before the server listens, which also makes the state of the head known after a
  restart without `validation-status.json`. A file whose `reject` mode the data does not
  pass stops the start. A dataset validated by `NAME` alone that does not pass is
  reported and keeps its configuration.
- **Mixed shapes sources.** `shapes` may name graphs and a file together, and the CLI
  takes `--shapes` with `--shapes-graph`. The file's triples, with fresh blank nodes,
  are merged with the graphs. A write to a shapes graph merges the file's shapes again.
- **Grandfather mode for ShEx** shipped with this phase ([G02](G02-shex.md#outcome)).
- **Tests.** A15 gained random `sh:targetWhere` targets with and without narrowing, nine
  query forms of SHACL-SPARQL constraints, seven anchored and two not, and an ASK
  component on random paths. It also counts the writes that change `rdfs:subClassOf` and
  stay incremental. At landing, 600 scenarios with 32,880 writes passed in warn, strict
  reject and grandfather reject. The sweep also exposed a flaw of the generator: a shape
  with two qualified value shapes is not well formed, and the two parses of such a shape
  could disagree. The generator now gives a shape at most one. Unit tests cover the
  uniqueness constraint, the specification's example of `sh:targetWhere`, a subclass
  change, mixed sources across a restart, the flag through a catalog rebuilt from the
  WAL, and `serve --validate`. The W3C suites stayed at 98/98 and 20/20.
- **Cost.** `bench:shacl-write` gained `EXTRA=1`, which adds a SHACL-SPARQL constraint on
  every person (each person they know must have a name) and a shape whose focus nodes
  are the people of 60 and over, by `sh:targetWhere`. It also gained `MODES`, whose
  `grandfather` mode is `reject --grandfather` over the shapes the data does not conform
  to. At 1.05M triples, on a machine with a load average near 30 from other builds, every
  timed write was validated incrementally. The person write took 16 to 19 ms in `warn`,
  `reject` and grandfather `reject`, against 18 ms with validation off. The write no
  shape reads took 13 to 19 ms. Validating the SHACL-SPARQL shape alone in full takes
  about 0.8 s, which every write paid before this phase, twice in grandfather mode.

**Shapes syntaxes (2026-10-02, with [G03](G03-shaclc.md)).** Inline shapes may be given
in any RDF syntax or in SHACLC (`"format": "text/shaclc"`). Shapes in another syntax than
Turtle are now stored in `validation-shapes.ttl` as Turtle. Before, inline JSON-LD or
RDF/XML was stored as given under that name and read back as Turtle. An unknown `format`
is now an error rather than Turtle. `sparkles validation --shapes` takes the syntax from
the file name. The list constraints of SHACL 1.2 are validated incrementally: they read
`rdf:first` and `rdf:rest` along each value node's list, and `sh:memberShape` nests the
member shape's reads under `path/rdf:rest*/rdf:first`.
