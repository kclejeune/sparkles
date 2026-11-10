# F13: Incremental views, live queries and fact provenance

> **Status:** implemented in part, Phase 1.
>
> **Phases:** Phase 1 provides the incremental core over commits, table views, SSE live
> queries, checkpoints, the library API, the CLI and recomputation fallbacks. Phase 2's
> graph views, commit-time maintenance, additional transports and branch views remain
> planned. Phase 3's per-fact provenance and recursive incremental execution remain
> planned, and GraphQL subscriptions are deferred until a workload asks for them. The
> remaining work is split into the lanes of roadmap items 414 to 429.
>
> **User docs:** [Incremental views](../API.md#incremental-views),
> [live queries](../API.md#live-queries) and [usage](../USAGE.md#incremental-views).
>
> This is the design as written before implementation. The [Outcome](#outcome) records
> what shipped and the remaining work.

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
It is the engine planned for GraphQL subscriptions ([C03 §13](C03-graphql/SPEC.md#13-subscriptions)),
for the derived-fact layer of [F16](F16-materialization-at-scale.md), for the tabled caches of [F17](F17-query-time-reasoning.md) and for the explanations
of [F18](F18-explanations-and-entailment-diffs.md).

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
  of change, and there are no tumbling or sliding windows. RSP-QL windows and complex
  event processing are outside the design of this spec, and [ROADMAP](../ROADMAP.md)
  item 467 plans them in this spec's Roadmap follow-ups.
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
| `onFailure` | `suspend`, `reject` | `suspend` | What a failed pass at commit time does (§3.3). |
| `declassify` | boolean | `false` | Releases a `graph` view's output to whoever can read the output graph. A view on a dataset with read restrictions needs it (§6.1). |
| `branches` | branch names or globs | `["main"]` | The branches with their own state (§7). |
| `memoryBytes` | bytes | 256 MiB | The budget of the view's state (§8.2). |
| `keepCommits` | integer | 0 | Diffs kept in memory for reads at past commits (§7.2). |

`PUT` compiles the query, reports its plan with each operator marked `incremental` or
`recompute`, and starts the first evaluation as a task in `/$/tasks`. The answer is `201`
with the plan, or `400` with the reason the query cannot be a view. `GET
/$/views/{ds}` lists the views with their state: `building`, `current`, `behind` with
the commits it lags, `suspended` with a reason such as `over-budget`, or `failed`.
`DELETE` removes the view and its state, and for a `graph` view leaves the graph in place
unless `?dropGraph=true`. Neither choice releases the graph, because the mark of
undeclassified view output stays with the graph and its history (§6.1). `POST
/$/views/{ds}/{name}/rebuild` evaluates the view again from scratch. Defining and deleting views needs the dataset's `admin` grant.

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

A `graph` view's CONSTRUCT output is kept in its named graph. Each maintenance round
writes the triples added and removed by every `graph` view as one commit of kind `view`,
by the principal `urn:x-sparkles:principal:views`, with the source commit and the names
of the views in its message. The graph
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
guard of C10 and before the commit is published, so a reader never sees a commit without
the view. It adds the pass's time to the commit's latency. A commit with a view
maintained at commit does not join a group commit of [X09](X09-group-commit-by-default.md)
and is made durable by itself, because the pass needs the transaction's net changes and
its result belongs to the same commit. A later change may stage the pass so that
grouping resumes. A `graph` view maintained at commit writes its triples in the same
commit.

A view is refused for commit-time maintenance at definition when its plan has a
`recompute` operator, when the dataset has RDFS on read or materialized inferences, or
when data-dependent protections apply to its principal, since none of these has a delta
that the pass can compute inside the transaction.

A pass that fails does not block the write by default. The view is suspended with the
reason `pass-failed`, the commit is published, and the view is rebuilt after the cause is
fixed. A view defined with `onFailure: "reject"` instead rolls the write back with
`409 view-maintenance`, which is the behaviour of a guard. The default keeps one faulty
view from stopping every write to the dataset.

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
of `--views-memory`, which [X08](X08-disk-spill-and-server-memory.md)'s server-wide memory pool provides. A view whose state
passes its budget is `suspended` with `over-budget`, and its reads fail with
`503 view-suspended` until a rebuild with a larger budget. A live query whose state
passes the per-query budget fails with an `error` event naming the budget.

### 3.6 Internal circuits

The server can attach a circuit that no user defined. An internal circuit has no
`view.json`, no name in `GET /$/views/{ds}` and no principal. It reads through
`InternalAccess`, which only server code can construct, and it is never granted to a
caller. Its sink is either a view's rows or an accumulator that keeps a count per key and not
the rows. Changes cancel inside a Z-set, both across commits and within one commit, so a
commit that removes one fact and adds another for the same community would cancel at a
circuit projected to the community key. The circuit therefore keeps the changed fact's
identity, the key columns and the fact's own columns, through to the sink. For each commit
the sink sums the absolute weight per key and fact, then totals it per key, and it records
the total before the changes of later commits can cancel it. A fact added by one commit and
removed by the next counts 2, a deletion counts 1, and a replacement within one commit
counts 2, which is what the feed-based counter that C22's refresh reads today produces.
C22's refresh counts the changed facts per community with such an accumulator. It does
not use a grouped `COUNT` view, whose rows hold the net weights and so lose the churn.

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
and derived again as in DRed. Derivation counts cannot weight recursive maintenance,
because a fact in a cycle can support itself, so deletions use backward and forward
rederivation and not counts. F13 hosts one fixed-point interface, and the reasoner plugs
its rules into it, so that the property paths of a view and the rules of F16 and F17 share
the operator. Until Phase 3 a path is a `recompute` node, and fixed-length
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
and the solution's bindings, as skolem IRIs `urn:x-sparkles:view:{name}:{hash}`. The
hash is taken over the DISTINCT solutions, because a graph is a set and two equal
solutions would otherwise write one node twice. A solution that disappears and returns
gives the same IRI, and a diff never churns them.

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
`FactKey`, a 128-bit content hash of the quad's vocabulary keys. Ids change at a
generation switch and 64 bits would collide at scale, so the key is derived from the
terms. It is resolved back to the quad through the snapshot. A witness set is truncated to the `maxWitnesses` smallest witnesses, and a
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

The reasoner records its witnesses with this algebra and not with a second
implementation, and it keeps them off unless a protection needs supported visibility
(F18 §7.1). The provenance of a `graph` view is what F18 needs for derivation trees and for "why
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

A `graph` view writes its output into a graph that anyone with access to that graph
reads, whatever the view's principal could read. The rule is deliberately conservative,
because a check of grant sets cannot account for protections of single triples (C12b), for
protections that depend on the data, such as a subject class or a caller-bound pattern, or
for output that was written before a policy changed.

- On a dataset whose access policy restricts reads in any way, a `graph` view always
  needs `declassify: true`. A `PUT` without it is refused with `403 declassify-required`.
- A dataset with no read restrictions needs no declassification, because every reader can
  already read every input.
- Declassification states that the view's output is releasable to whoever can read the
  output graph, now and after later policy changes. The audit log records the view, its
  query, the output graph and the principal who declassified it. It is the graph-view
  counterpart of the `view:{name}` grant, which declassifies a table view for the callers
  it names.
- A `graph` view without `declassify` marks its output graph as undeclassified view
  output when the view first writes that graph. The mark belongs to the graph and not to
  the view. It stays after the view is deleted, whether or not `?dropGraph=true` removes
  the graph, and it applies to the graph's history as well as its current state.
- While an access policy that restricts reads is in force, a marked graph is readable only
  by administrators, for current and for historical reads, until an administrator
  explicitly declassifies that graph. The declassification is written to the audit log
  with the graph and the administrator. When a read-restricting policy is first installed,
  every `graph` view without `declassify` is suspended with the reason
  `declassify-required`, whether or not it has written its graph yet, and the restriction
  on marked graphs takes effect before the new policy is visible to any request. Dropping
  or deleting the view does not lift it.
- The mark is stored in a versioned dataset file that releases which do not know the
  version refuse to open.

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

A view's definition lives in the dataset's settings on the main branch, and its
`branches` setting names the branches that keep state for it, with globs. A branch has no
definition of its own. A view keeps state for each branch its `branches` setting names. A branch created from a
branch with state starts from that state, shared copy-on-write at the checkpoint level
(§8.4), and diverges with its own commits. A merge commit is maintained as any commit,
from its net changes. Deleting a branch drops its state. A live query names its branch as
other reads do, with `?branch=` or the `{ds}@{branch}` dataset name.

### 7.2 Reads at past commits

The diffs of `keepCommits` are kept in memory only. They are not checkpointed, and a
restart drops them. A view read with `?at=` at or after the view's oldest kept commit is
answered from its state and those diffs, applied backwards. A read before that evaluates the
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
- `crates/sparkles-core/src/commit.rs`: `CommitKind` gains `View` for the commits of
  `graph` views.
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

`view.json` carries a format version, and a writer emits the lowest format that holds the
definition. Format 1 holds the Phase 1 members, format 2 marks a paused definition (F05),
and format 3 holds the members that Phase 2 adds, which are `graph` views, `commit`
maintenance, `onFailure`, `declassify`, `branches` beyond the main branch and
`keepCommits`. A definition is checked against its format when it loads or attaches,
so a format 1 file that carries a Phase 2 member is refused and not run as an async table
view. The checkpoint of a fixed-point operator's state needs its own version of
`VIEW_STATE`, and a reader that does not know a version refuses it and builds the view
from the data.

### 8.5 Interaction with other features

- **Replication (F10).** A replica maintains its own views from the commits it applies.
  A `graph` view's commits are replicated as data and the replica does not maintain that
  view itself; the view definition is marked `replicated` there.
- **Reasoning (C08).** A view over the inferred graph sees the reasoner's commits like any
  other. F16 may move the reasoner's maintenance onto the fixed-point operator of §4.3.
- **GraphQL (C03).** Subscriptions are deferred until a workload asks for them. They would
  compile a subscription document to the batched algebra C03 already produces and run it
  as a live query, replacing the re-run on predicate match.
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
one-off queries, and the fixed-point operator for property paths and rules. GraphQL
subscriptions are deferred until a workload asks for them.

The phases are delivered as lanes, which roadmap items 413 to 429 track. Three consumers
need only part of them. The community refresh of C22 needs internal circuits (§3.6), the
supported visibility of F18 needs the provenance algebra and the reasoner's witnesses, and
the maintained tables of F17 need the algebra and the fixed-point operator.

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

**Benchmarks** ([X06](X06-benchmark-coverage.md)):

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
  against [X09](X09-group-commit-by-default.md)'s group commit, to show the cost on the writer.

The commit path has these acceptance thresholds, measured with
`scripts/nsc-writes.sh` and its `--views` option.

- A dataset without views shows no change in commit latency or write throughput beyond
  noise.
- A dataset with views maintained asynchronously and subscribers attached adds at most
  4% to commit latency.
- A grouped `COUNT` view maintained at commit adds at most 10% to the median commit
  latency and at most 20% to the p99.
- The reasoner with witnesses on takes at most 1.5 times the time and 2 times the memory
  of the same run with witnesses off, and with witnesses off it shows no change beyond
  noise.

The target is a pass at least 100 times cheaper than re-evaluation for single-quad
commits on the SNB SF10 views, and no change to write throughput with async views until
the runtime falls behind.

## 12. Dependencies

Phase 1 needs nothing new beyond F06's change feed. Phase 2's data-dependent protections
need Phase 1. Phase 3's provenance is a prerequisite of F18, and its fixed-point operator
of F16's derived-fact layer if F16 chooses it. The WebSocket transport uses the `ws`
feature of axum, which adds `tokio-tungstenite` under the MIT license, and this
dependency is accepted.

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
3. **Ordering of view commits on replicas and branches with many views.** The decision
   is one `view` commit per maintenance round for all `graph` views (§2.2), which keeps
   the number of commits independent of the number of views.
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

## Provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

- **Spec:** [`F13-incremental-views-and-provenance.md`](F13-incremental-views-and-provenance.md),
  written on 2026-10-12 independently from:
  - Gupta, Mumick and Subrahmanian, "Maintaining views incrementally" (SIGMOD 1993),
    McSherry et al., "Differential dataflow" (CIDR 2013), Budiu et al., "DBSP: Automatic
    Incremental View Maintenance for Rich Query Languages" (VLDB 2023), and Gjengset et
    al., "Noria" (OSDI 2018);
  - Green, Karvounarakis and Tannen, "Provenance semirings" (PODS 2007), Buneman, Khanna
    and Tan, "Why and where" (ICDT 2001), and Geerts et al., "Algebraic structures for
    capturing the provenance of SPARQL queries" (ICDT 2013);
  - Motik, Nenov, Piro and Horrocks, the backward/forward algorithm (AAAI 2015), as
    already used by `sparkles-reasoner`;
  - the WHATWG HTML standard's server-sent events and RFC 6455;
  - the Sparkles code and specs C01, C03, C08, C10, C11, C12, C12b, C13, C16, F05, F06,
    F09 and F10.
- **Implementation:** not built. The spec plans no new runtime dependency. The core is
  written in `sparkles-core`; no code of the `differential-dataflow` or `dbsp` crates is
  copied.
- **Rejected** (spec §13):
  - the `differential-dataflow` and `dbsp` crates as the runtime;
  - re-running the query after each matching commit as the only strategy;
  - views as triggers that rewrite results with SPARQL Update;
  - a circuit per subscriber;
  - provenance as RDF 1.2 reifiers on derived triples;
  - the full provenance polynomial;
  - maintaining every view inside the commit;
  - WebSocket before server-sent events.

## Outcome

**Phase 1 delivered on 2026-10-24.** Item 413, the load-time checks and the split of the
runtime into modules, was delivered on 2026-10-31, and so were the internal circuits of
item 414, which [Internal circuits](#internal-circuits) records. The provenance algebra of
item 415 was delivered on 2026-11-01, and [Provenance annotations](#provenance-annotations)
records it. The fixed-point operator of item 417 was delivered on 2026-11-01, and
[Fixed-point operator](#fixed-point-operator) records it. The aggregates and the
correlated `EXISTS` of item 418 were delivered on 2026-11-10, and
[Aggregates and correlated EXISTS](#aggregates-and-correlated-exists) records them. The
rest of Phases 2 and 3 is not built, and [ROADMAP](../ROADMAP.md) item 17 tracks it as
items 416 and 419 to 429.

- `crates/sparkles-core/src/incr/` holds the core. `zset.rs` keeps rows of ids with
  `i64` weights. `compile.rs` compiles the spargebra algebra into a circuit of the
  operators in `ops/`, and `circuit.rs` runs a circuit's passes. `footprint.rs` resolves
  a circuit's (graph, predicate, constant) entries against a snapshot. `persist.rs`
  writes checkpoints, `views.rs` reads and writes `view.json` and `live.rs` holds the
  subscriptions. `runtime/` runs a dataset's passes, with one module per concern.
  `registry.rs` loads and checks the definitions, `attach.rs` attaches views and
  recovers their checkpoints, `round.rs` runs the passes, `catalog.rs`, `read.rs` and
  `subscribe.rs` serve definitions, reads and live queries, `key.rs` holds the circuit
  key and `COMPILER`, and `worker.rs` holds the worker thread and shutdown.
- `Store::commit_deltas` returns each commit's net changes as ids. While someone
  subscribes (`Store::subscribe_deltas`), the commit path copies each commit's changes
  into a ring of at most 4,096 commits and 4M changes before it publishes the commit.
  Without a subscriber the commit path does one atomic load. Commits the ring does not
  hold are read from the change feed, whose keys are mapped to ids, and a change whose
  term the current generation lacks makes the commit `complete: false`.
- A pass reads the net changes from a circuit's commit to the head as one set, so a run
  of commits costs one pass. A circuit whose footprint misses every changed quad is
  skipped. More than 1,000,000 net changes, or changes that are not all known, evaluate
  the circuit again, and the difference reaches live queries as a diff or, above
  `resetRows`, as a `reset`.
- `table` views live under `<root>/views/{name}/view.json` (`formats::SETTINGS`,
  version 1). `SERVICE view:read` is planned in `sparql/viewsvc.rs` before remote
  SERVICE planning. `/$/views/{ds}`, `/$/views/{ds}/{name}`, `…/rebuild` and
  `GET /{ds}/live` are in the route table, the rate limiter and the OpenAPI description.
  `Dataset::views()` and `Dataset::live` are the library API, `sparkles views` and
  `sparkles query --live` the CLI, and `Dataset.views` and `Dataset.live` the Python API.
- Checkpoints are `views/{name}/main/state-{commit}.bin` (`formats::VIEW_STATE`, version
  1), written every 60 seconds while a view changes and at a clean close. A file of
  another format is left in place and the view is built from the data, which a test
  checks.
- The metrics are `sparkles_view_lag_commits`, `sparkles_view_state_bytes`,
  `sparkles_view_pass_seconds`, `sparkles_view_recompute_seconds_total`,
  `sparkles_view_passes_skipped_total`, `sparkles_live_queries` and
  `sparkles_live_events_total`.

**Decisions where the design left a choice.**

- Circuits are compiled from the spargebra algebra, not from `sparql::plan::Node`. The
  planner fuses and reorders operators for one evaluation, and a circuit needs the
  algebra's operators one to one. A join of patterns is a delta rule over the store's
  permutations: the change of pattern `i` joins the patterns before it at the new
  snapshot and those after it at the old one. The old snapshot is built from the new one
  by reversing the net changes in its delta (`Snapshot::before_changes`), so no history
  is read.
- Operators that the circuit does not maintain become `recompute` nodes with their own
  footprint. These are paths with `?`, and the paths with `*` or `+` that the
  [fixed-point operator](#fixed-point-operator) does not take (paths without them are
  expanded into joins and unions of at most 16 alternatives), `LIMIT` and `OFFSET`, a dataset clause,
  custom aggregates and `FOLD`, an `EXISTS` in a `BIND`, in an `OPTIONAL` filter, in an
  aggregate's argument or under `GRAPH` when it is correlated, `LATERAL` and ARQ's other
  extensions, triple terms, property functions, a `GRAPH` inside another `GRAPH`, and
  `MINUS`, aggregates, `DISTINCT`, `VALUES` and subqueries under `GRAPH ?g`. `ORDER BY`
  without `LIMIT` does not change a view's rows and is ignored.
- `SUM` and `AVG` keep an exact integer sum, and value counts for decimals, floats and
  doubles. Decimals are summed exactly in 256 bits, and a sum outside the decimal range
  is unbound. A one-off evaluation adds the rows in order and can overflow on the way to
  a sum in the range, where a view returns the sum.
- A CONSTRUCT template's blank nodes are minted as `_:v` and a hex hash of the template's
  label and the solution's terms, so a solution that returns gives the same node. A
  solution that occurs twice gives two nodes, because the template keeps each solution's
  count and hashes the number of each occurrence after the first.
- Views and live queries with the same normalized query text and the same access
  fingerprint share one circuit. The parser's random names of aggregate variables are
  renamed in the text, so one query parsed twice gives one circuit and one checkpoint
  fingerprint.
- Protections that depend on the data make the whole query one `recompute` node over the
  masked snapshot, as §6.2 allows for Phase 1. Any change runs it again, even one in a
  graph the principal cannot read, because such a change can alter what the protection
  hides. Graph grants and protections by predicate filter the changes quad by quad.
- Circuits never read the graphs a disclosure guard protects (C22 §6.3). A request
  decides a guard at its own snapshot, and a circuit cannot decide it again at each
  commit, so every circuit and every access fingerprint treat a guard as exempting
  nothing. Item 227 tracks proving guards in circuits.
- A caller reads a view when it reads every quad of the dataset or has the same access
  fingerprint as the view's principal. A caller who cannot read a view still sees its
  definition and state through `GET /$/views/{ds}`, without `rows` and `stateBytes`. Comparing grant sets for containment and the
  `view:{name}` grant that declassifies a view are left to item 222.
- A view's principal is a configured user, resolved through the server's policy when
  the view is attached and again after each policy reload. A definer whose grants cover
  every graph defines a view without a principal by default, which reads every quad.
  Other definers are the default principal, and must then be users.
- Recovery loads the newest checkpoint into the head's id space and applies the commits
  after it from `Store::commit_deltas`, which reads the change log or, without it,
  history. A6 holds. A7 holds only when neither covers the gap, for example after a
  compaction with the change log off. The view is then `building` until the pass that
  evaluates it again ends.
- `SERVICE view:read` returns the view's rows at the query's commit. When a pass has
  moved the view past that commit, the rows are rewound with the view's recent diffs,
  which a circuit with views keeps for 16 commits, or else the view's query is evaluated
  at the query's snapshot. A read that has neither fails with `503 view-ahead`.
- A diff's `from` is the commit of the subscriber's previous event, so commits that
  leave the result alone do not break the chain.
- A live query resumed after commit N gets the merged diffs the circuit kept since N
  (256 commits and 1M rows at most). Otherwise the query is evaluated at the snapshot
  before the changes since N and compared with the current result, and when those
  changes are not known the client gets a `reset`.
- Each dataset has one runtime and one worker thread, which runs a round after each
  commit and each change to the views. `--view-threads` and parallel join deltas
  (§8.3) are left to item 223. The worker starts on first use in the library and when the
  server opens the dataset, so a view never attaches before the server's policy can
  resolve its principal.
- `--max-live-queries` and `--max-live-queries-per-principal` count per dataset, not per
  server.
- An SSE row is `{"values": [...]}` with one SPARQL JSON term or `null` per variable, and
  `count` when it is not 1. ASK streams `{commit, value}`. A stream ends with an
  `access-changed` error on a policy reload or when the stream's own session or token
  is revoked, and the client reconnects under the new grants.
- `sparkles query --live` needs `--server`, since a local database sees no other
  writer's commits. It prints text blocks, or JSON lines with `--results json`.

**Deviations.**

- §4.5 refuses RDFS on read. Instead, a view or live query on a dataset with RDFS on
  read, or whose materialized inferences join the default graph, evaluates its whole
  query again. Its circuit is one `recompute` node that the query engine runs with the
  dataset's inference, so its rows are those of a one-off query with the same options.
  The footprint is that of the query rewritten over stored triples for the current
  schema. A change of `rdfs:subClassOf`, `rdfs:subPropertyOf`, `rdfs:domain` or
  `rdfs:range` in the schema's graph touches every such circuit, and a change in the
  inferred graph counts as a change of the default graph. Item 221 tracks maintaining
  these views incrementally.
- Turning RDFS on read or the materialized inferences on or off compiles every circuit
  again for the new setting and evaluates it on the next round. Views stay attached, and
  live queries receive the difference as a diff or a `reset`. A live query resumed at or
  before the commit of the switch gets the whole result, because the switch changed the
  rows without a commit. A read of a view after the setting changed never returns the
  rows of the old mode.
- A view's state reports `evaluation`, which is `recompute` with an `evaluationReason`
  when every relevant commit evaluates the whole query again, and `incremental`
  otherwise. Protections that depend on the data report `recompute` as well.
- A checkpoint's fingerprint includes the inference settings, with a fixed schema named by
  a hash of its triples. A state written in the other mode is built again rather than
  loaded, and the checkpoint format keeps version 1.
- Backups and persistent clones carry the view definitions, captured under the store's
  writer lock with the data (item 220, recorded in
  [F05](F05-snapshot-repositories/OUTCOME.md#recovery-of-managed-definitions)). A
  restored or cloned view is suspended with the reason `restored` or `cloned`, kept in a
  paused `view.json` of format 2 that older releases refuse, until a rebuild resumes it,
  writes format 1 again, and its principal and stored query resolve again. A definition
  is at most 1 MiB. Checkpoints are not carried, and a resumed view is built from the
  data. In-memory clones carry no views, which item 401 tracks.
- PUT starts a `view-build` task that waits for the first evaluation on the worker,
  rather than running the evaluation in the task.

**Load-time checks.** `Runtime::open` and every attach check a definition as `PUT`
does. Every definition enters the runtime through `ViewEntry::new`, which applies
`ViewDef::check`, whether it comes from disk or from `PUT`. A `view.json` of format 1
that holds an option a later phase adds, such as `mode: graph`, a `graph` name,
`maintenance: commit`, `onFailure: reject`, `declassify: true`, other branches,
`keepCommits` or provenance, is listed as failed with `unsupported-option` and never
runs. Such a file can come from a hand edit or from a release that supports the option
at the same format. `onFailure` and `declassify` are part of the definition schema, so a
file that names them fails only its own view and does not make the whole set
unreadable. Their defaults, `suspend` and `false`, mean what Phase 1 does and run, and they are
not written to `view.json`, so a release that reads only the Phase 1 members still
reads the file. The
other views of the dataset attach and run, and defining the view again replaces the
file. A `view.json` of a format this release does not know, such as format 3, still
refuses changes to every view, so that a newer release's definitions are never
overwritten.

**Differential and recovery tests.** The differential tests of §11 draw queries from a
grammar-driven generator in `incr/gen.rs`. It covers the fragment of §4.1, the
aggregates and correlated `EXISTS` of §4.2 and the operators that compile to
`recompute` nodes. It leaves out `SAMPLE`, `GROUP_CONCAT` and orders with ties under a
`LIMIT`, since the engine may answer them in more than one way. The histories hold small and bulk transactions, transactions that add and remove
one quad, deletes by pattern and compactions. Each generated case runs over every quad,
and a second set runs as a random reader whose graph grants and protections by
predicate or by subject class mask the oracle's snapshot. A failing case is shrunk to a
short history. The runs are seeded, with 160 and 120 cases by default, and
`SPARKLES_INCR_GEN_CASES` and `SPARKLES_INCR_GEN_SEED` select longer runs.

A run of 4,000 cases of each kind found one circuit error. A `GRAPH` inside `GRAPH ?g`
compiled to the inner pattern alone and left `?g` unbound. Such a group is now a
`recompute` node whose footprint covers the outer graph. The run also found two queries
where the engine, the oracle, drops rows that the SPARQL semantics give and the circuits
keep. One is an equality filter between variables of two `UNION` branches above a join
that binds both. The other is a `BOUND` filter on a variable that `VALUES` leaves
unbound and a path binds. An ignored test in `incr/tests.rs` records both.

A later run found a second circuit error at seed 5897 (roadmap item 439). The compiler
turned `FILTER EXISTS` and `FILTER NOT EXISTS` into a semijoin or antijoin with the
inner pattern evaluated alone. A path that matches the empty path, such as `:p*`, `:p?`
or `:p{0,n}`, evaluated alone binds its free end only to nodes of the active graph.
`EXISTS` substitutes the outer row instead, and a zero-length step from a bound term
matches the term even when the graph does not hold it. So
`FILTER NOT EXISTS { GRAPH ?g { ?x :knows* ?y } }` kept a row whose `?x` no named
graph held, and the default graph, a constant `GRAPH` and an outer `VALUES` showed the
same fault. An `EXISTS` with such a path ending at a variable of the outer rows now
compiles to a `recompute` node that the engine evaluates by substitution. The compiler
version is now `f13-phase1-4`, so a checkpoint of an older compiler is refused and the
view is built again. `OPTIONAL` and plain joins keep join semantics and are unchanged.
`zero_length_paths_follow_the_oracle` checks seventeen of these queries against the
engine after every commit and against the answers of Jena's reference engine.

The recovery tests stop a child process with no cleanup at six points. These are after
a commit and before its pass, after a pass and before its checkpoint (failpoint
`view-pass-applied`), before a checkpoint's rename (`view-checkpoint-written`), while a
compaction builds its generation, before it switches `CURRENT`, and during the first
pass after the switch. After an open, every view loads its checkpoint, applies the
commits since and matches the oracle. A checkpoint interrupted before its rename used to
leave its temporary file behind, and the next checkpoint now removes it. A further test
fingerprints the variants of the operator enum and fails when they change while
`COMPILER` in `incr/runtime/key.rs`, which checkpoint fingerprints include, keeps its
name.

**Measurements.** See [BENCHMARKS](../benchmarks/writes/RESULTS.md#incremental-views). Three pinned forge
runs at 200,000 and 1,000,000 people put touching rounds near one query evaluation, with
median per-run ratios from 0.93 to 1.07. Unrelated commits skipped the view in at most
0.053 ms in the published medians. The materialized class-count view skipped 166 of its
200 measured commits. A matched persistent commit-path comparison remained inconclusive
after SSD trimming, with unsubscribed round medians varying from about 0.27 to 1.61 ms
within a build and the comparison reversing across interleaved groups. Four later
processes under syscall tracing located large variation inside aligned WAL and delta
vocabulary writes in both builds. Their blocks of 3,000 writes had median syscall times
from about 0.24 to 1.55 ms. Tracing affects absolute latency, so this locates the variation
without measuring a cost of the inference change. Item 265 tracks obtaining a comparison
with matched durable-write latency.


### Internal circuits

Item 414 delivers §3.6. Server code attaches a circuit with `Runtime::attach_internal`,
or `Dataset::attach_internal` in the library, from an `InternalSpec` and an
`InternalAccess`, and holds the `InternalCircuit` handle it gets back. The circuit is
maintained while a handle lives and is collected, after a final checkpoint, on the round
after the last handle is dropped. `incr/internal.rs` holds the types and the
accumulator's counts, and `incr/runtime/internal.rs` attaches, advances and checkpoints
the circuits.

- An `InternalSpec` names the circuit with an `id` that keeps its checkpoints, a query
  with full IRIs, a sink and a memory budget of 256 MiB by default. The sink is `Rows`,
  which `rows()` reads, or `Accumulator` with key columns. The spec is non-exhaustive, so
  that the provenance setting of item 415 and the fixed-point state of item 417 can join
  it without breaking server code.
- An accumulator's query is a SELECT without grouping whose result has the key columns
  and at least one other column, the changed fact's identity. A grouped query is refused,
  because its rows hold net weights and lose the churn. `counts(after)` returns the total
  per key of the commits after `after` up to the circuit's commit, and
  `discard_through(commit)` drops the counts that server code has acted on. The counts
  are kept per commit and key, so a read is exact for any `after` from the commit the
  counts start after, which is the commit of the first build or of the last discard.
  A read that reaches further back, or that spans a commit whose changes were not
  known, names the reason in `unknown`, and C22's refresh then falls back as it does
  for a commit the feed lists with counts only.
- `InternalAccess::everything()` reads every quad, including the graphs that a
  disclosure guard protects, and `InternalAccess::reading(access)` reads what a
  `GraphAccess` that server code resolved lets it read. The type has no default, no
  serialization and no conversion from anything a caller supplies. The access is fixed
  when the circuit attaches, and server code attaches again to follow a policy reload.
- Internal circuits live in a map of their own in the registry and never share a
  circuit with a view or live query, even one with the same query and access. They
  have no `view.json`, no entry in `GET /$/views/{ds}`, no rows in `SERVICE view:read`
  and no view label in the metrics, and listing, reading, rebuilding or deleting a view
  of the same name reaches only that view. `RuntimeStats` counts them separately as
  `internal_circuits`. They read the stored quads as the change feed lists them, and
  the dataset's inference does not apply to them.
- An accumulator's circuit keeps no result rows, only the state its operators need, so
  an accumulator over a pattern holds its counts and little else. Its first build still
  evaluates the query once. An internal circuit's state over its budget fails the
  circuit with `over-budget`. An accumulator's counts that pass the rest of the budget
  drop the oldest commits' counts first, with the keys and unknown ranges only they
  hold, which moves the commit the counts start after. The budget holds after each
  commit of a run, and a drop goes a quarter below it, so counts at the budget are
  dropped in batches. The counts' storage, unknown ranges included, is charged at its
  capacity. Each fit of the storage leaves it room to grow to the budget, so the counts
  that follow a drop are added without reallocation. A recovered checkpoint is held to
  the budget of the attach that loads it.
- Checkpoints are `circuits/{id}/main/state-{commit}.bin`
  (`formats::INTERNAL_CIRCUIT_STATE`, version 1), with the view checkpoint's header under
  the magic `SPKCIRC\0`. The body holds the circuit's state as a view checkpoint does,
  then the accumulator's starting commit, unknown ranges, keys as vocabulary keys and
  counts. They are written with the views' checkpoints and when a circuit is collected.
  A build always marks its state for the next one, so a build at commit 0 of a new store
  is checkpointed too. The fingerprint covers the compiler version, the id, the
  normalized query, the access, the sink and whether the default graph is the union of
  the named graphs, so a changed spec or setting builds the circuit again. View
  checkpoints do not cover the union setting, which item 507 adds. A file of
  another format is left in place and the circuit is built from the data, which a test
  checks. A checkpoint is never written over a file of another format at the same
  commit, for views as well.
- A circuit whose last handle is dropped stays registered while its final checkpoint is
  written, so an attach of the same id meanwhile joins it and keeps its counts.

**Ordering against commits.** Internal circuits are maintained after a commit is
published and never inside it, so they keep group commit. While one is attached, each
commit copies its changes into the change ring, as it does for a view or live query, and
item 226 measures that copy. A dataset without views, live queries or internal circuits
does neither. The argument that an accumulator counts every commit exactly once has six
steps.

1. A commit puts its changes in the ring of recent changes before it publishes its
   snapshot, so a worker that the publication wakes lists them. The ring is filled while
   any circuit is attached, internal ones included.
2. The worker advances an accumulator's circuit from its commit `c` to the head `h` one
   commit at a time. `DeltaSet::steps` lists commits `c+1` to `h` in order with each
   commit's own net changes. It reverses the run's net changes once with
   `Snapshot::before_changes` to get the snapshot at `c`, then builds each commit's
   snapshot forward from the previous one, so at most two derived snapshots are alive
   and the last commit uses the head's own snapshot. Commit `k` is applied against the
   snapshots of `k−1` and `k`, and its counts are added before commit `k+1` is read, so
   a later change cannot cancel them. Changes within one
   commit are net, as the feed lists them, and an addition with a removal of the same
   fact in one commit counts nothing in either counter.
3. The circuit's commit and its counts change together under the circuit's state lock,
   which readers also take. A read therefore sees every commit up to the circuit's
   commit counted and no later one.
4. A checkpoint holds the circuit's state and the counts at one commit `c`, written
   under the lock, synced, and renamed into place. An attach takes the state lock before
   the registry lists the circuit, so a round never builds it ahead of its recovery. The
   attach loads the newest checkpoint, and the next round counts commits `c+1` to the
   head from the ring or the change feed. Each commit is counted by the checkpoint or by
   the replay, never both. A crash before a rename leaves the older checkpoint, and the
   replay starts after its commit.
5. When the ring and the change log cannot list a commit after `c`, when a run lists
   more than 1,000,000 changes, or when a generation switch interrupts the listing, the
   circuit is evaluated again at the head and the range is recorded as unknown. A read
   that spans it says so instead of returning a low count.
6. A discard lives in memory until the next checkpoint. A crash before it brings back
   counts of commits that server code has already acted on, and a read after the
   discarded commit leaves them out, so the counts it asks for stay exact.

**Tests.** `incr/internal_tests.rs` compares the accumulator with a counter over the
change feed for churn across commits, a replacement within one commit, deletions, a
commit that adds and removes the same fact, and members who join and leave a team. Each
workload runs with a round after every commit and with one round for the whole run, and
the counts after every commit of the run are checked. A key that comes from a join is
checked against evaluations of the query at every commit. Further tests cover discards,
the rows sink, refused specs, restarts with the change log and without it, a checkpoint
of an unknown format, and crashes after a commit, after a pass, before a checkpoint's
rename and after a generation switch. The access test checks that no listing, read,
rebuild, removal, backup capture or metric reaches an internal circuit, that a view and
a live query of the same question have a circuit of their own, and that what callers do
to views leaves the counts exact. In the server, `graphrag/refresh.rs` checks the
accumulator against the counter that C22's refresh reads today for every community,
and `router_tests/views.rs` checks the routes, `SERVICE view:read` and the metrics, and
that no module under `http`, `mcp` or `auth` names `InternalAccess`.

**Write benchmarks.** `scripts/nsc-writes.sh --views none|async:N|commit:Q|graph:N`
defines views on every store before the writes. `async:N` defines N table views over the
writers' literal predicate, each its own circuit. `commit:Q` and `graph:N` are refused
until items 421 and 420 add commit-time maintenance and graph views.

**Measurements.** Namespace screens compared release builds of 04d49b57 and of the
item's code, both built on atlas, on 8-vCPU instances with 1.05M triples. The commit path
and the query path did not change. The query screen of 30 queries in four rounds gave a
geometric mean of 1.007. Its one flagged query, `two-hop-count`, took 1.048 times as
long over 12 rounds with every round between 1.02 and 1.08, which item 506 checks
on forge. Over six write rounds without views, `serial-literal` ran at 0.995 times the
transactions per second with a p99 of 1.013, and `writers` at 0.992 with a p99 of 1.028.
With four table views over the written predicate, four rounds gave 0.997 for
`serial-literal` and 0.978 for `writers`, with ranges that overlap. The raw runs are kept
under `.artifacts/nsc-bench` and `.artifacts/nsc-writes` of the lane's worktree.

### Provenance annotations

Item 415 delivers the algebra of §5.1 and §5.2 in `incr/prov.rs`, carried through every
operator of the circuit. Internal circuits that keep rows take it through
`InternalSpec::with_provenance`, and views do not take it until item 429 adds
`view:witness` and `view:why`.

- **Semantics.** Each row's provenance polynomial over the input quads is kept as its
  image under one of three homomorphisms, with signed integer coefficients so that a
  deletion subtracts what an insertion added. `count` is the number of derivations.
  `lineage` keeps the number of derivations that use each atom, together with the
  number of derivations, and a join combines them by inclusion and exclusion, which is
  bilinear and therefore exact under the delta rule. `why` counts the derivations per
  support, the set of atoms a derivation uses. Joins multiply annotations, and unions,
  projections, `DISTINCT` and groups add them. `EXISTS` multiplies a left row's
  annotation by the sum of its matches'. `count` therefore differs from the Z-set weight
  above `DISTINCT`, `EXISTS` and groups, and it is the weight elsewhere.
- **Atoms.** A quad is named by its `FactKey`, the first 128 bits of SHA-256 over the
  vocabulary keys of its four terms, each framed by its length, with the default graph as
  no key. A triple of the union default graph is represented by the visible named-graph
  quad with the smallest key among those that hold it, so its witnesses name a stored quad
  and many graphs do not multiply a row's supports. When that quad goes while the triple
  stays, the annotation moves to the next representative without a change of weight. The
  lineage of such a triple therefore names one graph and not every graph that holds it.
  Keys do not
  change at a generation switch, so the remap of a compaction leaves the annotations as
  they are. A key resolves back to its quad through the circuit's snapshot by matching
  the circuit's patterns (`resolve_facts`). A row that `MINUS`, `NOT EXISTS` or an
  unmatched `OPTIONAL` keeps is multiplied by an `Absence` atom. It holds the operator's
  number in the circuit's preorder and a 128-bit hash of the shared variables' names and
  values, ordered by name. An `OPTIONAL` with a filter binds every left variable, since
  the filter can read them all. An implicit group without rows has the absence of its pattern
  as its witness. `conditions()` describes each number's operator and pattern.
- **Bounded witnesses.** A `why` annotation in an operator's state keeps at most
  `maxWitnesses` supports, 8 by default and 256 at most, and counts the other
  derivations as unknown. Every state passes on its effective change, the change of what
  it keeps, so a support that does not fit, or that a smaller one displaces, reaches
  later operators as a move to the unknown count. A removal of a support the state does
  not keep is taken from the unknown count. Two derivations with the same row and support
  are indistinguishable downstream, so two rules hold at every operator. A kept count is
  never more than the row's derivations with that support, and an unknown count of zero
  means the supports are exact. A reported witness is therefore always real, `truncated`
  is set exactly when the unknown count is not zero, and a row is exact again once its
  unknown derivations are removed. A build is exact for a row whose supports fit the
  bound and whose atoms are known, and otherwise keeps the smallest supports. A build keeps the
  smallest supports first. A report lists the minimal kept supports, smallest first. This
  is the convergence rule that item 416 adopts for the reasoner.
- **Where atoms are unknown.** The rows of a `recompute` node, including a whole query
  evaluated again for inference or protections that depend on the data, and the triples
  of a CONSTRUCT template with blank nodes have unknown atoms. They report `count` and
  `truncated` with no witnesses or lineage. A template mints nodes per occurrence of a
  solution, so its first occurrence's triples carry the derivations beyond one per
  occurrence, and the triples of all occurrences together count the solution's
  derivations. Item 528 tracks witnesses through them.
- **State.** Each stateful operator keeps the annotations beside its weights, in a map
  from row to annotation that exists only when the circuit has a provenance setting. A
  pattern's matches are probed from the store with the quads they matched. A circuit
  without provenance runs the operators' code of Phase 1 unchanged, and its passes do
  not touch the annotation code. Annotations count against the circuit's memory budget.
- **API for items 416 and 417.** `Ann` holds a value of a semiring with `zero`, `one`,
  `derivation`, `unknown`, `add_assign`, `mul`, `scale`, `absorb` and `report`.
  `Witness`, `Atom`, `FactKey` and `Absence` are public, so the reasoner records rule
  firings as products of its premises' annotations and absorbs them with the same bound.
  The fixed-point operator of item 417 cannot weight recursion by counts (§4.3), and it is
  expected to keep `Witness` sets by rederivation and use `report` for minimality.
- **Checkpoints.** `INTERNAL_CIRCUIT_STATE` gains format 2, whose circuit state ends with
  the annotations of every operator and of the result. A circuit writes format 1 without
  provenance and format 2 with it, and reads only its own. A file of the other format is
  left for the next checkpoint to remove, and a file of an unknown format is left in place
  while the circuit is built from the data. The fingerprint covers the setting and its
  bound. `VIEW_STATE` keeps format 1.

**Tests.** `incr/prov_tests.rs` draws random queries from joins, unions, `OPTIONAL`,
`MINUS`, `FILTER EXISTS` and `NOT EXISTS`, `DISTINCT`, projections and grouped and implicit
`COUNT`, renders each as SPARQL and evaluates it by brute force with its exact `why`
polynomial. It applies random histories with insertions, deletions, a quad added and
removed in one commit, replacements, passes over several commits and compactions, and a
third of the cases read two named graphs as the union default graph. After
every pass it compares weights, `count`, the exact `lineage` and the `why` supports with
bounds of 8, 2 and 1, checking the two rules and, without truncation, the exact supports
and the minimal report. The default run has 120 cases, and runs of 2,600 cases over the
default graph and 600 cases with the union default graph pass.
`SPARKLES_PROV_CASES` and `SPARKLES_PROV_SEED` select longer runs. Unit tests in
`prov.rs` check the products, the bounded state against an exact one on random changes,
and the keys. Other tests check truncation and convergence on one row, negative
conditions, the state encoding of every mode, fact resolution, and a format 2 checkpoint
across a restart. Regressions from the review cover witnesses over the union default
graph, many graphs holding one triple, `OPTIONAL` filters, template blank nodes, a
circuit whose provenance is turned off, and cancelled fact resolution.
Measurements found and fixed two costs. A state's annotation map keeps the total size of
its annotations, so the memory a round reads does not visit every row. An annotated build
reads the pattern with the fewest estimated quads next, and it estimates each scan prefix
once per pass. Tests check the size against a recomputed sum, the build's scans on a
large team and the number of estimates.

**Measurements.** On an 8-vCPU Namespace instance at 200,000 people, the median annotated
pass of three runs was at most 0.025 ms above the plain one for each of the five views of
the incremental benchmark.
`lineage` makes a state 4 to 80 times larger and `why` 4 to 8 times, and `count` doubles
the state of a view with 200,000 output rows. This memory buys exact deletions without
evaluating the query again. An annotated build matches its patterns with index probes and
takes 1.3 to 10 times as long as a plain build, and 240 times as long, 2.2 seconds, for a
three-pattern join that the plain build evaluates with the engine's joins. Views without
annotations kept their pass costs, apart from 2 µs on one view whose plain operators did
not change. A query screen of the build before the measured fixes, whose later commits
change only incremental circuits, gave a geometric mean of 0.987. The
[writes benchmark](../benchmarks/writes/RESULTS.md#provenance-annotations) has the tables,
and 529, 530 and 531 track the forge checks, the plain pattern
order and faster annotated builds.

### Fixed-point operator

Item 417 delivers the fixed-point operator of §4.3 in `incr/fixpoint.rs`, maintains
property paths with `+` and `*` through it in `incr/ops/path.rs`, and plugs the reasoner's
forward rules into it in `sparkles-reasoner/src/fixpoint.rs`.

- **Interface.** A `Program` names its derived facts, the base facts its rule instances
  read, and the instances themselves through `axioms`, `consequences`, `prove`, `broken`
  and `added`. `FixedPoint` keeps every derived fact with a record, the one instance that
  derived it first. Records are well founded, since an instance is recorded only over
  facts that held before it, so following them from any fact ends at base facts and no
  fact in a cycle supports itself.
- **Maintenance.** A pass deletes by backward and forward rederivation. The facts whose
  recorded instance read a removed base fact are broken, and they and the facts whose
  records depend on them are the candidates. Each candidate is first checked backward for
  an instance at the new snapshot over facts that are not candidates. The instances over
  added base facts then derive new facts, and forward chaining from everything kept or
  derived proves the remaining candidates that still follow. The candidates left are
  deleted. A fact with another derivation is never deleted and derived again, so the
  operator's change holds only the facts that come and go, and a fact whose record is
  intact is never checked.
- **Paths.** A path `s step+ o` or `s step* o`, its inverse `^`, under `GRAPH` with a
  constant or a variable, compiles to the operator when the step is a simple path, an
  alternative of at most 16 sequences of IRIs and inverses, and each end is a variable,
  an IRI or a literal other than the graph's variable. The facts are (graph, start, end),
  recorded by the node before the end. With two variable ends every subject of a step
  starts a walk, and with a constant end the walk starts there, backward for a constant
  object. A pass finds the starts that reach a changed step by walking the steps backward
  from it at both snapshots, so the step relation is probed from the store's
  permutations and never copied. The walks a pass memoizes count against the budget
  and are released when the pass ends. `*` adds the zero-length rows without state. They are
  the nodes of the active graph for two variable ends, and for a constant end the
  constant itself in each graph that exists under the reader's access, as the query
  engine evaluates them, so a graph whose triples a protection hides does not exist. A path with `?`, a negated property set, a nested `*` or `+`, or
  a blank node end is still a `recompute` node, and `explain` names the reason.
- **Reasoner plug-in.** `RuleProgram` runs the forward Jena rules whose bodies are
  triple patterns over at most four variables and whose heads use only body variables,
  and refuses the others. Its facts are triples, indexed by predicate, by predicate and
  subject, and by predicate and object, and a record names the rule and its variables.
  Constants that the store lacks, in bodies and heads, are kept as local terms, so a
  rule matches the triples another rule derives with them, and a pass that first meets
  one in the store builds the program again. `RuleProgram::encode` and `decode`
  checkpoint a fixed point with those terms. The reasoner's own maintenance is
  unchanged. Moving it onto the operator is item 456's shared maintenance core, after item
  547 measures the plug-in on LUBM.
- **Provenance.** With a provenance setting a fact counts one derivation, its recorded
  one, and its support is the union of the instances' supports along its record chain,
  so the witness it reports is real. A fact whose record changes, and the facts recorded
  under it, are annotated again in the pass. A path row of `*` without a step is
  annotated by a quad of the active graph that names its node, and a constant end's row
  is `one` in the default graph and unknown in a named graph, whose existence no single
  quad witnesses. Rows report one witness rather than all of them, and complete witness
  sets for fixed-point rows are item 543.
- **Budget and state.** The operator counts about 40 bytes for each fact, and its rows
  are counted again by an operator above it that keeps them, such as a join's
  arrangement. Annotations count as they are made, and the budget is checked again at
  the end of every build and pass. A build or pass that takes the circuit over its memory budget stops with
  `BudgetExceeded`, and the view is `suspended` with `over-budget` as §3.5 describes for
  every operator. This memory buys
  passes that touch only the walks a change reaches.
- **Checkpoints.** `VIEW_STATE` gains format 2, whose body ends with the operators'
  fixed-point state, and `INTERNAL_CIRCUIT_STATE` gains formats 3 and 4, which add it to
  formats 1 and 2. A circuit writes the lowest format that holds its state and reads only
  its own, so a view without a path keeps format 1 and the format that older releases
  read. A file of another known format is left for the next checkpoint to remove, and a
  file of an unknown format is refused and left in place while the view is built from
  the data. `COMPILER` is `f13-phase3-1`, so views that recomputed a path are built again
  as maintained views.

**Deviations.** Paths in a sequence with other steps, such as `:a/:b+`, are not compiled
to the operator, and neither are `?` or nested closures, which item 544 tracks.
A view's circuit sees only the commits its reader can read, so a constant end's
zero-length row over the union default graph can stay stale when a named graph the reader
cannot read gains or loses its last quad, until the next commit the reader sees. Item
545 tracks it. A path whose ends are both variables walks from every start,
and seeding walks from the bindings of the patterns joined with it is item 546.

**Tests.** `incr/fixpoint_tests.rs` builds random graphs over a few nodes and two
predicates with cycles and self-loops, and applies random histories of insertions and
deletions to views over 37 path queries. The queries cover `+` and `*` with variable and
constant ends, inverse and alternative steps, `GRAPH` with a constant and a variable,
the union default graph, joins, `OPTIONAL`, `MINUS`, `EXISTS`, paths inside
`FILTER NOT EXISTS` with and without `GRAPH`, a group, `DISTINCT` and `CONSTRUCT`. After every pass the rows are compared with the query evaluated
again. Other tests delete the edges that close cycles and check that exactly the rows
that no longer follow go, check the encoding across compaction, the memory budget, and
that every reported `why` witness derives its row in each provenance mode, and compare
zero-length rows under graph access and the union default graph. `runtime_tests.rs`
restarts a path view from a format 2 checkpoint and refuses a checkpoint of the other
format, and the generated circuit tests of Phase 1 now include path views and pass 3,000
cases plain and masked. `sparkles-reasoner/src/fixpoint/tests.rs` maintains RDFS-style
rules with cycles of classes and a symmetric relation against a naive closure, and checks
that every witness derives its triple. Regressions from the review cover protections that
hide every triple of a named graph, constants that only derived triples hold, a pass
over many steps that reach nothing under a small budget, a rule body that repeats one
premise, annotations over the budget, query variables named like the step's own, and a
checkpoint of rules with local constants.

**Measurements.** On an 8-vCPU Namespace instance at 200,000 people, four path views of
the incremental benchmark passed in 0.02 to 0.24 ms at the 95th percentile, against 2.3 to
82 ms when the same views evaluated their paths again, 100 to 2,200 times less. The state
was up to 11% larger than a recomputed view's, and the builds of the two views with a
constant end took 2.6 and 3.3 times as long, while the other two built faster. Item
548 tracks faster builds. The five plain views kept their pass costs within the
spread of five processes. Overdeleting as DRed does, which was measured and reverted,
made a deletion inside a large `knows` component cost 441 ms, and
[PERFORMANCE-EXPERIMENTS.md](../PERFORMANCE-EXPERIMENTS.md#incremental-views) records
it. The [writes benchmark](../benchmarks/writes/RESULTS.md#property-path-views) has the
table. Item 417 changed no code of the query engine, so no query screen ran.

### Aggregates and correlated EXISTS

Item 418 delivered the operators of §4.2 other than the ordered windows of item 419 on
2026-11-10. `MIN`, `MAX`, `GROUP_CONCAT`, `SAMPLE` and a correlated `EXISTS` are
maintained, and `HAVING` follows the aggregates it reads.

- The group operator of `incr/ops/group.rs` keeps the values of a `MIN`, `MAX`,
  `GROUP_CONCAT` or `SAMPLE` as an ordered multiset: a vector of the group's distinct
  values in the order of `ORDER BY`, each with its count, 16 bytes per distinct value
  and 40 bytes per group. Values the order does not tell apart, such as
  `"01"^^xsd:integer` and `"1"^^xsd:integer`, form a run in the order they entered the
  group, which a checkpoint and a generation switch keep because the order depends on
  the terms and not on their ids. One state serves the four aggregates with and without
  `DISTINCT`. Adding or removing a value finds its place with a binary search that
  decodes about the logarithm of the group's distinct values and shifts the entries
  after it. `MIN` and `MAX` read the ends, so removing the extreme value costs the same
  as any other. A hash map of counts would take the same memory and make removing the
  extreme value scan the group, which the vector trades for the shift of its entries. A
  group of ten thousand distinct values costs about fourteen decodes and a move of at
  most 160 KiB per changed value.
- `MIN` and `MAX` leave out an unbound or failed argument, as the engine's running
  aggregates do, and among values the order does not tell apart both report the one that
  entered the group first, as the engine keeps the first of equal values. `GROUP_CONCAT` joins the group's values again for each changed group,
  which is the recompute per changed group that §4.2 allows, in the multiset's order,
  each value as often as the group holds it or once with `DISTINCT`. The engine joins
  the values in the order its rows arrive, so a view's string and a one-off query's
  hold the same parts in a possibly different order, and item 905 tracks one order for
  both. An unbound or failed argument, or a value without a lexical form, makes it
  unbound, as in a one-off evaluation. `SAMPLE` keeps the value it reported while the
  group holds it after the pass, so a replacement of its row within one commit keeps it,
  and then takes the smallest.
- `HAVING` is a filter over the group's rows, which the compiler maintained already for
  `COUNT`, `SUM` and `AVG`, so a `HAVING` over `MIN` or `MAX` is maintained now too.
- A correlated `EXISTS`, which reads an outer variable that no join key carries, such as
  one in a filter of its pattern or one the outer rows do not always bind, an `EXISTS`
  whose `*` or `?` path ends at an outer variable, and an `EXISTS` inside an expression
  compile to the operator of `incr/ops/exists.rs`, which the plan names `ExistsFilter`.
  It keeps each input row with its weight and the condition's last answer. It evaluates
  the condition with the query engine, with the row's bindings substituted as a one-off
  query substitutes them, so the answer is the engine's own. The expression context of
  a pass reads the snapshot as the circuit's principal reads it, through the same grant
  binding, masking and dataset restriction as a one-off query, and keeps the pass's
  deadline, cancellation and memory budget. A pass whose changes miss the condition's
  footprint keeps every answer. A pass inside it matches each changed quad against the
  condition's triple patterns, and a match binds the outer variables the pattern names,
  so only rows with those bindings, or without them, are evaluated again. A pattern
  without an outer variable, a property path, a property function, one of ARQ's special
  graphs, a `GRAPH` whose body can match without a quad or that holds another `GRAPH`,
  `SERVICE` or an extension sends every row to the engine again, which the plan reports
  as `every row on a change`. The variables of a subquery inside the condition count as
  outer only when the subquery projects them, as the engine substitutes them, and the
  expressions of aggregates, `FOLD` and `ORDER BY` inside the condition are read for
  nested `EXISTS`, by the footprint as well as by the probes.
  Finding the rows a change touches scans the operator's rows, and item 906 indexes
  them when a measurement asks for it.
- An `EXISTS` in a `BIND`, in an `OPTIONAL` filter or in an aggregate's argument, and a
  correlated `EXISTS` under `GRAPH`, are still `recompute` nodes, which item 904 tracks.
  Rows a correlated `EXISTS` keeps carry unknown derivations under a provenance setting,
  as the rows of a `recompute` node do, since the engine does not report why the
  condition holds, and item 528 covers them.
- The state encoding of the group operator gained the multiset, and `COMPILER` is
  `f13-phase2-1`, so a checkpoint written by an earlier release, whose circuit held these
  aggregates or conditions in `recompute` nodes, is refused by its fingerprint and the
  view is built again. The checkpoint formats are unchanged.
- The differential tests gained the aggregates with and without `DISTINCT`, over
  expressions and unbound variables, `HAVING` over `MIN`, and five correlated
  conditions, through fixed and random histories, checkpoints and a compaction, and a
  test that compares `GROUP_CONCAT` up to the order of its parts and checks that a
  `SAMPLE` is a member of its group and stays while it remains. The generator draws
  `MIN`, `MAX` and a filter over outer variables inside `EXISTS`.

The ignored test `aggregate_pass_against_recompute` of
`crates/sparkles-core/tests/incr_bench.rs`, which `mise run bench:incr` runs with the
other view tests, measures four such views over the benchmark's people data, and the
[writes benchmark](../benchmarks/writes/RESULTS.md#aggregates-and-correlated-exists-in-views)
has its table. Item 418 changed no code of the query engine, and the writes screen with
four table views ran as [ROADMAP](../ROADMAP.md) item 17 asks.

### Maintained assessments and conservative dependencies

[F22](F22-analysis-lifecycle.md) defines the lifecycle of maintained analysis results.
Roadmap item 17 supplies incremental graph views and per-fact provenance where an
executor can preserve their semantics. Existing Phase 1 table views remain unchanged.
F22 can initially recompute through [C26](C26-durable-jobs-and-delivery.md) without
waiting for the later incremental phases.

A read footprint must include insertions that can change a negative result, aggregates
and schema-dependent rewrites. The citations on a returned row are insufficient to
identify those dependencies. Circuits publish their checked frontier and report a gap
or an unsupported operator as unknown freshness or full recomputation. They cannot
advance a frontier merely because every cited fact still exists.

Later-phase provenance must identify exact dataset and branch commits and persisted
assertion occurrences, survive compaction without reusing vocabulary IDs as evidence
identities, and retain alternative supports within explicit bounds. Derived visibility
continues through F18. A proof that an incremental result equals full recomputation is
a prerequisite for replacing F22's fallback. External model runs and unsupported
nonmonotone analyses retain that fallback.

Recovery tests must cross input publication, durable job creation, partial circuit
maintenance and guarded result publication, checking that a result cannot remain
labelled current while an unexamined relevant input change is pending.

### Dataset shutdown and directory ownership

The final dataset handle synchronizes shutdown with worker acquisition and any round
already in progress. A separate shared lifetime guard counts dataset handles without
counting temporary worker or host references. Worker cleanup finishes before its store
reference and worker gate are released.

Synchronous host callbacks retain an owned close action until their enclosing runtime
operations release their metadata locks. Operation guards nest across runtimes and drain
those actions outside the thread-local borrow, including during panic unwind. Readers
acquire no shared lifecycle lock. Host replacement also defers cleanup from the old
host's destructor.

Three causal regressions fail against the earlier implementations. One pauses a host
while it holds a temporary third state reference. Another pauses worker acquisition
before it obtains a store reference. The third releases the final handle from a
synchronous inference callback while listing views under the registry lock.

Thirteen regression tests cover those cases, cloned handles, concurrent final drops,
worker and synchronous panic, worker store destruction and host replacement. A callback
matrix exercises 15 synchronous operation and callback combinations. Nested operations
cover both the same runtime and different runtimes. Operation tracking has no
thread-local destructor. A subprocess regression keeps earlier store-access bookkeeping
alive while a later thread-local owner's destructor lists views and releases the final
handle, then checks that the store reopens.

All 57 targeted incremental view tests pass. The GraphQL feature configuration passes
all 776 tests. Twenty repetitions of all six HTTP group commit tests pass, including
the persistent reopen assertion that exposed the directory lock race. The worker
callback case that reopens through a retained state and the thread-local destruction
case each pass 100 repetitions.

### Scope changes

**RSP-QL windows and complex event processing.** The non-goal in §1 excluded stream reasoning and windows over time, as in RSP-QL or C-SPARQL, so that a commit stayed the only unit of change. RSP-QL windows and complex event processing are now planned under [ROADMAP](../ROADMAP.md) item 467 in this spec's Roadmap follow-ups.

### Roadmap follow-ups

#### Operations and streaming workstream

These phases belong to [ROADMAP](../ROADMAP.md) item 467, the operations and streaming workstream. Each phase gets its own numbered item when it is ready to start.

**RSP-QL windows and CEP.** Add tumbling and sliding windows over event time and CEP patterns. Live queries exist without windows. It depends on items 341 and 419.

**Time-series values and downsampling.** Store compact time-series values or a per-predicate column store with downsampling and retention aggregates. No time-series support exists. It depends on item 341.
