# F13: Incremental views, live queries and fact provenance

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 is the incremental core over the commit stream,
> materialized views read as tables, live SELECT queries over server-sent events, the
> library API, the CLI and the fallback to re-evaluation. Phase 2 adds views written to
> named graphs, WebSocket, MCP, the bindings and the UI, views per branch, top-k views,
> and protections that depend on the data. Phase 3 adds per-fact provenance, recursion
> for property paths and rules, and GraphQL subscriptions on the core.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

This design was written from the published work on incremental view maintenance and
dataflow: Gupta, Mumick and Subrahmanian, "Maintaining views incrementally" (SIGMOD
1993), McSherry, Murray, Isaacs and Isard, "Differential dataflow" (CIDR 2013), Budiu,
Chajed, McSherry, Ryzhyk and Tannen, "DBSP: Automatic Incremental View Maintenance for
Rich Query Languages" (VLDB 2023), and Gjengset et al., "Noria: dynamic,
partially-stateful data-flow for high-performance web applications" (OSDI 2018). The
provenance model follows Green, Karvounarakis and Tannen, "Provenance semirings" (PODS
2007), Buneman, Khanna and Tan, "Why and where: a characterization of data provenance"
(ICDT 2001), and Geerts, Karvounarakis, Christophides and Fundulaki, "Algebraic
structures for capturing the provenance of SPARQL queries" (ICDT 2013). The transport
follows the WHATWG HTML standard's server-sent events and RFC 6455 (WebSocket). Motik,
Nenov, Piro and Horrocks, "Incremental Update of Datalog Materialisation: the
Backward/Forward Algorithm" (AAAI 2015), is the algorithm `sparkles-reasoner` already
uses. It builds on the change feed and change log of
[F06](F06-snapshots-and-point-in-time.md), budgets ([C01](C01-observability-and-budgets.md)),
the filtered and masked views of [C12](C12-graph-access-control.md) and
[C12b](C12b-triple-access-control.md), branches ([F09](F09-branches-and-merges.md)), the
MCP server ([C11](C11-mcp-server.md)) and stored queries ([C16](C16-stored-queries.md)).
It is the engine planned for GraphQL subscriptions ([C03 §13](C03-graphql.md#13-subscriptions)),
for the derived-fact layer of F16, for the tabled caches of F17 and for the explanations
of F18.

## 1. Summary, goals, non-goals

Sparkles answers every query from scratch. [FEATURES](../FEATURES.md) lists "materialized
views or live query monitoring" among what it lacks. Several features already keep a
result current from the changes, each with its own code: the reasoner maintains its
closure with the backward/forward algorithm (`crates/sparkles-reasoner/src/incremental.rs`),
C02 keeps schema reports current from the delta, and C03 §13 sketches subscriptions as
a re-run after any commit whose predicates match. A client that wants to know when an
answer changes polls, and a dashboard that shows an aggregate over ten million triples
recomputes it on every refresh.

This spec adds one incremental computation core and three features on it. A query is
compiled to a circuit of operators over Z-sets, multisets whose elements carry signed
integer weights, as in DBSP. Each commit's net changes, from the change feed of F06, go
into the circuit as a Z-set of quads, and the circuit produces the Z-set of changed
solution rows. The three features are:

1. **Materialized views.** A named query per dataset whose result is kept current after
   every commit and read through `SERVICE view:read` or written into a named graph.
2. **Live queries.** A query a client subscribes to. It receives the result once, then
   one diff per commit that changes it, over server-sent events, WebSocket, MCP or a
   binding's iterator.
3. **Per-fact provenance.** For each row or derived triple of a view, the sets of input
   quads that support it, kept as a semiring annotation during maintenance.

**Goals**

1. One core, in `sparkles-core`, that the three features, GraphQL subscriptions and the
   reasoning work of F16 and F17 share.
2. Exact results. After commit `n` is applied, a view equals the query evaluated at the
   snapshot of commit `n`, under bag semantics, for every query the core accepts.
3. A defined incremental fragment (§4) and, outside it, a fallback that re-evaluates the
   affected subtree and diffs its output, so that every read-only query without
   non-deterministic functions can be a view or a live query. EXPLAIN says which
   operators are incremental.
4. State bounded by budgets. Joins with a triple pattern probe the store's permutations
   at the two snapshots instead of keeping a copy of the pattern's matches.
5. Views that survive restarts from a checkpoint plus the commits after it, and are
   rebuilt when the change log cannot cover the gap.
6. Access control with the same answers a caller would get by running the query.
7. Views on branches and reads of a view at a past commit.

**Non-goals**

- Stream reasoning and windows over time, as in RSP-QL or C-SPARQL. A commit is the unit
  of change, and there are no tumbling or sliding windows.
- Views across datasets, over remote `SERVICE` endpoints or over federated sources.
- Answering ordinary queries from views by query rewriting. §14 records it as an open
  question.
- A distributed dataflow runtime. The core runs inside one process, with the parallelism
  of §8.3.
- Views over full-text, vector or GeoSPARQL indexes that are updated asynchronously.
  Their operators are refused in views (§4.6).

## 2. User-visible behaviour

### 2.1 Defining a view

A view is a named SELECT or CONSTRUCT query with options. It is stored per dataset in
`<root>/views/{name}/view.json` and managed through the administration routes.

```
PUT /$/views/{ds}/{name}
{
  "query": "SELECT ?team (COUNT(?p) AS ?members) WHERE { ?p org:memberOf ?team } GROUP BY ?team",
  "mode": "table",
  "principal": "reports",
  "provenance": "none",
  "maintenance": "async",
  "branches": ["main"],
  "memoryBytes": 268435456,
  "keepCommits": 0
}
```

| Field | Values | Default | Meaning |
|---|---|---|---|
| `query` | SPARQL text | required | A SELECT or CONSTRUCT query. Prefixes declared for the dataset (C20) apply. |
| `mode` | `table`, `graph` | `table` for SELECT, `graph` for CONSTRUCT | Where the result lives (§2.2). |
| `graph` | an IRI | `urn:x-sparkles:view:{name}` | The named graph of a `graph` view. |
| `principal` | a principal name | the caller | Whose view of the dataset the query reads (§6). |
| `provenance` | `none`, `count`, `why`, `lineage` | `none` | The annotation kept per row (§5). |
| `maintenance` | `async`, `commit` | `async` | Whether the view is maintained after the commit or inside it (§3.3). |
| `branches` | branch names or globs | `["main"]` | The branches with their own state (§7). |
| `memoryBytes` | bytes | 256 MiB | The budget of the view's state (§8.2). |
| `keepCommits` | integer | 0 | Diffs kept for reads at past commits (§7.2). |

`PUT` compiles the query, reports its plan with each operator marked `incremental` or
`recompute`, and starts the first evaluation as a task in `/$/tasks`. The answer is `201`
with the plan, or `400` with the reason the query cannot be a view. `GET
/$/views/{ds}` lists the views with their state: `building`, `current`, `behind` with
the commits it lags, `suspended` with a reason such as `over-budget`, or `failed`.
`DELETE` removes the view and its state, and for a `graph` view leaves the graph in place
unless `?dropGraph=true`. `POST /$/views/{ds}/{name}/rebuild` evaluates the view again
from scratch. Defining and deleting views needs the dataset's `admin` grant.

`sparkles views --loc DB list|get|put|delete|rebuild|explain` does the same on a local
store or, with `--url`, on a server. A stored query of C16 can be made a view by
reference, `"storedQuery": "name"`, which follows the stored query's current version.

### 2.2 Reading a view

A `table` view is read with a local `SERVICE`, in the form of F07.

```sparql
PREFIX view: <urn:x-sparkles:view#>
SELECT ?team ?members WHERE {
  SERVICE view:read { [] view:name "team-sizes" ; view:bind ?team, ?members . }
  FILTER (?members > 10)
}
```

`view:bind` lists variables, each of which must be a projected variable of the view by
name. The rows come with their multiplicities, so a view without `DISTINCT` reads as the
query would. `view:commit ?c` binds the commit the rows reflect. A `table` view's rows are
not quads and never appear in triple patterns.

A `graph` view's CONSTRUCT output is kept in its named graph. Each maintenance pass
writes the triples added and removed as one commit of kind `view` by the principal
`urn:x-sparkles:principal:view:{name}`, with the source commit in its message. The graph
is ordinary data: triple patterns read it, history records it, and backups and replicas
carry it. A write by anyone else to a view's graph is refused with `409 view-graph`.

A read at a commit the view has not reached waits up to `viewWaitMs`, 2,000 ms by
default, for the view to catch up, then fails with `503 view-behind` and a `Retry-After`.
`view:allowStale true` reads the latest state instead, and `view:commit` then says which
commit it reflects.

### 2.3 Live queries

```
GET /{ds}/live?query=…&after=SEL
Accept: text/event-stream
```

The query may be any SELECT, ASK or CONSTRUCT query that a view could be (§4). The
response is a stream of events whose `id` is a commit number.

| Event | Data | When |
|---|---|---|
| `snapshot` | `{commit, vars, rows, more}` | The result at the start commit, in pages of at most 10,000 rows. The last page has `more: false`. |
| `diff` | `{commit, from, removed, added}` | After each commit that changed the result. Rows carry a count when it is not 1. `from` is the previous event's commit. |
| `reset` | as `snapshot` | The server re-evaluated the query, after a gap the change feed cannot bridge or a fallback (§3.4). The client replaces its copy. |
| `error` | `{code, message}` | The stream ends. |

ASK streams `{commit, value}` only when the value changes. CONSTRUCT streams triples.
SELECT with `ORDER BY` and `LIMIT` streams the rows that enter and leave the window, with
their positions (§4.4). Commits that do not change the result send nothing, except a
comment line every 15 seconds.

`Last-Event-ID` or `after=commit:N` resumes. When the server can compute the diff from
`N` to the head, from the change feed and the state it keeps, it sends that diff;
otherwise it sends `reset`. `coalesceMs` (default 0) merges the diffs of commits that
arrive within the interval into one event whose `id` is the last commit.

A stream lasts until the client closes it or the server drains, without the five-minute
cap of the change feed. The server limits live queries to 1,000 in total and 20 per
principal by default (`--max-live-queries`, `--max-live-queries-per-principal`). The
initial evaluation counts as a query against the rate limit, and each diff as nothing.

Phase 2 adds `GET /{ds}/live/ws`, a WebSocket with one JSON message per event and the
same event names, which carries several subscriptions on one connection with a
client-chosen `id` each.

### 2.4 CLI, library, bindings and MCP

- **CLI.** `sparkles query --live` prints the snapshot, then one block per diff, with
  `+` and `-` prefixes, or JSON lines with `--json`.
- **Rust.** `Dataset::live(query, LiveOptions) -> LiveQuery`, whose `next()` returns a
  `LiveEvent` and blocks or awaits until one exists. `Dataset::views()` returns a
  `Views` handle with `put`, `get`, `list`, `delete`, `rebuild` and `read`, in
  `crates/sparkles/src/handles/views.rs` beside the other handles of P06.
- **Python.** `for ev in ds.live(q): …` and `async for` from the async wrapper. Node: an
  async iterator with an `AbortSignal`. JVM: a `LiveQuery` with a listener and `close()`.
- **MCP.** The tool `watch_query` registers a live query for the session and returns a
  resource URI `sparkles://{ds}/live/{id}`. A client subscribed through
  `subscriptions/listen` gets `notifications/resources/updated` after each diff, and
  reading the resource returns the changes since its last read in the compact table
  format of C11, or the whole result after a `reset`. `list_views` and `read_view` serve
  views. MCP live queries end with the session.
- **UI.** The query page gains a **Live** toggle. Rows that arrive are highlighted and
  removed rows fade out. The dataset page lists views with their state, lag and memory,
  and shows a view's plan with each operator's mode.

### 2.5 Provenance in queries

A view with `provenance` other than `none` answers why a row is in it.

```sparql
SELECT ?team ?w ?s ?p ?o ?g WHERE {
  SERVICE view:read {
    [] view:name "team-sizes" ; view:bind ?team ;
       view:witness ?w ; view:supportSubject ?s ; view:supportPredicate ?p ;
       view:supportObject ?o ; view:supportGraph ?g .
  }
  FILTER (?team = ex:payments)
}
```

Each witness is a set of input quads that is enough to derive the row, and the support
variables list its quads, one row per quad. `view:support ?t` binds each quad's triple
as a triple term. A `graph` view adds the same information for each derived triple
through `SERVICE view:why { [] view:name "…" ; view:triple <<( s p o )>> ; … }`.

## 3. The core

### 3.1 Z-sets and commits

A relation is a Z-set: a map from rows to non-zero integer weights. A set or bag of
solutions has positive weights, and a change has positive weights for rows added and
negative ones for rows removed. Z-sets form an abelian group under addition, which is
what makes the operators below compose.

The input is the dataset as a Z-set of quads. Commit `n` contributes `Δ_n`, its net
changes relative to its parent, which `Store::changes` already computes as
`CommitChanges` with removals and additions. A view's state after commit `n` is
`V_n = Q(S_n)`, and its change is `ΔV_n = V_n − V_{n−1}`, computed from `Δ_n` without
evaluating `Q(S_n)`.

The core reads changes as ids of the current generation, not as quads. A new
`Store::commit_deltas(after, &ChangesOptions)` returns, for each commit, the removed and
added quads as `[Id; 4]`, taken from the queue that the change log's background writer
already receives in the commit path (F06 §11.3), so no term is decoded. Commits that the
queue no longer holds are read from the write-ahead logs or the change log, whose keys
are mapped to ids through the vocabulary.

### 3.2 Operators

The compiler takes the planner's `plan::Node` tree after join ordering and turns each
node into an incremental operator.

| Algebra | Incremental rule | State |
|---|---|---|
| Triple pattern | Filter `Δ_n` by the pattern and graph. | None. |
| Filter, extend (`BIND`), project, `VALUES` | Applied to the change rows. Linear. | None. |
| Union | Sum of the changes. | None. |
| Join `A ⋈ B` | `ΔA ⋈ B_old + A_new ⋈ ΔB`. | An index of each side that is not a triple pattern (§3.5). |
| Distinct | A row's output weight is 1 while its input weight is positive. Emits `+1` or `−1` when the input weight crosses zero. | Input weights. |
| Group with `COUNT`, `SUM`, `AVG` | Per group, the count and sum are updated by the change rows. The group's old row is retracted and the new one emitted. | Per group: count and sum. |
| Group with `MIN`, `MAX` | As above, with an ordered multiset of the group's values. | Per group: value counts. |
| Optional `A ⟕ B` | The join, plus `A` rows without a match. A match count per `A` row moves rows between the two parts when it crosses zero. | Match counts by join key. |
| Minus, `FILTER NOT EXISTS` without correlation beyond the join key | An antijoin, with a match count per key as for optional. | Match counts. |
| Order by with limit and offset | §4.4. | A bounded heap with a reserve. |

Joins with a triple pattern do not keep the pattern's matches. `ΔA ⋈ B_old` probes the
permutations of the snapshot before the commit, and `A_new ⋈ ΔB` probes the indexed state
of `A`. When both sides of a join are patterns, both probes go to snapshots: the one
before the commit for the first term and the one after it for the second. The maintenance
pass holds both snapshots, which the store already keeps for the commit's readers. This
uses the store's own indexes as the arrangements that differential dataflow would build,
and is the main reason the core is not an external dataflow library (§13).

### 3.3 Maintenance passes

Each dataset with views or live queries has one `incr::Runtime`. It waits on
`Store::subscribe_commits`, reads the deltas after the last commit it applied, and runs
a pass per commit, or per run of commits when it is behind, since the deltas of
consecutive commits add. Views and live queries with the same compiled circuit and the
same access view share one state, so a thousand subscribers to one dashboard query cost
one pass.

Before a pass reaches a view, a footprint check skips it. The footprint is the set of
(graph, predicate) pairs and constant subjects and objects of the circuit's patterns,
built at compile time. A commit whose changes match no footprint entry skips the view
and costs one hash probe per changed quad, as C03 §13 proposed for subscriptions.

`maintenance: "async"` runs the pass on the runtime's worker after the commit is
published. Readers see the commit before the view does, and the view's lag is reported
in `GET /$/views/{ds}` and the metric `sparkles_view_lag_commits`.
`maintenance: "commit"` runs the pass inside the write transaction, after the write
guard of C10 and before the commit is published, so a reader never sees a commit
without the view. It adds the pass's time to the commit's latency and is refused for
views whose plan has a `recompute` operator. A `graph` view maintained at commit writes
its triples in the same commit. An error in such a pass rolls the write back with
`409 view-maintenance`, which is the behaviour of a guard.

### 3.4 Large commits and fallbacks

A commit whose changes the feed lists with counts only (`complete: false`), such as a
large bulk load, has no delta. The view is evaluated at the new snapshot, the output is
compared with the state, and the difference is the diff, which live queries receive as a
`reset` when it exceeds `resetRows` (100,000 by default). The same happens after a
restart whose gap the change log cannot bridge (§8.4).

An operator outside the incremental fragment becomes a `recompute` node. Its subtree is
evaluated at the snapshot of each commit that matches its footprint, and its previous
output, kept as indexed state, is subtracted. The rest of the circuit stays incremental.
A `recompute` node is exact and costs one evaluation of its subtree per relevant commit.
EXPLAIN on a view or live query shows each node's mode, and `GET /$/views/{ds}/{name}`
reports the time spent in recompute nodes.

### 3.5 State

The indexed state of an operator is an arrangement: a hash map from the join key to the
rows with their weights, keyed by `Id`s of the current generation. Rows are fixed-width
id tuples, stored in columnar chunks of the kind `sparql::table::Table` already uses.
The state of every view counts against its `memoryBytes` and against a server-wide pool
of `--views-memory`, which X08's server-wide memory pool provides. A view whose state
passes its budget is `suspended` with `over-budget`, and its reads fail with
`503 view-suspended` until a rebuild with a larger budget. A live query whose state
passes the per-query budget fails with an `error` event naming the budget.

## 4. The incremental fragment

### 4.1 Supported in Phase 1

Basic graph patterns, `GRAPH` with a constant or a variable, `FILTER` and `BIND` with
deterministic expressions, `UNION`, `OPTIONAL`, `MINUS`, `FILTER NOT EXISTS` and `EXISTS`
whose inner pattern shares only join variables with the outside, `VALUES`, `DISTINCT`,
`REDUCED` as `DISTINCT`, projection, subqueries built from these, and `GROUP BY` with
`COUNT`, `SUM` and `AVG`. CONSTRUCT templates are a projection to quads followed by a
distinct, since a graph is a set.

### 4.2 Added in Phase 2

`MIN`, `MAX`, `GROUP_CONCAT` without `ORDER BY` inside it (recomputed per changed group),
`SAMPLE` (kept stable while its value remains), `HAVING`, and `ORDER BY` with `LIMIT`
(§4.4). Correlated `EXISTS` is maintained by re-evaluating the inner pattern for the outer
rows whose bindings it touches, a recompute scoped to rows.

### 4.3 Added in Phase 3

Property paths with `+` and `*`, and recursive rules for F16, through a fixed-point
operator. The operator iterates a circuit until its change is empty, as DBSP's recursion
does, and keeps the iteration's state between commits so that a change propagates only
as far as it reaches. Deletions in recursion follow the backward/forward approach of the
reasoner: a candidate fact is kept when another derivation exists, rather than deleted
and derived again as in DRed. Until Phase 3 a path is a `recompute` node, and fixed-length
paths and `?`, which are joins and unions, are incremental from Phase 1.

### 4.4 Ordered windows

A SELECT with `ORDER BY` and `LIMIT k OFFSET m` keeps the `k + m + r` best rows, where
`r` is a reserve of `max(k, 100)` rows. A change that removes a row from the window
promotes the best reserve row. When the reserve is empty and the window has fewer than
`k + m` rows, the ordered subtree is evaluated again to refill it. A live query's `diff`
event lists each row that enters or leaves the window with its new position. A view
without `ORDER BY` has no order, and its rows are read in the order of its state.

### 4.5 Refused

A view or live query is refused at definition with `400 not-incremental` and the
feature's name when it uses `NOW()`, `RAND()`, `UUID()`, `STRUUID()` or `BNODE()`
outside a CONSTRUCT template's blank nodes, a remote `SERVICE`, `text:query`,
`spk:vectorSearch`, `spk:hybridSearch`, spatial property functions, `SERVICE
path:search`, `SERVICE hist:changes`, functions or property functions registered per
query (P03), or RDFS on read. The first five have no single value per snapshot. The
search operators read indexes that are built after the commit, so their answers at a
commit are not defined. Path search and history queries are planned for views in a
later phase, as `recompute` nodes with their own footprints.

### 4.6 Blank nodes in CONSTRUCT views

A CONSTRUCT template's blank nodes become fresh nodes per solution. A `graph` view mints
them deterministically, from a hash of the view's name, the template's blank node label
and the solution's bindings, as skolem IRIs `urn:x-sparkles:view:{name}:{hash}`. A
solution that disappears and returns gives the same IRI, and a diff never churns them.

## 5. Provenance

### 5.1 The annotation

A view's `provenance` setting chooses a semiring whose values annotate each row of each
operator's state, after Green, Karvounarakis and Tannen. The Z-set weights are the
counting annotation and are always kept, because maintenance needs them.

| Setting | Annotation | Answers |
|---|---|---|
| `count` | The number of derivations, the Z-set weight. | How many ways a row is derived. |
| `lineage` | The set of input quads that contribute to any derivation. | Which facts the row depends on. |
| `why` | A set of witnesses, each a set of input quads that derives the row, kept minimal and at most `maxWitnesses` (8 by default) per row. | Which facts together justify the row. |

Joins combine annotations by product (union of witnesses' quads, pairwise) and unions
and projections by sum (union of witness sets). Input quads are referenced by a
`FactId`, a 64-bit hash of the quad's ids, resolved back to the quad through the
snapshot. A witness set is truncated to the `maxWitnesses` smallest witnesses, and a
truncated row says so through `view:witnessesTruncated true`.

### 5.2 Negation and aggregates

The positive semirings do not describe `MINUS`, `NOT EXISTS` or the unmatched rows of
`OPTIONAL`, where a row holds because a fact is absent. A row produced by such an
operator records the absence as a negative condition, the pattern and bindings that must
have no match, following the treatment of difference by Geerts et al. A witness then
reads "these quads, and no quad matching this pattern". An aggregate row's witnesses are
the union of its group members' witnesses up to `maxWitnesses`, and `lineage` lists all
of them.

### 5.3 Uses

The provenance of a `graph` view is what F18 needs for derivation trees and for "why
not?", what C22 records for its summaries' source facts, and what a provenance-aware
protection of F18 reads to hide a derived triple whose support the caller cannot see.
The annotation is kept in the operator state and is never written as reifiers, because
a reifier per derived triple and witness multiplies the view's output in the store.
`POST /{ds}/sparql?provenance=why` in Phase 3 evaluates a one-off SELECT with
annotations, without keeping state, and adds a `provenance` member to each binding of
the JSON results.

## 6. Access control

### 6.1 Views

A view reads the dataset as its `principal`, through the same `GraphAccess` and masked
snapshot that a query of that principal reads (`access::GraphAccess::masked`). Its
deltas are filtered by the principal's graphs and predicate protections before they
enter the circuit, which is the filtering that `ChangesOptions::graphs` already does.

A caller may read a view when it has `read` on the dataset and its own view covers the
view principal's view: every graph the principal may read, the caller may read, and
every protection the principal's grants lift, the caller's grants lift. Otherwise the
read fails with `403 view-access`. The check compares grant sets, not data, and its
result is cached per (caller, view). An administrator who wants to publish an aggregate
to callers who cannot read its inputs grants them the view by name
(`view:{name}` in the grant's endpoints), which is an explicit declassification and is
written to the audit log.

### 6.2 Live queries

A live query reads as its subscriber. Subscribers with the same query and the same
access fingerprint, the hash of their resolved graph set and lifted protections, share
one circuit. Protections that depend only on predicates and graphs filter the delta.
Protections that depend on the data, by subject class or by a pattern with the caller
bound, change which quads are visible when other quads change, so the delta of the
visible quads is not a filter of the raw delta. In Phase 1 such a subscriber's query runs
as one `recompute` node over its masked snapshot. In Phase 2 each such protection is
compiled into a view of the quads it hides, maintained by the same core, and the visible
delta is the raw delta minus the change of the hidden set. The answers equal those of the
masked snapshot in both phases.

A caller whose grants change during a live query gets a `reset` computed with the new
grants, or an `error` with `403` when it loses `read`.

## 7. Branches and history

### 7.1 Branches

A view keeps state for each branch its `branches` setting names. A branch created from a
branch with state starts from that state, shared copy-on-write at the checkpoint level
(§8.4), and diverges with its own commits. A merge commit is maintained as any commit,
from its net changes. Deleting a branch drops its state. A live query names its branch as
other reads do, with `?branch=` or the `{ds}@{branch}` dataset name.

### 7.2 Reads at past commits

A view read with `?at=` at or after the view's oldest kept commit is answered from its
state and the diffs of `keepCommits`, applied backwards. A read before that evaluates the
view's query at the snapshot, if F06 can still materialize it, and says so with the
response header `Sparkles-View: evaluated`. A live query with `after=commit:N` replays the
diffs from `N`, read from the change feed, so a client can follow a query's history from
any commit the change log holds.

## 8. Architecture

### 8.1 Modules

- `crates/sparkles-core/src/incr/` holds the core: `zset.rs` (Z-sets of id rows with
  `i64` weights), `compile.rs` (from `sparql::plan::Node` after join ordering to an
  `incr::Circuit`), `ops/` (one file per operator of §3.2), `arrange.rs` (indexed state),
  `footprint.rs`, `prov.rs` (the annotations of §5), `runtime.rs` (per-dataset passes),
  `persist.rs` (checkpoints) and `views.rs` (`view.json` and the view registry).
- `crates/sparkles-core/src/store/changes.rs` gains `Store::commit_deltas`, and
  `store/changelog.rs` exposes the id-level queue of the commit path to it.
- `crates/sparkles-core/src/sparql/` gains `viewsvc.rs`, the `SERVICE view:read` and
  `view:why` leaves, recognized by the planner before remote SERVICE planning as
  `pathsearch.rs` and `history_svc.rs` are.
- `crates/sparkles/src/handles/views.rs` and `Dataset::live` in the embedding API.
- `crates/sparkles-server/src/http/views.rs` for `/$/views/{ds}`, `http/live.rs` for
  `/{ds}/live` on the SSE code of `http/changes.rs`, and `mcp/live.rs` for `watch_query`,
  `list_views` and `read_view`. The route table, `auth/routes.rs` and the OpenAPI
  description gain the routes, which X03's test keeps equal.
- The bindings of P01, P04 and P05 expose `live` and `views` through the shared
  administration surface of P06.

### 8.2 Budgets and metrics

Each pass has a deadline and a cancel flag from a `task::Control`. A pass that runs past
`--view-pass-timeout` (30 s by default) suspends its view with `pass-timeout`. Metrics:
`sparkles_view_lag_commits{dataset,view}`, `sparkles_view_state_bytes{dataset,view}`,
`sparkles_view_pass_seconds` (histogram), `sparkles_view_recompute_seconds_total`,
`sparkles_live_queries{dataset}` and `sparkles_live_events_total`.

### 8.3 Parallelism

Passes for different views run on a shared worker pool of `--view-threads` threads (the
number of cores by default). Inside a pass, a join whose delta has more than 10,000 rows
is split by the hash of its key across threads with rayon, which the reasoner already
uses. Passes for one view run in commit order.

### 8.4 Persistence

A view's state is checkpointed to `<root>/views/{name}/{branch}/state-{commit}.bin` every
`checkpointSeconds` (60 by default), at a clean close and before a generation switch.
Ids differ between generations, so a checkpoint stores terms as the vocabulary keys that
the change log uses (F06 §11.3), and loading it maps keys to ids. A generation switch by
compaction maps the in-memory state to the new generation's ids in the same way, while
the switch holds the old generation, which is the derived-index work at generation
switches that C13's addendum plans. On open, the runtime loads the newest checkpoint and
applies the commits after it from `Store::commit_deltas`. When the change log does not
cover them, because of retention, a gap or a summarized bulk commit, the view is rebuilt
from scratch in a task and is `building` meanwhile. Live queries are not persisted; a
client resumes with `Last-Event-ID` (§2.3). An in-memory dataset keeps view state in
memory only.

### 8.5 Interaction with other features

- **Replication (F10).** A replica maintains its own views from the commits it applies.
  A `graph` view's commits are replicated as data and the replica does not maintain that
  view itself; the view definition is marked `replicated` there.
- **Reasoning (C08).** A view over the inferred graph sees the reasoner's commits like any
  other. F16 may move the reasoner's maintenance onto the fixed-point operator of §4.3.
- **GraphQL (C03).** Phase 3 compiles a subscription document to the batched algebra it
  already produces and runs it as a live query, replacing the re-run on predicate match.
- **Compaction (C13).** Compaction does not change any view's result, so no pass runs.
- **Backups (F05).** View definitions are backed up with the dataset's settings. State
  is not; a restored dataset rebuilds its views.

## 9. Phases

**Phase 1.** The Z-set core and the operators of §3.2 and §4.1, `Store::commit_deltas`,
footprints, async maintenance, the fallback to `recompute`, `table` views with
`SERVICE view:read`, `/$/views/{ds}`, `GET /{ds}/live` over SSE for SELECT, ASK and
CONSTRUCT, `Dataset::live` and `Dataset::views`, `sparkles views` and `sparkles query
--live`, checkpoints and recovery, access control for graph grants and predicate
protections, and metrics.

**Phase 2.** `graph` views with their commits and skolem IRIs, `maintenance: "commit"`,
the WebSocket, MCP's `watch_query`, `list_views` and `read_view`, the Python, Node and JVM
APIs, the UI, views per branch, `keepCommits`, the operators of §4.2, ordered windows, and
data-dependent protections compiled to views.

**Phase 3.** Provenance annotations with `view:witness` and `view:why`, provenance on
one-off queries, the fixed-point operator for property paths and rules, and GraphQL
subscriptions.

## 10. Acceptance examples

The data for A1 to A8 is `ex:a org:memberOf ex:t1`, `ex:b org:memberOf ex:t1`,
`ex:c org:memberOf ex:t2`, in the default graph. The view `team-sizes` is
`SELECT ?team (COUNT(?p) AS ?n) WHERE { ?p org:memberOf ?team } GROUP BY ?team`.

- **A1.** After the view is built, `view:read` returns `(t1, 2)` and `(t2, 1)` at the
  head commit.
- **A2.** A commit that inserts `ex:d org:memberOf ex:t2` makes the view return
  `(t1, 2)` and `(t2, 2)`, and a live query on the same query receives one `diff` with
  `removed (t2, 1)` and `added (t2, 2)`.
- **A3.** A commit that inserts `ex:x foaf:name "X"` runs no pass for the view: its
  footprint does not match, and the metric of passes does not change.
- **A4.** A commit that deletes `ex:a org:memberOf ex:t1` and inserts it again in the
  same transaction has no net change and produces no diff.
- **A5.** A view with `OPTIONAL { ?team rdfs:label ?l }` returns `?l` unbound for `t1`;
  inserting `ex:t1 rdfs:label "One"` retracts the unbound row and adds the bound one.
- **A6.** After a restart with three commits made by another process since the last
  checkpoint, the view equals the query evaluated at the head, and the log shows three
  commits applied, not a rebuild.
- **A7.** With the change log off and a restart after a commit, the view is rebuilt and
  its state is `building` until the task ends.
- **A8.** A caller whose grant excludes the default graph reading `team-sizes`, defined
  with principal `reports` who can read it, gets `403 view-access`.
- **A9.** Two subscribers to one live query with different predicate protections get
  different diffs, each equal to the diff of the query over their masked snapshots.
- **A10.** A view using `?p foaf:knows+ ?q` in Phase 1 is accepted with the path as a
  `recompute` node, and EXPLAIN marks it so. A view using `NOW()` is refused with
  `400 not-incremental` naming `NOW`.
- **A11.** A CONSTRUCT view `{ ?p ex:inTeam ?t } WHERE { ?p org:memberOf ?t }` in `graph`
  mode writes three triples in one `view` commit, and after A2's insertion a second
  commit with one triple.
- **A12.** With `provenance: "why"`, the row `(t1, 2)` has one witness of two quads,
  `ex:a org:memberOf ex:t1` and `ex:b org:memberOf ex:t1`.
- **A13.** A view with `MINUS { ?p ex:left true }` and `provenance: "why"` reports, for a
  remaining member, a witness with its membership quad and the negative condition
  `?p ex:left true` with `?p` bound.
- **A14.** A live query with `ORDER BY DESC(?n) LIMIT 1` over the view's query emits a
  `diff` only when the first team changes.
- **A15.** A live query resumed with `Last-Event-ID: 40` when the head is 45 receives one
  `diff` from 40 to 45, and with the change log trimmed past 40 it receives a `reset`.

## 11. Testing and measurement

**Differential tests.** The oracle is the query evaluated at each snapshot. A test
generator builds random histories over a few predicates, with inserts, deletes,
transactions that add and remove the same quad, bulk commits and compactions, and random
queries from the fragment of §4 with a grammar-driven generator. After every commit it
compares each view's state, each live query's accumulated diffs and each provenance
witness (checked by evaluating the query on the witness's quads alone) with the oracle.
The same tests run with masked snapshots for random protections.

**Recovery tests.** Crashes injected between a commit and its pass, during a checkpoint
and during a generation switch, followed by an open, must give the oracle's answer.

**Benchmarks** (X06):

- LDBC SNB Interactive's update streams applied to the SF1 and SF10 RDF conversions,
  with the short reads as live queries and the complex reads that fall in the fragment
  as views. Reported: pass time per commit at the median, p95 and p99, the time from the
  commit to the client's receipt of its diff, state bytes per view, and the pass time
  against the time to re-run the query.
- BSBM's explore-and-update mix with its business-intelligence queries as views, for the
  aggregate operators.
- A fan-out test of 1,000 subscribers to 10 distinct queries, for shared circuits and the
  SSE writer.
- Write throughput with 0, 10 and 100 async views and with one view maintained at commit,
  against X09's group commit, to show the cost on the writer.

The target is a pass at least 100 times cheaper than re-evaluation for single-quad
commits on the SNB SF10 views, and no change to write throughput with async views until
the runtime falls behind.

## 12. Dependencies

Phase 1 needs nothing new beyond F06's change feed. Phase 2's data-dependent protections
need Phase 1. Phase 3's provenance is a prerequisite of F18, and its fixed-point operator
of F16's derived-fact layer if F16 chooses it.

## 13. Rejected alternatives

- **The `differential-dataflow` or `dbsp` crates.** Both are MIT-licensed Rust libraries
  that implement the models this spec follows. Each keeps its own arrangements of every
  input relation, so a view over a ten-million-triple predicate would copy it, where the
  store's permutations already index it at every snapshot. Both bring a worker runtime
  that would run beside the engine's own threads and budgets, and neither knows the
  planner's operators, ids or masked snapshots. The core borrows their algebra and keeps
  the store as the base arrangement.
- **Re-running the query after each matching commit**, the plan of C03 §13. It is the
  fallback here, and as the only strategy it costs a full evaluation per commit for every
  subscriber group, which an aggregate over a large predicate cannot afford.
- **Views as triggers that write results with SPARQL Update.** C23's triggers can do
  that, but an update re-derives the whole result each time and has no state to compute a
  diff from.
- **Per-subscriber circuits.** A circuit per subscriber multiplies state by the number of
  subscribers. Sharing by query and access fingerprint gives the same answers.
- **Provenance as RDF 1.2 reifiers on derived triples.** It writes `witnesses × triples`
  reifiers per view into the store and its history. Operator state answers the same
  questions without new quads.
- **The full provenance polynomial `N[X]`.** Its size grows with the number of
  derivations, which is exponential for some joins. Bounded witness sets, lineage and
  counts answer the questions users ask, and the bound is reported.
- **Maintaining every view inside the commit.** It ties write latency to the slowest view.
  It is available per view, and async is the default.
- **WebSocket first.** The change feed already streams over SSE, which proxies and the
  bindings' HTTP clients handle. WebSocket comes second for many subscriptions on one
  connection.

## 14. Open questions

1. **Answering queries from views.** The planner could answer a query, or a subtree,
   from a view whose definition contains it. Matching containment for SPARQL with
   `OPTIONAL` and aggregates is hard, and an exact-match rule (same normalized algebra)
   may be enough. The default chosen is no rewriting.
2. **Disk-backed state.** Large views may need their arrangements on disk. X08's spill
   would serve, and the default chosen is memory with the budget of §3.5.
3. **Ordering of view commits on replicas and branches with many views.** Each `graph`
   view writes its own commit, which multiplies commits for datasets with many views. A
   single commit for all `graph` views of one source commit is the likely answer.
4. **Diff granularity for SELECT without DISTINCT.** A row with multiplicity changes
   from 3 to 2 is reported as `removed` with count 1. Clients that cannot handle
   multiplicities can ask for `DISTINCT`.
5. **Semantics of `coalesceMs` with `after`.** A resumed stream with coalescing may skip
   commit ids a client expected. The default chosen is that ids are the last commit of
   each merged group, and the client keeps only the last id.

## 15. Sources

- A. Gupta, I. S. Mumick and V. S. Subrahmanian, "Maintaining views incrementally",
  SIGMOD 1993.
- F. McSherry, D. G. Murray, R. Isaacs and M. Isard, "Differential dataflow", CIDR 2013.
- M. Budiu, T. Chajed, F. McSherry, L. Ryzhyk and V. Tannen, "DBSP: Automatic Incremental
  View Maintenance for Rich Query Languages", VLDB 2023.
- J. Gjengset et al., "Noria: dynamic, partially-stateful data-flow for high-performance
  web applications", OSDI 2018.
- T. J. Green, G. Karvounarakis and V. Tannen, "Provenance semirings", PODS 2007.
- P. Buneman, S. Khanna and W.-C. Tan, "Why and where: a characterization of data
  provenance", ICDT 2001.
- F. Geerts, G. Karvounarakis, V. Christophides and I. Fundulaki, "Algebraic structures for
  capturing the provenance of SPARQL queries", ICDT 2013.
- B. Motik, Y. Nenov, R. Piro and I. Horrocks, "Incremental Update of Datalog
  Materialisation: the Backward/Forward Algorithm", AAAI 2015.
- WHATWG HTML Living Standard, "Server-sent events". IETF RFC 6455, "The WebSocket
  Protocol".
- The Sparkles code: `store/changes.rs`, `store/changelog.rs`, `Store::subscribe_commits`,
  `sparql/plan.rs`, `sparql/pathsearch.rs`, `sparql/history_svc.rs`, `sparql/cache.rs`,
  `access.rs` and `access/triples.rs`, `task.rs`, `crates/sparkles-reasoner/src/incremental.rs`
  and `maintain.rs`, and the specs C01, C03, C08, C10, C11, C12, C12b, C13, C16, F05, F06,
  F09 and F10.

## Outcome

Not built. See [ROADMAP](../ROADMAP.md) item 17.
