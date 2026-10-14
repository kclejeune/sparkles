# F16: Materialization at scale

> **Status:** implemented in part (Phase 1)
>
> **Phases:** Phase 1 shipped: rule evaluation over the store's index permutations, a
> parallel merge of derived facts, snapshot statistics in the rule planner, `max_memory`,
> the reasoning pool, incremental runs with GeoSPARQL default geometries, the `dt-not-type`
> check and a LUBM-style reasoning run. Phase 2 is the derived-fact layer, kept apart from
> the base indexes. Phase 3 is `owl:sameAs` through canonical representatives with
> expansion at query time. Phase 4 is evaluation without iteration barriers and
> incremental maintenance with equality. Phases 2 to 4 are not built.
>
> **User docs:** [API: reasoning](../API.md#reasoning-status-and-diagnostics) ·
> [Usage: `serve` options](../USAGE.md#serve-options) ·
> [Features](../FEATURES.md#reasoning-validation-and-schema)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This design was written from the W3C OWL 2 Profiles recommendation (the OWL 2 RL/RDF rules,
including `eq-*` and `dt-*`), the W3C RDF 1.1 Semantics and XML Schema Datatypes 1.1
recommendations, Motik, Nenov, Piro, Horrocks and Olteanu, "Parallel materialisation of
Datalog programs in centralised, main-memory RDF systems" (AAAI 2014), Motik, Nenov, Piro
and Horrocks, "Handling owl:sameAs via Rewriting" (AAAI 2015), the same authors'
"Incremental Update of Datalog Materialisation: the Backward/Forward Algorithm"
(AAAI 2015) and "Combining Rewriting and Incremental Materialisation Maintenance for
Datalog Programs with Equality" (IJCAI 2015), Guo, Pan and Heflin, "LUBM: A benchmark for
OWL knowledge base systems" (Journal of Web Semantics, 2005), the public OWL2Bench
description (Singh, Bhatia and Mutharaju, ISWC 2020), Tarjan's union-find analysis, and the
Sparkles code. It builds on inference freshness and incremental runs
([C08](C08-inference-freshness.md)), commit identity ([CI](CI-commit-identity.md)),
automatic compaction ([C13](C13-automatic-compaction.md)), triple protections
([C12b](C12b-triple-access-control.md)), branches ([F09](F09-branches-and-merges.md)),
history ([F06](F06-snapshots-and-point-in-time.md)) and the benchmark plan
([X06](X06-benchmark-coverage.md)). Query-time reasoning is [F17](F17-query-time-reasoning.md)
and explanations are [F18](F18-explanations-and-entailment-diffs.md).

## 1. Summary, goals, non-goals

The reasoner in `crates/sparkles-reasoner` materializes RDFS, an OWL 2 RL subset and Jena
forward rules into the graph `urn:x-sparkles:inferred`. C08 made the result's freshness
visible and added incremental runs with the backward/forward algorithm. Four properties
of the current design stop a user from reasoning over a whole large graph and leaving
reasoning on:

1. **Memory proportional to the input.** `load_graphs` in `src/lib.rs` copies every
   triple of the input graphs into the in-memory `Graph` of `src/graph.rs`, which holds
   a hash index from triple to row and lists by `p`, `(p, s)`, `(p, o)` and, on demand,
   `s` and `o`. C08's Outcome measured about 135 bytes per closure triple for a kept
   closure. A 1-billion-triple input therefore needs well over 100 GB before any rule
   runs, although the store already holds the same triples in compressed, sorted
   permutations.
2. **A sequential merge.** `engine::run` evaluates the rule plans of one iteration in
   parallel with rayon, in chunks of 2,048 candidate rows, and collects every chunk's
   output. `Graph::add_batch` then deduplicates all of it through one hash map on one
   thread, and builds the secondary index lists with one thread per index. Each
   iteration ends with a barrier, so one rule with a large delta holds every thread.
3. **Derived facts are ordinary quads.** `write_full` writes the inferred graph through
   the write transaction. A run above `bulk_threshold` (250,000 quads by default) is
   merged into a rebuilt generation, smaller ones go through the delta and the WAL. The
   derived quads are then copied by every compaction, logged in the change log and the
   history, counted in quotas and backed up, although they can be computed again from
   the base data.
4. **`owl:sameAs` by rules.** `rules/owl-rl.rules` implements equality with `eq-sym`,
   `eq-trans` and `eq-rep-s`, `eq-rep-p` and `eq-rep-o`. A class of `n` equal
   individuals gives `n²` `owl:sameAs` facts and copies every fact of each member to
   every other member. Data with large equality classes, common after entity linking,
   makes the closure grow quadratically.

**Goals**

1. Reason over the whole default graph, or the input graphs of C08 §11, of a dataset of
   at least 1 billion triples on one machine, with memory proportional to the derived
   facts and not to the input (§3).
2. Use every core in a run, without a single-threaded merge (§4).
3. Keep derived facts in a layer of their own, so that base writes, compaction, the
   change log and backups do not pay for them, and queries still read base ∪ derived
   with index scans (§5).
4. Handle `owl:sameAs` by canonical representatives, with answers expanded at query
   time, so equality classes cost linear space (§6).
5. Keep incremental maintenance (C08 Phase 3) correct across the new storage and
   equality handling, and remove the full-run fallbacks that can be removed (§7).
6. Check `dt-not-type`, the last OWL 2 RL rule with `false` in its head that C08 does not
   check (§8).
7. Publish measurements on LUBM and OWL2Bench through X06, with a target of reasoning
   left on during a write workload without significant loss of query or write
   throughput (§11).

**Non-goals.** Query-time derivation, which is F17. Derivation trees and explanations,
which are F18. OWL 2 DL or EL reasoning. Distributed reasoning across machines.
Datatype reasoning beyond `dt-not-type` and the existing `dt-diff` use in diagnostics.
Changing the default profile or turning reasoning on by default.

## 2. User-visible behaviour

### 2.1 Run options

`POST /$/reason/{ds}`, `ReasonOptions` and `sparkles infer` gain three options. Each is
recorded in `reasoning.json`, so re-runs and automatic runs repeat it.

| Field | CLI | Default | Meaning |
|---|---|---|---|
| `storage` | `--storage graph\|layer` | `graph` | Where derived facts go: the named graph `urn:x-sparkles:inferred` as today, or the derived layer of §5. |
| `equality` | `--equality rules\|representatives` | `rules` | How `owl:sameAs` is handled: by the `eq-*` rules as today, or by representatives (§6). Only the `owl-rl` profile and Jena rule sets that contain `eq-rep-*` rules read it. |
| `threads` | `--threads N` | the server's reasoning pool | Threads for rule evaluation. |

Phase 1 changes nothing that a user selects. The engine reads the base data from the
permutations and merges in parallel for every run. The defaults change only after X06's
measurements show the layer and representatives to be at least as fast for the
benchmark queries, and the maintainer decides it.

### 2.2 Status

`ReasoningStatus` (C08 §2.1) gains:

```ts
type ReasoningStatusF16 = ReasoningStatus & {
  storage: "graph" | "layer";
  equality: "rules" | "representatives";
  sameAs?: { classes: number; members: number; largest: number };  // equality = representatives
  layer?: { generation: string; quads: number; bytes: number };    // storage = layer
  run: ReasoningStatus["run"] & {
    peakBytes: number;        // the reasoner's own allocations at their highest
    baseScans: number;        // permutation scans issued by rule evaluation
    threads: number;
  };
};
```

The UI's reasoning panel shows the storage, the equality mode and the peak memory next
to the method of the last run. The panel's run form gains the three options.

### 2.3 Queries

With `storage: "layer"`, queries that include inferences (the default, or
`reasoning=true`) read base ∪ layer. The graph name `urn:x-sparkles:inferred` still
names the derived facts: `GRAPH <urn:x-sparkles:inferred> { … }`, `FROM` and
`default_graph_extra` work as before, and a Graph Store `GET` of that graph returns the
layer's triples. Writes into that graph are refused with `409` and the code
`derived-graph`, because the layer is owned by the reasoner.

With `equality: "representatives"`, results are the same as with `rules` (§6.4). Two
query parameters change that for one request:

- `sameAs=representatives` returns one member of each equality class, the
  representative, in place of all of them. This is cheaper and suits counting distinct
  entities.
- `sameAs=none` reads the stored facts and the layer without expansion. Each equality
  class appears under its representative only.

The same choices are `QueryOptions::same_as` in the library and the `sameAs` argument of
the MCP `sparql_query` tool.

### 2.4 Diagnostics

`GET /$/reason/{ds}/diagnostics` gains the check `literal-not-in-datatype` with rule
`dt-not-type` (§8). It is on by default, like the other checks.

### 2.5 CLI

```
sparkles infer --loc DB --profile owl-rl --storage layer --equality representatives --threads 16
sparkles infer --loc DB --status          # prints storage, equality, sameAs classes, peak memory
sparkles infer --loc DB --check --checks literal-not-in-datatype
```

## 3. Rules over the index permutations

### 3.1 Two tiers

The engine reads facts through a new type, `Facts`, with two tiers:

- **The base tier** is the snapshot's permutations, restricted to the input graphs. It
  is not copied. An atom with a constant predicate and a bound subject scans PSO with
  the prefix `(p, s)`; a bound object scans POS with `(p, o)`; a predicate alone scans
  PSO with `(p)`. A variable predicate with a bound subject scans SPO, with a bound
  object OSP, and with neither every permutation key. Every permutation key ends with
  the graph column (`Perm::order` in `crates/sparkles-core/src/index.rs`), so the scan
  keeps the rows whose graph is an input graph and drops repeats of a triple held in two
  input graphs, which are adjacent in the scan.
- **The derived tier** is the existing append-only `Graph`, holding only facts that are
  not in the base tier. Its rows are numbered from the base tier's size `B` on.

The semi-naive row ranges of `engine::run` keep their meaning. The range `[0, B)` is the
base tier and `[B, len)` the derived rows. Iteration 0 evaluates every rule over the base
tier (old = ∅), as now. Later iterations put the delta in the derived tier, so the delta
atom always reads the in-memory graph, and the other atoms read the base tier plus the
derived rows of their range. The plan's first step therefore usually scans the small
delta, and the base tier answers bound lookups.

`Cands` in `src/graph.rs` gains a third variant, `Base`, which carries the decoded rows
of one block range. A work item's chunk is then either a slice of derived rows or a
block range of a base scan. Splitting by block keeps the chunks independent of the
data's distribution, as `Cands::chunks` does now for rows.

### 3.2 Deduplication against the base

A derived fact that is already in the base tier must not enter the derived tier, or the
next iteration would treat it as new. Probing the snapshot once per derived fact costs a
block decode per probe. Instead, the merge sorts each iteration's new candidates by
`(p, s, o)` and probes PSO with a merge join, one pass per touched predicate, using the
block metadata to skip blocks with no candidate. The probe runs inside the merge of §4.

### 3.3 Join order and statistics

`engine::estimate` reads counts from the in-memory graph today. For the base tier it
reads `Snapshot::counts` and the predicate statistics, corrected for the delta, which
the query planner already uses. Counts are per graph only through GSPO, so with input
graphs other than the default graph the estimate uses the dataset-wide count as an upper
bound.

### 3.4 Memory

Without the copy of the input, a run's memory is the derived tier, the term table of
local terms and the per-iteration candidate buffers. The block cache holds the decoded
base blocks and is shared with queries. A run therefore needs memory proportional to
its derived facts, which for RDFS on LUBM is a small multiple of the input and for OWL 2
RL with representatives is bounded by §6. `ReasonOptions::max_inferred` stays the guard
against runaway rules, and a new `ReasonOptions::max_memory` fails the run with the
largest rule's name, as `max_inferred` does.

### 3.5 Incremental runs and the kept closure

A kept closure (`maintain::Cache`) today holds the explicit facts with a flag. With the
base tier it holds the derived tier only, and a fact is explicit when the snapshot of
the closure's commit holds it in an input graph. `Closure::explicit` becomes a batched
probe against that snapshot. Inserted explicit facts are added to the derived tier as
delta rows flagged explicit for the run, so the semi-naive insertion of
`incremental::update` starts from them, and are dropped from it at the end of the run.
The backward search of `Search::prove` asks the same two tiers for the body facts of a
rule instance. The cache's limit `serve --reason-cache-triples` then counts derived
facts only, so the same limit keeps the closure of an input roughly four times larger
on RDFS.

The id remap after a generation switch (`Closure::remap`) applies to the derived tier
only.

## 4. Parallel evaluation

### 4.1 Phase 1: a parallel merge

The rule evaluation of an iteration stays as it is. The merge changes:

1. Each work item's output is hashed into `N` shards by the triple's hash, keeping, for
   each candidate, its position in the sequential order that `collect` gives today.
2. Each shard deduplicates its candidates against its own part of the triple index and
   against the base tier (§3.2), in parallel.
3. The new facts of all shards are sorted by that position, and row numbers are given in
   that order. Rows therefore come out in the same order as with the sequential merge,
   which keeps runs deterministic. C08's Outcome notes that list builtins see an order
   that depends on the order of derivation, so determinism is required.
4. The secondary index lists are filled in parallel by predicate range, not one thread
   per index, so the work divides by the number of threads and not by five.

`Graph`'s single triple index becomes `N` shard maps. `Graph::position` hashes once to
pick the shard.

### 4.2 Phase 4: no iteration barrier

The algorithm of Motik et al. (AAAI 2014) evaluates rules fact by fact. Threads take the
next unprocessed fact from a shared, append-only fact table, match it against each body
atom, and match the other atoms only against facts that precede it in the table or
satisfy the ordering condition that avoids deriving one instance twice. New facts are
appended with a lock-free insert. A thread never waits for an iteration to end, so one
rule with a large delta does not stall the others.

Sparkles' `Graph` is already append-only with ascending index lists, which is the shape
the algorithm needs. Phase 4 replaces the shard maps with concurrent ones, appends rows
with an atomic counter, and publishes a row to readers after its index entries are
written. Determinism is lost in this mode, so it is not used when the rules read lists
(`engine::reads_lists`), and the closure is still the same set of facts. Phase 4 is
built only if X06's measurements show the barrier costing more than 20% of a run on
LUBM or OWL2Bench with 16 threads.

### 4.3 Threads

Reasoning runs on its own rayon pool with `serve --reason-threads N` threads, by default
half the cores, so that a run does not take the threads that serve queries. A run asked
with `threads` uses at most the pool's size.

## 5. The derived-fact layer

### 5.1 What it is

The layer is a set of sorted permutations of derived triples, in the same block format
as the base indexes (`PermIndex` in `crates/sparkles-core/src/index.rs`), with the same
ids as the generation it was built against, plus a small in-memory delta for incremental
runs. It belongs to a generation, sits in `<generation>/derived/`, and is described by
`derived.json`:

```json
{ "derivedFormat": 1, "commit": 812, "rulesDigest": "…", "inputs": ["default"],
  "equality": "representatives", "quads": 2512004, "perms": ["pso", "pos", "spo", "osp"],
  "delta": "derived.log" }
```

The layer has no graph column of its own. Its triples are in the graph
`urn:x-sparkles:inferred`. It keeps four permutations, enough for the access paths of
the planner's triple patterns, not the base's seven.

### 5.2 Reading it

`Snapshot` gains `derived: Option<Arc<DerivedLayer>>`. The planner maps the graph id of
`urn:x-sparkles:inferred` to a virtual graph backed by the layer: a scan of the default
graph with `default_graph_extra` containing that graph becomes a merge of the base scan
and the layer scan, as a scan already merges the base and the delta. The layer's rows
are disjoint from the base's input graphs by construction (§3.2), so the merge needs no
deduplication for the default graph. A query over `GRAPH ?g` sees the layer as one more
graph. The planner's statistics count the layer's triples per predicate from its own
statistics file.

### 5.3 Writing it

A full run builds the layer's permutations with the builder used by bulk loads, from
the derived tier sorted in each order, outside the writer lock, and then takes the lock
only to publish it. The run makes one commit of kind `reason` with no data records and a
new commit flag, `derivedChanged`. The commit's receipt names the layer. The base indexes,
the WAL's data records and the change log are untouched.

An incremental run writes the additions and removals of the layer to `derived.log` and
the layer's in-memory delta, under the writer lock, in the same `reason` commit. When the
layer's delta passes a tenth of the layer, the next run rebuilds the layer files in the
background and switches to them.

### 5.4 Compaction and generation switches

Compaction builds a new generation with a new vocabulary, so ids move. The layer is
remapped, not derived again. C13's generation build gains a step that maps each layer
key through the old-to-new id map, which the build already computes for the delta
vocabulary, sorts the keys again and writes the layer into the new generation. Its cost
is a sort of the derived triples, without rule evaluation and without reading the base.
When the build cannot remap in time, as when the layer is much larger than the delta,
the new generation is published without a layer and the status reads
`stale: true, staleReason: "the derived layer is being rebuilt"`. The next run, or an
automatic run, builds it. Item 14 of the roadmap (C13 addendum) covers this step for all
derived indexes.

### 5.5 What the layer is not part of

| Concern | With `storage: "graph"` | With `storage: "layer"` |
|---|---|---|
| WAL and change log | derived quads logged | one `reason` commit record |
| History (`?at=`) | inferences as of each commit | the layer of the current generation only; historical reads include no inferences unless the run's layer is kept (§5.6) |
| Commit diff and change feed | derived quads listed | not listed; the feed shows the `reason` commit |
| Quotas | counted | reported apart as `derivedBytes`, not counted |
| Backups (F05) | included | left out; restore marks the status stale |
| Branches (F09) | per branch, as quads | per branch; a new branch shares the layer files, which are immutable |
| Merges (F09 §4.9) | `inferences: "exclude"` by default | the layer is never merged |
| C12b protections | the inferred graph leaves the read set | the same: the virtual graph leaves the read set |

### 5.6 History of inferences

A user who needs inferences at past commits sets `keepLayers: N` in the reasoning
options. The last `N` layers stay on disk with the commits they reflect, and a read at a
commit uses the newest kept layer whose commit is at or before it, with the
`Sparkles-Inferences` header saying how many commits it lags. By default `N` is 0.

## 6. `owl:sameAs` through representatives

### 6.1 Representatives

With `equality: "representatives"`, the reasoner keeps a union-find over term ids. Each
equality class has one representative: the member with the smallest id in the base
vocabulary, so the choice is the same across runs on one generation. Every fact in the
derived tier is stored with each position replaced by its representative. Base facts are
not rewritten. They are read through the mapping: a base scan with a bound term scans
for every member of that term's class, and each fact read is rewritten to
representatives before it reaches a join.

The `eq-sym`, `eq-trans` and `eq-rep-*` rules of the profile are removed and replaced by
the union-find. Other rules that read `owl:sameAs` in their body, such as the
profile's `eq-class` and `eq-prop`, keep running and read the `owl:sameAs` pairs of the
classes as if they were facts:

1. A derived or explicit `x owl:sameAs y` with `rep(x) ≠ rep(y)` merges the two classes.
   The new representative is the smaller of the two.
2. Facts of the derived tier that mention the absorbed representative are killed and
   added again rewritten (`Graph::kill` and `add_batch`, as `Closure::remap` does today).
   They join the delta, so rules see them as new.
3. A rule instance binds variables to representatives only. A constant in a rule is
   replaced by its representative when the rules are compiled and again after a merge
   that absorbs it.

This is the rewriting approach of Motik et al. (AAAI 2015). `eq-ref` (reflexive
`owl:sameAs` for every individual) is not materialized, as today.

### 6.2 Rules that compare terms

`notEqual(?x, ?y)` and other builtins that compare term identity see representatives.
`notEqual` in a rule body means that the two values are different individuals under the
equality the rules derive, so comparing representatives is the intended reading. The
OWL 2 RL rules that use it (`prp-fp`, `prp-ifp`, `cls-maxc2` and `cls-maxqc3` in
`rules/owl-rl.rules`) are correct under it. Builtins that read a lexical form, such as `regex` or `strConcat`,
receive the representative's term. For a class whose members include literals, which
only generalized RDF can state, the reasoner uses the literal as representative, so
lexical builtins see it. A Jena rule set that reads lexical forms of IRIs that are
`owl:sameAs` other IRIs gets a warning at compile time, because its results then depend
on the representative.

### 6.3 Stored equality

The layer stores the classes as two tables, keyed by the generation's ids: member →
representative and representative → members, both sorted. The layer stores no
`owl:sameAs` triples. Queries that match `owl:sameAs` read them from the tables (§6.4).

### 6.4 Answers

The semantics of a query are those of the full materialization: the answers are the
answers over base ∪ derived where each class is expanded to all its members in every
position and `owl:sameAs` holds between every two members of a class, a member and
itself excluded, as `eq-sym`/`eq-trans` derive today.

The planner implements this as follows:

- **Constants.** A constant term in a triple pattern is replaced by its representative
  for the layer scan, and by every member of its class for the base scan.
- **Joins.** Variables bind representatives from the layer and from base scans after
  rewriting, so joins run on representatives and produce one row per combination of
  classes, not of members.
- **Expansion.** An `Expand(?v)` operator replaces a variable's representative by each
  member of its class. The planner places it above the highest operator that does not
  read the variable's term: below any `FILTER`, `BIND`, `ORDER BY`, `GROUP BY`,
  aggregate, `DISTINCT` or projection that reads `?v`, and below the result. A variable
  that reaches the result unread is expanded at the result. Expansion therefore happens
  once per output row, and `COUNT(*)` counts expanded rows as the full materialization
  would.
- **`owl:sameAs` patterns.** `?x owl:sameAs ?y` is answered from the tables: one row per
  ordered pair of distinct members of each class, plus stored `owl:sameAs` triples of the
  base with both sides in one class, which are the same rows.
- **Property paths.** Path evaluation runs on representatives, and the endpoints are
  expanded like variables.
- **`sameAs=representatives`** leaves out `Expand`. **`sameAs=none`** also leaves out the
  constant rewriting for base scans.

Expansion multiplies the rows of a query whose variables bind large classes. The query
budget of C01 counts expanded rows, and EXPLAIN shows `Expand` with its estimated
factor from the class size statistics.

### 6.5 Access control

A C12b protection hides triples, and a hidden `owl:sameAs` triple can merge classes. With
`hide_inferences` in force, the derived layer and its equality tables leave the read set,
as today. With `hide_inferences = false`, the layer is read, and the operator states that
the protections cannot change inferences, so the tables are read too.

## 7. Incremental maintenance

### 7.1 With the layer

`materialize_incremental` keeps its contract. The changes of the inferred graph are
written to the layer's delta instead of the inferred graph (§5.3). The kept closure holds
the derived tier only (§3.5).

### 7.2 With representatives

Additions are the semi-naive insertion of `incremental::update` with the union-find
merges of §6.1. Deletions are harder: removing an `owl:sameAs` fact can split a class,
and every fact rewritten through that class may lose its proof. Phase 3 runs in full when
a run removes an explicit `owl:sameAs` triple or a fact that derived one, with the
fallback reason "equality changed by a deletion". Phase 4 implements the maintenance
algorithm of Motik et al. (IJCAI 2015), which tracks the facts rewritten through each
merge and re-derives the affected classes.

### 7.3 Fallbacks

C08's Outcome lists the cases that run in full. This spec changes them as follows:

| Fallback today (`src/lib.rs`, `src/incremental.rs`) | Change |
|---|---|
| Rules with `noValue` | Phase 4: stratify the rules. A rule set whose `noValue` atoms read only predicates that no rule of the same or a later stratum derives is maintained stratum by stratum, the negated predicates' changes feeding the next stratum as deletions and insertions. Others still run in full. |
| `now`, `makeTemp`, `makeSkolem`, head actions, blank nodes in heads | Kept. The facts they derive are not a function of the input. |
| `listForAll` not covered by `listMember` and an atom | Kept. |
| GeoSPARQL default geometries | Phase 1: compute the default geometries of the changed features only, before the rules run, as an extra delta. |
| An RDF list changed, or derived, while the rules read lists | Phase 4: keep, per list head, the members the builtins read, and treat a list whose members changed as the deletion and insertion of the rule instances that read it. Lists derived by rules still run in full. |
| Changed rules, unreachable commit, changed input graphs, store position moved back | Kept. |
| Removal of more than 1 in 20 explicit triples of 10,000 or more (`LARGE_DELETION`) | The threshold becomes a cost estimate: the run estimates the backward search from the removed triples' predicates and the closure's fan-out and compares it with the last full run's time. The constant stays as a cap until X06 measures the estimate. |

Each change keeps the differential tests of C08 (random inserts and deletes compared
with a full run) and adds seeds for the new cases.

## 8. The `dt-not-type` check

OWL 2 RL's rule `dt-not-type` has `false` in its head: a literal `lt` typed with a
datatype `DT` of the OWL 2 RL datatype map, where `lt`'s value is not in `DT`'s value
space, is an inconsistency. Two cases give such a typing:

1. **An ill-typed literal.** `dt-type2` types every literal with its own datatype, so a
   literal such as `"abc"^^xsd:integer` is inconsistent on its own.
2. **A range.** `p rdfs:range DT` and `x p lt` type `lt` with `DT` (`prp-rng`). The
   literal `"abc"` with `ex:age rdfs:range xsd:integer` is inconsistent.

The check `literal-not-in-datatype` reports both. Ill-typed literals are found by
reading the literals of the vocabulary whose datatype is in the datatype map and parsing
each with the parser that SPARQL value comparison uses, which turns a lexical form it
cannot parse into an "other" value (`Value` in
`crates/sparkles-core/src/sparql/value.rs`). The scan reads the vocabulary once per
generation and remembers the result, and the delta vocabulary's literals are checked
each time. Ranges come from a SPARQL query, `datatype-range.rq`,
that finds `p rdfs:range DT` with `DT` in the datatype map, closed over
`rdfs:subPropertyOf*` as `closure=subclass` closes types, and the literal objects of `p`.
Rust code then decides value-space membership:

| Range | A literal is in the value space when |
|---|---|
| `xsd:decimal`, `xsd:integer` and its derived types | its value is numeric, is an integer for the integer types, and lies in the type's bounds; `"1.0"^^xsd:decimal` is in `xsd:integer`'s value space |
| `xsd:double`, `xsd:float` | it is a double or float; the OWL 2 value spaces of these are disjoint from `xsd:decimal`'s |
| `xsd:string` and its derived types | it is a string without a language tag, and matches the derived type's facets |
| `rdf:langString` | it has a language tag |
| `rdf:PlainLiteral` | it is a string, with or without a language tag |
| `xsd:boolean`, `xsd:dateTime`, `xsd:dateTimeStamp`, `xsd:anyURI`, `xsd:hexBinary`, `xsd:base64Binary` | it has that datatype, or a derived one, and is well typed |
| `rdfs:Literal`, a datatype outside the map, a datatype restriction | never reported |

A literal that the table cannot decide is not reported, so the check stays sound. The
finding's evidence is the property, the range and the literal, and `basis` follows C08
§4.2. The check uses the kept closure's generalized facts when one is available, since
those include `lt rdf:type DT` facts derived through rules other than `prp-rng`.

## 9. Architecture

| Crate / file | Change |
|---|---|
| `crates/sparkles-reasoner/src/facts.rs` (new) | `Facts`: the base tier over `Snapshot` and input graph ids, and the derived tier. Batched membership probes. |
| `crates/sparkles-reasoner/src/graph.rs` | Sharded triple index, `Cands::Base`, parallel index fill. Phase 4: concurrent append. |
| `crates/sparkles-reasoner/src/engine.rs` | `run` over `Facts`; estimates from snapshot statistics; the parallel merge; rule compilation with representatives. |
| `crates/sparkles-reasoner/src/equality.rs` (new) | The union-find, class tables, merge and rewrite of the derived tier. |
| `crates/sparkles-reasoner/src/incremental.rs` | Explicitness from the base tier; the equality fallback; Phase 4 strata and list tracking. |
| `crates/sparkles-reasoner/src/maintain.rs` | The kept closure without base facts; saved state of the derived tier only. |
| `crates/sparkles-reasoner/src/lib.rs` | `ReasonOptions::{storage, equality, threads, max_memory, keep_layers}`; `write_layer` next to `write_full`. |
| `crates/sparkles-reasoner/src/diagnostics.rs`, `diagnostics/datatype-range.rq` | The `literal-not-in-datatype` check. |
| `crates/sparkles-core/src/store/derived.rs` (new) | `DerivedLayer`: files, delta, `derived.log`, remap at generation switches. |
| `crates/sparkles-core/src/store.rs` | `Snapshot::derived`; the `reason` commit with `derivedChanged`; refusal of writes to the inferred graph in layer mode. |
| `crates/sparkles-core/src/store/compaction.rs` | The remap step of §5.4. |
| `crates/sparkles-core/src/sparql/` | The virtual inferred graph in scans; `sameas.rs` (new) with constant rewriting, `Expand` and `owl:sameAs` patterns; `QueryOptions::same_as`. |
| `crates/sparkles/src/reasoning/` | Recorded options and the status fields. |
| `crates/sparkles-server/src/reasoning.rs`, `http.rs` | Request fields, `--reason-threads`, the `sameAs` query parameter. |
| `ui/src/routes/datasets/[name]/+page.svelte` | The run form's options and the status line. |

## 10. Phases

**Phase 1 (M).** The base tier and the derived tier, the parallel merge, snapshot
statistics in the rule planner, `max_memory`, the reasoning pool, the incremental changes
for GeoSPARQL default geometries, the `dt-not-type` check, and the LUBM and OWL2Bench
reasoning runs in X06. No option changes behaviour that a user sees except memory and
time.

**Phase 2 (M).** The derived layer: building, reading, the virtual graph, incremental
updates through `derived.log`, the remap at compaction, branches, backups and
`keepLayers`. `storage: "layer"` is available, `graph` stays the default.

**Phase 3 (M–L).** Representatives: the union-find, rewriting in the engine, the class
tables in the layer, constant rewriting and `Expand` in the planner, the `sameAs` query
parameter, and the full-run fallback for deletions that change equality.

**Phase 4 (L).** Barrier-free evaluation if measurements justify it, incremental
maintenance with equality, stratified negation and list tracking.

## 11. Acceptance examples

The examples use `ex:` = `http://ex.org/` and a persistent dataset `t`.

**A1. Memory follows the derived facts.** Load LUBM with 1,000 universities (about 138
million triples). `sparkles infer --profile rdfs` completes, and `run.peakBytes` is at
most 3 × the bytes of the derived facts' tier plus 1 GiB, and less than half of what the
same run needs at the commit before Phase 1. The inferred graph equals the one the
previous engine writes on the 100-university data.

**A2. Same closure.** For every rule set and seed of the existing differential tests,
the closure computed over the two tiers equals the closure computed by the current
in-memory engine, generalized triples included, and the row order of the derived tier is
the same.

**A3. Parallel merge.** On LUBM 100 with OWL 2 RL, a run with 16 threads is at least
1.5 × faster than the run before Phase 1 on the same machine, and a run with 1 thread is
not slower.

**A4. Layer does not touch base writes.** With `storage: "layer"`, a full OWL 2 RL run on
LUBM 100 writes no WAL data records, and `GET /$/commits/t` shows one commit of kind
`reason`. A following compaction takes no more than 1.2 × the time of a compaction of the
same dataset without inferences.

**A5. Queries over the layer.** LUBM queries 1 to 14 return the same answers with
`storage: "graph"` and `storage: "layer"`, and each runs within 10% of its time with
`graph`.

**A6. Writes to the layer are refused.** `INSERT DATA { GRAPH <urn:x-sparkles:inferred>
{ ex:a ex:p ex:b } }` returns `409` with code `derived-graph` in layer mode.

**A7. Representatives.**

```turtle
ex:a owl:sameAs ex:b . ex:b owl:sameAs ex:c .
ex:a ex:name "A" . ex:c a ex:Person .
```

With `equality: "representatives"`, `status.sameAs` is `{classes: 1, members: 3,
largest: 3}`. `SELECT ?x { ?x a ex:Person }` returns `ex:a`, `ex:b` and `ex:c`.
`SELECT ?x ?y { ?x owl:sameAs ?y }` returns six rows. `SELECT (COUNT(*) AS ?n) { ?x
ex:name ?n0 }` returns 3. With `sameAs=representatives`, the first query returns `ex:a`
only.

**A8. Quadratic growth avoided.** 10,000 individuals in one class, linked by a chain of
`owl:sameAs`, each with 10 facts. With `rules`, the closure holds about 10⁸ `owl:sameAs`
facts and 10⁹ copied facts, and the run fails on `max_inferred`. With representatives,
the run finishes with derived facts in the order of 10⁵, and a query for one member's
facts returns 100,000 rows after expansion.

**A9. Equality deletion falls back.** Removing `ex:b owl:sameAs ex:c` from A7 runs in
full in Phase 3, with fallback reason "equality changed by a deletion", and afterwards
`ex:c` is in a class of its own. In Phase 4 the run is incremental with the same result.

**A10. dt-not-type.**

```turtle
ex:age rdfs:range xsd:integer .
ex:p ex:age "abc" .
ex:q ex:age "1.0"^^xsd:decimal .
ex:r ex:height "tall"^^xsd:decimal .
```

`literal-not-in-datatype` gives two findings: `ex:p` (range `xsd:integer`, literal
`"abc"`) and `ex:r` (ill-typed `"tall"^^xsd:decimal`). `ex:q` gives none, because 1.0 is
in the value space of `xsd:integer`.

**A11. Leaving reasoning on.** On LUBM 100 with `storage: "layer"`, the automatic re-run
of C08 at a debounce of 5 s, and a write load of 100 single-triple updates per second for
10 minutes with the LUBM queries running concurrently, the median query time rises by at
most 10% and the write throughput falls by at most 10% compared with the same load
without reasoning, and the status is fresh within 10 s after the writes stop.

## 12. Testing and measurement

- **Differential tests.** The tests of `incremental.rs` and the store tests of C08 run for
  both storage modes and both equality modes. With representatives, the tests compare the
  expanded closure with the closure of the `eq-*` rules.
- **W3C OWL 2 RL conformance tests.** The positive and negative entailment and
  inconsistency tests of the OWL 2 test suite that fall in the RL profile run for the
  `owl-rl` profile in both equality modes. The spec does not promise that all pass,
  because the profile is a subset. The Outcome will record the pass counts, and a
  regression fails CI.
- **Differential tests against Jena.** The existing fixture scripts gain a run of Jena's
  OWL Mini forward rules (`GenericRuleReasoner` in forward mode) on generated data, and
  the closures are compared on the triples both rule sets derive.
- **Benchmarks (X06).** LUBM at 10, 100 and 1,000 universities and OWL2Bench at the
  sizes it publishes, for RDFS and OWL 2 RL: full-run time, peak memory, inferred
  facts, incremental runs of 1, 100 and 10,000 inserts and deletes, the LUBM queries
  with and without the layer, and A11's mixed load. The existing
  `examples/reasoner-bench.rs` and `examples/incremental.rs` gain flags for the new
  options, so the numbers can be reproduced without X06's harness.
- **Determinism.** Two runs of the Phase 1 engine on the same data give derived tiers with
  identical rows in identical order.

## 13. Rejected alternatives

- **Spill the in-memory graph to disk.** It keeps the copy of the input and adds I/O.
  The base data already sits in sorted, compressed permutations that answer the rule
  engine's lookups.
- **Write derived facts into the base indexes with a flag column.** Every key would grow,
  every compaction would still copy derived facts, and base writes would still pay for
  them.
- **Materialize `owl:sameAs` in full and compress it.** The rewritten facts still grow
  with the square of the class size. Representatives avoid them.
- **Rewrite base facts to representatives in the store.** The data would no longer read
  as written, and every merge of classes would rewrite base indexes.
- **Pick the representative by IRI order or by the most-used member.** Ids give the same
  answer with no lookups. The choice does not change answers, because queries expand.
- **DRed for equality.** C08 measured DRed's overdeletion under RDFS. With equality it is
  worse, because a split class touches every fact of its members.
- **A thread per rule.** The rules' work is too uneven, and rayon's work stealing over
  chunks already balances better.
- **Moving the reasoner onto F13's fixed-point operator now.** F13 §4.3 plans recursion
  for rules on its incremental core and suggests the reasoner may move onto it. The
  reasoner's semi-naive engine and backward/forward maintenance are measured and exact
  today, and F13 Phase 3 is not built. This spec keeps the reasoner's own engine and
  revisits the question when F13's operator has measurements on LUBM.

## 14. Open questions

1. Should `storage: "layer"` become the default once A5 and A11 pass? It drops the
   history of inferences, which some users may read today.
2. Should the layer keep all seven permutations? Four cover triple patterns. GSPO is not
   needed because the layer is one graph, and SOP and OPS serve only the planner's rarer
   orders.
3. Should `sameAs=representatives` be offered for `equality: "rules"` too, computing
   classes at query time? It would help users who cannot re-run reasoning.
4. Should the `dt-not-type` check also report literals typed through `owl:allValuesFrom`
   on datatype properties? OWL 2 RL has no rule for it, so the spec leaves it out.
5. Does the reasoning pool need its own memory limit under C01's planned server-wide
   pool (X08)? The spec gives runs `max_memory` and leaves the pool to X08.

## Outcome

**Phase 1 delivered** on 2026-10-13. Phases 2, 3 and 4 are not built. See
[ROADMAP](../ROADMAP.md) item 9.

* **Base tier.** `crates/sparkles-reasoner/src/facts.rs` reads the input graphs from the
  snapshot's permutations. A lookup with one input graph and a bound subject uses GSPO.
  Other lookups use PSO, POS, SPO or OSP with a graph filter and drop the duplicates that
  several input graphs give. Each rule step keeps the last decoded blocks it read, so a
  join that probes neighbouring keys decodes each block once. Membership of a batch of
  candidates is one sorted pass per graph. A filter of local term ids, per-predicate counts
  and the objects of predicates with at most 4,096 distinct objects answers most probes
  without reading a block.
* **Derived tier.** `graph.rs` keeps the derived rows after the base rows. A sharded hash
  table of row ids, 64 shards, replaces the map from triple to row, and the per-subject and
  per-object indexes keep up to four rows inline.
* **Parallel merge.** Candidates are bucketed by shard, deduplicated per shard against the
  tiers and the batch, and probed against the base in key order. A bitset of the kept
  positions gives each new triple its row, so rows come in the order of first occurrence,
  as the sequential merge gave them. The index fill also runs per shard. A rule whose
  first step has fewer than four work units per thread is split on the bindings of its
  second step.
* **Planner.** Join order reads the per-predicate counts of the base tier, and the
  derived tier adds its own counts.
* **Limits and reports.** `ReasonOptions::max_memory` fails a run whose derived tier and
  candidate buffers are estimated above the limit, with the rule that produced the most
  candidates. `ReasonReport` and the recorded `run` gain `peakBytes`, `baseScans` and
  `threads`.
* **Reasoning pool.** `serve --reason-threads N` sets one rayon pool for all runs, half the
  cores by default, and `0` uses the global pool. A request's `threads` field or query
  parameter asks for fewer threads. `sparkles infer --threads N` sets the threads of a
  local run.
* **GeoSPARQL default geometries.** Incremental runs no longer fall back to a full run with
  `geoDefaultGeometry`. The default geometries count as explicit facts in the closure, and
  those of the features whose `geo:hasGeometry` or `geo:hasDefaultGeometry` facts changed
  join the run's added and removed facts. The kept closure is now kept for these runs too.
* **`dt-not-type`.** The check `literal-not-in-datatype` and `datatype-range.rq` follow §8.
* **LUBM.** `cargo run --release -p sparkles-reasoner --example lubm` generates
  Univ-Bench-style data, loads it and runs the reasoner. It reports the derived count, the
  peak heap, and digests of the derived set and of its row order.

**Measurements.** LUBM-style data with 100 universities, 13.1 million triples, on a host
with 20 Intel cores and 15 GB of memory, against the engine at `c7a6811c`:

| Run | Before | After | Peak heap before | Peak heap after |
|---|---|---|---|---|
| RDFS, 1 thread | 8.39 s | 5.54 s | 4,182 MiB | 1,384 MiB |
| RDFS, 16 threads | 5.80 s | 1.02 s | 4,182 MiB | 1,389 MiB |
| OWL 2 RL, 1 thread | 16.23 s | 15.43 s | 3,891 MiB | 1,890 MiB |
| OWL 2 RL, 16 threads | 9.84 s | 2.61 s | 3,892 MiB | 1,895 MiB |

RDFS derives 6.84 million facts and OWL 2 RL 12.48 million. Both digests match the
previous engine and match between 1 and 16 threads, so A2 and the determinism test of §12
hold on this data as well as in the differential tests. A3 holds, with 3.8 × at 16 threads
for OWL 2 RL and a 1-thread run 5% faster. A1 was measured at 100 universities, where the
peak is a third of the previous one for RDFS and under half for OWL 2 RL. The
1,000-university run does not fit the 15 GB host and was not run.

**Phase 4 decision.** At 16 threads, the time threads spend waiting at the iteration
barrier, the wall time of each evaluation less its busy time divided by the threads, is
130 ms of a 2.5 s OWL 2 RL run (5%) and under 10 ms of a 1.0 s RDFS run. Both are far below
the 20% that §4.2 sets, so barrier-free evaluation is not justified by these runs. The
merge is now the larger part of an OWL 2 RL iteration.

**Memory tradeoffs.** The derived tier costs about 135 bytes per derived fact, which is
what the in-memory graph cost per fact before. Each iteration's candidate buffers live
until the merge copies them. A kept closure still holds the explicit facts in memory, so
when `--reason-cache-triples` admits the closure, the run reads the input graphs into
memory at its end and the server keeps the same memory as before. The memory saving
applies to the run itself, and to the server only for closures the cache does not keep.

**Deviations.**
* §3.5 is not built. The kept closure holds the explicit facts with a flag, as before, and
  the cache limit counts them.
* OWL2Bench was not run, and the runs are not part of X06's harness. The LUBM data comes
  from the example's own generator, with about 131,000 triples per university, not from
  the UBA generator.
* `literal-not-in-datatype` narrows the candidates with one SPARQL query. Ill-typed
  literals are found by a scan of the literal objects, filtered in the query, instead of a
  vocabulary scan remembered per generation. The check does not read a kept closure's
  generalized facts.
* `examples/reasoner-bench.rs` and `examples/incremental.rs` did not gain flags. The new
  example takes `RAYON_NUM_THREADS`.
