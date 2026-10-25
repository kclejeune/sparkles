# C23: Batched updates and commit triggers

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 is batched updates that commit every N solutions, as
> tasks with progress, cancellation and resumption, in HTTP, the CLI and the library.
> Phase 2 is commit triggers that run a SPARQL Update after a matching commit, with
> recursion limits, ordering, validation and a run log. Phase 3 adds triggers inside the
> commit, webhook actions through C21, and matching on F13's incremental core.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

This design was written from the public APOC documentation (`apoc.periodic.iterate`,
`apoc.periodic.commit` and the `apoc.trigger` procedures with their `before`, `after`
and `afterAsync` phases), the Neo4j Cypher manual's `CALL { … } IN TRANSACTIONS OF n
ROWS`, the PostgreSQL documentation of `CREATE TRIGGER` and of trigger recursion, the W3C
SPARQL 1.1 Update recommendation, and the Sparkles code. It builds on server tasks and
the `task::Control` of [P06](P06-library-admin-api.md), write previews
([C15](C15-write-previews.md)), write-time validation ([C10](C10-write-time-validation.md)),
the change feed of [F06](F06-snapshots-and-point-in-time.md), outbound notifications
([C21](C21-outbound-notifications.md)), access control ([C09](C09-dataset-access-control.md),
[C12](C12-graph-access-control.md)) and, in Phase 3, the incremental core of
[F13](F13-incremental-views-and-provenance.md).

## 1. Summary, goals, non-goals

A SPARQL Update runs in one write transaction (`sparql/update.rs`). An update that
touches fifty million quads holds the writer for its whole run, keeps every change in
memory until it commits, and loses everything if it fails or is cancelled near the end.
Neo4j users split such work with `apoc.periodic.iterate` or `CALL … IN TRANSACTIONS`.
Sparkles has no equivalent, so users write scripts that page through a SELECT and post
updates.

Sparkles also has no way to react to a commit inside the server, other than the fixed
reactions it ships: automatic re-materialization (C08), embeddings on write (F08) and the
notifications of C21. A rule such as "when an order is marked shipped, set its shipped
date", or "post to this webhook when a person is deleted", needs an external program on
the change feed.

**Goals**

1. Batched updates: a SELECT whose solutions are fed, N at a time, to an update, with a
   commit per batch, progress, cancellation, resumption after a failure or restart, and
   the same guards, quotas and access checks as an ordinary update.
2. A repeating form that runs an update until it changes nothing, as
   `apoc.periodic.commit` does.
3. Commit triggers: a named rule per dataset that matches each commit's changes against
   a pattern and runs a SPARQL Update, or sends a webhook, with the matched solutions.
4. Bounded recursion, a defined order, validation at definition, and a run log for audit.

**Non-goals**

- Parallel batches. A dataset has one writer, so batches commit in order. Group commit
  ([X09](X09-group-commit-by-default.md)) makes their commits cheaper.
- Atomicity across batches. A batched update is a sequence of commits, and readers see
  the states in between.
- Triggers that run arbitrary code. Actions are SPARQL Updates or C21 deliveries.
- Triggers on reads.

## 2. Batched updates

### 2.1 Request

```
POST /$/batch/{ds}
{
  "iterate": "SELECT ?o WHERE { ?o a ex:Order ; ex:status ex:shipped . FILTER NOT EXISTS { ?o ex:shippedOn ?d } }",
  "action": "INSERT { ?o ex:shippedOn ?today } WHERE { BIND (xsd:date(NOW()) AS ?today) }",
  "batchSize": 10000,
  "onError": "stop",
  "message": "backfill shipped dates"
}
```

| Field | Meaning |
|---|---|
| `iterate` | A SELECT query. It runs once, at the commit current when the job starts. |
| `action` | A SPARQL Update. For each batch, the batch's solutions are joined into the `WHERE` clause of each operation as a `VALUES` block over the variables the operation mentions. `INSERT DATA` and `DELETE DATA` are refused, because they cannot use the solutions. |
| `batchSize` | Solutions per batch, 1 to 1,000,000. Default 10,000. |
| `onError` | `stop` (default) ends the job at the first failed batch. `skip` records the batch's first and last solution and continues. |
| `message` | The commit message prefix. Each commit's message adds `batch k of job J`. |
| `dryRun` | Runs the first batch as a C15 dry run and reports its receipt and changes, with the iterate query's solution count when it is at most `--max-rows`. |

The answer is `202` with a task id in `/$/tasks`, whose progress reports the solutions
read, the batches committed, the quads inserted and deleted and, when known, the total.
`DELETE /$/tasks/{id}` cancels between batches. Batches committed stay.

### 2.2 The repeating form

```
POST /$/batch/{ds}
{ "repeat": "DELETE { ?s ex:tmp ?o } WHERE { SELECT ?s ?o { ?s ex:tmp ?o } LIMIT 10000 }",
  "maxRounds": 100000 }
```

The update runs in its own transaction until a round changes nothing or `maxRounds` is
reached. The update must bound itself with a `LIMIT`. A round that changes nothing ends
the job, and a round that changes the same quads as the previous one, by its change
digest, ends it with an error, because it would loop.

### 2.3 Iteration, snapshot and resumption

The iterate query runs at the start commit with a cursor of X05, so its solutions stream
without being held in memory. The job pins the start commit as a named snapshot of F06,
`batch-{job}`, so that the commit stays readable for the job's lifetime, and unpins it
at the end.

The batches see the dataset as it is when each batch runs. A solution whose quads an
earlier batch changed is still applied. The action should therefore be written to be
idempotent, as with `apoc.periodic.iterate`, and the documentation says so with
examples.

A job's state is kept in `<root>/batch/{job}.json`: the requests, the start commit, the
number of solutions consumed, the last committed batch and its commit. On a restart, or
with `POST /$/batch/{ds}/{job}/resume` after a stop, the job runs the iterate query again
at the pinned commit and skips the solutions already consumed. That needs the same
solutions in the same order. The job adds `ORDER BY` over the projected variables when
the query has none, which costs a sort, unless the request says `"ordered": false`, in
which case the job records the plan's fingerprint and resumes only if the plan is the
same at the pinned commit. A job whose pinned commit is gone fails to resume with
`410`.

### 2.4 Checks per batch

Each batch is an ordinary update of the caller: the write guard of C10 validates it, the
caller's grants of C12 limit the graphs it writes, and quotas apply. The commit kind is a
new `Batch` code of `CommitKind`, and the receipt of each batch is in the job's log. A
read-only dataset or server refuses the job.

### 2.5 CLI and library

`sparkles update --loc DB --iterate select.rq --batch-size 10000 action.ru` runs a job
locally and prints progress, and `--url` starts it on a server. `sparkles batch
list|status|resume|cancel` manages jobs. The library has
`Dataset::batch_update(&BatchUpdate, &Control) -> BatchReport` and `Dataset::batch_resume`.
The bindings expose them through P06's mapping, with the binding's cancel token.

## 3. Commit triggers

### 3.1 Definition

```
PUT /$/triggers/{ds}/{name}
{
  "when": "GRAPH trig:added { ?o ex:status ex:shipped } FILTER NOT EXISTS { ?o ex:shippedOn ?d }",
  "action": { "update": "INSERT { ?o ex:shippedOn ?t } WHERE { BIND (xsd:date(NOW()) AS ?t) }" },
  "phase": "after",
  "principal": "automation",
  "priority": 10,
  "branches": ["main"],
  "maxSolutions": 10000,
  "enabled": true
}
```

**The pattern.** `when` is a SPARQL group graph pattern evaluated per commit. Two
reserved graphs, `PREFIX trig: <urn:x-sparkles:trigger#>`, hold the commit's net
changes: `GRAPH trig:added { … }` matches the quads it added and `GRAPH trig:removed { … }`
the quads it removed, each with the quad's graph as `trig:graph ?g` when a pattern needs
it. Every other pattern reads the state after the commit, as the trigger's principal
sees it. A pattern must mention at least one of the two reserved graphs. The commit's
number, author and kind are bound with `trig:commit`, `trig:author` and `trig:kind`
inside a `GRAPH trig:commit` block.

**The action.** An `update`, run with the pattern's solutions joined as a `VALUES`
block as in §2.1. Or, in Phase 3, a `notify` naming C21 channels, which delivers a
`trigger.fired` event with the commit, the trigger's name and the solutions, at most
`maxSolutions`, as JSON.

**Phases.** `after`, the default, runs the action in its own commit after the source
commit is published. `commit`, in Phase 3, runs it inside the source write transaction,
after the writer's operations and before the C10 guard, so that the source commit and
the action's changes are one commit, which is APOC's `before` phase. An error in a
`commit` trigger fails the write with `409 trigger-failed` and the trigger's message.

Defining, changing and deleting triggers needs the dataset's `admin` grant. The
definitions are stored in `<root>/triggers.json` and listed by `GET /$/triggers/{ds}`,
with each trigger's state and last runs.

### 3.2 Validation at definition

A `PUT` parses the pattern and the action, checks that the pattern mentions `trig:added`
or `trig:removed`, that the action is an update and not a query, that the trigger's
principal exists and has `write` on the graphs the action names and `read` on the
dataset, and that the action names no graph the principal cannot write when the graph
is a constant. It then evaluates the pattern against the head commit's changes and, with
`?dryRun=true`, runs the action as a C15 dry run and returns the receipt, so the author
sees what the trigger would have done. Non-deterministic functions are allowed in
actions, since each firing is a new commit.

### 3.3 Firing and ordering

A dataset with triggers has one trigger worker. It follows the commits with
`Store::subscribe_commits` and `Store::changes`, in commit order, filtered to the
trigger principal's graphs. For each commit, a footprint check skips triggers whose
reserved-graph patterns' predicates and graphs do not occur in the changes, as F13 §3.3
does for views. The remaining triggers run in order of `priority`, then name. Each
trigger's action is its own commit, so a later trigger sees the earlier one's changes
in the state it reads, but matches only the source commit's changes.

A commit that the feed lists with counts only, such as a large bulk load, has no change
list. A trigger then fires with no solutions and records `skipped: changes-unknown` in
its log, unless it sets `"onUnknownChanges": "fail"`, which disables it until an
administrator re-enables it.

The worker records the last commit it processed per trigger in `triggers.json`, so a
restart continues from there. Commits that the change feed can no longer list are
recorded as skipped. Triggers run only on primaries, never on read replicas, and only on
the branches their `branches` setting names.

### 3.4 Recursion

A trigger's commit is a commit like any other, so it can fire triggers. Each commit made
by a trigger carries a depth, the depth of its source commit plus one, recorded in a new field of the
commit's annotation. A trigger does not fire on a commit whose depth reaches
`maxDepth` (3 by default, at most 16, a dataset setting), and the log records the
skipped firing. A trigger never fires on its own commits unless it sets
`"selfFire": true`, and then still within `maxDepth`. `commit`-phase triggers repeat
inside the transaction, each round matching only the changes of the previous round, up
to `maxDepth` rounds. The limit follows PostgreSQL's advice that recursive triggers need
an explicit end, and APOC's lack of one is a known way to loop.

### 3.5 Failures

An action that fails is retried twice with a backoff of 1 and 10 seconds. If it still
fails, the trigger becomes `failed`, stops firing, and raises the C21 event
`trigger.failed` with the trigger, the commit and the error. Commits after it are not
processed for that trigger until an administrator resumes it, which processes them in
order, or skips to the head with `"skipTo": "head"`. A failed trigger does not hold up
the others.

### 3.6 Audit

Every firing is a record in `<root>/triggers/{name}.log`, with the source commit, the
solution count, the resulting commit or the error, the depth and the time, kept for
`triggerLogDays` (30 by default). `GET /$/triggers/{ds}/{name}/runs` pages through it.
The commits a trigger makes have the author `urn:x-sparkles:principal:trigger:{name}`, the
new `CommitKind` code `Trigger`, and a message that names the source commit, so
`sparkles log` and the change feed show where each change came from.

### 3.7 Matching on the incremental core

In Phase 2, the worker evaluates each pattern on a small in-memory dataset of the
commit's changes, joined with the snapshot for the other patterns. In Phase 3, a trigger
is compiled to an F13 circuit whose output is the pattern's change, so a pattern that
reads the state is evaluated incrementally, and a trigger fires on the solutions that
appear (`"on": "added"`, the default) or disappear (`"on": "removed"`) from the pattern
over the whole state, not only over the reserved graphs. That gives triggers on derived
conditions, such as "when a team's member count passes 10".

## 4. Architecture

- `crates/sparkles-core/src/batch.rs`: the job, its state file, the iteration with
  `QueryCursor`, the `VALUES` injection into each operation's algebra before planning
  (the parsed `spargebra::Update` is rewritten, not the text), and resumption. The
  library's `Dataset::batch_update` wraps it in `crates/sparkles/src/dataset.rs`.
- `crates/sparkles-core/src/commit.rs`: `CommitKind` gains `Batch` and `Trigger`, with new
  codes that older binaries read as unknown kinds.
- `crates/sparkles-server/src/triggers/`: `config.rs` (`triggers.json` and validation),
  `worker.rs` (the per-dataset worker on the change feed), `log.rs` and `http.rs`. The
  `commit`-phase hook is called from the write path beside the guard of
  `crates/sparkles/src/write_guard.rs`.
- `crates/sparkles-server/src/notify/events.rs`: the events `trigger.fired` and
  `trigger.failed`.
- `crates/sparkles-server/src/http/batch.rs` for `/$/batch/{ds}`, and the routes in
  `auth/routes.rs` and the OpenAPI description.
- MCP: a `batch_update` tool, available only when the update tool is enabled for the
  session, which starts a job with a dry run first and reports through the tasks
  extension. Triggers are not exposed through MCP.

## 5. Phases

**Phase 1.** Batched updates and the repeating form, jobs as tasks with progress,
cancellation and resumption, the dry run, `sparkles update --iterate` and `sparkles
batch`, the library API and the bindings.

**Phase 2.** `after` triggers with the reserved graphs, validation and dry runs,
ordering, recursion limits, retries and failure states, the run log and the C21 event
`trigger.failed`.

**Phase 3.** `commit`-phase triggers, webhook actions through `trigger.fired`, and
matching on F13.

## 6. Acceptance examples

- **A1.** A batched update over 25,000 solutions with `batchSize` 10,000 makes three
  commits of kind `Batch`, and the result equals the single update.
- **A2.** Cancelling after the first batch leaves one commit, and `resume` makes the other
  two with the same final state as A1.
- **A3.** Killing the server during the second batch and restarting resumes the job from
  the second batch.
- **A4.** A batch that the C10 guard rejects with `onError: "stop"` ends the job with
  the guard's report, and earlier batches remain.
- **A5.** The repeating form deletes 25,000 `ex:tmp` triples with `LIMIT 10000` in three
  rounds and stops after a fourth round that changes nothing.
- **A6.** A trigger on `GRAPH trig:added { ?o ex:status ex:shipped }` that inserts
  `?o ex:shippedOn ?t` fires once for a commit that ships two orders and makes one
  commit of kind `Trigger` with two new triples.
- **A7.** A trigger whose action re-adds the triple that fires it, with `selfFire: true`
  and `maxDepth` 3, fires three times and logs a skipped firing at depth 3.
- **A8.** Two triggers with priorities 1 and 2 on the same commit run in that order, and
  the second reads the first one's changes.
- **A9.** A trigger whose principal lacks `write` on the action's graph is refused at
  definition.
- **A10.** A trigger whose action fails three times becomes `failed`, raises
  `trigger.failed`, and after `resume` processes the commits it missed in order.

## 7. Testing

Batched updates are tested against the single update on random data and random batch
sizes, with cancellations and restarts injected at every batch boundary. Trigger tests
cover the reserved graphs, ordering, recursion, restarts with unprocessed commits, a
bulk commit with unknown changes, branches and replicas. A benchmark in [X06](X06-benchmark-coverage.md) measures a
50-million-quad update as one transaction and in batches of 10,000, 100,000 and
1,000,000, with and without group commit, reporting time, peak memory and the writer's
longest hold.

## 8. Rejected alternatives

- **A SPARQL grammar extension**, such as an `IN TRANSACTIONS OF n` clause. It would need
  changes to the vendored parser and would not parse anywhere else. A job wraps two
  standard texts.
- **Batches by `LIMIT` and `OFFSET` on the iterate query.** Each batch would re-run the
  query on a changed state, so solutions shift between pages.
- **Triggers as stored updates run by the change feed's clients.** That is the status
  quo, and it leaves ordering, recursion and audit to every client.
- **Unbounded recursion with a cycle detector.** A content-based cycle check misses
  cycles whose terms change, such as counters. A depth limit is simple and predictable.
- **Triggers that veto writes.** C10's guards already reject writes on shapes, so
  triggers only add changes.

## 9. Open questions

1. **The default for `ordered`.** Sorting the iterate query's solutions makes resumption
   exact but costs memory until [X08](X08-disk-spill-and-server-memory.md)'s spill exists. The default chosen is to sort.
2. **Trigger actions across datasets.** An action could write to another dataset of the
   same server. The default chosen is the same dataset only.
3. **Triggers on branches created later.** A glob in `branches` covers branches created
   after the trigger. Whether a new branch's existing commits fire is undecided; the
   default chosen is that only commits after the branch's creation fire.

## Outcome

Not built. See [ROADMAP](../ROADMAP.md) item 36.


### Shared durable execution

[C26](C26-durable-jobs-and-delivery.md) is the planned durable execution substrate for
roadmap item 36. C23 continues to define batch semantics, trigger matching and bounded
recursion. Its resumable batches and deferred trigger actions use C26's job identities,
checkpoints, publication fences and outbox rather than a second independent scheduler.
Inside-commit triggers remain a separate C23 phase and may enqueue a durable job, but
never perform an external request while holding the writer. This integration is not
implemented and does not make current server tasks restart-durable.
