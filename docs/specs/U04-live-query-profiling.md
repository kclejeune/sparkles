# U04: Live query profiling

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 is a progress registry in the engine and server,
> `GET /$/running/{id}` as JSON and server-sent events, cancellation by id, and the query
> page's plan tree updating while a query runs. Phase 2 adds hot-spot marks, **Stop and
> explain**, and the server page's list of running queries. Phase 3 shows C18's verified
> rewrite suggestions in the live plan once C18 Phase 6 exists.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

This design was written from the Sparkles code and specs: the executed plan with node
ids that `/{ds}/sparql` returns in `meta.plan` and the query page draws with `PlanView`,
the explanation of plans and the rewrite suggestions of
[C18 §6.6 and §6.7](C18-natural-language-questions-and-ingest.md#66-explaining-a-query),
request ids, budgets and cancellation ([C01](C01-observability-and-budgets.md)),
streamed execution ([X05](X05-streaming-query-execution.md)), graph access control
([C12](C12-graph-access-control.md)) and the library's `Control`
([P06](P06-library-admin-api.md)). It also used the WHATWG HTML specification of
server-sent events, PostgreSQL's documentation of `EXPLAIN ANALYZE` and of
`pg_stat_progress_*` views, and Microsoft SQL Server's documentation of Live Query
Statistics, as public descriptions of per-operator progress.

## 1. Summary, goals, non-goals

Today a query's plan with actual row counts and times arrives only when the query ends,
in `meta.plan`, or in the error body when a budget stops it. A person whose query runs for
30 seconds sees a spinner and learns afterwards which operator was slow. The engine
already counts rows and time per operator for that final plan, so the information exists
while the query runs, but nothing exposes it.

This spec exposes it. A query that asks for progress registers its plan under its
request id, its operators publish row counts as they go, and a server-sent event stream
reports them four times a second. The query page draws the plan tree as it fills in,
marks the operators that are running, those whose rows have passed their estimate, and
those that take the time, and lets the person stop the query and explain what it did.
When C18 Phase 6 exists, the stopped or finished plan leads straight to verified
rewrites.

**Goals**

1. Live row counts and states per operator, with the same node ids as the final plan, so
   the tree the person watches becomes the final tree without reshaping.
2. No cost when progress is not asked for, and at most 1% median slowdown on the
   benchmark queries when it is.
3. The stream reveals nothing the final plan would not: the same redactions for limited
   views (C12) and the same permission.
4. Cancellation by id from another connection, for the person who started the query and
   for server administrators.
5. A path from a slow live plan to an explanation and to verified rewrites.

**Non-goals**

- Progress of updates, uploads or tasks. Tasks already report `progress` in `/$/tasks`.
- A completion percentage for the whole query. Join sizes are unknown in advance, so the
  page shows what has happened, not a prediction. Scans report the fraction of their
  range read, which is known.
- Sampling profilers or CPU flame graphs of the server. The flame view stays the
  operator-time view `PlanView` already draws.
- Changing plans while they run.

## 2. User-visible behaviour

### 2.1 The live plan

While a query runs, the query page's result area shows the **Plan** tab instead of a
spinner after one second, unless the person chose another tab.

```
┌─ Query › Plan · running 12.4 s ───────────────────── [Stop]  [Stop and explain] ┐
│ [Tree] Flame                                     rows sent 0 · memory 310 MiB    │
├──────────────────────────────────────────────────────────────────────────────────┤
│ Operator                         est      rows so far     time     state         │
│ ▾ Project ?name                    9                0       —      waiting       │
│   ▾ Filter regex(?name,"^Ana")   900               14    11.9 s ▶ running  ●     │
│     ▾ HashJoin ?team             900     1,204,331 ⚠      0.4 s   done           │
│         Scan ex:memberOf         214          214         1 ms    done           │
│         Path ex:partOf+            9        4,100 ⚠      0.3 s   done           │
│ ● takes 96% of the time so far   ⚠ more than 10× its estimate                     │
└──────────────────────────────────────────────────────────────────────────────────┘
```

- **States.** Each operator is `waiting`, `running`, `done` or `skipped`. Running
  operators carry the text "running" and a mark, which pulses only without reduced
  motion.
- **Rows so far.** Counts update at most four times a second. A scan whose range is
  known shows a thin bar of the fraction read.
- **Time.** A finished operator shows its own time as in the final plan. A running one
  shows the time since it started.
- **Hot spots (Phase 2).** `●` marks the operator with the largest share of time so far
  when that share is over 50%. `⚠` marks an operator whose rows have passed ten times
  its estimate, as C18's notes do for the final plan. The legend explains both.
- **Flame.** The flame view redraws with the same data.
- **End.** When the query ends, the final `meta.plan` replaces the live tree, which then
  matches what the Plan tab shows today. Results appear in their tab as before.

### 2.2 Stopping

**Stop** cancels the query by id (§3.3). The live tree keeps its last counts, marked
partial with the `ᵖ` mark `PlanView` already uses for plans that a budget stopped.
**Stop and explain** (Phase 2) cancels and opens C18's explanation panel with
`profile: "given"` and the partial plan, so the notes explain what was slow. With C18
Phase 6 built, the explanation ends with **Optimize ▸** as C18 §6.6 describes (Phase 3).

### 2.3 Rewrites in the live plan (Phase 3)

When the dataset enables `optimize` and the query has run for more than five seconds,
**Find a faster query** appears in the header. It cancels the query and opens C18's
optimize panel, which runs the original and its candidates under the caller's budgets
and shows only verified rewrites. A deterministic rule of C18 §6.7.2 whose syntactic
condition matches an operator marked `●`, such as `regex-prefix` on a regex filter, is
named next to that operator as "a rewrite may help: regex-prefix", without a claim that
it is faster until verification.

### 2.4 Running queries on the server page (Phase 2)

The server page gets a **Running queries** card: the caller's running queries, or all of
them for a server administrator, with the dataset, the principal, the time running, the
rows sent and the memory estimate. A row opens the same live plan in a dialog and has
**Stop**. Query text is shown to its principal and to server administrators only.

### 2.5 Errors and edge cases

| Case | What the page shows |
|---|---|
| A server without progress support | The spinner, as today. The page detects it from the `404` of the stream and does not retry. |
| A query that ends in under a second | No live view. The final plan arrives with the results. |
| The stream drops | The page reconnects with the same id. If the query has ended, the final plan comes with the results. |
| A limited view (C12) | Estimates are hidden in the stream as they are in the final plan, and the `⚠` mark is not shown. |
| A streamed (cursor) query | The tree is the `CursorPlan`'s, with the same ids, and `rows sent` counts rows written to the response. |

### 2.6 Accessibility

The tree is the existing `PlanView` tree with its roles. Counts are not announced on
every update. A polite live region announces state changes of the operator marked `●`,
and the end of the query. Marks are text and symbols as well as colour.

## 3. Server and API changes

### 3.1 Asking for progress

A client asks for progress by sending `Sparkles-Progress: true` with
`/{ds}/sparql`, `/{ds}/query` or a stored query run, and an `X-Request-Id` it chose. The
server already keeps an incoming `X-Request-Id` of 1–128 characters from
`[A-Za-z0-9._:-]`. A request with `Sparkles-Progress` and no valid id gets one assigned,
returned in the response headers as today, but a client that wants the stream sets its
own so it can subscribe before the response starts. The query runs exactly as without
the header.

### 3.2 `GET /$/running/{id}`

| Accept | Answer |
|---|---|
| `application/json` | A snapshot: `{id, dataset, branch, principal, startedAt, elapsedMs, state, plan, nodes, rowsSent, memoryBytes}`. |
| `text/event-stream` | A `plan` event with the planned tree, its node ids and estimates, then `progress` events every 250 ms with the nodes that changed since the last event, then one `end` event with `outcome` (`ok`, `cancelled`, `timeout`, `budget`, `error`) and the final plan. |

```ts
type NodeProgress = {
  id: string;                       // the path id, "0.1.2", as in meta.plan
  state: "waiting" | "running" | "done" | "skipped";
  rows: number;                     // rows produced so far, over all runs
  runs: number;
  startedMs?: number;               // since the query started, for running nodes
  selfMs?: number;                  // own time, for done nodes
  fraction?: number;                // 0..1, scans over a known range
};
```

The id is matched together with the caller's principal, so another caller's id answers
`404`, as an unknown id does. A server administrator can read any id. A stream opened
before the query registers waits up to five seconds for it. The stream ends with the
query, and a query ended less than 60 seconds ago still answers its `end` event once, so
a late subscriber gets the final state. Opening a stream needs `read` on the dataset and
counts against no rate-limit class beyond the connection limit.

### 3.3 `DELETE /$/running/{id}` and `GET /$/running`

`DELETE` cancels the query through the same path as a client disconnect (C01), answers
`202`, and the query's own response ends as a cancelled request does today. The caller
must be the query's principal or a server administrator. `GET /$/running?dataset=` lists
the running queries the caller may see, without text unless the caller is the principal
or a server administrator. The access log and the metric `sparkles_requests_total`
record a cancellation by id with `outcome=cancelled` like a disconnect.

### 3.4 Engine changes

- **A progress sink.** `sparql::Ctx` gains `progress: Option<Arc<Progress>>`. `Progress`
  holds one `NodeCounters { rows: AtomicU64, runs: AtomicU32, state: AtomicU8,
  started_ns: AtomicU64, self_ns: AtomicU64, scanned: AtomicU64, range: AtomicU64 }`
  per plan node, indexed densely, with a table from the dense index to the path id. The
  planner assigns the indexes when it builds the tree, so the `plan` event can be sent
  before execution starts.
- **Counting while running.** Today the executor fills `actual_rows` when an operator
  finishes (`exec.rs`). Scans, joins, filters and paths add their output to their counter
  in batches, at the points where they already check the deadline and cancellation, and
  at least every 1,024 rows. Cursor operators of X05 add each batch they emit. Updates
  are `Relaxed` atomic adds. When `progress` is `None` the code path is one branch per
  batch.
- **Final plan.** The final `meta.plan` is built as today. The progress sink does not
  replace it, so the final numbers stay exact.
- **Registry.** The server keeps `RunningQueries`, a map from `(principal, id)` to the
  sink, the cancel handle, the dataset and the start time, cleaned 60 seconds after the
  end. Its size is bounded by the server's query concurrency limit.

All routes are added to `docs/API.md`, the route table and the OpenAPI description.

## 4. Design notes

### 4.1 Why server-sent events and a second request

The query response carries results, and its format is fixed by the SPARQL protocol or
the UI's JSON. Interleaving progress into it would break both. A separate stream, keyed
by the request id the client chose, leaves the query untouched and works for any result
format. The UI already reads server-sent events for Ask and Explain (`SseParser` in
`lib/ask-api.ts`).

### 4.2 Overhead

The overhead is measured on the BSBM explore mix and the LUBM queries of BENCHMARKS.md
with progress on and off, at the dataset sizes those runs use. The goal of §1 holds when
the median slowdown is at most 1% and no query is more than 3% slower. If a hot loop
misses the goal, its batch grows, not the reporting interval.

### 4.3 Redaction

The stream applies the same redaction function as the final plan: for a caller whose
view is limited (C12), estimates and counts of operators over hidden graphs are left
out. `fraction` is left out too in that case, because a fraction of an index range can
reveal the size of hidden data.

## 5. Architecture

| Part | Where |
|---|---|
| Progress sink, counters | `sparkles-core/src/sparql/ctx.rs`, `exec.rs`, `cursor/`, `plan.rs` (dense indexes) |
| Library | `sparkles` crate: `QueryOptions::progress`, returning the sink with the cursor, so P06 bindings can read it |
| Registry, routes | `sparkles-server` `http` (new `running` module), `obs` for the log fields |
| Live tree | `ui/src/lib/components/PlanView.svelte` gains `live: Map<string, NodeProgress>` |
| Stream client | `ui/src/lib/progress.ts` (new), reusing `SseParser` |
| Query page wiring | `routes/query/+page.svelte`: sets `X-Request-Id` and `Sparkles-Progress` in `api.query` |
| Running queries card | `routes/server/+page.svelte` |
| Explanation | the existing `ExplainPanel.svelte` and `explainStream` with `profile: "given"` |

## 6. Phases

1. **Phase 1.** The progress sink in the eager and cursor executors, the registry,
   `GET /$/running/{id}` and `DELETE`, the live tree on the query page, and the overhead
   measurement.
2. **Phase 2.** Hot-spot marks, **Stop and explain**, `GET /$/running` and the server
   page's card.
3. **Phase 3.** **Find a faster query** and rule hints, after C18 Phase 6.

## 7. Acceptance examples

The examples use a dataset `big` with 2 million `ex:memberOf` triples and a name per
subject. Operators are made slow and deterministic with `afn:wait(ms)`, which Sparkles
already implements and which stops early when the query is cancelled.

- **A1. Live counts.** Running a query whose filter calls the slow function shows the
  Plan tab after one second, with the scan `done`, the filter `running` and its row count
  increasing at least twice before the query ends.
- **A2. Same tree.** When A1's query ends, every node id of the live tree is in
  `meta.plan`, and the final row counts equal those the query reports without
  `Sparkles-Progress`.
- **A3. Stop.** Pressing **Stop** during A1 ends the query's response as cancelled within
  one second, the access log has `outcome=cancelled`, and the tree keeps its counts with
  the partial mark.
- **A4. Other callers.** User `bob` requesting `GET /$/running/{id}` for `ana`'s query
  gets `404`. A server administrator gets the snapshot.
- **A5. Limited view.** For a caller whose grants exclude a named graph, the stream of a
  query over the union has no `fraction` and no estimate on the scans of that graph.
- **A6. No overhead when off.** The benchmark run of §4.2 shows no measurable change with
  progress off against the commit before the change.
- **A7. Hot spot (Phase 2).** In A1, the filter is marked `●` with the text "takes N% of
  the time so far". **Stop and explain** opens the explanation panel, which names the
  filter as the slow operator.
- **A8. Server page (Phase 2).** While A1 runs, the server page's Running queries card
  lists it, and **Stop** there cancels it.

## 8. Testing

- **Rust.** Counters for each operator kind, with progress on and off, against the final
  plan's counts. Cancellation by id. The registry's cleanup and its bound. Redaction for
  limited views. A stream subscriber that connects before the query registers.
- **Vitest.** `lib/progress.ts` merging deltas into the tree state, the hot-spot rules,
  reconnection.
- **Playwright.** A1–A4 and A7–A8 against a real server, with `afn:wait` making the
  queries slow. The mock server serves a scripted stream for layout tests.
- **Benchmarks.** A6 and §4.2 in the benchmark harness, recorded in BENCHMARKS.md.

## 9. Rejected alternatives

- **Progress inside the query response**, as trailing headers or a multipart body. It
  breaks standard SPARQL result formats and clients.
- **WebSockets.** The UI and the server already use server-sent events for one-way
  streams, which work through the same proxies and auth as other requests.
- **Polling `/$/running/{id}` as JSON from the UI.** It works and stays available, but a
  stream sends only changes and ends with the final plan without another request.
- **Sampling the executor's stack.** It shows functions, not plan operators, and needs
  platform-specific code.
- **A server-assigned id returned before execution.** The client would need a second
  round trip before the query starts. The client-chosen request id already exists.

## 10. Open questions

1. Whether MCP's progress notifications (roadmap item 42) should read the same sink, so
   `sparql_query` reports progress to agents.
2. Whether a `fraction` for hash-join probes, measured against the build side's size, is
   useful or misleading.
3. Whether the Running queries card should show other principals' queries to dataset
   administrators, not only server administrators.

## Outcome

Not built. See [ROADMAP](../ROADMAP.md) item 27.
