# U04: Live query profiling

> **Status:** partly implemented
>
> **Phases:** Phase 4 shipped on 2026-10-23, and Phases 1 to 3 are not built. Phase 1 is a
> progress registry in the engine and server,
> `GET /$/running/{id}` as JSON and server-sent events, cancellation by id, and the query
> page's plan tree updating while a query runs. Phase 2 adds hot-spot marks, **Stop and
> explain**, and the server page's list of running queries. Phase 3 shows C18's verified
> rewrite suggestions in the live plan once C18 Phase 6 exists. Phase 4, added on
> 2026-10-23 in §11, draws estimated and actual rows on the query's pattern graph from
> [U08](U08-visual-query-builder.md) and marks misestimates.
>
> **User docs:** [Usage: The plan on the pattern](../USAGE.md#the-plan-on-the-pattern) ·
> [API: Explain](../API.md#explain) · [Features](../visualization/README.md#web-ui)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how Phase 4 landed.

This design was written from the Sparkles code and specs: the executed plan with node
ids that `/{ds}/sparql` returns in `meta.plan` and the query page draws with `PlanView`,
the explanation of plans and the rewrite suggestions of
[C18 §6.6 and §6.7](C18-natural-language-questions-and-ingest/SPEC.md#66-explaining-a-query),
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
4. **Phase 4.** Structured pattern references in plans, then the plan drawn on U08's
   pattern graph (§11). It needs U08 Phase 1, and its live counts need Phase 1 here.

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

## 11. Phase 4: the plan drawn on the query's pattern (2026-10-23)

This section was added on 2026-10-23, when the user approved a set of graph-interaction
features. It extends the plan views of this spec to the pattern graph of
[U08](U08-visual-query-builder.md). It was written from the same sources as the rest of
this spec, together with U08 and the plan JSON of `/{ds}/explain`,
`POST /{ds}/sparql/explain` and `meta.plan`.

### 11.1 Goals and non-goals

The plan tree says which operator was slow, but a reader thinks of a query as its
pattern, and the tree's operators are hard to match with the triples they read. Phase 4
draws the plan's numbers on U08's pattern graph, so a person sees at once which edge the
engine expected to match a hundred rows and which matched a million.

**Goals**

1. Every triple pattern and filter of the pattern graph shows the estimated rows of the
   operator that reads it, and its actual rows when the query ran.
2. A misestimate is marked on the edge, with the same rule the plan tree uses.
3. Joins are shown where they happen, on the variable they join on, in the order the
   engine ran them.
4. While a query runs with progress, the counts on the pattern update from the stream of
   Phases 1 and 2.
5. Nothing appears on the pattern that the plan tree would not show the same caller.

**Non-goals**

- Changing the plan from the pattern, such as forcing a join order. Sparkles has no plan
  hints.
- Drawing operators that read no part of the pattern, such as `Project` or `Limit`. They
  stay in the tree.
- Drawing a plan over text that differs from the query that produced it. A stale overlay
  is hidden, not guessed.

### 11.2 User-visible behaviour

The Plan tab and U08's Pattern tab get a shared **Plan on pattern** switch. With it on,
the Pattern tab draws the numbers of the plan of the same request.

```
   (?person) ──ex:worksFor── est 214 · rows 214 ──▶ (?org)
       │                                              │
   ex:name  est 900 · rows 1,204,331 ⚠ ×1,338          ex:partOf+  est 9 · rows 4,100 ⚠
       ▼                                              ▼
   (?name) [regex "^Ana"]  est 900 · rows 14     ① hash join on ?org  ② join on ?person
```

- **Edges.** Each edge carries `est N · rows M`. Its width grows with the logarithm of
  the actual rows, or of the estimate before a run, so the heavy parts of the pattern
  stand out.
- **Misestimates.** An edge whose actual rows are over ten times its estimate, or under a
  tenth of it, gets `⚠` and the ratio as text, such as `×1,338` or `÷40`. The rule is the
  one PlanView and C18's notes already use, and the legend states it.
- **Filters.** A filter chip shows the rows that reached it and the rows that passed it.
- **Joins.** A join appears as a numbered badge on the variable node it joins on, in the
  order the plan runs the joins, with its operator and its rows. Hovering or focusing the
  badge highlights the edges below it in the plan.
- **Time.** A **Time** toggle replaces rows with each operator's own time, and the
  operator with the largest share is marked `●` as in §2.1.
- **Cross-highlighting.** Selecting an edge selects its operator in the plan tree, and
  selecting an operator in the tree highlights its edges and nodes on the pattern.
- **Locked blocks.** A locked block shows the rows of the operator that evaluates it when
  the plan maps it to one operator, and no number otherwise.
- **Live counts.** With progress on, edges show the rows so far and the running mark of
  §2.1, and update from the same `progress` events as the tree.

The overlay adds nothing to U08's focus order. Each element's description gains its
numbers, such as "edge ex:name, estimated 900 rows, actual 1,204,331, more than ten times
the estimate". `J` moves focus through the join badges in plan order, and `T` toggles
**Time**.

At 390 px the pattern and the tree are two tabs. A tap on an edge opens a sheet with its
operator, its numbers and **Show in plan**, which switches to the tree with the operator
selected. Labels shorten to `214 / 1.2M ⚠`.

### 11.3 Server and API changes

The plan JSON names operators and describes them in text, which the browser cannot match
with the pattern reliably. Phase 4 therefore starts with one backend change.

**Prerequisite.** Every `PlanNode` that reads triple patterns gains an additive member
`patterns`, which lists the patterns it reads in query order.

```ts
type PlanPattern = {
  s: string; p: string; o: string;   // N-Triples terms, or "?name" for a variable of the query
  graph?: string;                    // the graph term inside GRAPH
  occurrence: number;                // index among equal patterns in the query, from 0
};
type PlanNodeAdditions = {
  patterns?: PlanPattern[];          // IndexScan, IndexRangeScan, IndexJoin, StarJoin, TransitivePath, CountFromIndex and the like
  filterVars?: string[];             // Filter: the query variables the expression reads
  joinVars?: string[];               // joins: the variables they join on
};
```

A variable that the planner generated, such as the hidden variables of paths and blank
nodes, is written as `?_`, so the browser does not try to match it. The members are added
to `/{ds}/explain`, `POST /{ds}/sparql/explain`, `meta.plan`, the plan in a budget error
body, and the `plan` event of §3.2. They are additive under the plan's
`additionalProperties`, so `planFormat` does not change, and a client that does not know
them ignores them. The OpenAPI description, `docs/API.md` and the route tests change with
them.

The browser matches a plan pattern with a triple of the pattern graph by its three terms
after prefix expansion and by its occurrence index. A plan pattern that matches nothing,
such as one the planner rewrote, is listed under the pattern as "operators not drawn" with
its numbers, so nothing is hidden. Two triples that the planner merged into one star join
share that operator's numbers, and both edges say so.

No other route is needed. The numbers come from the plans the query page already
receives.

### 11.4 Access, branches and past states

The overlay draws only numbers that are already in the plan the caller received. For a
caller whose view is limited by C12 or C12b, estimates arrive as `-1`, so edges show
actual rows only and no `⚠` mark, as §2.5 already says for the tree. `patterns` carries
only terms that appear in the query text, so it reveals nothing the caller did not write.
A plan read with `at=` or on a branch is drawn the same way, and the overlay's header names
the commit and branch of the plan, which is the state the numbers describe.

### 11.5 Limits

The overlay draws on U08's canvas within its caps of 300 triple patterns. A plan deeper
than 256 levels uses plan format 2, whose ids the client derives, and matching works the
same way. Matching is linear in the plan's nodes and the pattern's triples. Live updates
redraw at most four times a second, as the tree does.

### 11.6 Testing

- **Rust.** `patterns`, `filterVars` and `joinVars` for each operator kind, generated
  variables written as `?_`, occurrence indexes for repeated patterns, star joins with
  several probes, plan format 2, and redaction for limited views beside the plan tests of
  C12 §7.3.
- **Router.** The members appear in `/{ds}/explain`, `POST /{ds}/sparql/explain`,
  `meta.plan` and a budget error body, and the OpenAPI test keeps the schema equal.
- **Vitest.** `lib/plan-pattern.ts` (new), for matching by terms and occurrence, unmatched
  operators, merged star joins, the misestimate rule, the width scale and the join order.
- **Playwright against the mock server.** A14 to A16 and A19, with the mock's fabricated
  plans gaining `patterns`, and the 390 px sheet.
- **Playwright against a real server.** A14, A15 and A17, and A18 once Phase 1 lands.

### 11.7 Acceptance examples

The dataset is §7's `big`.

- **A14. Estimates.** `/{ds}/explain` of a query with three triple patterns returns
  `patterns` on each operator that reads them, and the Pattern tab shows `est N` on each of
  the three edges, equal to the tree's estimates.
- **A15. Misestimate.** After a run in which an edge's actual rows exceed ten times its
  estimate, that edge shows `⚠` with the ratio, and the tree marks the same operator.
- **A16. Cross-highlight.** Selecting that edge selects its operator in the tree, and
  selecting a join in the tree highlights the edges below it.
- **A17. Limited view.** For a caller whose grants exclude a named graph, every edge shows
  actual rows and no estimate or `⚠`.
- **A18. Live.** With progress on, the slow filter of A1 shows rows so far on its chip,
  and the number increases at least twice before the query ends.
- **A19. Stale text.** Editing the editor's text after a run hides the overlay on the
  editor's pane and keeps it on the result's Pattern tab, which draws the text that ran.
- **A20. Speed.** For a plan of 200 triple patterns, matching and drawing the overlay
  takes under 50 ms at the median in the performance project of U08 A10.

### 11.8 Open questions

1. Whether the planner should keep each triple pattern's source range from the parser.
   Matching would be exact, but the range would have to be carried through spargebra's
   algebra.
2. Whether edges should show an estimate from the schema report before any plan exists,
   which is U08's open question 3.

## Provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

- **Spec:** [`U04-live-query-profiling.md`](U04-live-query-profiling.md), written on 2026-10-12 independently from:
  - the WHATWG HTML specification of server-sent events;
  - PostgreSQL's documentation of `EXPLAIN ANALYZE` and the `pg_stat_progress_*` views,
    and Microsoft SQL Server's documentation of Live Query Statistics, as public
    descriptions of per-operator progress;
  - the Sparkles code (`sparkles-core` executor and plan, `PlanView`, `explain.ts`) and
    specs C01, C12, C18, P03, P06 and X05.
- **Implementation:** not built. No new dependency is planned. The server uses its
  existing HTTP and event-stream code, and the UI its existing `SseParser`.
- **Rejected** (spec §9):
  - progress inside the query response;
  - WebSockets;
  - polling as the UI's only mechanism;
  - sampling the executor's stack;
  - a server-assigned id returned before execution.

## Outcome

Phases 1 to 3 are not built, and [ROADMAP](../ROADMAP.md) item 27 tracks them. Phase 4,
item 102, shipped on 2026-10-23.

### Phase 4 (2026-10-23)

**Server.** Every plan node that reads triple patterns or paths lists them in `patterns`,
and filters and joins list `filterVars` and `joinVars`, as §11.3 describes. The planner
records each pattern's terms when it collects the pattern, so the scans, range scans,
ordered top-k scans, spatial scans, counts from the index, index joins, star joins,
paths and the operators that read the predicate patterns built from it carry it, and the
members are written when a plan node is described.
Generated variables and blank nodes are `?_`. The occurrence index counts equal patterns as
the planner meets them and wraps at the number of equal patterns in the parsed query, so a
group that the planner plans once per named graph keeps the index of its text. Two
behaviours of the parser shape the members. A sequence such as `ex:a/ex:b` reaches the
planner as its steps joined by a blank node, so each step is a pattern of its own with
`?_` in the middle. An outer inverse such as `^ex:a` or `^ex:a+` reaches it as the path
inside it with the two ends swapped. Other paths are written whole in a form without
spaces, which `path_text` defines and the browser reproduces. `joinVars` of an optional
join, `MINUS`, a semi-join, an anti-join or a count join are the variables its two sides
share. The members are in `/{ds}/explain`,
`POST /{ds}/sparql/explain`, `meta.plan`, the plan of a budget error body and the
OpenAPI description, and `planFormat` did not change. MCP's `explain_query` returns node
facts rather than the plan, so it does not carry them.

**Client.** `ui/src/lib/plan-pattern.ts` matches the plan with the drawing by the three
terms and the occurrence, writing literals, prefixed names and paths of the pattern in
the server's form. A literal is escaped as OxRDF writes N-Triples, a boolean keyword in
any case is written in lower case, a datatype IRI is resolved against `BASE` after its
escapes are undone, and a range modifier has its bounds without leading zeros and one bound
when both are equal. A pattern inside `GRAPH` is in a locked block and is always listed
with the operators not drawn. An edge with several operators, such as a sequence read by two scans,
shows the operator nearest the root and says how many read it. The Pattern pane draws the
labels, widths and marks of §11.2, the filter rows, the join badges in run order and the
list of operators not drawn. Only a filter chip of the outer group takes a `Filter`
operator's numbers, since a filter inside an `OPTIONAL` becomes its join's condition, and
each operator serves one chip, so a chip shows nothing rather than another filter's rows.
A variable with two join badges is two stops for `J`, which announces and lights the join
it is on. The pane of the editor draws the explanation when the Explain view is open,
because an Explain keeps the result of the run before it, and **Show in plan** goes to
the tree the numbers came from. `T` works only on a plan that ran. The Plan tab, the Pattern tab and the pane share one
**Plan on pattern** switch, and the Pattern tab puts the plan tree beside the drawing on a
desktop, with selection and highlight both ways. `J`, `T`, the spoken numbers and the
phone's sheet with **Show in plan** are as specified. A locked block shows numbers when it
is the only block of its kind and the plan has exactly one operator of the matching name,
such as one `BIND` and one `Bind`, and none otherwise. The Pattern pane's **Edit** button
of a result moved into the pane's header, where it no longer covers the canvas buttons.

**Deviations.** Live counts and A18 wait for Phases 1 and 2, which are not built. The
misestimate rule of PlanView, a ratio of ten or more either way, moved into
`plan-pattern.ts`, and both views use it. A pattern inside a locked block counts toward
the server's occurrence index but is not a triple of the drawing, so an equal pattern
after it in the text can take the wrong operator's numbers. That case needs an earlier
equal pattern inside a `UNION`, `GRAPH` or subquery and is left as it is. The answer to
open question 1 stays open, because matching by terms worked for every acceptance example.

**Tests.** `plan_refs_tests.rs` covers the members for each operator kind, generated
variables, occurrences in a union and in per-graph evaluation, named graphs, paths, star
joins, counts from the index, redaction and plan format 2. The router test
`plan_patterns_are_on_every_route_that_returns_a_plan` checks the four routes and a
budget error body. Vitest covers matching, a copied answer, occurrences, unmatched
patterns, star joins, sequences and inverses, filters, locked blocks, hidden estimates,
the hot operator and A20, whose 200 patterns match in well under 50 ms. The mock
Playwright spec covers A14 to A16, A19, the keyboard, the list of operators not drawn and
the 390 px sheet, and the real-server spec covers A14, A15 and A17.

**Cost.** At first every query paid for the references, because the planner recorded them
and an executed plan carried them whether or not anything read it. On forge they added 4
to 9% of server time to queries under 50 µs, such as 45.5 µs against 47.8 µs on
`star-lookup`, and about 1.8% of client time. A follow-up on 2026-10-24 keyed the
occurrence count by the recorded pattern and wrote IRIs and variables directly, which
removed 28 to 48% of the added instructions with the same output.

Roadmap item 180 then made the references cost nothing when no plan is shown.
`QueryOptions::no_plan_patterns` tells the planner not to name patterns and the executor
not to build `patterns`, `filterVars` and `joinVars`. The query route sets it for every
format but the Sparkles format, whose answer and error body carry the plan, and an
in-process SERVICE call sets it because its plan is dropped. The explain routes, MCP and
library callers keep the default and name their patterns as before. A plain SPARQL JSON
request then runs 0.7 to 5.8 thousand fewer instructions than with the references, which
puts it at or below the commit before Phase 4 on `star-lookup`, `values-star` and
`types-grouped`. A request in the Sparkles format still pays for them, since it shows
them. [Benchmarks](../benchmarks/engine/RESULTS.md#the-cost-of-plan-pattern-references) has the numbers.

**Review.** The Codex review of the phase reported twelve findings, and all twelve were
confirmed and fixed with a regression test each. Count joins named the aggregate rather
than the join key, the predicate-pattern operators named no pattern, a filter chip in an
`OPTIONAL` borrowed the outer filter's numbers, the pane preferred a retained result to a
newer explanation, inverses of modified paths, literals with control characters, datatypes
under `BASE`, boolean keywords in capitals and non-canonical ranges failed to match,
patterns inside `GRAPH` vanished from the list, the second of two join badges on one
variable had no keyboard stop, and `T` switched an estimate to empty times. The second
review found three more, all fixed. A chip whose operator another chip had taken borrowed
the numbers of a filter on more variables, so a chip now takes an operator on exactly its
variables first. A selection made on the second of two join badges fell back to the first
once the focus left the canvas, and moving between the Plan and Pattern tabs cleared the
selected operator although the plan was the same.

