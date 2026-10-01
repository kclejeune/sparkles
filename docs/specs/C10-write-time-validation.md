# C10: Write-time SHACL validation

> **Status:** implemented in part
>
> **Phases:** Phase 1 (the store's write guard, full SHACL validation of every write's
> post-state, `/$/validation/{ds}`, `sparkles validation`, metrics, logs and
> `bench:shacl-write`) shipped. Phase 2's incremental validation, persisted baseline,
> grandfather mode and UI panel were not built; ShEx later became a second guard language
> ([G02](G02-shex.md)).
>
> **User docs:** [API: Write-time validation](../API.md#write-time-validation) ·
> [API: Metrics](../API.md#metrics) · [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation; the [Outcome](#outcome) section at the end
> records how it landed.

A clean-room design. It depends on durable commit identity ([CI](CI-commit-identity.md):
`Receipt`, `CommitKind`, `Snapshot::commit`, `WriteTxn::commit() -> Receipt`) and on the
existing SHACL validator (`crates/sparkles-shacl`).

## 1. Summary, goals, non-goals

Today SHACL is an on-demand check: `POST /{ds}/shacl` and `sparkles shacl` validate a
snapshot against a shapes graph sent with the request. Nothing prevents a write from making
the data non-conforming. This feature adds a per-dataset **write guard**. Before a write
transaction commits, the guard validates the transaction's **post-state**, which is the
uncommitted snapshot view under the writer lock. The mode decides what happens next:

* `reject`: a write that would leave blocking results is not committed. The client gets
  `422` and a bounded validation report. No WAL bytes are written, no generation is
  switched, and no commit `seq` is consumed.
* `warn`: the write commits. A summary of the report goes on the receipt and in a
  response header.
* `off`: the default, identical to today.

**Goals**

* One hook in the store's commit path, so every write path is covered: SPARQL Update,
  GSP, upload, bulk load, the reasoner, CLI writes, and the Rust `Transaction`/`WriteTxn`
  API.
* Fail closed. A database that requires validation refuses writes from a process that
  cannot validate, unless that process explicitly bypasses validation.
* A per-dataset configuration persisted in the database directory, settable over HTTP
  and the CLI.
* Phase 1 runs a full validation per write, with a measured cost model.
* Phase 2 validates incrementally, checking only the focus nodes the delta can affect. It
  names the cases where that is unsound and falls back to full validation for them.

**Non-goals**

* Automatic repair and suggested fixes.
* Validation of individual SPARQL Update operations. A request is validated once, at
  its end.
* ShEx.
* SHACL Rules and inference driven by shapes.
* Authentication. Sparkles had none when this was written, so a server flag gates the
  HTTP bypass (§2.3.4).
* Constraints across datasets.

## 2. User-visible behavior

### 2.1 Configuration

One document per dataset, `ValidationConfig`, stored as `<db>/validation.json`
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
| `mode` | `reject` \| `warn` \| `off` | `off` | §4.2 |
| `shapes` | `{ "graphs": [iri, …] }` or `{ "file": "validation-shapes.ttl", "source": "/abs/path.ttl", "sha256": "…" }` | required unless `off` | **Named graphs of this dataset**, merged into one shapes graph and read from the post-state view on every write, so changes to them are validated (§4.4). **Or a file**, whose content is *copied* into `<db>/validation-shapes.ttl` when the config is set. `source` and `sha256` are informational. A later edit of the source file has no effect until the config is set again. |
| `dataGraph` | `"default"` \| `"union"` \| `[iri, …]` | `"default"` | The data graph, as in `/{ds}/shacl`. `default` is the default graph, or all graphs when the store uses `--union-default-graph`. `union` is every graph. A list merges the listed graphs into one data graph; `urn:x-arq:DefaultGraph` names the default graph, and absent graphs are empty. The shapes graphs are always excluded from the data graph. So is `urn:x-sparkles:inferred`, unless `includeInferences` is set. |
| `includeInferences` | bool | `false` | Merge `urn:x-sparkles:inferred` into the data graph (§4.3, reasoner row). |
| `threshold` | `violation` \| `warning` \| `info` | `violation` | A result is **blocking** iff its severity rank is at or above the threshold. The ranks are `sh:Violation` 3, `sh:Warning` 2, `sh:Info` 1, and SHACL 1.2 `sh:Debug`/`sh:Trace` 0. Any other severity IRI counts as `sh:Violation` (fail closed). |
| `timeoutSeconds` | number > 0 | `10` | Validation budget per write (§4.6). |
| `reportLimit` | 1 … 10 000 | `100` | Results carried in a rejection or receipt. The counts are always exact. |

A config with `mode: "off"` is equivalent to having no file. Clearing the config removes
`validation.json` and `validation-shapes.ttl`.

### 2.2 HTTP: configuration

| Method | Path | Behavior |
|---|---|---|
| GET | `/$/validation/{ds}` | `200` `{config, status}`, or `{config: null}` when there is no config. Works on read-only servers. |
| PUT | `/$/validation/{ds}` | Sets the config (JSON body as §2.1, without `format` and `updated`). For file shapes, the body is `{"shapes": {"inline": "<turtle>", "format": "text/turtle"}, …}` and the server writes `validation-shapes.ttl`. Returns `200` `{config, status}`. |
| DELETE | `/$/validation/{ds}` | Turns validation off (`204`). |

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

A PUT takes the writer lock: it begins a write transaction that makes no changes. It then
parses the shapes and runs a **full validation of the head** with the new config, installs
the guard, writes `validation.json`, and drops the transaction. Because this happens under
the lock, no write can commit between the check and the switch.

* `reject` with blocking results at the head → `409`
  `{"error":"dataset does not conform; fix the data or use mode 'warn' first","validation":{…§2.3.2…}}`.
  Nothing changes. This keeps the invariant of §4.2.
* Errors:
  * `400` for an invalid config, a shapes parse error, or a shapes graph IRI that is not
    a valid IRI;
  * `403` on a `--read-only` server;
  * `404` for an unknown dataset;
  * `408` when the enabling validation exceeds `timeoutSeconds`;
  * `501` when the server was built without the `shacl` feature.
* A shapes graph that is empty or absent is accepted, with a warning. Shapes can then be
  added by later writes, and those writes are validated (§4.4).

### 2.3 HTTP: writes

#### 2.3.1 Header

Every response of a write endpoint on a dataset with a config gets a
`Sparkles-Validation` header. It is an RFC 9651 Dictionary and is added to
`Access-Control-Expose-Headers`:

```
Sparkles-Validation: status=warned, mode=warn, strategy=full, blocking=2, total=3, violations=2, warnings=1, infos=0, ms=14
```

`status` is one of:

* `passed`: no blocking results;
* `warned`: `warn` mode, blocking results present, committed;
* `rejected`;
* `skipped`: nothing validated was touched (§4.1), or a no-op transaction;
* `bypassed`.

`strategy` is `full`, `incremental` (Phase 2) or `none`. Datasets without a config get
no header.

#### 2.3.2 Rejection

Status **`422 Unprocessable Content`**. It applies to `/{ds}/update`, GSP
`PUT`/`POST`/`DELETE`, `/{ds}/upload`, and the same operations through `/{ds}`. The
response also carries the unchanged `Sparkles-Commit: <head>`. The format depends on
`Accept`:

* By default, `application/json`:
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
* When `Accept` lists `text/turtle` explicitly (`*/*` does not count): a standard
  `sh:ValidationReport` in Turtle with the first `limit` results and `sh:conforms false`.
  The counts are in the header only.

`?validationLimit=N` (up to 10 000) overrides `reportLimit` for one request.

#### 2.3.3 Warn mode and receipts

The write commits and the default body is unchanged. With a receipt (`receipt=true` or
`Accept: application/x-sparkles+json`, [CI §2.4](CI-commit-identity.md)), the receipt gains the same `validation`
object, with `results` bounded by `limit`:

```json
{ "committed": true, "commit": { "seq": 42, "kind": "update" },
  "validation": { "status": "warned", "blocking": 2, "total": 3, "results": [] } }
```

In `reject` mode a successful write's receipt also carries `validation` (status
`passed`, with non-blocking results if any).

#### 2.3.4 Bypass

A write request may carry `?validate=false` (or the header `Sparkles-Validate: off`).
Whether it is honored depends on the server flag:

* Without the flag, the bypass is refused with `403` `"validation bypass is disabled on
  this server"`.
* With `sparkles serve --allow-unvalidated-writes`, the write skips the guard. It is
  logged at WARN, counted as `bypassed`, answered with `Sparkles-Validation:
  status=bypassed`, and it marks the baseline unknown (§4.2).

The flag exists because Sparkles has no authentication. A per-request override would
otherwise let any client bypass validation.

### 2.4 CLI

```
sparkles validation --loc DB --status [--format text|json]
sparkles validation --loc DB --mode reject|warn \
    (--shapes-graph IRI … | --shapes FILE) [--data-graph default|union|IRI …] \
    [--include-inferences] [--threshold violation|warning|info] [--timeout S] [--report-limit N]
sparkles validation --loc DB --off
```

The command takes the database lock, like other CLI writes. It applies the same enable
check as PUT and exits with:

* 0 when the config is set;
* 1 when `reject` was refused because the head does not conform. The report goes to
  stdout, like `sparkles shacl`.

The data-writing commands (`load`, `update`, `infer`) install the guard from
`validation.json`. A rejection prints the summary and the first results to stderr and
exits with **3**. Status 1 remains every other error. `--no-validate` bypasses the guard
for that command, prints a warning and counts as `bypassed`. The CLI already has direct
file access, so no server flag is needed. A binary built without `shacl` refuses to
write to a validated database unless `--no-validate` is given.

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

Library writers see `Err(Error::Rejected(r))` from `WriteTxn::commit`,
`Dataset::transaction`, `Dataset::update`, `Store::load` and the other write calls.
Opening a validated database without installing a guard makes every non-empty commit
fail with `Error::GuardMissing("dataset requires write-time SHACL validation; call
sparkles_shacl::guard::ShaclGuard::install or set StoreOptions::unvalidated_writes")`.

### 2.6 Metrics and logs

| Name | Type | Labels |
|---|---|---|
| `sparkles_validation_total` | counter | `dataset`, `status` (`passed` `warned` `rejected` `skipped` `bypassed` `timeout` `error`) |
| `sparkles_validation_duration_seconds` | histogram (1 ms … 300 s, the request histogram's buckets) | `dataset`, `strategy` (`full` `incremental`) |
| `sparkles_validation_results_total` | counter, results found on validated writes | `dataset`, `severity` |
| `sparkles_validation_fallbacks_total` (Phase 2) | counter | `dataset`, `reason` (§6.2) |
| `sparkles_validation_focus_nodes` (Phase 2) | histogram | `dataset`, `strategy` |

Other changes:

* The access log gains `validation` (status) and `validation_ms` fields.
* `Outcome` gains `rejected`, so a 422 is not counted as a plain `client_error` in
  `sparkles_requests_total`.
* A rejection is logged at INFO under `sparkles::validation` with the dataset, kind,
  counts and the first result's shape and focus node. Bypasses are logged at WARN.

## 3. Standards basis

* **SHACL (W3C Rec, 2017).**
  * §3.6.1.1: `sh:conforms` is true iff validation produced no results, whatever their
    severity. Reports built here keep that meaning. The commit decision uses the separate
    `threshold`.
  * §2.1.4: severities "have no impact on the validation". That is why the threshold is
    ours and not part of the report.
  * §3.3: `sh:shapesGraph` in the data graph only suggests shapes. This feature uses its
    explicit config and ignores those triples.
  * §3.4.1: failures are signaled outside the report. Here a failure fails the write
    (§4.6).
  * §3.4.3: validation with recursive shapes is implementation-defined. Sparkles keeps
    its current treatment, and Phase 2 always validates recursive shapes in full.
  * §2.1.6: deactivated shapes are skipped.
  * §§2.3.1.2 and 2.3.1.4: sequence and inverse paths, which drive reverse evaluation in
    §6.2.
  * §4.8.1: `sh:closed`.
  * Appendix A: pre-binding, as used by SHACL-SPARQL constraints.
* **SHACL 1.2 Core (W3C Working Draft, September 2026).**
  * §6.6 *Conformance Checking* is a boolean-only form of validation. A write guard is
    such a check. This spec still produces a bounded report, because clients need to
    know *why*.
  * §3.1.3.6 `sh:targetWhere` makes target membership depend on conformance to another
    shape. Phase 2 treats it as full-validation only, until Sparkles implements it.
  * The added severities `sh:Debug` and `sh:Trace` rank below `sh:Info` (§2.1).
* **SPARQL 1.1 Update §2.2.** Each request SHOULD be atomic: no effect or complete effect,
  whatever the number of operations. Operations must have the effect of sequential
  execution. Validating the state after the *last* operation, and aborting the whole
  request on rejection, satisfies both. Intermediate states are not constrained.
* **SPARQL 1.1 Protocol §2.2.5.** A failed update should get 400 (syntax) or 500, and
  "may use other 4XX or 5XX HTTP response codes for other failure conditions, as per
  HTTP". The failure body is implementation-defined and may be content-negotiated.
  That covers a 422 with a JSON or Turtle report.
* **RFC 9110.**
  * §15.5.21 `422 Unprocessable Content`: the request is well-formed but its
    instructions cannot be processed.
  * §15.5.10 `409 Conflict`: a conflict with the target resource's current state that
    the user might resolve and resubmit.
  * §9.2.2: idempotency, which a rejected PUT keeps.
* **RFC 9651.** The `Sparkles-Validation` header value is a Dictionary.
* **Apache Jena SHACL documentation (Apache-2.0).** Its example
  `Shacl02_validateTransaction` updates a graph "only if, after the changes, the graph is
  validated". That is the same post-state semantics, done in application code there and
  in the store here.
* **Incremental integrity checking.** Nicolas (1982) introduced *simplification*.
  Assuming the database satisfied the constraints before an update, only instances of
  the constraints that the update can affect need rechecking. Phase 2 is that method,
  specialized to SHACL shape dependencies. The assumption becomes the "conforming
  baseline" of §4.2. See also Blakeley, Larson and Tompa (1986) and Gupta and Mumick
  (1995) on relevant-update detection for views. Corman, Reutter and Savković (2018)
  study the semantics of recursive SHACL, which is why recursive shapes get no
  incremental treatment.

## 4. Semantics

### 4.1 What is validated, when

* **Once per transaction, at commit.** The guard runs inside `WriteTxn::commit` (and in
  the bulk rebuild, §5.2) while the writer lock is held. It runs after the transaction's
  last change and before any WAL byte, `commit.json` or `CURRENT` switch.
  * A multi-operation SPARQL Update is validated once, on its final state.
  * A GSP `PUT` is validated on the replaced state, never on the cleared intermediate
    state.
* **The post-state** is `Arc::new(txn.view())`: base generation ⊕ transaction delta,
  including terms interned by the transaction (`dvocab_len` covers them) and with a
  zero-size result cache, so SHACL-SPARQL queries neither read nor fill the shared
  cache. On the bulk path the post-state is a snapshot over the freshly built,
  not-yet-switched generation, because staged bulk quads are invisible to `view()`.
* **Skipped** (status `skipped`, no validation):
  * a no-op transaction (no net change, no bulk batch), which creates no commit;
  * a transaction whose changes touch neither a validated data graph nor a shapes graph.
    The data graph and shapes graph are then unchanged, so the report is unchanged.
    Rejecting such a write for pre-existing results would block unrelated work, such as
    reasoner output when `includeInferences` is false.
  * `Changes::Unknown` is never skipped.
* Readers are unaffected. They keep reading the head. Other writers wait on the writer
  lock for the validation's duration (§6.1).

### 4.2 Decision rule and the baseline invariant

Let `R` be the results of validating the post-state. `blocking(R)` is the set of results
at or above `threshold`.

| mode | `blocking(R)` empty | `blocking(R)` non-empty |
|---|---|---|
| `reject` | commit, `passed` | abort, `Error::Rejected`, `rejected` |
| `warn` | commit, `passed` | commit, `warned` |

**Strict post-state semantics.** Phase 1 decides on the post-state alone, not on the
difference from the pre-state. That is sound and simple *because of the enable
precondition*. `reject` can only be enabled on a head without blocking results (§2.2), and
every later commit is validated. So in `reject` mode every committed head has no blocking
results (**the baseline invariant**), and "no blocking results after the write" is the
same as "the write introduced none".

Two things can leave the baseline unknown:

* a bypassed write (§2.3.4, `--no-validate`, `unvalidated_writes`);
* a clone (§5.4).

After either, `baseline.conforms` is `null`. The next validated write is judged strictly
and runs as a full validation. If the head carries blocking results, every write that
touches validated graphs is rejected until the data is repaired. The repair can be one
transaction that fixes everything, a bypassed write, or switching to `warn`. The workflow
for existing dirty data is: `warn` until clean, then `reject`. Phase 2 adds a
`baseline: "grandfather"` option that blocks only *new* results (§6.2.5).

The guard keeps `baseline = {commit, conforms, blocking, total}` in memory. It is updated
after every validated commit. A commit the guard did not see (a seq gap) or a config
change resets it to unknown. Phase 2 persists it (§5.4).

### 4.3 Coverage of write paths

| write | path through the store | validated | notes |
|---|---|---|---|
| SPARQL Update (HTTP, `sparkles update`, `Dataset::update`), incl. `LOAD` | `update_as` → one `WriteTxn` → `commit` | yes, once per request | `QueryOptions.write` carries deadline, cancel and bypass |
| GSP `PUT` | `replace_as` → `WriteTxn` (+ `insert_bulk`) | yes | Large bodies take the bulk path (§5.2). |
| GSP `POST`, `/upload`, `sparkles load`, `Dataset::load_*` | `load_as`: `WriteTxn`, or `rebuild_locked` from sources | yes | A load into an empty store or above `bulk_threshold` is validated on the built generation before `CURRENT` switches (`Changes::Unknown`). |
| GSP `DELETE` | `update_as("CLEAR …", GspDelete)` | yes | Deletions can violate (`sh:minCount`, `sh:class` of a referencing node). |
| Reasoner `materialize` / `clear` (`/$/reason`, `sparkles infer`, auto-reason) | `write_as(Reason/ReasonClear)` + `insert_bulk` | yes, but skipped unless `includeInferences` | A rejected run fails its task with the summary as the task error, and the inferred graph is unchanged. |
| `Dataset::transaction`, `insert`, `remove`, `extend`, `WriteTxn` | `WriteTxn::commit` | yes | `Error::Rejected` to the caller |
| Compaction | `rebuild_locked(bulk = None)` | no | Data unchanged, no commit. |
| Clone (`/$/datasets/{ds}/clone`, `sparkles clone`) | `clone_to` builds a new database from a committed snapshot | no (source untouched) | Copies the config, and the clone's baseline is unknown (§5.4). |
| Prefix changes | `add_prefixes` | no | Not data. |
| Bypass: `?validate=false` + `--allow-unvalidated-writes`, CLI `--no-validate`, `StoreOptions::unvalidated_writes` | `WriteOptions.bypass_validation` | no | These are the only bypasses. Each is counted and logged, and each makes the baseline unknown. |

### 4.4 Changes to the shapes graph

With `shapes.graphs`, a transaction may change the shapes themselves. When any change
falls in a shapes graph:

1. The guard re-reads the shapes graphs from the **post-state view**. It uses
   `Shapes::from_store_graphs`, and blank nodes stay store blank nodes.
2. A parse error rejects the write with `422` and `"validation":{"status":"rejected",
   "shapesError":"…"}`, in both modes. A write must not leave the dataset with shapes
   that cannot be read.
3. It validates the whole data graph against the **new** shapes. This is always a full
   validation. Adding a constraint the data violates is rejected in `reject` mode.
   Removing constraints always passes.
4. After the commit, the new parsed shapes replace the guard's cache. After a rejection,
   they are dropped.

Emptying the shapes graphs (`DROP GRAPH`) is an ordinary write and passes, because
nothing can violate an empty shapes graph. The status then shows a warning. See Open
question 1 on protecting the shapes graph. A config change, meaning a new shapes file,
new graphs or a new data graph, goes through PUT (§2.2), never through a data write.

### 4.5 Errors

| condition | HTTP | CLI exit | library |
|---|---|---|---|
| blocking results in `reject` mode; shapes graph made unparseable | 422 | 3 | `Error::Rejected` |
| enabling `reject` on a non-conforming head | 409 | 1 | `SetOutcome::NotConforming(summary)` |
| validation exceeded its budget (§4.6) | 408, `validation.status: "timeout"` | 1 | `Error::Timeout` |
| request cancelled (client gone) during validation | 503 (existing `Cancelled`) | – | `Error::Cancelled` |
| engine failure (SHACL §3.4.1, e.g. a SPARQL constraint fails, recursion too deep) | 500, `validation.status: "error"` | 1 | `Error::Invalid` |
| bypass requested without `--allow-unvalidated-writes` | 403 | – | – |
| validated database, no guard installed, no bypass | 501 | 1 | `Error::GuardMissing` |

In every row nothing is committed. No WAL record is written, no generation is switched
(a bulk candidate generation directory is removed), and the head `seq` is unchanged. The
next successful commit gets `head + 1`. Terms interned by the aborted transaction stay in
the append-only delta vocabulary, as they do for aborted transactions today.

### 4.6 Budgets and timeouts

* The validation deadline is the earlier of `now + timeoutSeconds` and the request's own
  deadline (`update_timeout`). The validation engine checks it every 256 focus nodes
  (`Engine::check_limits`) and between shapes. Exceeding it **fails the write** in both
  modes. `warn` never blocks on *results*, but an unvalidated commit is still a bypass.
  Open question 4 asks whether `warn` should commit instead.
* The validation runs on the rayon pool with `parallel: true`. SHACL-SPARQL constraints
  use the request's memory budget (`QueryOptions.max_memory_bytes`).
* A bulk candidate that is rejected or times out has already cost a full generation
  build. That cost is accepted in Phase 1 (§6.1).

## 5. Design sketch

### 5.1 `sparkles` (store)

* New `guard.rs`: the types of §2.5.
* `Store` gains:
  * `guard: ArcSwapOption<dyn CommitGuard>`;
  * `guard_required: AtomicBool`, set at `open` when `validation.json` exists with
    `mode != "off"`. `open` parses only the `mode` member, so the store does not depend
    on SHACL.
* `WriteTxn` gains `opts: WriteOptions` and `validation: Option<Arc<ValidationSummary>>`.
* `Error` gains `Rejected`, `GuardMissing`.

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

`run_guard`:

* bypass → `Bypassed`;
* no guard and `guard_required && !store.opts.unvalidated_writes` → `Err(GuardMissing)`;
* no guard → `None`;
* otherwise `guard.check(&Candidate{…})`.

`rebuild_locked` with `bulk: Some(_)` calls the guard **after**
`Generation::open(&dir, …)` and **before** `write_synced(dir/commit.json)`. The candidate
is `Snapshot { generation: Arc::new(gen_), delta: empty, commit: w.head.seq, results:
zero-size cache, … }`. On `Err`, it drops the generation and `remove_dir_all(dir)` (for
in-memory stores the tempdir drops) and returns the error. `w.next_bnode` may have
advanced; blank-node id gaps are harmless. `load_as` from sources passes
`Changes::Unknown`. `add_prefixes` runs only after a successful switch, as today.

The summary is stored on the receipt (`Receipt.validation`) by `publish_log` and
`rebuild_locked`.

### 5.3 `sparkles-shacl`

* `ValidateOptions` gains `graphs: Option<GraphScope>` with
  `GraphScope::Only(Vec<String>)` (absent graphs are empty) and
  `GraphScope::AllExcept(Vec<String>)`. `data::GraphSel` gains the matching
  `AllExcept(Vec<u64>)`. This avoids listing every graph id per write for `union`, and
  keeps the shapes graphs out.
* `Shapes::from_store_graphs(snap, &[iri])`: merges the graphs and sets `source_graph` to
  the first graph, which is what `$shapesGraph` binds to (Open question 6).
* `report::to_json` / `path_json`: moved from `sparkles-server/src/shacl.rs`, so the
  guard builds the bounded JSON results.
* New `guard.rs`, `ShaclGuard::check`:
  1. **Relevance.** Map `changes` to the set of touched graph ids. If it hits neither a
     data graph nor a shapes graph, return `Skipped`. `Unknown` always proceeds.
  2. **Shapes.** Use the cached `Arc<Shapes>`, or re-parse from the view when a shapes
     graph was touched (§4.4).
  3. **Validate.** Call `validate(&view, &shapes, &opts)` with the deadline and cancel
     flag from `Candidate.opts` and `parallel: true`.
  4. Count the results by severity, then sort: blocking first, then by severity, source
     shape and focus node. Keep the first `limit` results as JSON and, for `reject`
     mode, as Turtle.
  5. Decide per §4.2. Update the baseline (pending until commit; see below) and the
     counters.

  The baseline changes only after the store confirms the commit. The guard records a
  *pending* baseline keyed by `(base.commit + 1)`. A later `check` whose `base.commit`
  equals that seq promotes it. A mismatch resets the baseline to unknown.
* `set_config` takes the writer lock with `store.write_as(Transaction)` and makes no
  changes. It validates `txn.base()`, then writes `validation.json` (and
  `validation-shapes.ttl`) with a temp-file, fsync and rename sequence (as `write_atomic`
  does), calls `set_guard`, and drops the transaction.

### 5.4 Persistence, open, clone

* `<db>/validation.json` and `<db>/validation-shapes.ttl` sit next to `text.json` and
  `reasoning.json`.
  * The server (`AppState::attach`/`create`) and the CLI write commands call
    `ShaclGuard::install` right after `Store::open`.
  * A malformed `validation.json` makes `install` fail. The dataset then opens with
    `guard_required` set, so writes fail with `GuardMissing` and reads work. The server
    logs an error.
  * In-memory datasets keep their config in memory only.
* **Clone** copies both files, as it copies `text.json`. The clone's baseline is unknown,
  so its first validated write runs a full validation. If the source used
  `includeInferences` and the clone dropped inferences (`inferences=drop`), the status
  shows a warning.
* **Phase 2** persists the baseline in `validation-status.json`
  (`{commit, configHash, conforms, blocking, total}`), written after the commit is
  durable. It is trusted only when `commit == head.seq` and `configHash` matches. A crash
  between the commit and the status write therefore yields "unknown", never a false
  "conforming".

### 5.5 Server plumbing

* `http.rs`:
  * `update_endpoint`, `gsp`, `upload` build `WriteOptions` from `update_timeout`, the
    request cancel flag, `validate=false` (checked against `st.allow_unvalidated_writes`)
    and `validationLimit`.
  * `From<Error> for ApiError` maps `Rejected` to 422 with the §2.3.2 body, choosing the
    Turtle form by `Accept`, and `GuardMissing` to 501.
  * `with_validation(resp, summary)` adds the header. `write_response` inserts
    `validation` into receipts.
  * The CORS expose list gains `Sparkles-Validation`.
* New routes `/$/validation/{ds}` (GET/PUT/DELETE).
* `DatasetInfo` gains `validation: {mode, baselineConforms} | null`.
* `obs.rs`: the §2.6 metrics and `Outcome::Rejected`.
* `main.rs`: the `Validation` subcommand, `--no-validate` on `load`/`update`/`infer`,
  `serve --allow-unvalidated-writes`.
* Reasoning tasks: a `Rejected` error becomes the task error text
  `"inferences rejected by SHACL validation: 2 blocking results (first: <shape> at <node>)"`.

## 6. Performance

### 6.1 Phase 1: full validation per write

Validation over a snapshot is index-backed. Focus nodes come from `PSO`/`POS` scans, and
paths are evaluated with `SPO`/`POS` prefix scans. It runs in parallel over chunks of 256
focus nodes. Measured today (`docs/BENCHMARKS.md`, `mise run bench:shacl 100000`, 20
shapes, compacted base, empty delta): **1.05M triples, 48,428 results: 164 ms parallel,
741 ms sequential.**

| validated data | full validation (parallel) | writes/s ceiling with `reject` (the lock is held for validation + commit) |
|---|---:|---:|
| 100k triples | ≈ 16 ms (extrapolated) | ≈ 45 |
| 1.05M triples | 164 ms (measured) | ≈ 6 |
| 10M triples | ≈ 1.6 s (extrapolated, unmeasured) | < 1 |

Baseline: a 1-triple `INSERT DATA` commit costs 5–7 ms today. The cost is linear in
(focus nodes × constraints evaluated), plus result construction. A conforming dataset
builds no results and should be cheaper than the benchmark. A large uncompacted delta
makes scans merge rows and costs more. Both effects are unmeasured.

Phase 1 fits datasets of up to about 1M validated triples with modest write rates. It
also suits any size where writes are rare bulk loads. Rules that follow from this:

* **Measure.** A new `mise run bench:shacl-write N` measures 1-triple `INSERT DATA`
  latency with `off`, `warn` and `reject` at N = 100k and 1M. It also runs 0 and 10k
  pending delta quads. The results are recorded in `docs/BENCHMARKS.md`. Acceptance
  targets:
  * `reject` latency ≤ the standalone validation time + 10 ms;
  * `off` unchanged within noise.
* `ValidationStatus.lastFullMillis` is recorded at every full validation. PUT adds a
  warning when it exceeds 1000 ms.
* The relevance skip (§4.1) makes writes to graphs outside the validated set, including
  reasoner output, free.

### 6.2 Phase 2: incremental validation

#### 6.2.1 Predicate relevance filter (cheap, first)

For each active shape, statically compute `reads(S)`. This is the set of predicates its
evaluation can read:

* path predicates;
* target predicates;
* `sh:equals`/`sh:disjoint`/`sh:lessThan`/`sh:lessThanOrEquals` predicates;
* `rdf:type` and `rdfs:subClassOf` for class targets and `sh:class`;
* `*` (every predicate) for `sh:closed`, SHACL-SPARQL constraints and components;
* the predicates of every shape it references, transitively.

If no change's predicate is in the union of `reads` over all shapes, the report is
unchanged. The write is then `skipped`, provided the baseline is known.

#### 6.2.2 Affected focus nodes

The inputs are the changed quads Δ, restricted to data graphs. Δ⁺ and Δ⁻ are taken from
`Changes::Log`/`Rebuilt`. G is the pre-state (`base`) and G′ the post-state (`view`).

A **dependency** is a pair `(π, p, dir)`. It says that evaluating the shape at focus node
f reads edges with predicate p (or `*`) at the nodes reachable from f by path π, as
subjects (`out`) or objects (`in`). `deps(S)` is computed statically from the shape:

| construct | dependencies |
|---|---|
| property shape with path P | for each predicate step `p` in P: `(prefix(P, step), p, out)`, or `in` under `sh:inversePath`. For `*`/`+`/`?`, the prefix includes the closure itself (`(p*, p, out)` for `p*`). |
| value-level constraints (`datatype`, `nodeKind`, `in`, `pattern`, lengths, ranges, `languageIn`, `hasValue`, counts, `uniqueLang`) | none beyond the path's, because they depend only on the value set |
| `sh:class C` | `(P, rdf:type, out)` plus a global dependency on `rdfs:subClassOf` |
| `equals`/`disjoint`/`lessThan[OrEquals]` q | `(ε, q, out)` |
| `sh:closed` | `(ε, *, out)` at the value nodes: `(P, *, out)` on a property shape, `(ε, *, out)` on a node shape |
| `sh:node`/`sh:property`/`and`/`or`/`not`/`xone`/`qualifiedValueShape` → S₂ | `{ (P·π₂, p₂, d₂) : (π₂, p₂, d₂) ∈ deps(S₂) }`, where P = ε on node shapes |
| SHACL-SPARQL constraint or component, `sh:targetWhere` | ⊤ (the whole shape is recomputed) |

Target delta:

* rdf:type changes → their subjects (for class targets; `is_target` filters them later);
* `targetSubjectsOf p` → s of changed p-triples;
* `targetObjectsOf p` → o of changed p-triples;
* `targetNode` → none (reached through deps).

`A(S)` is the target delta ∪ ⋃ over each change `(s, p, o)` and each dependency
`(π, p′, d)` with `p′ ∈ {p, *}` of `reverse(π, x)`. Here `x = s` for `out` and `x = o`
for `in`, and `reverse(π, x)` is the set of f with `x ∈ π(f)`. It is computed with the
reversed path (inverse steps use `SPO`, forward steps use `POS`), evaluated in **both G
and G′** and united. The engine then validates S on `{f ∈ A(S) : is_target(S, f) in G′}`.
This uses a new `validate_nodes(snap, shapes, &[(ShapeId, Vec<Id>)], opts)`, built on
`Engine::run(Some(_))` generalized to node lists.

**Soundness sketch.** Take a focus node f ∉ A(S) and suppose some edge read while
evaluating S at f differs between G and G′. Take the first differing edge along the
evaluation's read path, reached from f via π. Every earlier edge on that path is
unchanged, so π reaches the edge's endpoint x in both states. Then f ∈ reverse(π, x) ⊆
A(S), a contradiction. So S's reads at f are identical in G and G′, and so is its result.
Combined with the baseline invariant (no blocking results at G), a blocking result in G′
can only occur at a node in A. Induction on the path length gives the general case, and
composition through referenced shapes follows because `deps` composes paths.

#### 6.2.3 Where incremental is unsound: fall back to full

| # | case | fallback |
|---|---|---|
| F1 | baseline unknown or not conforming (strict mode) | full |
| F2 | shapes graph or config changed | full |
| F3 | `rdfs:subClassOf` changed in a validated graph while some shape uses class targets or `sh:class` | full. A later refinement: `A ∪=` instances of the changed subject's subclass tree, in G and G′. |
| F4 | shape has a SHACL-SPARQL constraint/component or `sh:targetWhere` | full *for that shape only* (all its focus nodes); other shapes stay incremental |
| F5 | shape is in a reference cycle (recursive, SHACL §3.4.3) | full for the shapes in the cycle |
| F6 | `Changes::Unknown` (bulk load from sources) | full |
| F7 | budget: \|A\| > 50k focus nodes, or \|A\| > 25 % of the shape's last full focus count, or a reverse traversal through `*`/`+` visits > 100k nodes | full |
| F8 | `includeInferences` and the change is a reasoner commit | incremental is sound (the inferred graph is an ordinary graph); no fallback |

Every fallback increments `sparkles_validation_fallbacks_total{reason}`.

#### 6.2.4 Expected cost

Cost is O(|Δ| × reverse fan-in × constraints). Each reverse step is an index prefix
scan, so a 1-triple write is expected to take well under 1 ms at 10M triples, compared
with ≈ 1.6 s for full validation. The benchmark of §6.1 gains an `incremental` row, and
a test compares incremental against full reports on randomized writes (A15).

#### 6.2.5 Grandfather mode

`baseline: "grandfather"` (Phase 2) allows `reject` on a non-conforming head. The guard
validates A in G *and* in G′ and blocks only post results whose key `(focus, path, value,
sourceShape, component, sourceConstraint)` is not among the pre results. Full-validation
fallbacks do the same over the whole graph, which costs two validations.

### 6.3 Uniqueness constraints (later item)

Single-writer validation on the exact post-state is race-free: no concurrent transaction
can insert a duplicate between check and commit, so uniqueness needs no extra locking.
Two forms fit:

* **Core SHACL (recommended).** "A value of `ex:email` identifies at most one node":
  ```turtle
  ex:EmailUnique a sh:NodeShape ; sh:targetObjectsOf ex:email ;
      sh:property [ sh:path [ sh:inversePath ex:email ] ; sh:maxCount 1 ] .
  ```
  This works in Phase 1, where it costs a full validation. In Phase 2 it is incremental:
  an inserted `(s, ex:email, o)` gives `A = {o}` (target delta), and the check is one
  `POS` prefix scan. It is O(log n) per changed triple, with no fallback.
* **SHACL-SPARQL** (e.g. `FILTER EXISTS { ?other ex:key ?k . FILTER(?other != $this) }`)
  falls under F4 (full for that shape) in Phase 2. Phase 3 adds a
  **localizable-query analysis**. When every triple pattern of the constraint's WHERE
  clause is connected to `$this` through variables, the query has no subqueries or
  `SERVICE`, and its property paths are closure-free, the query's patterns become `deps`
  like shape paths (a pattern `?a p ?b` reached from `$this` by a chain π gives
  `(π, p, out)`). The shape then leaves F4. Negation (`FILTER NOT EXISTS`, `MINUS`) is
  allowed when anchored the same way.
* Composite keys (several properties) need SHACL-SPARQL. Core SHACL has no tuple
  uniqueness. They are covered by the same Phase 3 analysis.

## 7. Phasing

**Phase 1 (MVP, ~2–3 days)**

* `sparkles::guard`: the trait, `Candidate`, `Changes`, `WriteOptions` and
  `ValidationSummary`.
* The hooks in `WriteTxn::commit` and in bulk `rebuild_locked` (candidate generation,
  removal on reject), `Error::Rejected`/`GuardMissing`, `Receipt.validation`, and the
  `guard_required` fail-closed check.
* `sparkles-shacl`:
  * `GraphScope`, `from_store_graphs`, `report::to_json`;
  * `ShaclGuard` with full validation, the relevance skip, the shapes cache and re-parse
    on shapes-graph change, threshold and limits, and the in-memory baseline;
  * `set_config` with the enable check.
* Server:
  * `/$/validation/{ds}` GET/PUT/DELETE;
  * 422/409/501 mapping, JSON and Turtle rejection bodies;
  * the `Sparkles-Validation` header and receipt member;
  * `WriteOptions` plumbing in update/GSP/upload;
  * `validate=false` behind `--allow-unvalidated-writes`;
  * `sparkles_validation_total` and `_duration_seconds`, the access-log fields and
    `Outcome::Rejected`;
  * reasoner task errors.
* CLI: `sparkles validation`, guard installation plus `--no-validate` on
  `load`/`update`/`infer`, exit code 3, and the `stats` line.
* Clone copies the config.
* `docs/API.md` section "Write-time validation".
* Tests A1–A14. `bench:shacl-write`.

**Phase 2**

* The predicate relevance filter (§6.2.1).
* Incremental focus nodes (§6.2.2) with fallbacks F1–F8 and the fallback/focus metrics.
* A persisted baseline (`validation-status.json`).
* Grandfather mode.
* UI:
  * a dataset-page *Validation* panel (mode, shapes, baseline badge, counters, last full
    time, a config form);
  * the query page renders a 422 as a results table with a "N of M shown" footer.
* Tests A15–A16.

**Phase 3**

* Localizable SHACL-SPARQL analysis (§6.3) and the refinement of F3.
* `sh:targetWhere` once Sparkles supports SHACL 1.2 targets.
* A catalog flag bit3 `unvalidated` for bypassed commits, so `sparkles log` shows them.
  This needs CI's reserved WAL commit byte 27.
* `serve --validate NAME=CONFIG.json`, like `--text`.
* Mixed shapes sources (graphs plus a file).

## 8. Acceptance examples

Setup: `sparkles serve --data DIR`, with a persistent dataset `ds`. Prefix `ex:` is
`http://ex.org/`. Shapes, loaded with `PUT /ds/data?graph=urn:shapes` while validation is
off:

```turtle
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ; sh:datatype xsd:string ] ;
  sh:property [ sh:path ex:age ; sh:maxInclusive 150 ; sh:severity sh:Warning ] .
ex:EmailUnique a sh:NodeShape ; sh:targetObjectsOf ex:email ;
  sh:property [ sh:path [ sh:inversePath ex:email ] ; sh:maxCount 1 ] .
```

**A1. Enable.** `PUT /$/validation/ds` with
`{"mode":"reject","shapes":{"graphs":["urn:shapes"]}}` → `200`. The response has
`status.baseline.conforms: true` and `status.shapes.shapeCount ≥ 2`. `<db>/validation.json`
exists with `"format":1`. `GET /$/validation/ds` returns the same config.

**A2. Conforming write.** `INSERT DATA { ex:a a ex:Person ; ex:name "A" }` with a
receipt → `200`. The header is `Sparkles-Validation: status=passed, mode=reject,
strategy=full, blocking=0, …`. The receipt has `"validation":{"status":"passed",…}` and
`commit.seq` = head+1 (call it n).

**A3. Rejection, nothing consumed.** `INSERT DATA { ex:b a ex:Person }` → `422`
`application/json`, with:

* `validation.blocking: 1`;
* `results[0].sourceConstraintComponent` = `sh:MinCountConstraintComponent`;
* `focusNode` = `ex:b`;
* `Sparkles-Commit: n`.

`ASK { ex:b ?p ?o }` → false. `GET /$/commits/ds` has head n. The next conforming write
gets seq n+1.

**A4. One validation per request.**
`INSERT DATA { ex:c a ex:Person } ; INSERT DATA { ex:c ex:name "C" }` → `200`. The
intermediate state is not validated. `DELETE DATA { ex:a ex:name "A" } ; INSERT DATA {
ex:x ex:p 1 }` → `422`, and neither operation is applied.

**A5. Turtle report and bounds.** Insert 250 `ex:Person` nodes without names, with
`Accept: text/turtle` → `422 text/turtle`. The body parses as an `sh:ValidationReport`
with `sh:conforms false` and exactly 100 `sh:result`. The header has `blocking=250,
total=250`. The same request as JSON with `validationLimit=300` → `results.length == 250`,
`truncated: false`. With the default limit, `truncated: true`, `limit: 100`.

**A6. Threshold.** `INSERT DATA { ex:a ex:age 200 }` → `200`, status `passed`,
`warnings=1`, `blocking=0`. Then `PUT /$/validation/ds` with `"threshold":"warning"` →
`409`, because the head now has a warning-level result. With `"mode":"warn",
"threshold":"warning"` → `200`.

**A7. Warn mode.** With `mode: warn`: `INSERT DATA { ex:d a ex:Person }` → `200` and
committed. The header is `status=warned, blocking=1`. The receipt carries `results` with
one entry. `GET /$/validation/ds` → `baseline.conforms: false`,
`counters.warned ≥ 1`.

**A8. GSP and deletions.** Back to `reject` on a conforming head. The config is
`"dataGraph":"union"` with the shapes graph excluded. The data is `ex:o1 ex:email
"x@ex.org"` in graph `urn:g1`.

* `POST /ds/data?graph=urn:g2` with `ex:o2 ex:email "x@ex.org"` → `422`
  (`sh:MaxCountConstraintComponent`, focus `"x@ex.org"`).
* `PUT /ds/data?graph=urn:g1` with a body that renames the email → `200`.
* A `PUT` that is rejected leaves `urn:g1` byte-identical: GSP `GET` returns the same
  triples.
* `DELETE /ds/data?graph=urn:g1`, where `urn:g1` holds the only `ex:name` of a person →
  `422`, and the graph is still present.

**A9. Bulk path.** Unit test with `StoreOptions { bulk_threshold: 10 }`, a guard
installed, and `load` of 100 triples of which one person lacks a name →
`Err(Error::Rejected)`. `CURRENT` is unchanged, no new `gen-NNNN` directory remains,
`head_commit()` is unchanged, and `snapshot().len()` is unchanged. The same load without
the violation → `bulk: true` receipt with `validation.status == Passed`.

**A10. Shapes change.** In `reject` mode on a conforming head, with one person lacking
`ex:email`:

* inserting `ex:PersonShape sh:property [ sh:path ex:email ; sh:minCount 1 ]` into
  `urn:shapes` → `422`;
* inserting a shape the data satisfies → `200`, and the next write is validated with it;
* inserting `ex:S sh:property [ sh:path ]` (a malformed path) → `422` with
  `shapesError`;
* `DROP GRAPH <urn:shapes>` → `200`, and `GET /$/validation/ds` shows the warning
  "shapes graph <urn:shapes> has no shapes".

**A11. Relevance skip.** With `dataGraph: ["urn:g1"]`: `INSERT DATA { GRAPH <urn:other>
{ ex:z a ex:Person } }` → `200`, `status=skipped`, `strategy=none`.
`sparkles_validation_total{status="skipped"}` increases.

**A12. Bypass.**

* `POST /ds/update?validate=false` with a violating insert → `403` on a server without
  the flag.
* Restarted with `--allow-unvalidated-writes` → `200`, `Sparkles-Validation:
  status=bypassed`, a WARN log line, and `baseline.conforms: null`.
* The next conforming-but-incomplete write, which does not fix the bypassed violation →
  `422`, because the post-state still has it.

**A13. Timeout.** Unit test with a guard over a store holding 200k persons and
`timeoutSeconds: 0.001` → `Error::Timeout`, head unchanged. Over HTTP → `408` with
`validation.status: "timeout"`. `sparkles_validation_total{status="timeout"}` increases.

**A14. Library, CLI, reasoner and fail-closed.**

* `Dataset::open(db)` without a guard, on a database whose `validation.json` says
  `reject` → `ds.update("INSERT DATA {…}")` returns `Err(Error::GuardMissing(_))`. Reads
  work.
* With `StoreOptions { unvalidated_writes: true }` → `Ok`.
* After `ShaclGuard::install(ds.store())`, a violating `ds.transaction(|tx| …)` →
  `Err(Error::Rejected(r))` with `r.summary.blocking == 1`.
* `sparkles update --loc db 'INSERT DATA { ex:q a ex:Person }'` → exit 3, with the
  summary on stderr.
* `sparkles update --loc db --no-validate …` → exit 0 and a warning.
* With `includeInferences: true` and a shape `sh:targetClass ex:Agent` requiring
  `ex:name`, where the rules infer `ex:q a ex:Agent` for a nameless node →
  `POST /$/reason/ds` task state `failed`, with the error starting
  "inferences rejected by SHACL validation". The inferred graph is unchanged.

**A15. Incremental equals full (Phase 2).** Property test: random shapes from the bench
set, random data (10k triples), 500 random single- and multi-quad transactions. For each
transaction, the incremental decision and the blocking result set restricted to A equal
those of full validation. Every fallback case F1–F7 is hit at least once and counted in
the metric.

**A16. Uniqueness is local (Phase 2).** At 1M triples with `ex:EmailUnique`, inserting
one `ex:email` triple validates exactly 1 focus node (`sparkles_validation_focus_nodes`)
with strategy `incremental` and takes < 5 ms.

## 9. Rejected alternatives

* **Validate after commit and revert with a compensating commit.** That consumes two
  seqs, readers can observe the invalid state, and search indexes and watermarks index
  it.
* **Validate only the request's data** (the inserted triples in isolation). That misses
  deletions, cardinality across old and new data, `sh:class` of existing nodes, and
  uniqueness. SHACL defines conformance of a data graph, not of a diff.
* **Validate each operation of a multi-operation update.** That contradicts SPARQL Update
  §2.2 request atomicity: fix-ups in later operations would be rejected.
* **A guard in the HTTP layer only.** It misses CLI writes, the reasoner and library
  writes. The store-level hook covers all of them.
* **Validate outside the writer lock** (optimistic: validate, then commit if the head is
  unchanged). With a single writer there is no concurrency to gain, and a retry loop
  complicates bulk paths. Locking also makes uniqueness race-free.
* **Validate the bulk batch through `view()`.** Staged bulk quads are invisible to it, so
  the guard would validate the wrong state. The candidate generation is the post-state.
* **`409 Conflict` for rejected writes.**
  * Sparkles already uses 409 for retryable state conflicts: "task already running",
    "dataset exists", and here the enable precondition.
  * A SHACL rejection means the request's instructions produce invalid content, which is
    422's meaning (RFC 9110 §15.5.21). A client must change the request, not merely
    retry it.
  * Clients need to tell the two apart without parsing bodies.
* **`400 Bad Request`.** It is reserved for syntax errors (SPARQL Protocol §2.2.5), and
  UI and tooling treat it as "fix your query".
* **Referencing the shapes file by path.** The file could change without the data being
  re-validated, which breaks the baseline invariant. The content is copied instead.
* **Reject on any result (`sh:conforms false`).** Warnings and infos would block writes,
  and SHACL §2.1.4 says severities do not affect validation. A separate threshold keeps
  the report standard.
* **A default reserved shapes graph name.** Users choose the graph.
  `urn:x-sparkles:shapes` is not special-cased.
* **Honor `sh:shapesGraph` triples in the data (§3.3).** Any writer could then change the
  shapes by writing data. The explicit config is authoritative.
* **Diff semantics (new results only) as the Phase 1 default.** That needs the pre-state
  report per write, which doubles the cost. The enable precondition plus strict
  semantics gives the same answer on conforming baselines. Diff semantics come in
  Phase 2 as the grandfather mode.

## 10. Open questions

1. **Protect the shapes graph?** Today a data write can edit or drop the shapes (§4.4),
   and dropping them always passes. Option: `protectShapes: true`, which rejects any
   change to a shapes graph unless the write is a bypass or goes through a new
   `PUT /$/validation/{ds}/shapes`. Default proposal: off in Phase 1, and the status
   warns when the shapes graph is empty.
2. **`includeInferences` and staleness ([C08](C08-inference-freshness.md)).** User writes are validated against
   inferences that are stale by construction until the next reason run. Should the guard
   refuse `includeInferences` without `--auto-reason`, or only warn? Default: warn in the
   status.
3. **Unknown severity IRIs** count as `sh:Violation` (fail closed). The alternative is
   to ignore them. The SHACL 1.2 severity ordering, once final, may settle this.
4. **Timeouts in `warn` mode** currently fail the write. Should `warn` commit with
   `status=incomplete` instead (`onTimeout: "commit"`)?
5. **Long bulk validations hold the writer lock** (multi-second at 10M+). Should the
   candidate generation be built and validated *before* taking the lock, and committed
   only if the head is unchanged, else rebuilt? That is an optimistic scheme for bulk
   only.
6. **`$shapesGraph` with several shapes graphs.** It binds to the first graph. Should it
   bind to a union view instead?
7. **Per-graph validation.** `dataGraph: [g1, g2]` merges the graphs into one data graph.
   Some users may want each graph validated separately (for example one graph per
   tenant). This could be a later `perGraph: true`.
8. **Report ordering.** Results are sorted blocking-first, then by shape and focus. Is a
   deterministic order worth its sort cost at 10k results?

## 11. Sources

* **Sparkles repository (read):**
  * `crates/sparkles-shacl/src/{lib.rs,data.rs,validate.rs,shapes.rs,path.rs,report.rs,sparql.rs}`
    (snapshot access, `GraphSel`, parallel chunking, deadlines, targets, constraints, the
    `$shapesGraph` binding);
  * `crates/sparkles-shacl/examples/bench.rs`;
  * `crates/sparkles/src/store.rs` (`WriteTxn`, `view`, `commit`, `publish_log`,
    `insert_bulk`, `load_as`, `replace_as`, `rebuild_locked`, `clone_to`, text config
    persistence);
  * `crates/sparkles/src/sparql/update.rs`, `crates/sparkles/src/dataset.rs`,
    `crates/sparkles/src/error.rs`, `crates/sparkles/src/commit.rs` (`Receipt`);
  * `crates/sparkles-reasoner/src/lib.rs` (`materialize`/`clear` commit kinds and bulk
    insert);
  * `crates/sparkles-server/src/{http.rs,shacl.rs,state.rs,obs.rs,clone.rs,main.rs}`
    (the update, GSP, upload and shacl handlers, error mapping, receipts, metrics, CLI);
  * `docs/API.md` (SHACL validation, metrics, access log);
  * `docs/BENCHMARKS.md` (164 ms / 741 ms SHACL, 5–7 ms update latency);
  * `README.md`.
* **Existing specs:** [CI](CI-commit-identity.md) (receipts, seq assignment, abort
  semantics), [C08](C08-inference-freshness.md) (style, reasoner interaction),
  [C06](C06-clone-to-sandbox.md) and [C01](C01-observability-and-budgets.md) (the
  config-copy and budget sections only), and [PROVENANCE](PROVENANCE.md). No other
  planning document was read.
* **W3C Shapes Constraint Language (SHACL)**, https://www.w3.org/TR/shacl/ (fetched):
  §§2.1.4, 2.1.6, 2.3.1.2, 2.3.1.4, 3.3, 3.4.1, 3.4.3, 3.6.1.1, 4.8.1, Appendix A.
* **W3C SHACL 1.2 Core, Working Draft**, https://www.w3.org/TR/shacl12-core/ (fetched):
  §3.1.3.6 `sh:targetWhere`, §6.6 conformance checking, extended severities.
* **W3C SPARQL 1.1 Update**, https://www.w3.org/TR/sparql11-update/ (fetched): §2.2
  atomicity and sequential effect, §3 abort on failure.
* **W3C SPARQL 1.1 Protocol**, https://www.w3.org/TR/sparql11-protocol/ (fetched): §2.2.5
  update failure responses.
* **Apache Jena SHACL documentation** (Apache-2.0), https://jena.apache.org/documentation/shacl/
  (fetched): the `Shacl02_validateTransaction` example, SHACL-SPARQL support.
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
* **Not consulted**: Fluree in any form: its code, docs, tests, site or talks. TopBraid
  documentation was not consulted either.

## Outcome

**Phase 1 shipped on 2026-09-30** in three steps:

1. The store's write guard: a check of every commit's post-state before anything is
   written, on the WAL path and the bulk path, with fail-closed `GuardMissing` and
   `Error::Rejected`. Receipts carry the summary, so `Receipt` is no longer `Copy`.
2. The SHACL guard in the library.
3. The server and CLI: `/$/validation/{ds}`, `422`/`409`/`501`, the `Sparkles-Validation`
   header, `--allow-unvalidated-writes`, `sparkles validation`, `--no-validate`, exit
   status 3, and clone keeping the configuration.

A follow-up added the `sparkles_validation_*` metrics, the
`validation`/`validation_ms` access-log fields, the reasoner's "inferences rejected by SHACL
validation" task error, the status in CLI summaries and `sparkles stats`, and
`mise run bench:shacl-write` (1-triple `INSERT DATA` latency with validation off, warn and
reject). The W3C SPARQL suites and the SHACL suite (98/98 Core, 20/20 SPARQL) stayed green.

**Deviations.**
- `sparkles stats` shows no "last full N ms" for SHACL: the baseline is in memory only, as
  Phase 1 has no persisted baseline.
- The library gained a public `GuardObserver` trait, which the server uses for its metrics
  and logs.
- When ShEx became a second guard language ([G02](G02-shex.md) Phase 2), the configuration
  gained `language`.
  - Decided by the maintainer: `validation.json` is always written as format 2 with
    `language`, SHACL included. Format 1 files are still read (it is an internal file, so
    only older Sparkles binaries are affected).
  - Also decided by the maintainer: the `sparkles_validation_*` series gain a `language`
    label (`shacl` for SHACL).
  - Chosen during implementation: the SHACL `Sparkles-Validation` header is unchanged
    (ShEx adds `lang=shex`), and summaries and `GET /$/validation/{ds}` gain an additive
    `language` field.
  - The ShEx guard also skips writes whose predicates no shape reads, a finer relevance
    skip than SHACL's graph-level one.

**Cost.** Every validated write runs a full validation of the data graph, about 160 ms for
SHACL at 1M triples ([docs/API.md](../API.md#write-time-validation)); skipped writes are
free. The standalone validator's numbers are in
[BENCHMARKS: Other measurements](../BENCHMARKS.md#other-measurements).

**Not built.** All of Phase 2: the predicate relevance filter and incremental focus-node
validation with its fallbacks (§6.2), the persisted baseline, grandfather mode, and the
dataset-page validation panel (the UI's Validate panel is on-demand only). Phase 3
(localizable SHACL-SPARQL, `sh:targetWhere`, the `unvalidated` catalog flag,
`serve --validate`, mixed shapes sources) is not started. The open questions of §10 keep
their Phase 1 defaults.
