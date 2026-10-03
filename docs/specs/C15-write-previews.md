# C15: Write previews (dry runs)

> **Status:** implemented
>
> **Phases:** One phase shipped on 2026-10-02. It added dry runs of updates, Graph Store
> writes, uploads and the MCP update tool, and made Graph Store `PUT` log only the
> difference.
>
> **User docs:** [API: Write previews](../API.md#write-previews) ·
> [Usage: Previewing a write](../USAGE.md#previewing-a-write) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This is a clean-room spec. It builds on durable commit identity ([CI](CI-commit-identity.md)),
write-time validation ([C10](C10-write-time-validation.md), [G02](G02-shex.md)), storage
quotas, entity tags and graph-level access control ([C12](C12-graph-access-control.md)).
The sources were the SPARQL 1.1 Update, Protocol and Graph Store Protocol standards,
RFC 9110, the public documentation of Kubernetes' server-side dry run and of RDF4J's
transaction API, Apache Jena's RDF Patch documentation, and the Sparkles code.

## 1. Summary, goals, non-goals

A client that is about to change a dataset often wants to know what the change will do
before it makes it. It may want to know how many quads an `INSERT … WHERE` will add,
whether a Graph Store `PUT` will pass the dataset's SHACL guard, whether an upload fits
the storage quota, or whether its `If-Match` tag is still current. Today the only way to
find out is to make the write and undo it, which uses up a commit number, leaves the
change in the history and runs every hook a commit runs.

This feature adds a **dry run** to every write endpoint. A dry run executes the write in
a transaction exactly as the write would run, under the writer lock and against the
current head. It then reports what the commit would be and rolls the transaction back.
The report holds:

* the commit the write would create, with the same members as a receipt: its sequence
  number, kind, net inserted and deleted counts, the size of the dataset after it, its
  generation, and whether it would be a bulk commit;
* the net inserted and deleted counts of each graph the write changes;
* optionally, the changed quads up to a limit, as JSON or as an RDF Patch;
* the write-time validation summary the write would get, in the guard's mode, including
  grandfather mode;
* the effect on the storage quota, and whether the quota or the disk reserve would refuse
  the write;
* the outcome of the request's `If-Match` and `If-None-Match` preconditions.

**Goals**

* One switch, the same on every write: SPARQL Update, Graph Store `PUT`, `POST` and
  `DELETE`, uploads, and the MCP `sparql_update` tool.
* The preview is the receipt. The commit a dry run reports equals the receipt of the same
  write made right after it, except for the timestamp and the change digest, which depend
  on the moment of the commit.
* A dry run changes nothing. No WAL record is written, no commit number is used, the
  catalog and the annotations are untouched, the published snapshot stays the same, the
  inference status does not change, and the full-text, vector and spatial indexes see
  nothing. Terms the write would add to the vocabulary are removed again, and the
  blank-node counter is restored.
* A dry run needs exactly the permission the write needs.
* A dry run costs about what the write costs, and no more than it, under the same budgets.

**Non-goals**

* Previewing a write against a state other than the head. The preview is of the write as
  it would run now.
* Holding the result for a later commit. A dry run is not a prepared transaction, and a
  write made after it runs again from the start. Section 8 covers why.
* Multi-request transactions. Sparkles still has one request per transaction.
* Previews of admin operations such as compaction, reasoning runs, clones and restores.
* A `--dry-run` for the local CLI commands. The library API of §2.7 makes one easy to add.

## 2. User-visible behaviour

### 2.1 Asking for a dry run

A write becomes a dry run when the request carries either of these:

* the parameter `dryRun=true`, in the query string, or in the form body of an update;
* the header `Sparkles-Dry-Run: true`.

`dryRun` without a value counts as `true`, like `?default`. `dryRun=false` is an ordinary
write. Any other value is a `400`. A misspelt value must not turn a preview into a real
write, so it is refused instead of read as false. The header takes the same values.

Every other parameter and header of the write applies unchanged: `timeout`, the query
budgets of an update, `validate=false` with `Sparkles-Validate`, `validationLimit`,
`Sparkles-Commit-Message`, `If-Match` and `If-None-Match`. `receipt=true` and the
Sparkles media type in `Accept` make no difference, because a preview is always a
receipt-shaped document.

Two parameters belong to dry runs:

| Parameter | Values | Meaning |
|---|---|---|
| `changes` | 0 to 10,000, default 0 | List up to this many changed quads in the preview. The counts are always complete. |
| `Accept: application/rdf-patch` | also `text/rdf-patch`, and `application/rdf-patch+thrift` for the binary form | Answer with the whole net change as an RDF Patch instead of the JSON preview. |

### 2.2 What a dry run does

The server handles a dry run like the write, up to the moment of the commit. It reads and
parses the body, checks the caller's permissions and graph view, takes the writer lock,
runs the update's operations or the Graph Store change in a write transaction, and runs
the dataset's guard on the state after the write. Where a real commit would write its
WAL record, the dry run collects its report and rolls back.

A Graph Store `PUT` or upload large enough for the bulk path builds the new index
generation in the dataset directory, as the write would. The dry run validates it and
measures it against the quota, then deletes it. Section 4.2 lists what that leaves on
disk, which is nothing once the dry run returns.

### 2.3 The preview document

A successful dry run answers `200 OK` with `Content-Type: application/json`:

```json
{
  "dryRun": true,
  "dataset": "ds",
  "datasetId": "3f1c9a2e-7b4d-4c1e-9a55-0c2b8e61d7aa",
  "committed": false,
  "wouldCommit": true,
  "outcome": "commit",
  "head": 42,
  "commit": {
    "seq": 43, "parent": 42, "ref": "commit:43", "kind": "update",
    "inserted": 2, "deleted": 1, "quads": 1205,
    "generation": "gen-0007", "bulk": false, "exact": true,
    "message": "fix titles"
  },
  "graphs": [
    { "graph": null, "inserted": 1, "deleted": 1 },
    { "graph": "http://example.org/g1", "inserted": 1, "deleted": 0 }
  ],
  "changes": {
    "total": 3, "limit": 10, "truncated": false,
    "quads": [
      { "op": "-", "subject": "<urn:a>", "predicate": "<urn:p>", "object": "\"old\"", "graph": null },
      { "op": "+", "subject": "<urn:a>", "predicate": "<urn:p>", "object": "\"new\"", "graph": null },
      { "op": "+", "subject": "<urn:b>", "predicate": "<urn:p>", "object": "\"x\"", "graph": "<http://example.org/g1>" }
    ]
  },
  "validation": { "language": "shacl", "status": "passed", "mode": "reject", "blocking": 0, … },
  "precondition": { "status": "passed" },
  "storage": { "status": "fits", "limit": 1073741824, "used": 52428800, "projected": 52428899 }
}
```

The members are:

| Member | Meaning |
|---|---|
| `dryRun` | Always `true`. A client that sent a dry run to a server without this feature gets an ordinary write response, which has no such member. |
| `committed` | Always `false`, so a client that reads receipts never takes a preview for a commit. |
| `wouldCommit` | Whether the write would create a commit. A write with no net effect would not. |
| `outcome` | `commit`, `no-change`, `precondition-failed`, `rejected` or `storage-refused` (§2.4). |
| `head` | The head the write ran against. `Sparkles-Commit` carries the same number. |
| `commit` | The commit the write would create, as in a receipt, without `timestamp` and `digest`. When `wouldCommit` is false, the unchanged head, as in a receipt of a write with no net effect. |
| `graphs` | The graphs the write changes, each with its net `inserted` and `deleted` counts. `graph` is `null` for the default graph, else the graph's IRI or blank node label. The default graph comes first, then the named graphs by name. The sums equal the commit's counts. |
| `changes` | Only with `changes=N` above 0. `total` is the number of changed quads, `quads` holds the first `limit` of them, and `truncated` says whether some were left out. Removals come before additions. Each quad has the form of the [diff endpoint's](../API.md#diffs-between-commits) JSON quads. |
| `validation` | The summary the dataset's guard would put on the receipt, or on the `422` when it would reject. Absent when no guard runs. |
| `precondition` | Only when the request had `If-Match` or `If-None-Match`. `status` is `passed` or `failed`, with the `error` the write would get. |
| `storage` | Whether the write fits. `status` is `fits` or `refused`. A refusal has the `error` and, for the quota, `budget: "dataset-bytes"`. With a quota, `limit`, `used` and `projected` give the bytes before and after the write. An in-memory dataset with a size limit reports that limit instead. |
| `error` | When the write would be refused, the message it would get. |

A write that the guard would reject, the quota would refuse or a precondition would stop
still gets the whole preview. The real write stops at its first refusal, but a dry run
evaluates every check, so a client learns all of the reasons at once.

On the bulk path the commit is not exact when the write also removes quads, as in a
receipt ([CI §2.3](CI-commit-identity.md)). The per-graph counts then follow the same
rule. A removed graph counts all its old quads as deleted, and the counts of its new
content as inserted. The `changes` listing compares the two states and is always exact.

### 2.4 Status codes and headers

The status of a dry run is the status the write would get:

| Outcome | Status | Meaning |
|---|---|---|
| `commit`, `no-change` | `200` | The write would succeed. |
| `precondition-failed` | `412` | `If-Match` or `If-None-Match` does not hold. The body has `code: "precondition-failed"`. |
| `rejected` | `422` | The guard would reject the write. The body has `validation`, as a rejection does. |
| `storage-refused` | `507` | The quota, the disk reserve or an in-memory size limit would refuse the write. |

When more than one of these applies, the outcome is the one the write would hit first.
Preconditions come first, because the write checks them as soon as it holds the writer
lock. A write through the WAL runs the guard before it checks the storage. A bulk write
measures its new generation before the guard validates it. The body is the preview in
every case, and it carries the members a client reads from the real error (`error`,
`code`, `validation`, `budget`).

Any other failure is the failure the write would have, with the write's status and body:
`400` for a syntax error, a bad parameter or an invalid message, `403` for a missing
permission or a bypass that the server does not allow, `404` for an unknown dataset or a
missing graph on `DELETE`, `408` for a timeout, `413` and `415` for bodies, `503` for a
cancelled request or a failed WAL, `501` for a guard that is missing.

Every dry-run response carries:

* `Sparkles-Dry-Run: true`;
* `Sparkles-Commit` and `Sparkles-Dataset-Id`, naming the unchanged head;
* `Sparkles-Validation`, when a guard ran, with the summary's values.

`Sparkles-Dry-Run` and the request header are listed in CORS `Access-Control-Expose-Headers`
and `Access-Control-Allow-Headers`.

### 2.5 RDF Patch

With `Accept: application/rdf-patch`, the body is the net change as an
[RDF Patch](../API.md#diffs-between-commits) that leads from the head to the state the
write would leave:

```
H id <urn:uuid:3f1c9a2e-7b4d-4c1e-9a55-0c2b8e61d7aa#commit:43> .
H prev <urn:uuid:3f1c9a2e-7b4d-4c1e-9a55-0c2b8e61d7aa#commit:42> .
TX .
D <urn:a> <urn:p> "old" .
A <urn:a> <urn:p> "new" .
A <urn:b> <urn:p> "x" <http://example.org/g1> .
TC .
```

`id` names the commit the write would create and `prev` the head. A write with no net
effect gives a patch with an empty transaction. The patch lists every change, so
`changes` does not apply to it and is a `400` with a patch format. The checks are reported
in headers: `Sparkles-Dry-Run-Outcome` holds the outcome, `Sparkles-Validation` the
summary, and the status follows §2.4. A refused write answers with the JSON preview
instead of a patch, because a patch of a write that would not happen describes nothing.

### 2.6 MCP

`sparql_update` gains two arguments:

| Argument | Meaning |
|---|---|
| `dryRun` (boolean) | Preview the update instead of committing it. |
| `changes` (0 to 100, default 0) | List up to this many changed quads, as rendered terms. |

A dry run returns `{dataset, dryRun: true, committed: false, wouldCommit, outcome, head,
commit, inserted, deleted, graphs, changes?, validation?, storage, elapsedMs}`. `commit`
is the sequence number the commit would get, and `inserted` and `deleted` are its net
counts. A rejected or refused update is not a tool error in a dry run. The result says
`outcome: "rejected"` or `"storage-refused"`, so an agent can read the findings and
revise the update. The tool keeps its annotations, because the same tool also writes.

### 2.7 Rust library API

```rust
// sparkles::preview (new module)
pub struct DryRun {
    pub changes: usize,       // changed quads to list (0: counts only)
    pub all_changes: bool,    // list every change (for an RDF Patch)
    pub max_changes: u64,     // the most changes a full listing may hold (0: no limit)
}
pub struct Preview {
    pub dataset_id: uuid::Uuid,
    pub head: CommitInfo,
    pub commit: Option<CommitInfo>,     // None: no net change
    pub message: Option<Arc<str>>,
    pub graphs: Vec<GraphChange>,       // {graph: GraphName, inserted, deleted}
    pub changes: Vec<(DiffOp, Quad)>,
    pub changes_total: u64,
    pub validation: Option<Arc<ValidationSummary>>,  // may have status Rejected
    pub precondition: Option<Result<(), String>>,
    pub storage: StorageCheck,          // fits or refused, with the quota's numbers
}
impl Preview {
    pub fn outcome(&self) -> Outcome;   // Commit | NoChange | PreconditionFailed | Rejected | StorageRefused
    pub fn refusal(&self) -> Option<&Error>;
}
pub fn catch<T>(r: Result<T>) -> Result<Preview>;  // Err(DryRun(p)) → Ok(p)

// sparkles::guard::WriteOptions gains
pub dry_run: Option<DryRun>,
// sparkles::Error gains
DryRun(Box<Preview>),
// WriteTxn gains
pub fn preview(self, dr: DryRun) -> Result<Preview>;
```

A write whose options carry `dry_run` ends with `Err(Error::DryRun(preview))` instead of
a receipt. Every write path of the store then stops where it would have committed:
`WriteTxn::commit`, `Store::load_with`, `Store::replace_with`, `sparql::update::update_as`
and the server's handlers. Nothing that follows a commit runs, such as adding the
prefixes of a loaded file. `preview::catch` turns the result back into the preview for
callers that want an `Ok`.

## 3. Standards and precedents

* **SPARQL 1.1 Update §2.2** asks each request to be atomic: it has its whole effect or
  none. A dry run is a request with none, whose report describes the whole effect it
  would have had.
* **SPARQL 1.1 Protocol §2.2.4** leaves the body of a successful update to the
  implementation and allows content negotiation for it. §2.2.5 allows other 4XX and 5XX
  codes "as per HTTP" for failures. A preview with the write's status fits both.
* **SPARQL 1.1 Graph Store HTTP Protocol §5.3 and §5.5** define `PUT` as
  `DROP SILENT GRAPH <g>; INSERT DATA { GRAPH <g> { … } }` and `POST` as an RDF merge.
  §5.7, which is informative, describes `PATCH` with a SPARQL Update as the patch
  document. Section 6 builds on these.
* **RFC 9110.** §9.2.1 defines safe methods. A dry run is sent with an unsafe method and
  has no effect, which the method cannot express, so the parameter carries it. §13.1.1
  and §13.1.2 define `If-Match` and `If-None-Match`, evaluated as Sparkles already does
  for writes. §15.5.13 `412`, §15.5.21 `422`.
* **RFC 4918 §11.5** defines `507 Insufficient Storage`, which Sparkles already uses for
  budgets and quotas.
* **RFC 6648** advises against an `X-` prefix, hence `Sparkles-Dry-Run`.
* **Kubernetes server-side dry run.** The API server accepts `dryRun=All` on its writes.
  The request "is still processed as a typical request", including validation and
  admission, and the response "will look the same as a normal request, but the server
  will not persist" the resource. Server-computed values are returned "just as it would
  for a real request". "Authorization for dry-run requests is identical to non-dry-run
  requests." Admission plugins with side effects must not have them on dry runs. This
  spec takes the same four rules: the same pipeline, the same response shape, the same
  authorization, and hooks that see the flag.
* **RDF4J's `RepositoryConnection`.** `prepare()` "checks for an error state in the
  active transaction that would force the transaction to be rolled back", and
  `rollback()` ends a transaction without effect. RDF4J's SHACL validation reports its
  violations at `prepare()`. A Sparkles dry run is `prepare()` followed by `rollback()`
  in one request.
* **Apache Jena RDF Patch.** The patch format of §2.5 is the one `/{ds}/diff` already
  writes.

## 4. Semantics

### 4.1 The same path as the write

A dry run runs the write's own code path. It takes the same writer lock, runs in the
same transaction type, evaluates the update's WHERE clauses under the same budgets and
graph view, and calls the same guard with a candidate built the same way. The commit it
reports is assembled by the same arithmetic as the commit. The only branches are at the
end, where the store would write. That is what makes the preview equal to the receipt.

Because the dry run holds the writer lock as long as the write would, other writers wait
for it as they would wait for the write. A head that changes after the dry run makes its
preview stale. A client that wants the write to happen only if nothing changed in
between sends the write with `If-Match` and the entity tag of the head the preview names.

### 4.2 What a dry run leaves unchanged

| State | How it stays unchanged |
|---|---|
| WAL, catalog, annotations | The dry run stops before the commit record is written. No sequence number is assigned, so the next commit gets `head + 1`. |
| Delta vocabulary | A write interns the terms it inserts, and the vocabulary is append-only. The dry run records the vocabulary's length and its file's length when it begins, and truncates both back when it ends, whether it succeeded or failed. The writer lock keeps every other writer out, and no snapshot a reader holds can reach the removed ids, because only the transaction's own view did. Entries still in the file's write buffer are dropped without being written. |
| Blank-node counter | Restored to its value at the start. |
| Published snapshot, result cache | The transaction's view has its own empty result cache, and nothing is published. |
| Full-text, vector and spatial indexes | They are maintained only when a commit is published. |
| Inference status | Staleness is decided by commits, and there is none. Automatic reasoning is not triggered. |
| Write guard | The guard sees the dry-run flag on the candidate. It validates as for a write, and records no counters, no history entry, no pending state for the next commit and no baseline. The guard observer, which feeds the metrics and the logs, is not told. |
| Prefixes | A load adds the prefixes of its source after the commit. A dry run never gets there. |
| Bulk generation | Built in the next generation's directory as the write would build it, then deleted. The catalog is not synced and the full-text index is not checkpointed first, since neither is needed when no generation switches. The quota's cached measurement is dropped afterwards, because a walk during the build may have counted the candidate. |
| Commit feed, change feed | Nothing is published to them. |

A crash in the middle of a dry run leaves what an aborted write leaves: at most some
unused terms at the end of the vocabulary file, or a candidate generation directory that
the next rebuild removes.

### 4.3 Validation

The guard runs on the candidate state with the write's options, so the mode, the
threshold, the report limit, grandfather mode, incremental validation and its fallbacks
all apply as they would. A `validate=false` bypass is reported as `bypassed`, and is
allowed only where the write may bypass. The summary is the one the receipt would carry.
A rejection's summary is the one the `422` would carry.

The guard must not change its own state during a dry run. The SHACL and ShEx guards keep
pending state between a check and the commit it belongs to, along with counters and a
history of recent checks. A dry run that left pending state would be applied to the next
commit with the same sequence number, which could be a write that the guard skipped or
that bypassed it. `WriteOptions::dry_run` reaches the guard through
`Candidate::opts`, and both guards check it.

A caller limited to some graphs gets no validation results, as for a real write
([C12 §6](C12-graph-access-control.md)). The preview keeps `outcome` and the status, and
leaves out `validation`.

### 4.4 Quota and storage

The dry run evaluates the storage checks of the path the write takes. A write through
the WAL is checked against the disk reserve for its WAL bytes, against the quota when it
inserts quads, and against an in-memory dataset's size limit. A bulk write is checked
against the size limit and the quota with the generation it built. `projected` is the
size the check compared with the limit. A caller limited to some graphs gets the outcome
without the byte counts, since the size of the dataset covers every graph.

### 4.5 Preconditions

The write checks `If-Match` and `If-None-Match` on the head once it holds the writer
lock. A dry run evaluates the same check on the same head and reports it, then runs the
rest of the write. The head cannot change while the lock is held, so the answer is the
one the write would get.

### 4.6 Authorization

A dry run needs the permission of the write it previews: `write` on the dataset through
the endpoint (`update`, `gsp-rw` or `upload`), and write access to every graph the write
touches. The route and its permission do not depend on the flag. A read-only server
refuses dry runs as it refuses writes. The MCP tool needs `--mcp-allow-update` for a dry
run too.

A dry run tells the caller nothing that the write would not. It may tell it sooner, and
repeatedly, without leaving a trace in the history. The per-graph counts and the listed
changes cover only graphs the caller may write, because a write outside them is refused
before anything is looked up. The commit's dataset-wide counts are redacted for limited
callers as in their receipts.

### 4.7 Budgets

A dry run runs under the write's budgets and adds none of its own:

* **Time.** The write's `timeout`, under `--update-timeout` and `--max-timeout`. The
  timeout covers the rollback too. A dry run that times out answers `408`.
* **Rows and memory.** An update's WHERE clauses run under `memory-mb`, `max-rows` and
  `max-rows-produced` as they would. The transaction's changes are held in memory as for
  the write.
* **Listing.** `changes` caps the listed quads at 10,000. On the WAL path the listing
  reads the transaction's change log, which is already in memory. On the bulk path it
  compares the head with the candidate generation, like a diff across a bulk commit,
  under the `rows` budget (`--max-rows`). An RDF Patch lists every change under the same
  budget, and a body larger than `--max-export-mb` fails with `result-bytes`.
* **Disk.** A bulk dry run needs room for the candidate generation, under the disk
  reserve of `--min-free-disk-mb`, like the write.

Dry runs count against rate limits and concurrency limits as writes.

### 4.8 Errors

| Condition | Status |
|---|---|
| `dryRun` or `Sparkles-Dry-Run` with a value other than `true`, `false` or empty | `400` |
| `changes` not a number from 0 to 10,000 | `400` |
| `changes` with an RDF Patch format | `400` |
| everything else | as the write (§2.4) |

## 5. Design sketch

**Engine (`crates/sparkles`).**

* `preview.rs` is new. It holds `DryRun`, `Preview`, `GraphChange`, `StorageCheck`,
  `Outcome` and `catch`.
* `guard::WriteOptions` gains `dry_run`. `Error` gains `DryRun(Box<Preview>)`.
* `vocab::DeltaVocab` gains `mark()` and `rollback(mark)`. The file keeps its logical
  length. A rollback drops the bytes still in the write buffer past the mark, or
  truncates the file when the buffer already spilled past it.
* `store.rs`. `WriteTxn` records the vocabulary mark and the blank-node counter at
  `begin`, and rolls both back when a dry-run transaction is dropped. `lock_writer` skips
  the precondition for a dry run. `commit_inner` branches to `dry_run` where it would
  call `publish_log`. The storage checks of `publish_log` move into one function that
  both use. `rebuild_locked` takes the dry-run branch after the guard, where it would
  write `commit.json`. `run_guard` neither calls the observer nor tells the guard about
  a bypass on a dry run, and returns a rejection as a summary.
* `store/preview.rs` is new. It counts the net changes per graph from the transaction's
  log, or from the graph counts of the two generations on the bulk path, and lists the
  changes. On the bulk path it compares the two states with the diff module's
  `compare_states`.
* `sparkles-shacl` and `sparkles-shex`: `check` records nothing when
  `c.opts.dry_run` is set.

**Server (`crates/sparkles-server`).**

* `http/validation.rs::write_options` parses `dryRun`, `Sparkles-Dry-Run` and `changes`.
* `http/dry_run.rs` is new. It renders a `Preview` as JSON or RDF Patch with its status
  and headers, and redacts it for limited callers.
* The update, Graph Store and upload handlers catch `Error::DryRun` before they map
  errors, and answer with the preview.
* `mcp/update.rs` gains `dryRun` and `changes`.

## 6. Upsert and graph sync

Two more write verbs were considered for Graph Store users.

**Upsert** replaces the values of given subject and predicate pairs. The payload is RDF,
and for each subject and predicate in it the existing objects in the target graph are
removed before the payload is inserted. SPARQL Update expresses this directly, and
atomically:

```sparql
DELETE { GRAPH <g> { ?s ?p ?o } }
WHERE  { VALUES (?s ?p) { (<urn:a> <urn:title>) (<urn:b> <urn:title>) } GRAPH <g> { ?s ?p ?o } } ;
INSERT DATA { GRAPH <g> { <urn:a> <urn:title> "A" . <urn:b> <urn:title> "B" } }
```

A client that holds an RDF payload builds this from it in a few lines, and the two
operations commit as one. The verb is also ill-defined for blank-node subjects, which
match no existing node. GSP's own answer to partial changes is `PATCH` with a SPARQL
Update body (§5.7), which is the same thing sent to a different place. Upsert adds no
capability a Fuseki user lacks, so **Sparkles does not add it.** The usage guide shows
the update above.

**Sync a graph** makes a named graph equal to a payload and commits only the difference.
GSP `PUT` already makes the graph equal to the payload, atomically. The difference lies
in what the commit records. Sparkles already counts the net change of a `PUT` in its
receipt ([CI §2.3](CI-commit-identity.md)), so a quad that is deleted and inserted again
counts as neither. The change feed, diffs and digests see only the net change as well.
What a sync verb would add is internal. Today a `PUT` through the WAL logs a delete for
every old quad and an insert for every new one, so its WAL records, its full-text
maintenance and its incremental validation all scale with the size of the graph rather
than the size of the change. That is a cost of the implementation, not of the protocol,
and fixing it inside `PUT` helps every existing client. **Sparkles does not add a sync
verb.** Instead, `PUT` through the WAL removes only the old quads the payload does not
contain, and inserts only the quads the graph does not have. Its result, its receipt and
its validation decision stay the same.

Blank nodes in a payload are new nodes, as GSP defines them, so a quad with a blank node
always counts as changed. Matching the payload's blank nodes to stored ones would need
graph isomorphism or canonicalization of the whole graph, which no Graph Store client
expects. A bulk `PUT`, which rebuilds the index, keeps its inexact counts.

## 7. Acceptance examples

Setup: `sparkles serve --mem --allow-unvalidated-writes` with a dataset `ds` that holds
`<urn:a> <urn:p> 1` in the default graph, at head 1.

**A1. Update preview.**
`POST /ds/update?dryRun=true&changes=10` with
`DELETE DATA { <urn:a> <urn:p> 1 } ; INSERT DATA { <urn:a> <urn:p> 2 . GRAPH <urn:g> { <urn:b> <urn:p> 3 } }`
→ `200`, `Sparkles-Dry-Run: true`, `Sparkles-Commit: 1`, `outcome: "commit"`,
`commit: {seq: 2, inserted: 2, deleted: 1, quads: 2, kind: "update", bulk: false,
exact: true}`, `graphs: [{graph: null, inserted: 1, deleted: 1}, {graph: "urn:g",
inserted: 1, deleted: 0}]`, three listed changes. `GET /$/commits/ds` still has head 1.
The same update without `dryRun` then gets a receipt whose commit equals the preview's
apart from `timestamp`.

**A2. No change.** A dry run of `INSERT DATA { <urn:a> <urn:p> 1 }` → `200`,
`wouldCommit: false`, `outcome: "no-change"`, `commit.seq: 1`.

**A3. Nothing changes.** After 1,000 random dry runs, every file in the dataset directory
has the bytes it had, the head and the published snapshot are the same, the next commit
gets `head + 1`, and the full-text index's watermark has not moved.

**A4. Rejection.** With a SHACL guard in `reject` mode that requires `<urn:p>` to be an
integer, a dry run that inserts `<urn:a> <urn:p> "x"` → `422`, `outcome: "rejected"`,
`validation.status: "rejected"`, and the guard's counters and last check are unchanged.
In `warn` mode → `200` with `validation.status: "warned"`. In grandfather mode the
summary has `introduced`.

**A5. Quota.** With a quota just above the dataset's size, a dry run of a large insert
→ `507`, `outcome: "storage-refused"`, `storage.budget: "dataset-bytes"`, with `limit`,
`used` and `projected`. A dry run that only deletes → `200`.

**A6. Preconditions.** `PUT /ds/data?graph=urn:g&dryRun=true` with a stale `If-Match`
→ `412`, `outcome: "precondition-failed"`, and the body still has `commit` and `graphs`.
With the current tag → `200`.

**A7. Bulk.** With `bulk_threshold: 10`, a dry run of a `PUT` of 100 triples →
`commit.bulk: true`, the next generation's name, and no new directory once it returns.
The real `PUT` then gets the same commit.

**A8. Auth.** A caller with `read` only gets `403` for a dry run. A caller limited to
`http://ex/a/*` gets `403` for a dry run that writes `http://ex/b/1`, and a preview
without `validation` or quota bytes for one that writes `http://ex/a/1`.

**A9. RDF Patch.** `Accept: application/rdf-patch` on A1 → a patch with `H id …#commit:2`,
`H prev …#commit:1` and one `D` and two `A` rows. With `changes=1` → `400`.

**A10. MCP.** `sparql_update` with `dryRun: true` → `{dryRun: true, committed: false,
wouldCommit: true, commit: 2, …}`, and `list_commits` still shows head 1.

**A11. Bad flags.** `dryRun=ture` → `400`, and nothing is written.

**A12. PUT commits the difference.** A `PUT` of a graph's own content plus one triple
writes one WAL insert record, and its receipt says `inserted: 1, deleted: 0`.

## 8. Rejected alternatives

* **A separate preview endpoint** such as `/{ds}/update/preview`. It would fail safely on
  a server that does not know it, which a parameter does not. But it would duplicate
  every write route, its permission and its rate-limit class, and the Kubernetes
  precedent and the Graph Store's fixed paths both favour a flag. The `dryRun` member of
  the response lets a client confirm that the server understood the flag.
* **`Prefer: dry-run`** (RFC 7240). A preference may be ignored, and an ignored dry run
  is a real write.
* **Holding the transaction for a later commit**, like a prepared transaction or
  RDF4J's begin and commit over several requests. It would hold the writer lock across
  requests, so one slow client would stop every writer. A token naming the previewed
  state with a later "commit this" would need the whole change set kept on the server.
  `If-Match` with the head's tag gives the same safety: the write fails if anything
  changed since the preview.
* **Reporting through a `200` whatever the outcome.** Scripts check the status. A
  pipeline that previews a migration should fail when the migration would be rejected.
* **Stopping at the first refusal**, as the write does. The preview is for learning what
  would happen, and every check is cheap next to the write itself.
* **Previewing on the WAL path only**, by turning a bulk write into a transactional one.
  The validation strategy, the counts and the quota check would then differ from those
  of the real write, which defeats the purpose.
* **A guard trait method to discard pending state.** The guards would still count the
  check and record it in their history. A flag on the candidate lets them skip all of it.
* **Leaving interned terms in the vocabulary**, as an aborted write does. Previews may be
  frequent, for example from an editor that previews as the user types, and each would
  grow the vocabulary file.
* **Upsert and sync verbs** (§6).

## 9. Open questions

1. **Dry runs on read-only servers.** A replica could answer previews. The writer lock
   and the vocabulary rollback would work there, but the route rules refuse every write
   on a read-only server today. Default: refused.
2. **Bulk previews without a build.** A bulk dry run builds the whole generation. An
   estimate from the parsed input would be much cheaper but inexact. Default: build it.
3. **Rolling back the vocabulary of every aborted write**, not only of dry runs. The
   mechanism is the same. Default: dry runs only.
4. **A CLI flag** for `sparkles update`, `load` and `infer`. Default: not built.

## 10. Sources

* Sparkles repository (read): `README.md`, `docs/API.md`, the specs CI, C10, C12, G02,
  `crates/sparkles/src/{store.rs,guard.rs,vocab.rs,commit.rs,patch.rs,error.rs}`,
  `crates/sparkles/src/store/{quota.rs,diff.rs}`, `crates/sparkles/src/sparql/update.rs`,
  `crates/sparkles-shacl/src/guard.rs`, `crates/sparkles-shex/src/guard.rs`,
  `crates/sparkles-server/src/{http.rs,http/validation.rs,http/conditional.rs,http/diff.rs}`
  and `crates/sparkles-server/src/mcp/update.rs`.
* W3C SPARQL 1.1 Graph Store HTTP Protocol, https://www.w3.org/TR/sparql11-http-rdf-update/
  (§5.3 PUT, §5.5 POST, §5.7 PATCH), fetched.
* Kubernetes API concepts, "Dry-run", https://kubernetes.io/docs/reference/using-api/api-concepts/#dry-run,
  fetched.
* RDF4J `RepositoryConnection` Javadoc,
  https://rdf4j.org/javadoc/latest/org/eclipse/rdf4j/repository/RepositoryConnection.html
  (`prepare`, `rollback`), fetched.
* Cited from general knowledge, not fetched: SPARQL 1.1 Update and Protocol, RFC 9110,
  RFC 4918, RFC 6648, RFC 7240, Apache Jena's RDF Patch documentation, RDF4J's SHACL
  validation at `prepare()`.

## Outcome

**Delivered on 2026-10-02** in three commits after the spec. `49472ed` changed the
engine and the two guards. `b544fbb` changed the server and the MCP tool. `cd29e75` made
Graph Store `PUT` log only the difference, as §6 decided. A fourth commit updated the
documentation.

* **Engine.** `sparkles::preview` holds `DryRun`, `Preview`, `GraphChange`,
  `StorageCheck`, `Outcome` and `catch`. `WriteOptions::dry_run` turns a write into a
  dry run, which ends with `Error::DryRun`. On the WAL path the preview is assembled in
  `WriteTxn::commit` where `publish_log` would run. The storage checks and the commit's
  arithmetic moved into `check_storage` and `next_commit`, which the commit and the
  preview share. On the bulk path `rebuild_locked` takes the dry-run branch after the
  guard has run on the candidate generation, then deletes the generation and drops the
  quota's cached measurement. `WriteTxn::preview` previews a transaction directly.
* **Rollback.** `DeltaVocab::mark` and `rollback` remove the terms a dry run interned.
  The vocabulary file keeps its logical length. A rollback takes the write buffer's bytes
  out with `BufWriter::into_parts`, which does not flush them, and keeps those before the
  mark. When the buffer had already spilled past the mark, the file is truncated back to
  it. `WriteTxn` records the mark and the blank-node counter when it begins, and its new
  `Drop` restores both for a dry run, however the transaction ended.
* **Guards.** The SHACL and ShEx guards check `opts.dry_run` and then record no counters,
  no history entry, no pending state, no last full time, and, for ShEx, no association
  count or warnings. The store calls neither the guard observer nor `bypassed` for a dry
  run.
* **Server.** `http/dry_run.rs` parses the flags in `validation::write_options`, which
  every write already calls, and renders the preview. The update, Graph Store and upload
  handlers match `Error::DryRun` before they map errors. CORS exposes `Sparkles-Dry-Run`
  and `Sparkles-Dry-Run-Outcome` and allows the request header.
* **MCP.** `sparql_update` takes `dryRun` and `changes`, and its output schema lists the
  preview's members.

**Deviations from the design.**

- `Preview` has a `kind` member, and `changes_total` is `None` when no listing was asked
  for, because counting the changes of a bulk write needs the state comparison. The
  design's `refusal()` became `error()`, `rejected()`, `rejection()` and
  `receipt_commit()`, which are what the server needed. The library preview's commit
  carries the timestamp it would get at the moment of the dry run. The HTTP preview
  leaves it out.
- The status of a dry run follows the real order of the checks as designed, and on the
  bulk path the storage check comes before the guard. The preview evaluates the guard
  even when the storage check failed, so both are reported.
- MCP's `changes` without `dryRun` is a `bad-argument` error. The design did not say.
  The MCP result also has `prefixes`, for the rendered terms, and `message`.
- The access log records a dry run's `Sparkles-Validation` status, and a dry run that
  validation would reject counts as a `rejected` request in `sparkles_requests_total`.
  The `sparkles_validation_*` series count no dry run, since they come from the guard
  observer.
- A dry run's `RequestReport` counts the changed quads as its rows, as a write does.

**Tests at landing.**

- `crates/sparkles/tests/dry_run.rs` previews 600 random updates on in-memory and
  persistent stores, before and after a compaction, then makes each one. The update
  mixes `INSERT DATA`, `DELETE DATA`, `DELETE … INSERT … WHERE`, `DELETE WHERE`,
  `CLEAR`, copies between graphs, new terms and blank nodes. For each, the head, the
  published snapshot and the data are unchanged after the preview, the preview's commit
  equals the receipt apart from the time, the per-graph counts add up to the receipt's,
  and the listed changes equal the difference of the states before and after. 400 more
  run under a test guard in reject and warn mode, where the rejections must match too.
  The file also checks that 200 dry runs, one that spills the vocabulary file's buffer,
  and two bulk dry runs leave every file of the database byte for byte as it was, that a
  reopen and `sparkles check` find the database sound, that terms and blank-node ids are
  rolled back, the reporting of preconditions, rejections, bypasses and quotas, bulk
  previews of loads, replaces and a transaction with a bulk batch against their receipts,
  the bound on full listings, the full-text index, and the `PUT` change of §6.
- `crates/sparkles-shacl/tests/dry_run.rs` and a unit test of the ShEx guard run the
  same writes on two stores, one of which previews every write and the next one first.
  In strict `reject`, `warn` and grandfather `reject` mode, the summaries of the two
  stores and the previews agree at every step, and the guard's status, counters and
  history are unchanged by the previews. The writes include writes the guard skips right
  after previews, which would have applied a preview's pending state. Making the guards
  record dry runs fails both tests.
- `http/dry_run_tests.rs` previews 60 random updates and every Graph Store method and
  uploads, on the WAL path and the bulk path, and compares each with the receipt of the
  write made right after it, and the dataset directory with its state before. It also
  covers the preview document, the form, header and empty-value flags, the refusal of
  misspelt flags, RDF Patch in both forms, `412`, `422` with the guard's status
  unchanged, `507`, read-only servers and the change feed. `router_tests::auth::graphs`
  covers a reader, a write outside the caller's graphs, and the redactions of a limited
  caller. `router_tests::mcp` covers the tool's dry runs, its argument errors and a
  rejected dry run. `router_tests::reasoning` checks that a dry run leaves inferences
  fresh and starts no automatic run.
- Crate and server tests, Clippy over all targets, `mise run lint:features`, the W3C
  suites (SPARQL 1.0 482/482, 1.1 query 328/328, 1.1 update 157/157, 1.2 269/269) and
  `mise run ci` pass.

**Cost.** A dry run costs what its write costs, without the WAL fsync. Its listing adds
a sort of the transaction's log on the WAL path, and a state comparison on the bulk
path. No separate benchmark was run.

**Later.** Applying RDF Patch ([F10](F10-replication.md)) came after this spec and takes
the same flags, so a patch can be previewed like any other write.

**Not built.** The open questions of §9 keep their defaults. Dry runs are refused on
read-only servers, a bulk dry run builds its generation, only dry runs roll their
vocabulary back, and the CLI has no `--dry-run`. The UI does not offer previews.
