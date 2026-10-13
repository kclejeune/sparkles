# X10: Reusable query plans

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 splits the planner's state out of the execution
> context and adds a cache of parsed queries keyed by their exact text. Phase 2 adds the
> cache of parameterized plans with the estimate check, invalidation and metrics. Phase 3
> adds explicit prepared queries in the Rust, Python, JVM and Node APIs and plan reuse for
> stored queries.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

This design was written from the Sparkles code at `fc123d04` (the planner in
`crates/sparkles-core/src/sparql/plan.rs` and `joinorder.rs`, the query context in
`ctx.rs`, query preparation in `sparql/mod.rs`, the result cache in `sparql/cache.rs`, the
foreign function layer in `crates/sparkles-ffi/src/query.rs`, the Python and Node
bindings and stored queries in `crates/sparkles-core/src/stored.rs`), from the plan cache
sketched in the Outcome of [P01](P01-python-bindings.md) and the measurements in the
Outcomes of P01, [P04](P04-jvm-bindings.md) and [P05](P05-node-bindings.md), and from the
specs [C16](C16-stored-queries.md), [C12](C12-graph-access-control.md),
[C12b](C12b-triple-access-control.md), [C13](C13-automatic-compaction.md),
[P03](P03-query-extensions.md) and [X05](X05-streaming-query-execution.md). The
public documentation of prepared statements and plan caching in PostgreSQL (`PREPARE`,
and the generic and custom plans of `plan_cache_mode`) and Jena's `ParameterizedSparqlString`
and `QueryExec.substitution` informed the API, without code from either.

## 1. Summary, goals, non-goals

Every query that Sparkles runs is parsed and planned again, even when the same text or
the same shape with other constants ran a moment before. There is no plan cache of any
kind. For small queries the fixed cost dominates:

* In Rust, the small `VALUES` query of the bindings benchmark takes about 65 µs, of which
  planning is about 45 µs and parsing about 10 µs (P01 Outcome). Join ordering is about
  half of planning, mostly allocation and cloning of plan nodes.
* Through Jena, Sparkles' parsing, planning and evaluation take about 40 µs of the
  `VALUES` query, and parsing and planning are about 50 of the 62 µs the engine spends on
  such a query (P04 Outcome). The JVM's `FfiQuery::prepare` only copies the text and
  options, and `FfiQuery::execute` calls `sparql::query` with the text each time.
* Through Node, a small query takes about 137 µs hot against about 62 µs in Rust, and 69%
  of the user-space cycles are parsing, planning and evaluation (P05 Outcome).
* Initial bindings are planner constants. `prepare_query` in `sparql/mod.rs` interns each
  bound term and inserts it into `Planner::subst`, so a plan is built for the values.

This spec makes plans reusable across executions with different constants, while
keeping the plan the planner would have chosen whenever the constants change the
estimates enough to matter.

**Goals**

1. Skip parsing for a repeated query text, and skip planning for a repeated query shape
   whose constants fall into the same estimate classes.
2. Treat constants in triple patterns, `VALUES` rows, initial bindings, stored-query
   parameters and the bindings of prepared queries alike, as parameter slots.
3. Re-plan when a constant would change the planner's choice: when its count falls into
   another power-of-two class, when it is absent from the store, or when the data's
   generation changed.
4. Bound the cache's memory, and invalidate entries on compaction, on changes to the
   settings that alter planning, and on changes of access view.
5. Offer explicit prepared queries in Rust, Python, the JVM and Node, with execution
   that takes a map of bindings.

**Non-goals**

* Caching results. The result cache (`sparql/cache.rs`) stays as it is and keeps keying
  on the instantiated plan.
* Parameterizing constants inside expressions, such as a string in `REGEX` or a number
  in `FILTER(?x > 5)`. They stay part of the key (§3.2), except where §3.3 says otherwise.
* Caching update plans. Updates keep their path.
* Sharing plans between processes or persisting them.

## 2. User-visible behaviour

### 2.1 Implicit caching

Every query through `sparql::query`, `Dataset::query`, the HTTP endpoints, the CLI and
the bindings consults the cache. Nothing changes in the answers. `QueryResult::timing`
gains `plan_cache: Miss | Text | Shape | Replanned(reason)`, which the Sparkles JSON
format reports as `meta.timing.planCache` and the access log as `plan_cache`. The plan
explanation states when a plan was reused and with which estimate classes.

`QueryOptions::no_plan_cache` and the request parameter `noplancache` bypass the cache
for one query, as `nocache` bypasses the result cache.

### 2.2 Prepared queries

| API | Form |
|---|---|
| Rust | `Dataset::prepare(text) -> Result<PreparedQuery>`; `PreparedQuery::query(bindings, options)`, `::select_cursor(bindings, options, cursor_options)`; `sparql::prepare(snapshot, text, options)` |
| Python | `ds.prepare(text) -> PreparedQuery`; `pq.query(bindings=None, **options)`, `pq.cursor(...)`; `pq.variables`, `pq.parameters` |
| JVM | `FfiDataset::prepare_plan(text, options) -> FfiPlan`; `FfiPlan::bind(names, values) -> FfiQuery`; in Kotlin `SparklesDataset.prepare(query)`; Jena's `QueryExec.substitution` and `ParameterizedSparqlString` reach the implicit cache |
| Node | `dataset.prepare(text)` resolving to `PreparedQuery` with `query(bindings?, options?)` and `cursor(...)` |

A prepared query parses once and plans on first use. Its parameters are the variables it
is executed with, and any variable may be bound. Binding a variable that the query does
not mention is an error, as it is for C16's stored queries. A prepared query is tied to
its dataset handle and reads the current snapshot at each execution, or the snapshot of
the transaction it runs in.

### 2.3 Server

`--plan-cache-mb` (default 16) bounds the cache for the whole server, shared by every
dataset as the caches of [X08](X08-disk-spill-and-server-memory.md) are. 0 turns it off.
Metrics:

| Name | Type | Labels |
|---|---|---|
| `sparkles_plan_cache_hits_total` | counter | `dataset,kind=text\|shape` |
| `sparkles_plan_cache_misses_total` | counter | `dataset` |
| `sparkles_plan_cache_replans_total` | counter | `dataset,reason=class\|absent\|generation\|bound` |
| `sparkles_plan_cache_bytes`, `…_entries` | gauge | – |

## 3. The cache key

### 3.1 Two levels

* **Text level.** The exact query text, the base IRI, the prefixes passed with the query,
  and the parser's options (the custom aggregate IRIs of the registry) map to the parsed
  `spargebra::Query` and its shape key. A hit skips parsing.
* **Shape level.** The shape key (§3.2) maps to a cached plan (§4). A hit skips
  planning when the estimate check (§5) passes.

Both levels live in one cache weighted by bytes. Text entries are small and point to
shape entries, so many texts that differ only in constants share one plan.

### 3.2 The shape key

The shape key is a canonical string built from the algebra, in the way `cache::raw_key`
builds result-cache keys, with these replacements:

* Each constant IRI, literal or triple term in a triple pattern's subject, predicate,
  object or graph position becomes a slot `$n`, numbered in the order of a depth-first
  walk. Whether a position is a constant or a variable stays in the key, because it
  decides the index permutation.
* A `VALUES` block keeps its variables and its row count, and each cell becomes a slot.
  An `UNDEF` cell stays `UNDEF` in the key, because it changes the block's shape.
* Each initial binding becomes a slot named by its variable, `$?name`. This covers
  `QueryOptions::initial_bindings`, C16 parameters (bound through `stored::Definition::bind`), the
  bindings of prepared queries, and Jena's substitution.
* Generated variable names are replaced by positions, as `CacheKey` does.

Constants elsewhere, in expressions, `LIMIT` and `OFFSET`, property path operators and
SERVICE IRIs, stay literally in the key. The same predicate as a constant in a pattern
and in an expression is a slot in the first place and text in the second.

Two exceptions keep the number of distinct keys low where the planner does not look at
the value:

* A literal compared in a `FILTER` with a variable that a range scan reads (`RangeSpec`)
  is a slot, because the range pushdown takes its bounds from the value at execution.
* `LIMIT` and `OFFSET` stay text, because top-k and LIMIT propagation plan on them.

### 3.3 Planning context in the key

A plan depends on more than the query. The key also holds:

* the dataset's namespace (X08) and the generation's `uid`;
* the query dataset after protocol overrides and defaults (`DatasetSpec`), the union
  default graph flag and `default_graph_extra`, which carries the inference overlay;
* the RDFS-on-read schema's identity (`QueryOptions::rdfs`), the access view (the
  `GraphAccess` identity of C12 and the mask key of C12b, as `raw_key` uses it), and the
  `Optimizations` in effect;
* the extension registry's identity when the query names registered functions, property
  functions or aggregates (P03), and otherwise nothing;
* the spatial index's epoch and configuration hash (`geo_view_key`), and whether the
  dataset has full-text and vector indexes, since rewrites depend on them;
* the DESCRIBE strategy for DESCRIBE queries.

The commit is not in the key. Deltas change the data without changing the plan's
validity unless the estimates move, which §5 checks.

## 4. Plan state and execution state

### 4.1 The split

`Ctx` holds both what the planner writes and what execution needs. A cached plan must
not carry execution state, and an execution must not mutate a shared plan. `Ctx` is
divided:

| Plan state (`PlanState`, shared, immutable after planning) | Execution state (`Ctx`, one per execution) |
|---|---|
| The variable table (`var_names`) | Deadline, cancellation, budgets and their counters |
| The query dataset and the base IRI | The cursor owner and cursor accounting |
| The characteristic-set registrations (`stars`) | Warnings raised during execution, the failed plan |
| Filter samples (`sampled`), key probes (`probes`) used to plan | The query-local vocabulary and minted blank nodes |
| The RDFS schema, the access view, extension resolution | Expression and decoding caches, `NOW()`, the geometry memo |
| Plan-time warnings | The parameter values (§4.2) |

`Ctx` gains `plan: Arc<PlanState>`, and the fields move behind it. The planner writes into
a `PlanState` under construction and freezes it into the `Arc`. A variable that execution
creates (for example a hidden variable of a rewrite run at execution) goes to an overflow
table in `Ctx` numbered after the plan's variables. Phase 1 finds every such site by making
`var_names` read-only after the freeze and running the test suites.

### 4.2 Parameters in the plan

A cached plan is a `plan::Node` tree in which the ids of slots are placeholders, and a
list of patch sites: for each slot, the places where its id appears (a `ScanSpec::prefix`
entry, a `RangeSpec` bound, a `Kind::Values` cell, a `GraphFilter` graph). Instantiating a
plan for an execution:

1. interns each slot's term in the new `Ctx` (a vocabulary lookup; a term the store lacks
   gets a query-local id, as `intern_outside_term` gives it today);
2. clones the tree and writes the ids into the patch sites;
3. renders `Node::desc`, which holds a template with the slot markers, only when the plan
   is explained or when the result cache needs a key.

Cloning a tree costs far less than planning it: it allocates the same nodes but does no
estimation, join enumeration or sampling. Phase 2 measures whether a tree that refers to a
parameter table at execution instead of being cloned is worth its intrusion into every
operator. It is not part of this design.

Initial bindings stay semantically as they are. A bound variable is still replaced
everywhere, so a plan built with `$?name` as a slot is the plan of the substituted query.
Projected variables still report the bound value.

## 5. Reuse and re-planning

### 5.1 The estimate check

For each slot in a scan, the cached plan records the count class it was planned with: 0
for a count of zero, and otherwise `1 + floor(log2(count))`. The count is the one the
planner already takes for the scan's prefix with `Snapshot::count` or `estimate`, which
are logarithmic over the base's block metadata and the delta's counted sets since C13's
"Small queries over the delta" change.

On a shape hit, the instantiation counts the prefix of each parameterized scan with the
new ids and compares classes:

* All classes equal: the plan is reused.
* A count of zero: re-plan, because the planner turns such a scan into an empty node,
  which may empty the whole query.
* A class differs: re-plan. The new plan replaces the cached one when the new class is
  the more frequent of the two among the last 16 instantiations of this key, so that a
  key used with two populations of constants does not thrash. Each entry keeps up to four
  plans, one per distinct class vector, and picks the one that matches.

For `VALUES` slots the check applies to the key probes the planner made with the block's
values (`keyprobe.rs`): the plan records the class of each probe's count, and the probes
run again with the new values. When the planner sampled a filter (`sample.rs`) whose
conjunct contains a slot, the plan is reusable only with the same value for that slot,
which then joins the key. Samples are too costly to repeat on every hit.

### 5.2 Generation, statistics and settings

* **Compaction.** A compaction makes a new generation with a new `uid`, and its
  statistics are rebuilt (C13 §10.2 updates them in a partial compaction). Entries keyed
  on the old `uid` stop matching and age out. The store clears its namespace's shape
  entries at the switch to free the memory at once.
* **Delta statistics.** The delta's per-predicate statistics change with each write.
  They feed the estimates the class check reads, so a plan stays valid while the counts
  stay in class.
* **Settings.** A change of the dataset's query defaults (inference, RDFS on read, the
  union default graph, DESCRIBE, prefixes under C20) changes the key, since the key holds
  their values. A change of the dataset's spatial, full-text or vector configuration bumps
  the epochs in the key.
* **Branches and history.** A branch has its own generation and namespace. A query
  `?at=` a past state runs on that state's generation, whose `uid` is in the key.

### 5.3 What is not cached

* Queries with `SERVICE` to a remote endpoint are cached, because the remote call happens
  at execution. Queries whose planning calls a SERVICE (none at `fc123d04`) would not be.
* Plans larger than 256 KiB are not cached.
* A query with a registered extension is cached only under the registry's identity, so a
  per-query registry never hits.
* Plans built while the planner raised a dynamic warning that depends on data, such as
  `geo-index-building`, are not cached.

## 6. Memory bound

The cache is a `quick_cache` weighted by the bytes of each entry: the key, the parsed
query for text entries, and the plan tree with its patch sites and `PlanState` for shape
entries. The P01 sketch estimated a few kilobytes per plan. The default of 16 MiB for the
server holds a few thousand plans. With X08, the cache is part of `SharedCaches` with a
namespace per dataset. If this spec lands before X08, each store has its own cache of
`--plan-cache-mb`, and the per-dataset multiplication that X08 removes applies to it
until X08 lands.

A plan's `PlanState` and nodes are behind `Arc`, so an entry evicted while an execution
uses it lives until that execution ends. Its memory is not charged to a query budget,
because it is cache memory, reported with the caches.

## 7. Prepared queries and the bindings

### 7.1 Rust

`PreparedQuery` holds the parsed query, its shape key, the dataset handle and a cached
reference to the shape entry. `query(bindings, options)` builds `QueryOptions` with the
bindings as `initial_bindings`, takes the snapshot, and instantiates or plans. It keeps
working when the cache evicts its entry, because it holds the parsed query.

### 7.2 Python

`Dataset.prepare(text)` returns a `PreparedQuery` that parses at once and raises the
parse error there. `query` and `cursor` take `bindings` as `Dataset.query` does
(`crates/sparkles-py/src/dataset.rs`) and the same keyword options. The query builder's
`set_var` (`querybuilder.rs`) still produces text, which reaches the implicit cache and
hits at the shape level. P01 estimated that the engine's part of the small `VALUES` query
falls from about 65 to about 15 µs with parsing and planning skipped. A prepared query
also skips the text lookup.

### 7.3 JVM

`FfiQuery` keeps its two-call shape (`prepare_query`, then `execute`), and its `execute`
goes through the implicit cache. The new `FfiPlan` (UniFFI object, and a JNI entry point
under the rule of P04 §5.4 when the measurements meet it) parses once. `FfiPlan::bind`
takes the binding names and values in the `QueryOpts` encoding (`binding_names`,
`binding_values`) and returns an `FfiQuery` that instantiates the plan. In Kotlin,
`QueryEngineSparkles` keys a small map from the Jena `Query` object's identity to an
`FfiPlan`, so a parsed Jena query executed many times with different substitutions
serializes and prepares once. `ParameterizedSparqlString` produces new text each time,
which hits the shape level after a parse.

### 7.4 Node

`dataset.prepare(text)` parses on the worker thread and resolves to a `PreparedQuery`.
`query(bindings, options)` takes bindings as `{ name: Term }` or a `Bindings`, as the
`bindings` option of `query` does today (`crates/sparkles-node/src/lib.rs`). The
prepared query's handle carries the parsed query and the shape entry, so the worker skips
both steps.

### 7.5 Stored queries

A C16 stored query's version is fixed text with typed parameters. Running it by name
uses the text level keyed on the stored query's name and version, and its parameters are
`$?name` slots. A new version is a new key.

## 8. Architecture

| Part | Location | Change |
|---|---|---|
| `PlanState` | `crates/sparkles-core/src/sparql/ctx.rs` | New struct behind `Arc`, fields moved from `Ctx` |
| `Planner` | `sparql/plan.rs`, `joinorder.rs`, `keyprobe.rs`, `sample.rs`, `charsets.rs` | Write into `PlanState`, record slots, patch sites and count classes |
| `Node::desc` | `sparql/plan.rs` | Template with slot markers, rendered on demand |
| `PlanCache` | new `crates/sparkles-core/src/sparql/plancache.rs` | Text and shape entries, instantiation, the estimate check |
| `prepare_query` | `crates/sparkles-core/src/sparql/mod.rs` | Looks up the cache, plans on a miss, inserts |
| `PreparedQuery` | `crates/sparkles/src/dataset.rs`, `sparql/mod.rs` | Public API |
| Store and catalog | `crates/sparkles-core/src/store.rs`, `crates/sparkles/src/catalog/mod.rs` | Owns the cache, clears a namespace at a generation switch |
| Bindings | `crates/sparkles-ffi/src/query.rs`, `jni_calls.rs`, `crates/sparkles-py/src/`, `crates/sparkles-node/src/` | `FfiPlan`, `PreparedQuery` |
| Server | `crates/sparkles-server/src/main.rs`, `obs.rs`, `http.rs` | `--plan-cache-mb`, metrics, `noplancache` |

## 9. Phases

| Phase | Deliverable | Gate |
|---|---|---|
| 1 | `PlanState` split; text-level cache | All suites pass with the split. A1, A2. Parsing skipped on repeated text. |
| 2 | Shape-level cache with slots, patch sites, the estimate check, invalidation, metrics | A3–A9. Small-query medians drop as §11 states, no query slower than the tie threshold. |
| 3 | Prepared queries in Rust, Python, the JVM and Node; stored queries | A10–A12 and the binding benchmarks. |

## 10. Acceptance examples

* **A1 — Text hit.** Running `SELECT ?o WHERE { <s1> <p> ?o }` twice reports `Text` on
  the second run with a parse time of zero, and the same answer.
* **A2 — Split.** The W3C SPARQL 1.0 and 1.1 evaluation suites and X05's differential
  test pass with a cache of zero bytes and with a cache large enough to hold every query,
  in both execution modes, at batch sizes 1 and 4,096.
* **A3 — Shape hit.** `SELECT ?o WHERE { <s1> <p> ?o }` then `… <s2> …`, where `s1` and
  `s2` have 3 and 2 `p` values, reports `Shape` for the second, and its plan explanation
  matches a fresh plan for `s2`.
* **A4 — Class change.** With `s3` having 5,000 values of `p` and a join to another
  pattern, the third query reports `Replanned(class)`, and its plan equals a fresh plan.
* **A5 — Absent constant.** A constant the store does not hold reports
  `Replanned(absent)` and returns no rows.
* **A6 — Compaction.** After a compaction, a repeated query reports a miss, and the
  dataset's shape entries are gone from the cache's bytes.
* **A7 — Access views.** The same query under two C12 grants with different graphs, and
  under a C12b protection, never shares a plan, and each answer matches its view.
* **A8 — Initial bindings.** `SELECT ?name WHERE { ?p <name> ?name }` with `?p` bound to
  each of 1,000 people hits the shape level after the first, and each answer projects the
  bound `?p` when selected.
* **A9 — VALUES.** A `VALUES ?s { … }` block of five subjects run with five other
  subjects hits, and one of six subjects misses, because the row count is in the key.
* **A10 — Python.** `pq = ds.prepare(q)` followed by 10,000 `pq.query(bindings={...})`
  calls takes at most 30% of the time of 10,000 `ds.query` calls with substituted text on
  the bindings benchmark's small queries.
* **A11 — JVM.** On `mise run bench:jena`, the `VALUES` query over five subjects and the
  filtered employees query, which P04 measured at 185 and 243 µs against TDB2's 119 and
  139 µs, reach TDB2's time or the Outcome records the remaining cost.
* **A12 — Stored queries.** Running a stored query by name with new parameter values hits
  the shape level, and a new version misses.

## 11. Testing and measurement

* A property test plans random queries from the generator of X05's differential test
  with random constants twice, once fresh and once through the cache, and checks that the
  answers are equal, and that the plans are equal whenever the count classes are.
* A concurrency test runs the same shapes from 16 threads with eviction forced by a tiny
  cache, and checks answers and that no execution sees another's parameters.
* Benchmarks: the Rust small-query cases of the bindings benchmark (P01 measured 65 µs
  for the `VALUES` query, with a target near 15 µs), `bench:bindings` for Python and Node,
  and `bench:jena` for the JVM, A/B/B/A in fresh processes as those specs do. The server
  suite at 1.05M and 10.5M with the cache on and off, to show no regression above the tie
  threshold on queries that never repeat.

## 12. Rejected alternatives

* **Plans for the first constants only (PostgreSQL's generic plan, always).** A plan
  chosen for a rare constant can be far worse for a frequent one, and SPARQL's estimates
  vary by orders of magnitude between subjects. The class check keeps the custom plan's
  quality.
* **Re-plan on every commit.** Commits are frequent and rarely move estimates across a
  class. The class check makes the commit irrelevant to validity.
* **A parameter table read by operators at execution instead of cloning.** It changes
  every operator that reads a constant. Cloning is measured first.
* **Caching by text only.** The JVM's measurements show each operation's text differs
  (P04 Outcome), so text-only caching would not hit where the cost is.
* **Translating Jena's algebra directly into the engine's algebra.** It saves parsing but
  needs a translation of the whole algebra, and the shape cache already hits after a
  parse of about 10 µs.

## 13. Open questions

1. Should a class difference of one bucket in a scan that the planner placed last be
   tolerated without re-planning? It would raise the hit rate at a small risk.
2. Should the server expose the cache's contents (shapes, hit counts) to administrators,
   for tuning? It must redact constants under C12 and C12b.
3. Should HTTP clients be able to name a prepared query for a session, as the PostgreSQL
   protocol does? Stored queries cover the shared case.

## Outcome

Not built. See [ROADMAP](../ROADMAP.md) item 32.
