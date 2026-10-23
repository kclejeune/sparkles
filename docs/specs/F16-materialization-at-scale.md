# F16: Materialization at scale

> **Status:** implemented in part, Phase 1 built and its acceptance open
>
> **Phases:** Phase 1 shipped: rule evaluation over the store's index permutations, a
> parallel merge of derived facts, snapshot statistics in the rule planner, `max_memory`,
> the reasoning pool, incremental runs with GeoSPARQL default geometries, the `dt-not-type`
> check and a LUBM-style reasoning run. Its acceptance runs are open under roadmap item 1.
> Phase 2 is the derived-fact layer, kept apart from
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
generation and the generation remembers the ids it found, at most one more than the
check's cap of 262,144. The delta vocabulary's literals are checked each time, a chunk
of positions at a time, so the scan never holds the lock that writers need for longer
than one chunk. Ranges come from a SPARQL query, `datatype-range.rq`,
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

**Phase 1 built** on 2026-10-13. A review on 2026-10-16 led to the fixes recorded below.
Item 1 ran its acceptance runs on 2026-10-23, and item 98 decided A1 at 1,000 universities
the same day, as "A1 at 1,000 universities" below records. A follow-up of item 9
built §3.5 and §8's coverage on 2026-10-18, as recorded under "Kept closure without the
explicit facts" and "`dt-not-type` coverage" below, with the fixes of its review under
"Review of the follow-up". Phases 2, 3 and 4 are not built.

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
* **Limits and reports.** `ReasonOptions::max_memory` bounds the estimated heap of the
  whole run, as described under "Memory budget" below. `ReasonReport` and the recorded
  `run` gain `peakBytes`, `baseScans` and `threads`. The three fields are additive members
  of `reasoning.json` with defaults, so its format stays 2, and an older release that
  rewrites the record drops them.
* **Reasoning pool.** `serve --reason-threads N` sets one rayon pool for all runs, half the
  cores by default, and `0` uses the global pool. A request's `threads` field or query
  parameter asks for fewer threads. `sparkles infer --threads N` sets the threads of a
  local run. Runs are admitted to the pool as described under "Reasoning pool admission"
  below.
* **GeoSPARQL default geometries.** Incremental runs no longer fall back to a full run with
  `geoDefaultGeometry`. The default geometries count as explicit facts in the closure, and
  those of the features whose `geo:hasGeometry` or `geo:hasDefaultGeometry` facts changed
  join the run's added and removed facts. The kept closure is now kept for these runs too.
  The closure remembers its default geometries, so an update reads only the changed
  features, with two probes bound to the feature and the property. A closure read back
  from the dataset does not know them, and that run reads them with one scan of each
  property. `baseScans` of an incremental run counts these reads together with every
  other read of the input graphs.
* **`dt-not-type`.** The check `literal-not-in-datatype` and `datatype-range.rq` follow §8,
  with the corrections under "Diagnostics" below.
* **LUBM.** `cargo run --release -p sparkles-reasoner --example lubm` generates
  Univ-Bench-style data, loads it and runs the reasoner. It reports the derived count, the
  peak heap, and digests of the derived set and of its row order.

**Measurements.** LUBM-style data with 100 universities, 13.1 million triples, measured on
atlas (Intel Core i5-13500, 20 threads, 16 GiB) on 2026-10-13 against `c7a6811c`, the
commit of main the work started from. These are development measurements with the
example's own generator, not published benchmarks. The published LUBM runs are part of
roadmap item 1. The table gives the time and the peak heap before and after the work.

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
1,000-university run does not fit atlas's memory and was not run.

**Phase 4 decision.** At 16 threads, the time threads spend waiting at the iteration
barrier, the wall time of each evaluation less its busy time divided by the threads, is
130 ms of a 2.5 s OWL 2 RL run (5%) and under 10 ms of a 1.0 s RDFS run. Both are far below
the 20% that §4.2 sets, so barrier-free evaluation is not justified by these runs. The
merge is now the larger part of an OWL 2 RL iteration.

**Memory tradeoffs.** The derived tier costs about 135 bytes per derived fact, which is
what the in-memory graph cost per fact before. Each iteration's candidate buffers live
until the merge copies them. Since the follow-up of item 9, a kept closure holds the
derived facts only, so the memory saving applies to the closure a server keeps as well as
to the run.

**Reasoning pool admission.** The review found that a run called on a rayon worker could
deadlock. While it held the writer lock, the waiting worker could take another queued job
that asked for the same lock. It also found that runs asking for fewer threads than the
pool each built a pool of their own beside it, so their threads added up past the pool's
size. `pool.rs` now owns both concerns. A run called on a rayon worker moves to a native
thread of its own while the worker waits without taking other jobs, and a run is admitted
to its `ReasoningPool` before it takes the writer lock. Admission is first come, first
served, against a capacity equal to the pool's threads. Runs that use the whole pool share
its threads and run together. A run that asks for fewer threads counts those threads
against the capacity, waits until they fit, and then evaluates on a pool of exactly that
size. That pool's threads are joined before the run gives its threads back, also when the
run fails or panics. A waiting run checks its cancel flag every 20 ms. Threads that do not
stop within 30 seconds are not waited for, and the run logs a warning. A run that waits
for the writer lock of its store keeps its admission meanwhile, so a run on another store
that needs a different grant waits behind it. A run started on a thread of the pool, on a
thread of another run's own pool, or on the thread that holds another run's admission to
the same pool, such as from a progress callback, is refused with an error, since it would
wait for itself. Without a pool, a run on a rayon worker keeps the size of the caller's
pool on a pool of its own.

**Graph-major visitation.** With several input graphs, the base tier now visits the facts
graph by graph, in the order of the input graphs, and within a graph in key order, as the
in-memory copy did. A fact in several input graphs is visited with the first. Rule
evaluation, list builtins and the order of derived rows therefore match the copy again.
An existence probe still reads one pass.

**Memory budget.** Every run has one budget, shared by its threads, and every structure
the run owns is charged to it before it grows: the derived tier with its indexes and the
cache of base facts found by merges, the local terms the rules make, the candidates and
lookup caches of rule evaluation, the merge's buffers, the previous closure of an
incremental run with its search state, the buffers that write the inferred graph, and the
closure kept for the next run. Sizes come from capacities and from the growth rules of
`Vec` and of hashbrown's tables, so the figures estimate the reasoner's own heap. The
store's block cache is shared with queries and not counted. A charge that would pass the
limit fails the run where it happens, with the rule or the phase in the message.
`strConcat`, `regex` and the other builtins charge the text they read and the literal they
build before building it, and a term that is interned twice is charged once. A run whose
charges passed the limit fails before its commit, also when they are back under it, so a
run that succeeds reports a peak within its limit. After the commit, keeping the closure
for the next run is charged strictly, and a closure that does not fit is not kept. A
closure kept in memory is taken by a run only when it fits that run's budget, and a run
refused at that point leaves it for the next one. A run that fails later has already
changed the closure and drops it, so the next run reads the closure back from a persistent
dataset or runs in full. A kept closure of another commit is dropped without counting
against the run. To keep the shared counter out of the hot paths, each holder of charges
takes 16 KiB at a time, or a 16,384th of the limit when that is smaller, and keeps up to
two such steps of what it frees. The steps that holders keep unused count against the
limit and in the reported peak. They come to a few tenths of a percent of the limit at 16
threads, so a run fails only when its need comes that close to its limit. A run's need
also grows with its threads, by about 2% from 1 to 16 threads on 30,000 triples, so a
limit taken from a 1-thread run can be too small for 16 threads. The cache
of base facts that merges found keeps at most 128 facts per shard and starts over when
full. A fact it no longer holds is found again in the base tier, so an identity rule over
a large input no longer grows it without bound.

The tests are `f16_local_term_budget`, `f16_shared_candidate_budget`,
`f16_incremental_runs_are_charged`, `f16_incremental_reports_peak`,
`f16_limit_holds_on_many_threads`, `f16_stale_kept_closure_is_ignored` and
`f16_keeping_after_commit_stays_within_limit` in `tests/memory.rs`,
`f16_reentry_from_run_threads_is_refused` in `tests/pool.rs`, and
`f16_identity_rule_membership_cache_bound` and `add_rows_charges_the_final_buffers` in
`graph.rs`. With
16 threads and a limit of 48,000 bytes, a run over 200,000 candidates allocated 2.5 MB
beyond its starting heap before it failed, and now allocates 185 KB, mostly decoded store
blocks. On atlas, with 1 million generated triples and interleaved runs, inference took
1,333 ms for RDFS and 1,363 ms for OWL 2 RL before the budget and 1,330 ms and 1,368 ms
with it. Incremental runs that read the closure back from a persistent dataset of 200,000
triples took about 8% longer, 120 ms instead of 112 ms for a run without changes, mostly
in the last step, which commits and saves the generalized facts. The figures are A/B measurements on
a shared host, not published benchmarks.

After the review fixes, a check on atlas compared the build before them with the build
after them on LUBM-style data with 20 universities, 2.5 million triples, running
`materialize` on a fresh copy of the dataset each time. The medians of three interleaved
runs were 4,136 ms before and 4,104 ms after for RDFS on 1 thread, 6,370 ms and 6,370 ms
for OWL 2 RL on 1 thread, 3,446 ms and 3,422 ms for OWL 2 RL on 16 threads, and 2,510 ms
and 2,791 ms for RDFS on 16 threads. The last pair differed by 11%, which eight more
interleaved runs of each build did not reproduce, with medians of 2,634 ms before and
2,569 ms after. The runs are noisy at that size. The load average on atlas was between 2
and 4 during the runs, mostly from the runs themselves. The reported peak heap did not
change in any configuration.

**Diagnostics.** `literal-not-in-datatype` judges each candidate before it counts toward
the finding limit, so valid literals no longer hide a violation behind the limit. It reads
the candidates in pages of one total order, from 256 to 16,384 rows, and stops at the
first finding past the limit, at the end of the candidates, at the deadline or after
262,144 candidate rows. The check is truncated in the first and the last case, and at the
deadline when it has findings, and a truncated check without findings makes the report
incomplete. Every page runs the query and its sort again, and the sort keeps the rows up to
the end of the page, so memory is bounded by 278,528 candidate rows rather than by all of
them, at the cost of repeated query work. Integer bounds compare exactly at any number of
digits. Names follow the XML 1.0 productions. `xsd:dateTime` values with a timezone are in
`xsd:dateTimeStamp`, and `rdf:PlainLiteral` holds well-typed literals of the types derived
from `xsd:string`. `xsd:hexBinary` and `xsd:base64Binary` have disjoint value spaces, as
OWL 2 defines them. A literal whose own datatype the check does not implement is undecided
for every range and never reported. `xsd:double` and `xsd:float` are treated as one value
space, which OWL 2 does not do. The check therefore misses a float literal under a double
range and the reverse, but it never reports a literal that OWL 2 would not report, so it
stays sound. The tests `a_timeout_after_findings_truncates` and
`the_candidate_cap_truncates` in `diagnostics.rs` set the page that times out and the
candidate cap through test-only switches.

**Build.** `Cargo.lock` is current again, so `cargo metadata --locked` succeeds.

**Kept closure without the explicit facts.** The follow-up built §3.5. A kept closure, the
closure of `Derivation::into_closure` and the closure read back from the dataset hold the
derived facts only, over the input graphs of the closure's commit as their base tier. A
fact of the closure is explicit when the base tier holds it, or when its derived row is
flagged, which only GeoSPARQL default geometries and the inserted facts of a running update
are. An incremental run first moves the closure's base tier to the snapshot it runs on.
Removed input facts join the derived tier as candidates of the backward search, so a
removed fact that still follows from the rules stays as a derived fact. Inserted input
facts join it as flagged rows, which start the semi-naive insertion, and at the end of the
run one sorted pass over the base tier drops those that the input graphs hold. When a
default geometry becomes a stated fact or the reverse, the fact moves between the tiers.
A closure read back from the dataset starts from the inferred graph as of the recorded
commit, drops the facts that the input graphs held at that commit, found with one sorted
probe of the base tier, and adds the saved generalized facts. The saved files already held
only the generalized facts, so no on-disk format changed. `--reason-cache-triples` counts
derived facts, and the memory budget charges only the derived tier. `baseScans` of an
incremental run now counts every read of the input graphs, where it counted only the reads
of default geometries.

The example's `keep` subcommand measures this as a server sees it. It runs in full, runs
again without changes so that the closure is kept, measures the kept closure, and then
runs incrementally after a change that removes 1,000 triples of the first university and
adds 1,000, after the change is undone and after it is made again, all with the kept
closure, and once more after a second undo with the closure read back from the dataset.
The data has 100 universities, 13.1 million triples. The runs were made on a Namespace
instance with 16 vCPUs and 32 GiB on 2026-10-18, comparing `a6179e58`, the commit of main
the work started from, with the follow-up, each with the same example. Each build ran
three times at 16 threads and twice at 1 thread, interleaved, on a fresh copy of the
dataset. These are development A/B measurements, not published benchmarks. The table gives
the range of the runs.

| Run | Before | After |
|---|---|---|
| RDFS, kept closure | 19.97 million facts, 2,267 MiB | 6.84 million facts, 918 MiB |
| OWL 2 RL, kept closure | 25.60 million facts, 2,654 MiB | 12.48 million facts, 1,226 MiB |
| RDFS, full run, 16 threads | 12.3 to 13.3 s, 3,313 MiB peak | 10.7 to 11.7 s, 2,409 MiB peak |
| OWL 2 RL, full run, 16 threads | 17.9 to 18.5 s, 3,910 MiB peak | 14.9 to 15.1 s, 3,351 MiB peak |
| RDFS, run without changes from the dataset, 16 threads | 5.7 to 6.1 s, 2,719 MiB peak | 3.4 to 4.1 s, 1,334 MiB peak |
| OWL 2 RL, run without changes from the dataset, 16 threads | 4.1 to 4.5 s, 2,878 MiB peak | 1.8 to 2.0 s, 1,450 MiB peak |
| RDFS, change read back from the dataset, 16 threads | 5.6 to 6.3 s, 2,735 MiB peak | 3.5 to 4.1 s, 1,079 MiB peak |
| OWL 2 RL, change read back from the dataset, 16 threads | 4.1 to 4.7 s, 3,248 MiB peak | 2.3 to 2.4 s, 1,528 MiB peak |
| RDFS, first change with the kept closure, 16 threads | 18 to 34 ms, 462 MiB peak | 72 to 89 ms, 177 MiB peak |
| RDFS, later changes with the kept closure, 16 threads | 14 to 18 ms | 24 to 56 ms |
| OWL 2 RL, first change with the kept closure, 16 threads | 34 to 41 ms, 595 MiB peak | 133 to 142 ms, 304 MiB peak |
| OWL 2 RL, later changes with the kept closure, 16 threads | 26 to 45 ms | 87 to 115 ms |
| RDFS, full run, 1 thread | 24.3 and 25.1 s | 20.8 and 22.4 s |
| RDFS, change read back from the dataset, 1 thread | 7.6 and 7.9 s | 4.4 and 4.5 s |
| RDFS, first change with the kept closure, 1 thread | 193 and 245 ms | 81 and 82 ms |
| RDFS, later changes with the kept closure, 1 thread | 13 to 16 ms | 30 to 85 ms |

The peaks are the process's allocations during the run, which include the data the run
returns, and the kept closure is the heap it holds after the run.

**Memory tradeoffs of the kept closure.** The kept closure takes 40% of its previous memory
for RDFS and 46% for OWL 2 RL on this data, and a run that reads it back from the dataset
peaks at 39% to 50% of its previous heap and finishes in 40% to 73% of the time. Small
changes with a kept closure became slower, 1.3 to 4.9 times at 16 threads and 1.9 to 6.5
times at 1 thread for the later changes, though the first change at 1 thread became faster.
[ROADMAP](../ROADMAP.md) item 87 is to win this time back, as designed under "Roadmap
follow-ups (2026-10-18)" below. The backward search and the insertion now read the input
graphs from the store's compressed blocks, where the previous closure answered them from
its own hash tables. The first change after the closure is kept decodes most of the blocks
it reads, and later changes find more of them in the block cache. On LUBM-style data with
20 universities on a 16-thread development machine, the backward search of a change took
about 30 ms when its blocks were not decoded and 6 to 9 ms when they were. Handing the
decoded blocks of one rule evaluation of the backward search to the next did not change the
time measurably, so the follow-up does not do it. The tradeoff is the opposite of the one
the engineering rules prefer by default. It was taken because the kept closure is what
bounds the number of datasets a server can keep warm, and a change of 1,000 triples still
finishes in under 150 ms.

**`dt-not-type` coverage.** The follow-up built §8's coverage. `literal-not-in-datatype`
finds the literals that are ill-typed for their own datatype by a scan of the vocabulary.
The scan reads the literal keys of a generation's base vocabulary once, and the generation
keeps the ids of the ill-typed ones until it is dropped. The scan stops at 262,145 ids, one
more than the check's cap, so the list takes at most 2 MiB. The check holds its own lock on
the generation's list while the scan runs, so a second check that starts meanwhile waits
for it instead of reading the vocabulary again. The delta vocabulary is read each time, up
to the end of the checked snapshot, 4,096 positions to a hold of its lock. Literals inlined
in their ids take only canonical, valid forms and are not read. The check then decodes the
ill-typed literals 256 at a time, in the order of their ids with those of the base
vocabulary first, and looks up the subjects and properties that have each one as a value in
the checked graphs, so a literal that no checked graph holds is not reported.
`datatype-range.rq` keeps only its range branch. With the inferences, the check also reads
the literal typings, the facts `lt rdf:type DT` with a literal subject, among the
generalized facts of the materialization. It reads them from the closure the dataset keeps
for the recorded commit, or else from the generalized facts that a persistent dataset
saved. These typings come from rules such as a range through a subclass of a datatype, and
RDF cannot state them, so they never reach the inferred graph. Their findings have the
literal as their focus and the basis `uses-inferences`, and only the typings of literals
that the checked graphs hold are reported. Reading them from a kept closure holds the
cache's lock only for a pass that compares ids, and the literals are decoded after it is
released, because a run takes that lock while it holds the store's writer lock. While a run
has an in-memory dataset's closure out of the cache, its diagnostics have no typing
findings. The typings are read only for the RDFS, RDFS simple and OWL 2 RL profiles, whose
rules OWL 2 entails, so a literal typed outside its value space is one OWL 2 reports too,
and the check stays sound. Jena rules can type a literal with any datatype, so their
typings are not read. The test `f16_ill_typed_literals_come_from_the_vocabulary` in
`diagnostics.rs` counts the scans of the base vocabulary, and
`f16_literal_typings_of_a_kept_closure` in `tests/diagnostics.rs` reads typings from a kept
closure and from saved files.

The example's `diag` subcommand runs the check twice on the same snapshot. On the same
instance and data, with 100 universities and no ill-typed literals, the first check took
10.9 to 11.6 s with a peak heap of 1,098 MiB before, when a query read every literal value,
and 9 to 17 ms with 1.5 MiB after. A second check on the same generation took 10.7 to
12.5 s before and under 1 ms after. Diagnostics have no memory budget, so the memory
they keep is bounded by construction, at 2 MiB per generation.

**Review of the follow-up.** A review of the follow-up found the remembered ill-typed
literals unbounded, kept in a process-wide list of eight generations that outlived their
datasets, and cloned on each call. They are now the bounded list of ids described above.
It also found that the delta vocabulary was read under one hold of its lock, that the
typings ignored the checked graphs, and that `Graph::set_base` left the distinct-count
estimates at the size of the first base tier. The validation found that `literal_typings`
decoded every typing while it held the cache's lock. All five are fixed, with the tests
`f16_ill_typed_memory_is_bounded`, `f16_concurrent_first_scans_share_one_read`,
`f16_delta_scan_releases_the_lock` and `f16_delta_literal_in_a_named_graph` in
`diagnostics.rs`, `f16_typings_follow_the_checked_graphs` in `tests/diagnostics.rs`,
`f16_estimates_follow_the_new_base` in `graph.rs` and
`literal_typings_come_only_from_sound_profiles` in the `sparkles` crate's
`tests/handles.rs`. The two readers of the saved generalized facts are now one streaming
reader, which also refuses a key longer than the bytes left in its file.

**Re-review of the fixes.** The re-review of these fixes found nothing above P3, and its
test gaps are closed. `corrupt_saved_files_are_refused` in `maintain.rs` writes saved files
with an overlong key, an unknown log op, a wrong fact count and a short log, and checks
that each is refused. It also showed that a fact count short of the facts was accepted, so
the reader now requires the facts to end the file. `f16_delta_scan_stops_at_the_snapshot`
in `diagnostics.rs` pins the snapshot bound of the delta read with literals, and
`f16_ill_typed_list_waits_and_grows` covers a deadline that passes while another call holds
the list and a list that stopped early. Diagnostics now read the literal typings only when
`literal-not-in-datatype` runs, and the read counts toward the report's timeout. Two points
stay open. `literal_typings` keeps a pair of ids for every `rdf:type` fact of the kept
closure whose subject is a literal and whose object is a decidable datatype, under the
cache's lock and outside the memory budget. That is 16 bytes per typing whether or not it
is a violation. The branch that decodes a typing whose subject is a term the rules made has
no test. The RDFS, RDFS simple and OWL 2 RL rules do not seem to make literal terms, so the
branch may be unreachable for the profiles whose typings are read.

**The flaky budget test.** `f16_shared_candidate_budget` in `tests/memory.rs` failed now and
then under load, when the heap it measured grew by 1.4 to 3 MB past its bound. The
reasoner's own charges were not the cause. Each strict charge is taken from the shared
counter before the allocation it covers, so a thread that is descheduled between the two
holds its charge and cannot let another thread pass the limit. Allocation backtraces showed
the store's change log writer thread, which encodes the loaded triples in the background
and so allocated during the measured window whenever the scheduler delayed it. The test
now writes the change log out before it measures. Under 32 busy processes on a 16-thread
development machine, 3 of about 40 runs failed before the fix. After it, 60 runs grew by at
most 162 KB, against a bound of the 48,000-byte limit plus 256 KiB.

**Open parts of Phase 1.** The acceptance runs of §11 and §12 moved to
[ROADMAP](../ROADMAP.md) item 1. They are A1 at 1,000 universities, OWL2Bench, and the
LUBM and OWL2Bench runs in X06's harness. The measurements above come from the example's
own LUBM-style generator at 100 universities. On 2026-10-18 X06's harness gained what
these runs need. `scripts/bench-lubm.sh` and `scripts/bench-owl2bench.sh` record each
materialization's `run.peakBytes`, threads and a digest of the sorted derived triples, so
A1 compares the head with a build of the commit before Phase 1 from two runs of
`bench-lubm.sh 1000` with `ENGINES=""` and `INFER_PROFILES=rdfs`. Both scripts also time
Jena's rule reasoners on the same data on request. X06's Outcome records the smoke runs.
On 2026-10-19 the LUBM(100) run of X06's harness on forge, with the release binary of
commit 81478fbe and 20 threads, materialized the 13,876,262 triples of the UBA data. The
`rdfs` profile took 11.4 s, peaked at 3,204 MiB of RSS with a peak heap estimate of
1,224 MiB, and derived 5,527,675 triples. The `owl-rl` profile took 17.3 s, peaked at
3,837 MiB with an estimate of 1,785 MiB, and derived 10,907,029 triples. BENCHMARKS.md
compares the five engines' queries over that closure. Jena's reasoners were not run in this
batch.

**Acceptance runs of 2026-10-23.** Roadmap item 1 compared the release binary of the
settled head 6699f27d with a release build of c7a6811c, the parent of the first Phase 1
commit 57a71124, on forge. Each materialization ran with no engines, with 20 threads, in a
systemd scope of 12 GiB without swap, and the closure dump was left out, so the digest of
the sorted derived triples is the comparison.

| Run | Head 6699f27d | Baseline c7a6811c |
|---|---|---|
| LUBM(100), 13.9M triples, `rdfs` | 11.4 s, 3,181 MiB peak RSS, 1,213 MiB peak heap estimate | 18.7 s, 4,884 MiB |
| LUBM(100), `owl-rl` | 16.6 s, 3,972 MiB, 1,785 MiB | 25.6 s, 5,450 MiB |
| OWL2Bench, 200 universities, 15.6M triples, `rdfs` | 16.6 s, 3,815 MiB, 1,608 MiB | 26.8 s, 6,140 MiB |
| OWL2Bench, 1 university, `owl-rl` | 4.8 s, 885 MiB | 24.2 s, 2,721 MiB |
| OWL2Bench, 2 universities, `owl-rl` | 44.7 s, 1,894 MiB, 4,914,915 derived triples | Stopped at the limit of 50 million inferred triples, when `prp-trp` produced 213 million candidates |
| LUBM(1000), 138.3M triples, `rdfs` | Stopped at the limit after 15 s and 9,377 MiB, when `rdfs2` produced 40.8 million candidates in the first iteration | Killed at the 12 GiB limit after 25 s |

Both binaries derived the same closure, by digest, in every run that both finished, at
every OWL2Bench size from 1 to 200 universities under `rdfs` and in both LUBM(100)
profiles. A3 ran separately on LUBM(100) with OWL 2 RL, with `RAYON_NUM_THREADS` set for
both binaries because the baseline has no `--threads`, twice per binary in the order head,
base, base, head. At 16 threads the head took 16.6 and 16.5 s against the baseline's 26.1
and 25.4 s, 1.56 times faster, and at 1 thread 33.1 and 33.0 s against 36.2 and 36.1 s, so
A3 is met. All eight runs derived the same closure. The head's peak RSS is 0.65 times the baseline's on LUBM(100) `rdfs` and
0.62 times on OWL2Bench at 200 universities, short of A1's half, although A1 sets the
half for LUBM(1000), which neither binary can finish. OWL2Bench under OWL 2 RL from five
universities on also stops at the limit in both binaries.

These runs left A1 at 1,000 universities open, because the reasoner's limit of 50 million
inferred triples was fixed and the RDFS closure of LUBM(1000) passes it in the first
iteration. Item 98 made the limit configurable and decided A1, as the next section
records.

**A1 at 1,000 universities, item 98 (2026-10-23).** `sparkles infer --max-inferred`,
`serve --reason-max-inferred`, a request's `maxInferred`, `ReasonRequest::max_inferred`
and the Python, Node and JVM bindings now set the limit. It stays at 50 million by
default, and 0 removes it. The default still guards against runaway rules. OWL2Bench
under OWL 2 RL at five universities, for example, grows past 28 GiB in both engines
before it ends, and a user who expects a closure above 50 million triples, such as RDFS
on LUBM(1000), raises the limit for that run. A run past its limit fails with
`InferredLimitExceeded`, the code `inferred-limit` outside the reasoner, the rule that
produced the most candidates and the settings that raise the limit, and it commits
nothing.

The runs used a Namespace instance with 16 vCPUs and 32 GiB, the largest shape the
account allows, because forge has 16 GiB. Both binaries were built there with Rust 1.98.1,
one from 0e0fa5b4, whose code the item's commit keeps, and one from c7a6811c, the commit
before Phase 1, with its fixed limit raised to 4 billion so that only memory could stop
it. Each materialization ran alone with 16 Rayon threads, after the page cache was
dropped, in a cgroup of 28 GiB without swap, on the store its own binary loaded from the
inputs of the item 1 refresh. The digest is the SHA-256 of the sorted derived triples.

| Run | Head | Baseline c7a6811c, limit raised |
|---|---|---|
| LUBM(100), `rdfs` | 8.6 s, 3,125 MiB peak RSS, 1,213 MiB peak heap estimate | 13.5 s, 4,903 MiB |
| LUBM(1000), 138.3M triples, `rdfs` | 90.9 s, 15,187 MiB peak RSS, 12,319 MiB peak heap estimate, 55,069,413 derived triples | Killed at the 28 GiB limit after 4 min 41 s, at 28,614 MiB RSS |
| OWL2Bench, 1 university, `owl-rl` | 3.0 s, 809 MiB, 499 MiB | 19.8 s, 3,031 MiB |
| OWL2Bench, 2 universities, `owl-rl` | 25.0 s, 1,892 MiB, 1,755 MiB, 4,914,915 derived triples | 162.5 s, 13,713 MiB |
| OWL2Bench, 5 universities, `owl-rl` | Killed at the 28 GiB limit after 12 min 50 s | Killed at the 28 GiB limit after 31 s |

Every run that both binaries finished derived the same closure by digest, the LUBM(100)
RDFS closure included, so A1's last condition holds. The LUBM(1000) run also left
11,170,179 generalized triples unwritten, as both engines do. A1 is met. The head
completes LUBM(1000) under RDFS. Its peak heap estimate of 12,319 MiB is well within 3
times the derived tier plus 1 GiB, which at about 135 bytes for each of its 66.2 million
derived rows comes to about 26 GiB. It is also less than half of what the baseline needs,
since the baseline was still growing when the 28 GiB limit stopped it, and half of that
is 14,336 MiB. By peak RSS the head's 15,187 MiB is 0.53 of the 28 GiB, so on that measure
the half holds only if the baseline needs more than 29.7 GiB, which an instance of 32 GiB
cannot show. OWL2Bench under OWL 2 RL is 6.5 times faster with 0.14 times the RSS at two
universities, where the baseline now finishes. From five universities on, neither binary
fits 28 GiB, so the larger sizes were not run.

**Deviations.**
* §3.5 expected the same cache limit to keep the closure of an input about four times
  larger on RDFS. On the LUBM-style data a closure of 19.97 million facts became one of
  6.84 million, so the same limit keeps the closure of an input about 2.9 times larger.
* The LUBM data comes from the example's own generator, with about 131,000 triples per
  university, not from the UBA generator.
* `literal-not-in-datatype` narrows the candidates with one SPARQL query per page of
  candidates.
* `examples/reasoner-bench.rs` and `examples/incremental.rs` did not gain flags. The new
  example takes `RAYON_NUM_THREADS`.

### Roadmap follow-ups (2026-10-18)

**Small incremental runs over a kept closure.** This is [ROADMAP](../ROADMAP.md) item 87.
Since the kept closure holds only the derived facts, the backward search and the insertion
read the explicit facts from the store's compressed blocks. On LUBM-style data with 13.1
million triples, a change of 1,000 triples took 1.3 to 4.9 times longer at 16 threads than
when the closure held the explicit facts. Handing decoded blocks from one rule evaluation
to the next did not change the time measurably.

The work was to be done when a change of 1,000 triples with a kept closure on the same
data and instance took at most 1.2 times its time before the follow-up, at 16 threads, and
the kept closure's memory stayed under 60% of the closure that held the explicit facts. The
differential tests of `incremental.rs` and the multi-commit runs of the follow-up's review
were to pass unchanged.

**What was built.** Profiles of the change showed that most of the backward search's time
went to the first read of each block, which decodes it, and that the searches made those
reads one after another on one thread. A kept closure's cache of decoded blocks does not
fit that pattern, since a search reads a few rows from each of many blocks. The work
therefore remembers answers instead of blocks, and spreads the remaining reads over the
threads.

* The base tier of a kept closure remembers its lookups. A lookup is keyed by its bound
  positions. Its answer is kept whole when it has at most 64 facts, and otherwise as its
  first facts, up to 64. A later lookup with the same bindings reads the kept facts first
  and reads the blocks only for the facts after them. Most lookups of the backward search
  find a few facts or stop at the first, so they read no block once their answer is kept.
  The answers of earlier runs are in a table that the run's threads read without a lock,
  and the answers a run finds go to 64 shards behind locks until the closure is kept again.
* The answers are charged to the run's memory budget before they are kept, they are capped
  at 64 MiB per closure, and they count in the kept closure's heap. A lookup whose answer
  does not fit reads the blocks as before, so the answers change speed and never results.
* The answers follow the input graphs from one snapshot to the next. When the closure moves
  to a later snapshot of the same generation, each triple that the input graphs gained or
  lost updates the answers of the seven patterns it matches. With one input graph a scan
  visits the facts in key order, so a gained triple joins an answer at its place. With
  several input graphs the order depends on the graphs, so the answers those triples match
  are dropped. A closure of another generation is dropped with its answers, and a lookup
  with a term that the rules made is not kept. `Closure::set_inputs` hands the old answers
  to its caller, which puts them back only after it has brought them up to date.
* The searches of one deletion round run in parallel. The round deletes its unprovable
  facts only after every search, so all its searches see the same live facts and none
  depends on what another learns. They split into chunks, each searched by a search of its
  own that also reads what earlier rounds learned, and the chunks' results join afterwards.
* A closure kept after its commit gets room for more rows in its row buffer and its index
  shards, a 64th to a 16th of its rows. Without it, the first change doubled the row buffer
  and rehashed the index while the run waited, 12 ms on LUBM-style data with 20
  universities.
* An evaluation of the backward search allocates its per-step caches and its memory
  charges on first use, since most evaluations read no block.

Two other approaches were measured and dropped. A bounded search run ahead of the first
change, to decode the blocks it would read, cut the first change but slowed the later ones.
The insertion's rule evaluations look in their own per-step caches first and read the
remembered answers when those miss. Making them read the remembered answers directly, as
the backward search does, left the OWL 2 RL change at the same time.

**Outcome.** The runs repeat the follow-up's `keep` measurement on the same 16-vCPU
Namespace instance shape and data on 2026-10-18. They compare `a6179e58`, before the
follow-up, with `5b2085bc`, the follow-up, and with this work, interleaved on fresh copies
of the dataset, three times at 16 threads and twice at 1 thread. These are development A/B
measurements, not published benchmarks. The table gives the range of the runs.

| Run | Before the follow-up | Follow-up | Item 87 |
|---|---|---|---|
| RDFS, kept closure | 2,267 MiB | 918 MiB | 927 MiB |
| OWL 2 RL, kept closure | 2,654 MiB | 1,226 MiB | 1,244 MiB |
| RDFS, first change, 16 threads | 18 to 20 ms, 462 MiB peak | 77 to 90 ms, 172 MiB peak | 26 to 31 ms, 22 to 24 MiB peak |
| RDFS, undo, 16 threads | 11 to 14 ms | 28 to 29 ms | 15 to 17 ms |
| RDFS, change again, 16 threads | 11 to 19 ms | 46 to 60 ms | 12 to 13 ms |
| OWL 2 RL, first change, 16 threads | 34 to 36 ms, 595 MiB peak | 140 to 165 ms, 303 MiB peak | 82 to 88 ms, 35 to 52 MiB peak |
| OWL 2 RL, undo, 16 threads | 40 to 44 ms | 109 to 116 ms | 61 to 65 ms |
| OWL 2 RL, change again, 16 threads | 21 to 26 ms | 88 to 97 ms | 51 to 54 ms |
| RDFS, full run, 16 threads | 11.8 to 12.5 s | 9.8 to 10.3 s | 10.4 to 10.6 s |
| OWL 2 RL, full run, 16 threads | 16.1 to 16.3 s | 13.9 to 14.5 s | 14.2 to 14.9 s |
| RDFS, change read back from the dataset, 16 threads | 5.4 to 5.7 s | 3.3 to 3.8 s | 3.4 to 3.6 s |
| OWL 2 RL, change read back from the dataset, 16 threads | 4.0 to 4.1 s | 2.2 to 2.4 s | 2.1 to 2.2 s |
| RDFS, first change, 1 thread | 234 and 235 ms | 80 and 86 ms | 74 and 80 ms |
| RDFS, undo and change again, 1 thread | 12 ms | 29 to 47 ms | 11 to 16 ms |
| OWL 2 RL, first change, 1 thread | 274 and 284 ms | 132 and 142 ms | 137 and 143 ms |
| OWL 2 RL, undo and change again, 1 thread | 26 to 45 ms | 78 to 113 ms | 51 to 73 ms |
| RDFS, full run, 1 thread | 22.4 and 22.6 s | 18.6 and 19.0 s | 18.9 and 20.4 s |
| OWL 2 RL, full run, 1 thread | 38.4 and 44.8 s | 33.6 and 34.0 s | 36.9 and 37.7 s |

The goal is met for memory and for the RDFS changes after the first, and missed for the
first RDFS change and for OWL 2 RL. The kept closure grew by 10 MiB for RDFS and 18 MiB for
OWL 2 RL, mostly the room for more rows, and stays at 41% and 47% of the closure that held
the explicit facts, against 40% and 46% after the follow-up. That is under the 60% bound
even with the full 64 MiB of answers. The first RDFS change takes about 1.5 times its time
before the follow-up by the medians of the runs, the undo about 1.25 times and the change
made again the same time. The OWL 2 RL changes take 1.5 to 2.4 times their time before the
follow-up, where the follow-up took 2.7 to 4.3 times. Full runs and runs that read the
closure back from the dataset did not change beyond the noise of the instance at 16
threads. At 1 thread the RDFS changes after the first are back to their time before the
follow-up, and the first change stays well under it. The OWL 2 RL changes after the first
take about twice their time before the follow-up, and the first change is as it was after
the follow-up. The OWL 2 RL full runs at 1 thread took 36.9 and 37.7 s, against 33.6 and
34.0 s for the follow-up, 10% to 12% more. Full runs of one build at 1 thread differed by
up to 17% on this instance, and a full run executes only a few added checks per rule
evaluation of this work, so two runs cannot tell a regression from noise. The difference is
past the 5% that an AMD Namespace instance resolves, so it has to be measured again on an
Intel host before item 87 closes.

**Intel rerun of the 1-thread full run.** The rerun ran on atlas, an Intel Core i5-13500 with 20
threads and 15 GiB, on 2026-10-18, with `lubm keep` on the same LUBM-style data with 100
universities, for `5b2085bc` and for `23075691`, the tip of main with item 87. The two
builds ran in turn on a fresh copy of the dataset, six times each, and the order of the two
swapped from round to round. The runs at 1 thread were pinned to one performance core, and
the runs at 16 threads were not pinned. Nothing else ran on the machine during the runs, and
the whole process peaked at 3.74 GiB resident for OWL 2 RL and 2.88 GiB for RDFS. These are
development A/B measurements, not published benchmarks. The table gives the range of the
six runs and the median where the runs differ.

| Run | `5b2085bc` | Item 87 |
|---|---|---|
| OWL 2 RL, full run, 1 thread | 42.1 to 43.7 s, median 42.2 | 42.1 to 43.5 s, median 42.4 |
| OWL 2 RL, full run, 16 threads | 19.0 to 19.4 s, median 19.1 | 19.0 to 19.4 s, median 19.2 |
| RDFS, full run, 1 thread | 26.3 to 26.4 s, median 26.3 | 26.3 to 26.7 s, median 26.5 |
| RDFS, full run, 16 threads | 13.7 to 14.0 s, median 13.9 | 13.7 to 13.9 s, median 13.8 |
| OWL 2 RL, change read back from the dataset, 1 thread | 3.09 to 3.16 s, median 3.11 | 3.02 to 3.20 s, median 3.08 |
| OWL 2 RL, change read back from the dataset, 16 threads | 2.52 to 2.60 s, median 2.55 | 2.49 to 2.56 s, median 2.50 |
| OWL 2 RL, first change, 1 thread | 167 to 173 ms, median 169 | 156 to 168 ms, median 158 |
| OWL 2 RL, first change, 16 threads | 167 to 183 ms, median 173 | 101 to 150 ms, median 127 |
| OWL 2 RL, undo, 1 thread | 133 to 147 ms, median 137 | 79 to 86 ms, median 79 |
| OWL 2 RL, undo, 16 threads | 135 to 154 ms, median 141 | 53 to 64 ms, median 56 |
| OWL 2 RL, change again, 1 thread | 93 to 96 ms, median 94 | 56 to 58 ms, median 56 |
| OWL 2 RL, change again, 16 threads | 92 to 95 ms, median 94 | 44 to 76 ms, median 44 |
| RDFS, first change, 1 thread | 101 to 103 ms, median 103 | 101 to 103 ms, median 102 |
| RDFS, first change, 16 threads | 108 to 114 ms, median 111 | 40 to 50 ms, median 44 |
| RDFS, undo, 1 thread | 33 ms | 19 to 20 ms, median 19 |
| RDFS, undo, 16 threads | 32 to 33 ms | 14 to 23 ms, median 17 |
| RDFS, change again, 1 thread | 51 to 54 ms, median 53 | 16 to 17 ms, median 17 |
| RDFS, change again, 16 threads | 57 to 59 ms, median 58 | 12 to 17 ms, median 13 |
| OWL 2 RL, kept closure | 1,226 MiB | 1,244 MiB |
| RDFS, kept closure | 918 MiB | 927 MiB |

The 1-thread gap does not hold on Intel. The OWL 2 RL full run at 1 thread took 0.4% more
time by the medians, against 10% to 12% on Namespace, and the ranges of the two builds
overlap almost completely. The other full runs differ by between minus 0.7% and plus 0.8%.
The run on Namespace therefore most likely measured the noise of that instance, which differed by up
to 17% between runs of one build, and not a cost of item 87. The runs that read the closure
back from the dataset did not change either. Every small change except the first at 1 thread
is faster than after the follow-up, by 1.4 to 4.5 times at 16 threads and by 1.7 to 3.1 times
at 1 thread for the changes after the first. The first changes at 1 thread are as fast as
after the follow-up for RDFS and 7% faster for OWL 2 RL. The runs
did not include `a6179e58`, so they do not restate the goal of 1.2 times the time before
the follow-up. The kept closure grew by 9 and 18 MiB, as it did on Namespace. Atlas is
slower than the Namespace instance at 1 thread for these runs, and its 16-thread runs are
slower too, since six of its 14 cores are performance cores that run two threads each.

Two costs remain. The first change of a kept closure still decodes blocks that no answer
covers, and a few of its lookups read long ranges in one thread, such as the members of a
class that the inferred graph's rows interleave with, through rule `rdfs3`. In the OWL 2 RL
changes, the insertion takes about 30 ms at 16 threads. Its rule evaluations read the input
graphs from the blocks or the remembered answers for each of the 2,000 changed facts, where
they probed the closure's hash tables before the follow-up. The evaluations' allocations
and shared counters contend at 16 threads, and so may the locks of the answers that the run
finds, which were not measured on their own. [ROADMAP](../ROADMAP.md) item 87 stays open
for them.

The tests check the answers for coherence as well as the results. The differential tests of
`incremental.rs` now check every remembered answer against a scan of the input graphs after
each step, and they gain steps that undo the previous one, steps whose state is reached
through an earlier commit, and runs on one thread and on six. A test that skips the update
of the answers fails them. `kept_lookups_follow_commits_undos_and_failed_runs` in
`tests/incremental.rs` interleaves cancelled runs and runs that fail for memory with
several commits and undos for every profile. The unit tests of `lookups.rs` cover the
budget, the cap, the order of updated answers, bulk changes and the move to another budget.
A failed or cancelled run cannot leave a wrong answer behind, because an answer is a fact
of the snapshot it was read from, and the closure that a run takes out of the cache is
dropped when the run fails. A read that the remembered answers serve counts in `baseScans`
as the scan it saves, so with one input graph the count does not depend on what is
remembered. `f16_default_geometry_changed_subjects` in `tests/geosparql.rs` therefore still
pins the difference in `baseScans` between the kept and the read-back closure, and it fails
when the kept closure scans its default geometries again.

**Review of item 87.** A review found no defect in the results and three P2s. The GeoSPARQL
scan test had been relaxed so far that it no longer caught a kept closure that scans its
default geometries again, which the count of remembered reads above restores.
`Closure::new` rebuilt the base tier with a panic on an I/O error, and it now attaches the
answers to a tier it holds alone and returns the error of a rebuild. This section had said
that the insertion does not read the remembered answers, and the text above is corrected.
Of the P3s, a read now prefers a longer answer that the run found to an earlier run's first
facts, a change of more than 65,536 triples and more than the answers kept drops the
answers instead of bringing them up to date, `set_budget` asserts that it can move the
answers, and the id of `literal-not-in-datatype` is exported for the typings gate. A check
of a contains answer against a scan with several input graphs found no difference, since
such a scan emits each triple once. A Namespace A/B of these fixes against the commit
before them measured no change in the small runs at 16 threads.

Some points stay open under item 87. The 64 MiB cap is checked before each answer without a
reservation, so threads can pass it by one answer each, and an update can grow the answers
past it until the closure is kept again, when they are dropped. Only the run's budget
bounds that growth, and it has no limit unless `max_memory` sets one. The answers and the
room for more rows are not counted by `--reason-cache-triples`, which counts derived
triples, and a run with a tight `max_memory` can now be refused when it takes a closure
whose answers do not fit. Bringing the answers up to date has no cancel check below the
bulk threshold. `checked` in the report of an incremental run now depends on the thread
count, since the chunks of a round do not share what they learn, and no test proves that
the parallel round search runs. The answers are checked against a scan only with one input
graph.
