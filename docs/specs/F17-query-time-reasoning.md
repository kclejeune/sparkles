# F17: Query-time reasoning

> **Status:** implemented in part, Phase 1.
>
> **Phases:** Demand derivation, Jena backward rules, tables cached per snapshot and
> view, budgets and EXPLAIN are available. Automatic cost selection, hybrid execution,
> OWL 2 QL rewriting, additional settings and surfaces, and reuse across commits remain
> planned in Phase 2. Incremental maintenance through F13 and later provenance remain
> planned in Phase 3.
>
> **User docs:** [Query-time reasoning](../API.md#query-time-reasoning) and
> [features](../reasoning/README.md#reasoning-validation-and-schema).
>
> This is the design as written before implementation. The [Outcome](#outcome) records
> what shipped and the remaining work.

This design was written from Bancilhon, Maier, Sagiv and Ullman, "Magic sets and other
strange ways to implement logic programs" (PODS 1986), Beeri and Ramakrishnan, "On the
power of magic" (Journal of Logic Programming, 1991), Calvanese, De Giacomo, Lembo,
Lenzerini and Rosati, "Tractable reasoning and efficient query answering in description
logics: the DL-Lite family" (Journal of Automated Reasoning, 2007), the W3C OWL 2 Profiles
recommendation (OWL 2 QL and OWL 2 RL), the W3C RDF 1.1 Semantics and SPARQL 1.1
Entailment Regimes recommendations, Apache Jena's public documentation of its rule
engines (forward, backward and hybrid modes of `GenericRuleReasoner`, tabling, and the
assembler's `ja:InfModel`), and the Sparkles code. It builds on materialized reasoning
and RDFS on read ([C08](C08-inference-freshness.md)), materialization at scale
([F16](F16-materialization-at-scale.md)), the incremental core
([F13](F13-incremental-views-and-provenance.md)), budgets
([C01](C01-observability-and-budgets.md)), triple protections
([C12b](C12b-triple-access-control.md)), the MCP server ([C11](C11-mcp-server.md)) and
the Fuseki converter ([G08](G08-fuseki-configuration.md)).

## 1. Summary, goals, non-goals

Sparkles reasons in two ways today. Materialization (C08) derives every entailment into
`urn:x-sparkles:inferred` ahead of time, and queries read it. RDFS on read
(`crates/sparkles-core/src/sparql/rdfs.rs`) rewrites a query's triple patterns per Jena's
`MatchRDFS`, for RDFS only. Between them is a gap:

- A user with a large graph who asks a few selective questions pays for the whole
  closure before the first answer, and again, incrementally, after each write.
- A materialization that is stale gives answers that may be wrong. The
  `Sparkles-Inferences` header says so, and the only remedy is a run.
- Jena's backward (`<-`) rules are parsed and skipped with a warning (`engine::compile`
  in `crates/sparkles-reasoner/src/engine.rs`), so a Jena rule set written for the LP
  engine loses its backward part. Nested backward rules in a forward rule's head are
  flattened into forward rules and materialized.
- OWL 2 QL ontologies, whose standard query answering is by rewriting, have no support.

This spec adds lazy reasoning: a query derives the facts it needs at the time it runs,
from the snapshot it reads, and the planner picks per pattern between reading
materialized facts and deriving them.

**Goals**

1. On-demand derivation of the facts a query's patterns need, through the demand
   transformation of the rule program for the patterns' bound positions (§3), evaluated
   with the existing semi-naive engine.
2. The same answers as materializing with the same rules, for every rule set the
   materializer accepts, plus Jena backward and hybrid rules (§5).
3. A cost-based choice per pattern between reading materialized facts and deriving them
   (§4), with fresh materialization preferred and stale materialization never used
   silently.
4. Derived tables cached per snapshot and caller view, reused across commits that do not
   touch their inputs, and later kept current by F13 (§6).
5. OWL 2 QL query rewriting for ontologies in that profile (§7).
6. Bounded work: every derivation runs under the query's deadline and memory budget and
   a fact limit, and fails with a clear error (§8).
7. A decision on a Datalog surface syntax (§9).

**Non-goals.** OWL 2 DL or EL reasoning. Full SPARQL entailment regimes (the SPARQL 1.1
Entailment Regimes recommendation's RDFS and OWL Direct Semantics regimes are cited for
terminology, not implemented as such). Rules with side effects. Changing RDFS on read,
which stays the Jena-compatible mode for RDFS. A distributed or federated reasoner.

## 2. User-visible behaviour

### 2.1 Choosing the mode

The query parameter `reasoning` takes `true` and `false` today: whether the default
graph includes the inferred graph. It gains three values:

| Value | Meaning |
|---|---|
| `materialized` (alias `true`) | Read the inferred graph, as today. |
| `demand` | Derive on demand with the dataset's query-time rules (§2.2), and do not read the inferred graph. |
| `auto` | Choose per pattern (§4). |
| `false` | No reasoning. |

The default stays `materialized` when the dataset has a materialization and `false`
otherwise, until the dataset sets a query-time default (§2.2). The same values are
`QueryOptions::reasoning` in the library, `--reasoning` on `sparkles query`, the
`reasoning` argument of the MCP `sparql_query` and `explain_query` tools, and a selector
on the UI's query page.

### 2.2 Dataset settings

`PUT /$/reason/{ds}/query` sets the dataset's query-time reasoning:

```json
{ "default": "auto",
  "profile": "owl-rl",              // rdfs | rdfs-simple | owl-rl | owl-ql | rules
  "rules": null,                    // Jena rule text for profile "rules"
  "inputs": { "ontologyGraphs": ["http://ex.org/onto"] },   // C08 §11 input graphs
  "budget": { "maxDerived": 1000000, "maxMillis": null } }
```

`GET` returns it with the status of the tables (§6), and `DELETE` removes it. When the
dataset has a materialization, the query-time profile defaults to the materialization's
profile and inputs, so `auto` compares like with like. A different profile is allowed,
and then `auto` never reads the materialization (§4.1). The setting is stored in
`reasoning.json` next to the C08 record, and `serve --query-reasoning NAME=PROFILE` sets
it at startup. A server locks it through C19 settings like other dataset settings.

### 2.3 Responses

A response that used derivation carries
`Sparkles-Reasoning: demand; derived=48211; tables=3; cached=2`, giving the facts derived
for this query, the tables it used and how many came from the cache. A response under
`auto` that read a stale materialization for some pattern also carries C08's
`Sparkles-Inferences` header, which cannot happen with the default policy (§4.2).

### 2.4 EXPLAIN

EXPLAIN and the UI's plan view show a `Derive` operator for each derived pattern, with
the predicate, the adornment (which positions are bound, such as `rdf:type^bf`), the
rules in the pattern's dependency cone, the estimated and actual facts, and whether the
table came from the cache. Under `auto`, each intensional pattern shows the chosen source
and the two estimated costs.

### 2.5 CLI

```
sparkles query --loc DB --reasoning demand --profile owl-rl 'SELECT ?x { ?x a ex:Person }'
sparkles query --loc DB --reasoning demand --rules family.rules --explain '…'
sparkles infer --loc DB --query-default auto        # sets the dataset's query-time default
```

## 3. Derivation on demand

### 3.1 Intensional patterns

The rule program's **intensional** predicates are those in a rule head, and for
`rdf:type` the classes in a head's object position. A triple pattern is intensional when
its predicate is intensional, when its predicate is a variable and the program derives
any fact, or when it is `?x rdf:type C` with `C` intensional. Other patterns are plain
scans and keep every index optimization.

### 3.2 Adornments and the demand transformation

For each intensional pattern, the planner computes an adornment: which of subject and
object are bound when the pattern runs, either by a constant or by a variable that the
join order binds earlier. `rdf:type` is split by class when the class is a constant, so
`?x rdf:type ex:Person` is the predicate `type:ex:Person` with adornment `f`.

The demand transformation (magic sets, after Bancilhon et al. and Beeri and
Ramakrishnan) rewrites the program for that adorned predicate:

1. Starting from the query's adorned predicate, it propagates adornments through each
   rule body in the order the engine's planner would evaluate it (sideways information
   passing), giving adorned versions of every intensional predicate the query can reach.
2. Each adorned rule gets a magic atom in front of its body: `magic_p^bf(?s)` holds the
   subject values for which `p` is needed.
3. Magic rules derive the demand of each body atom from the magic atom of the head and
   the atoms before it.
4. The query's constants, and the bindings of earlier joins, are the seed facts of the
   query's magic predicate.

The result is an ordinary rule program over triples plus magic predicates. Magic facts are
represented as triples with reserved predicate ids local to the run (`Tag::Local` in
`crates/sparkles-core/src/id.rs`), so the semi-naive engine of `engine.rs` evaluates it
unchanged, in parallel, over the base tier of F16 §3 without copying the input. Only the
facts reachable from the seeds are derived.

The engine already compiles, in `incremental::Plans`, a plan for each rule head with the
head's variables bound, for the backward search of incremental maintenance. The demand
transformation reuses that compilation for its per-adornment join orders.

### 3.3 Seeds from joins

A pattern whose bound variable comes from an earlier join gets its seeds from that join's
output. The planner evaluates the left side, collects its distinct values for the bound
variable, derives with those seeds, and joins. When the left side is large, seeding
gains little, and the planner instead derives with the free adornment and joins after
(§4.3). Each `Derive` operator therefore runs once per query execution, not once per
row.

### 3.4 Negation and builtins

`noValue` makes a rule non-monotonic. The demand transformation keeps the program
stratified: the predicate a `noValue` reads is evaluated with the free adornment in a
lower stratum, in full over the snapshot, before the strata that read it. A program that
is not stratified is refused, as Jena's forward engine would give an order-dependent
result. Builtins that only test bound values (`notEqual`, `lessThan`, `regex` and the
rest of `builtins.rs`) pass adornments through. Builtins that bind their output
(`sum`, `strConcat`, `makeSkolem`) bind it for the atoms after them. List builtins read
the snapshot's lists and the derived facts as they do in materialization.

`makeTemp` and blank nodes in heads create a fresh blank node per rule instance. On
demand, the instance is the same for the same bindings within one derivation, so a table
is deterministic per query. Two queries may name the node differently. The response says
so with a warning, and such tables are not cached (§6).

### 3.5 Equality

OWL 2 RL's `eq-rep-s`, `eq-rep-p` and `eq-rep-o` have a variable predicate in their body,
so every intensional predicate depends on `owl:sameAs`, and `owl:sameAs` depends on
`prp-fp`, `prp-ifp` and `cls-maxc2` over every functional property. On data with such
properties, the demand cone of any pattern is the whole program. Phase 1 therefore
evaluates `owl:sameAs` with the free adornment as a lower stratum and keeps its classes
in a per-snapshot union-find, the structure of F16 §6, so other predicates read equality
through representatives without the `eq-rep-*` rules. The `owl:sameAs` table is cached
like any table (§6). When the dataset has F16 representatives in a fresh layer, the
derivation reads its class tables instead.

## 4. Choosing between materialized and derived facts

### 4.1 When materialized facts can answer

Materialized facts answer an intensional pattern under `auto` when all of these hold:

- the materialization's profile, rules digest (`rules_digest` in `src/lib.rs`) and input
  graphs equal the query-time ones;
- C08's status is fresh for the snapshot the query reads (`Freshness` in
  `crates/sparkles/src/reasoning/freshness.rs`);
- the caller's view reads the inferred graph, so no C12b protection with
  `hide_inferences` is in force for the caller.

### 4.2 Stale materialization

A stale materialization is never read by `auto` unless the dataset setting says
`"stale": "allow"`. Then a pattern reads it, and the response carries
`Sparkles-Inferences`. The default is `"stale": "derive"`.

### 4.3 Cost

When both sources can answer, the planner compares estimates:

- **Reading** costs a scan of the inferred graph (or the F16 layer) for the pattern, from
  the planner's existing statistics.
- **Deriving** costs the facts the demand program derives. The estimate walks the
  dependency cone from the seeds, multiplying by each rule's fan-out from the predicate
  statistics (`Snapshot::counts` and the characteristic sets), and adds the cost of
  the strata evaluated with the free adornment. A cached table costs a lookup.

The planner reads when reading is cheaper and derives otherwise. A pattern with a
variable predicate under `auto` reads when it can, because its demand cone is the whole
program. The estimates and the choice appear in EXPLAIN. The calibration constants are
measured with X06's LUBM and OWL2Bench queries and kept with the cost constants the
planner already calibrates.

### 4.4 Interaction with RDFS on read

RDFS on read (C08 §11.5) and query-time reasoning are exclusive for one query. When the
dataset has an RDFS-on-read schema and the query asks for `demand` or `auto`, query-time
reasoning applies and RDFS on read does not. RDFS on read stays Jena's `MatchRDFS`, with
its documented differences from RDFS entailment, and `profile: "rdfs"` on demand gives
full RDFS entailment over the input graphs. EXPLAIN states which applied. With
`reasoning=materialized` or `false`, RDFS on read applies as today.

### 4.5 Access control

A derivation reads the caller's view, so its facts follow from triples the caller can
see. This is the property C12b §7 notes for RDFS on read. Query-time reasoning therefore
works for callers with protections in force, where materialized inferences are hidden
from them. Tables are cached per view (§6).

## 5. Jena rule compatibility

### 5.1 Backward rules

Top-level backward rules (`head <- body`) are accepted on demand and become rules of the
query-time program. The materializer keeps skipping them with its warning, so a rule set
can mix forward rules for materialization and backward rules for queries, as Jena's
hybrid mode does.

Jena's LP engine evaluates backward rules top-down and tables the predicates named by
`table(p)` or `tableAll()`. Sparkles evaluates them bottom-up with the demand
transformation, which computes the same least fixpoint for function-free programs and
for programs whose functors and builtins produce finitely many terms. It differs from
Jena in these ways, which the user docs will list:

- Every predicate is tabled. `table` and `tableAll`, which `builtins.rs` refuses today,
  are accepted and ignored. A left-recursive rule that does not terminate in Jena without
  tabling terminates in Sparkles.
- Solutions come without Jena's order and without duplicates.
- Side-effect builtins (`print`, `drop`, `remove`, `hide`) stay refused.
- Functor terms in heads are supported where the materializer supports them.

### 5.2 Hybrid rules

A forward rule with backward rules in its head (`B1 -> [H <- B2]`) is materialized today
as `B1, B2 -> H` (`engine::flatten`). With query-time reasoning, the rule set's
`hybrid` option chooses the split:

- `"materialize"` (the default, today's behaviour) flattens as now.
- `"jena"` materializes the forward part and adds, for each match of `B1`, the instantiated
  backward rule `H <- B2` to the query-time program. The instantiated rules are kept with
  the materialization, as Jena keeps the backward rules its RETE network generates, and
  are updated by incremental runs like derived facts.

### 5.3 The Fuseki converter

G08's converter maps a `ja:InfModel` whose reasoner is `GenericRuleReasoner` with
`ja:rulesFrom` to a dataset with the rule text, `profile: "rules"` and the query-time
default `demand` for the backward and hybrid modes and `materialized` for the forward
mode, and reports which it chose.

## 6. Tables

### 6.1 The cache

A table is the set of facts derived for one adorned predicate and one set of seeds. The
cache keys a table by:

- the snapshot's commit and generation (`Snapshot::commit`, `Generation::uid`);
- the program's digest;
- the caller's view, as the result cache keys views (C12, C12b);
- the adorned predicate and a hash of the sorted seeds.

The tables of the free-adornment strata (§3.4, §3.5) are cached under their predicate
alone, so every query on the snapshot shares them. The cache is in memory, charged to the
dataset's result cache budget, and evicted least recently used. A table with fresh blank
nodes is not cached.

### 6.2 Reuse across commits (Phase 2)

A table stays valid at a later commit when no commit in between changed a predicate in
the table's dependency cone, the predicates of the base facts its rules read. A commit's
changed predicates come from the delta's per-predicate statistics (`Delta::predicates`
in `crates/sparkles-core/src/store.rs`) compared between the two snapshots, or from the
commit diff for a bulk commit. A commit that changes an input graph's membership, or a
generation switch, drops the tables, since ids move.

### 6.3 Kept current (Phase 3)

With F13 Phase 3, a table becomes a view on F13's core with its fixed-point operator
(F13 §4.3). Its rules are the demand program with the seeds fixed, and the core maintains
it from each commit's changes. A table that the cache would evict is dropped from the core
too. Counts cannot weight the maintenance of a recursive rule, because a fact in a cycle can
count its own derivations, so deletions use backward and forward rederivation as the
reasoner does (F13 §4.3). Provenance on a table is what F18's explanations read for
derived answers, and F13's `why` annotation supplies it.

## 7. OWL 2 QL query rewriting

When the dataset's query-time profile is `owl-ql`, the ontology graphs must be in the
OWL 2 QL profile. A check on setting the profile and on each change of an ontology graph
lists the axioms outside the profile, and such axioms are ignored with a warning in the
status.

A basic graph pattern is rewritten into a union of basic graph patterns over the base
data with the PerfectRef procedure of Calvanese et al.: each atom is replaced in turn by
the atoms that imply it under the ontology's inclusions, and atoms are unified when a
reduction makes them equal. Existential axioms (`owl:someValuesFrom` on the right of an
inclusion) let a pattern match individuals with an unnamed successor, as the procedure
defines. Variables that are projected, or used in a filter, are distinguished and are not
bound to unnamed successors.

The rewriting can grow exponentially in the number of atoms. It is bounded by
`budget.maxUnion` (1,024 branches by default). A pattern whose rewriting passes the bound
is answered by the `owl-rl` demand program over the same ontology for that BGP, which is
sound but may miss answers that need unnamed individuals, and the response carries a
warning saying so. The rewriting happens in the planner, like RDFS on read, so it reads no
data and caches with the plan.

## 8. Budgets and termination

Every derivation runs under the query's budgets (C01):

- **Deadline.** The engine checks the query's cancellation flag between iterations and
  inside long iterations, as `engine::run` checks `Limits::cancel` today.
- **Memory.** Derived facts and magic facts are charged to the query's memory budget.
- **Facts.** `budget.maxDerived` caps the facts derived for one query (1,000,000 by
  default). The engine's `Limits::max_inferred` carries it.
- **Iterations.** `Limits::max_iterations` as in materialization.

A derivation that passes a limit fails the query with `422` and the code
`reasoning-budget`, naming the limit, the pattern and the rule that produced the most
facts, as materialization reports today. Under `auto`, when a fresh materialization
could answer the pattern, the planner reads it instead and the response says so.

Function-free rule programs over a finite snapshot terminate. Programs whose builtins
create terms (`strConcat`, `makeSkolem`, functors) may not, and the fact budget stops
them.

## 9. A Datalog surface syntax

The specs README names a Datalog frontend among the numbering gaps that have no spec
([README](README.md#ids), the paragraph after the table of prefixes). This spec
decides the surface for rules over triples: **Jena's rule language is the Datalog syntax
of Sparkles.** Forward rules are materialized, backward rules are evaluated on demand,
and both are queried with SPARQL. The parser (`crates/sparkles-reasoner/src/parser.rs`)
already reads it, Jena users have rule files in it, and it covers recursive,
stratified-negation Datalog over triples with builtins.

What the README's item also covers, and this spec does not: n-ary predicates that are not
triples, a separate Datalog query form, and a syntax such as Soufflé's `.decl` and
`.output`. They are left for a later spec, and the README's gap entry will say that F17
covers Datalog over triples.

## 10. Architecture

| Crate / file | Change |
|---|---|
| `crates/sparkles-reasoner/src/demand.rs` (new) | Adornment propagation, the demand transformation, stratification, seeds. |
| `crates/sparkles-reasoner/src/engine.rs` | Compiles backward rules for the demand program; reserved magic predicates; per-query limits. |
| `crates/sparkles-reasoner/src/builtins.rs` | `table`, `tableAll` accepted as no-ops. |
| `crates/sparkles-reasoner/src/qlrewrite.rs` (new) | The OWL 2 QL profile check and PerfectRef. |
| `crates/sparkles-reasoner/src/tables.rs` (new) | The table cache, keys and dependency cones. |
| `crates/sparkles-core/src/sparql/` | A hook for a reasoning provider, `QueryOptions::reasoning`, the `Derive` operator and its EXPLAIN node, the cost comparison. `QueryOptions` carries an `Arc<dyn ReasoningProvider>` that the reasoner implements, as it carries the per-query extension registry of P03 (`QueryOptions::extensions`), so `sparkles-core` does not depend on `sparkles-reasoner`. |
| `crates/sparkles/src/reasoning/` | The query-time setting, its storage in `reasoning.json` and the library API. |
| `crates/sparkles-server/src/reasoning.rs`, `http.rs`, `mcp/tools.rs` | `/$/reason/{ds}/query`, the `reasoning` parameter values, the `Sparkles-Reasoning` header, the MCP tool argument. |
| `crates/sparkles-server/src/fuseki_config/` | The `ja:InfModel` mapping. |
| `ui/src/routes/query/+page.svelte` | The reasoning selector and the derivation summary. |

## 11. Phases

**Phase 1 (L).** The demand transformation for RDFS, RDFS simple, OWL 2 RL and Jena
rules including top-level backward rules; stratified `noValue`; equality as a lower
stratum; `reasoning=demand`; tables cached per snapshot and view; budgets; EXPLAIN;
the CLI; the library API. It needs F16 Phase 1's base tier, or builds it.

**Phase 2 (M).** `auto` and its cost model, the dataset setting, `hybrid: "jena"`, OWL 2
QL rewriting, reuse of tables across commits, the MCP argument, the UI and the G08
mapping.

**Phase 3 (M).** Tables maintained by F13's core, and `why` provenance on tables for
F18.

## 12. Acceptance examples

The examples use `ex:` = `http://ex.org/`.

**A1. Backward rules.** With the rules

```
[anc1: (?x ex:ancestor ?y) <- (?x ex:parent ?y)]
[anc2: (?x ex:ancestor ?z) <- (?x ex:parent ?y), (?y ex:ancestor ?z)]
```

and a chain of 100,000 `ex:parent` triples, `SELECT ?a { ex:n10 ex:ancestor ?a }` with
`reasoning=demand` returns the 99,990 ancestors of `ex:n10` and derives no fact about
`ex:n0` to `ex:n9`. `sparkles infer` with the same rules still warns that backward rules
are skipped and derives nothing.

**A2. Same answers as materialization.** For each LUBM query and each of RDFS and OWL 2
RL, `reasoning=demand` and `reasoning=materialized` on a fresh materialization return the
same solutions as multisets after `DISTINCT`.

**A3. Only what is needed.** On LUBM 100 with OWL 2 RL, LUBM query 1 (a constant course)
under `demand` derives fewer than 1% of the facts a full materialization derives, as the
`Sparkles-Reasoning` header reports.

**A4. Stale materialization.** Materialize, insert `ex:y a ex:C` with
`ex:C rdfs:subClassOf ex:B`, and query `SELECT ?x { ?x a ex:B }` with `reasoning=auto`.
The answer includes `ex:y`, the response has `Sparkles-Reasoning` and no
`Sparkles-Inferences` header, and EXPLAIN shows `Derive` for the pattern with the reason
"materialization stale".

**A5. Fresh materialization preferred.** After a re-run, the same query reads the
inferred graph, EXPLAIN shows the read with both estimates, and there is no
`Sparkles-Reasoning` header.

**A6. Budget.** The backward rule `[r: (?x ex:val ?w) <- (?x ex:val ?v), sum(?v, 1, ?w)]`
creates a new value at each step. With `budget.maxDerived` = 10,000,
`SELECT ?w { ex:a ex:val ?w }` over `ex:a ex:val 0` fails with `422 reasoning-budget`
naming rule `r`.

**A7. Protections.** A caller with a C12b protection that hides `ex:salary` triples, and
the rule `[(?x ex:salary ?s), greaterThan(?s, 100000) -> (?x a ex:HighEarner)]` as a
backward rule, gets no `ex:HighEarner` answers with `demand`, while an unrestricted caller
gets them. Neither reads the inferred graph.

**A8. OWL 2 QL.** With the ontology `ex:Professor rdfs:subClassOf [ owl:onProperty
ex:teaches ; owl:someValuesFrom owl:Thing ]` and `ex:ann a ex:Professor`, the query
`ASK { ex:ann ex:teaches ?c }` returns `true` under `owl-ql`, and
`SELECT ?c { ex:ann ex:teaches ?c }` returns no rows, because `?c` is distinguished.

**A9. Table reuse.** Two runs of LUBM query 6 on one snapshot: the second reports
`cached=` equal to `tables=`. After an insert of a triple whose predicate is outside the
query's cone, the tables are still cached; after an insert of a `ub:GraduateStudent`
type, they are derived again.

**A10. RDFS on read exclusive.** On a dataset with an RDFS-on-read schema, the C08 example
O query with `reasoning=demand, profile=rdfs` returns `ex:tom`, and EXPLAIN says RDFS on
read did not apply.

## 13. Testing and measurement

- **Differential against materialization.** Random rule programs and data (the generators
  of `incremental.rs`'s tests), random queries with random bound positions: the answers
  under `demand` equal those over a full materialization, for every profile and for Jena
  rules with builtins and stratified `noValue`.
- **Differential against Jena.** A fixture script runs Jena's `GenericRuleReasoner` in
  backward and hybrid modes on rule files with `tableAll()`, and Sparkles on the same
  files, and compares the answers as sets, as `scripts/rdfs-jena-expected.sh` does for
  RDFS on read.
- **W3C tests.** The OWL 2 RL entailment tests of the OWL 2 test suite run under
  `demand`, with the expectation set by F16's materialized run. The OWL 2 QL tests run
  under `owl-ql`.
- **Benchmarks (X06).** LUBM queries 1 to 14 at 10, 100 and 1,000 universities and
  OWL2Bench queries, under `materialized`, `demand` and `auto`, reporting time, facts
  derived and cache hits. The target is that `auto` is within 1.2 × of the better of the
  other two for each query, and that `demand` answers the selective LUBM queries (1, 3,
  4, 5, 7, 8, 10, 11, 12, 13) within 2 × of `materialized` without any materialization.

## 14. Rejected alternatives

- **Top-down SLG resolution.** It is the classic way to answer backward rules with
  tabling, and Jena's LP engine works top-down. Magic sets give the same goal-directed
  evaluation through the bottom-up engine Sparkles already has, with its parallelism,
  join planning and incremental maintenance.
- **Extending RDFS on read to OWL 2 RL by rewriting.** OWL 2 RL is recursive (transitive
  properties, `owl:sameAs`, property chains), and rewriting into a union cannot express
  recursion without property paths that the patterns cannot always take.
- **Answering stale patterns from the materialization plus the delta.** Deletions since
  the run would need the backward/forward search per query, and the result would still
  be the materialization's commit, not the query's.
- **A new Datalog syntax.** It would split users between two rule languages for the same
  semantics.
- **Caching tables across generations.** Ids move at a generation switch, and remapping a
  cache costs more than deriving again.

## 15. Open questions

1. Should `auto` become the default for datasets with a materialization? It changes no
   answer when the materialization is fresh and corrects answers when it is stale, but it
   changes response times.
2. Should backward rules also be materializable on request, as the hybrid flattening does
   for nested ones?
3. Should the OWL 2 QL fallback to the RL demand program be an error by default, so that
   incomplete answers are never silent beyond a warning?
4. Should tables be shared across views when the view does not hide any predicate in the
   table's cone? It would raise hit rates for many callers with narrow protections.

## Provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

- **Spec:** [`F17-query-time-reasoning.md`](F17-query-time-reasoning.md), written on 2026-10-12 independently from:
  - Bancilhon, Maier, Sagiv and Ullman, "Magic sets and other strange ways to implement logic programs" (PODS 1986), and Beeri and Ramakrishnan, "On the power of magic" (Journal of Logic Programming, 1991);
  - Calvanese, De Giacomo, Lembo, Lenzerini and Rosati, "Tractable reasoning and efficient query answering in description logics: the DL-Lite family" (Journal of Automated Reasoning, 2007), for OWL 2 QL rewriting (PerfectRef);
  - W3C OWL 2 Web Ontology Language Profiles (OWL 2 QL and OWL 2 RL), RDF 1.1 Semantics and SPARQL 1.1 Entailment Regimes, cited from working knowledge and not re-fetched;
  - Apache Jena's public documentation of its rule engines (forward, backward and hybrid modes of `GenericRuleReasoner`, tabling) and of the assembler's `ja:InfModel`, cited from working knowledge; no Jena code was read for this spec;
  - the Sparkles code (`crates/sparkles-reasoner/src/{engine.rs,incremental.rs,parser.rs,builtins.rs,lib.rs}`, `crates/sparkles-core/src/sparql/{rdfs.rs,extensions.rs}`, `crates/sparkles/src/reasoning/freshness.rs`) and specs C01, C08, C11, C12b, F13, F16, G08 and X06.
- **Implementation:** not built. The spec plans no new runtime dependency.
- **Rejected** (spec §14):
  - top-down SLG resolution;
  - extending RDFS on read to OWL 2 RL by rewriting;
  - answering stale patterns from the materialization plus the delta;
  - a new Datalog syntax;
  - caching tables across generations.

## Outcome

**Phase 1 built** on 2026-10-24 for roadmap item 19. Phases 2 and 3 are not built, and
`reasoning=auto` answers `400`.

* **Engine hook.** `crates/sparkles-core/src/sparql/reasoning.rs` defines
  `ReasoningProvider` and `ReasoningSession`. Before a query runs, the engine orders each
  group's triple patterns, turns the patterns rules can give facts for into demands, and
  asks the session to derive them. Patterns with constants alone derive first. A pattern
  whose subject or object an earlier pattern binds is then seeded with that pattern's
  values, read from the store with the facts derived so far, up to `MAX_SEEDS` (10,000)
  values, and with its constants alone past that. Property path steps derive with their
  predicate alone. The derived facts join the snapshot as an overlay for the query, and
  local terms the store lacks are kept so that the planner's shortcut for unknown
  constants does not empty a pattern that only derived facts match.
* **Demand transformation.** `crates/sparkles-reasoner/src/demand.rs` rewrites the
  compiled rules for the demands, as §3.2 describes, over the one ternary relation of
  triples. A key says which of subject and object are bound and whether the predicate is a
  constant, a value bound at run time or free. An `rdf:type` key with a constant class is
  split by the class. Magic facts are triples whose predicate is a blank node local to the
  run, hidden from atoms and `noValue` builtins whose predicate is a variable. A rule
  whose head has a variable predicate or class answers a constant one through a copy with
  the variable replaced. Linearly recursive predicates use reach facts, so the A1 chain
  derives n facts for n ancestors rather than n². A key asks only for facts whose subject
  and object differ when its rule tests `notEqual` on them or would restate its own body
  fact, which keeps reflexive schema rules from asking whether every class has an
  instance. Sideways information passing follows a greedy order by the input graphs'
  statistics, in which an atom that rules derive counts as at least a pattern of unknown
  predicate, and one with a free predicate or a free class comes last.
* **Rules specialized to the schema.** `crates/sparkles-reasoner/src/specialize.rs`
  handles rules that join schema facts with instance facts, such as `prp-dom`. A schema
  pass derives the facts of the RDFS and OWL vocabulary the rules name, and the
  `rdf:type` facts of its classes of properties and classes, once per snapshot, rules and
  view, and keeps them in the table cache. Each rule's schema atoms, the RDF list atoms
  they bind and the test and list builtins over their values are joined over that schema,
  and the rule becomes one rule per binding with those atoms gone. A `listForAll` over a
  constant list becomes one atom per member. Over LUBM's ontology, 108 OWL 2 RL rules
  become 456. A rule whose schema join has more than 4,096 bindings, or any rule after
  the program reaches 20,000 specialized rules, stays as it is. The query's program is then
  made over the specialized rules, with the schema's atoms asking for nothing. Before
  specialization, a demand for `?x rdf:type ub:GraduateStudent` asked for every fact of
  every property with a domain, and OWL 2 RL on LUBM query 1 derived 832,000 types. After
  it, the demand asks only for the properties and classes that can give the class.
* **Equality.** When rules derive `owl:sameAs` and read it, equality is a lower stage
  (§3.5). A first pass without the rules that read `owl:sameAs` to derive other facts
  runs first, and the full stage runs only when the first pass found an `owl:sameAs` fact.
  The stages above read `owl:sameAs` as stored. When the store has no `owl:sameAs` fact
  and no specialized rule gives one without reading one, the rules that read it are left
  out, since the `eq-rep-*` rules, whose predicate is a variable, would otherwise answer
  every demand.
* **Negation.** The pattern a `noValue` reads, with its constants bound, is a demand of a
  lower stage evaluated to its fixpoint first. The lower stage's key keeps the pattern's
  constant subject and object, so a rule whose head has other constants there is not
  part of it, and a negated `rdf:type` pattern with a constant class asks for that class
  alone, as a positive one does. The schema pass runs its lower stages first too, and
  they derive instance facts in full, so a schema rule's `noValue` reads a complete
  pattern. The schema facts they derive are kept in the schema's table. A program whose stages do not end is not
  stratified and fails with `422` and `reasoning-rules`.
* **Lists.** A builtin or head action that reads an RDF list is not evaluated again when
  the list's facts grow. When rules give `rdf:first` or `rdf:rest` facts with that
  constant predicate, a lower stage therefore derives every such list before the stage
  that reads it. When that stage would depend on itself, as when a rule reads one list
  to build another, the program is made again with lists read as the stage holds them,
  unless a `noValue` lies on a cycle of the rules' predicates, with `rdf:type` split by
  its constant class, which stays refused. The facts a `listForAll` reads are asked for behind the copy's guard,
  unless an atom of the rule already matches them as in `cls-int1`.
* **Evaluation.** `crates/sparkles-reasoner/src/query.rs` evaluates the program with the
  semi-naive engine over the base tier of the caller's input graphs, under the query's
  deadline, cancellation and memory budget, and the schema joins of specialization stop
  with the query too. Each derivation adds its magic facts and continues from them. The
  program is made at the first derivation that misses the table cache. The facts a query
  adds to its view are charged to its memory budget at 448 bytes each, seven index keys
  with their node overhead, also when they come from cached tables. The facts the view
  does not hold yet are reserved before it grows, and the reservation then shrinks to
  what it grew by, so a fact that several tables hold is charged once.
* **Tables.** `crates/sparkles-reasoner/src/tables.rs` caches what a demand derived,
  keyed by the generation, the commit, the mask of a view that hides triples, the input
  graphs, the rules' digest, the demand with its constants and a hash of its seeds. Local
  terms are carried by key and given the reading session's ids. A table that holds a blank
  node a rule made is not cached.
* **Surfaces.** The SPARQL endpoints take `reasoning=demand` with `reasoningProfile` and
  `maxDerived`, and answer with `Sparkles-Reasoning`. `sparkles query` takes
  `--reasoning`, `--profile`, `--rules` and `--max-derived`, and `serve` takes
  `--reason-max-derived`. `Dataset::query_reasoner` gives the library's provider for
  `QueryOptions::reasoning`. EXPLAIN shows a `Reason` node over the plan with a `Derive`
  node per pattern. A DESCRIBE under `reasoning=demand` fails with `501` and the code
  `unsupported`, since the description reads facts outside the WHERE pattern that no
  demand asks for. Item 193 covers it.
* **Access.** `owl:imports` are followed only through graphs the caller can read, so a
  denied graph adds no schema and its imports appear in no warning.

**Decisions where this spec left the choice open.**

1. `maxDerived` counts derived facts and not magic facts, which §8 charges to the memory
   budget. Counting them made LUBM query 6 at 10 universities pass 1,000,000 when it had
   derived 352,000 facts.
2. A derivation has no iteration limit. §8 asks for materialization's 10,000, but A1's
   chain of 100,000 takes one iteration per node, so the fact limit, the deadline and the
   memory budget bound a run instead.
3. The preliminary schema pass reads instance facts as stored. Its valid facts are
   reusable seeds, while a separate completeness proof decides which schema relations
   can be specialized or treated as closed. Ordinary demand follows derived instance
   premises for schema-producing heads. Schema facts go to a query when its demands
   can read them. Programs containing `makeTemp` use one ordinary demand program with
   their complete normal lower stages, preserving fresh witnesses across the query.
4. `nocache=true` bypasses the tables of the query's demands but not the schema's table,
   which every query of the view shares like the compiled rules.
5. The rules in a `Derive` node are the static dependency cone of its pattern over the
   unspecialized rules, since the program is made only when a table misses. The node
   shows the derived facts and the seeds after a run, and no estimate.
6. The engine ranks a group's patterns by the bound subjects and objects first, counting
   the class of an `rdf:type` pattern after them, then by bound positions, then patterns
   read without derivation, then the fewest stored facts.
7. The table cache is process-wide with its own budget of 256 MiB rather than the
   dataset's result cache budget of §6.1. Item 192 covers this.
8. Lists that rules derive are complete in a lower stage, which derives every list the
   rules give rather than the lists a query reaches. This costs a derivation of all such
   lists for each query that reads one and keeps the answers equal to a fixpoint. Lists
   that only the equality rules rename, and lists whose stage would depend on itself, are
   read with what they hold so far.
9. DESCRIBE is refused rather than answered from the asserted facts, so a description is
   never silently incomplete.

**Measurements.** These measurements precede the schema-completeness changes and do
not establish current-head performance. Development measurements on an nsc instance (16 vCPUs of an AMD EPYC
host, 32 GiB) on 2026-10-24 at `d80c0ce7`, with the server alone on it. Each query ran
once to warm up and then five rounds of the three modes interleaved, and the tables give
the median with the range. "Materialized" reads a store materialized with the profile.
"Demand" reads a store of the asserted data with `nocache=true`, so it derives every time
from the cached schema, and "cached" repeats the query with its tables in the cache.
Derived counts are those of the `Sparkles-Reasoning` header.

LUBM at 10 universities (1.3 million triples) with OWL 2 RL, where a materialization
derives 1,037,750 facts:

| Query | Materialized | Demand | Demand, cached | Derived |
|---|---|---|---|---|
| q1 | 0.42 ms (0.41 to 0.53) | 2.51 ms (2.34 to 3.12) | 1.04 ms (0.98 to 1.19) | 0 |
| q2 | 3.14 ms (2.91 to 3.28) | 11.6 ms (11.1 to 13.1) | 3.72 ms (3.61 to 4.09) | 1,170 |
| q3 | 0.44 ms (0.43 to 0.46) | 3.23 ms (2.76 to 3.73) | 1.08 ms (1.05 to 1.14) | 0 |
| q4 | 0.55 ms (0.49 to 0.62) | 11.7 ms (11.0 to 12.4) | 2.67 ms (2.21 to 2.82) | 1,207 |
| q5 | 0.52 ms (0.49 to 0.55) | 24.9 ms (24.1 to 24.9) | 3.34 ms (3.24 to 3.66) | 3,890 |
| q6 | 12.7 ms (9.56 to 14.0) | 841 ms (830 to 853) | 419 ms (418 to 436) | 594,919 |
| q7 | 0.47 ms (0.41 to 0.63) | 8.81 ms (8.29 to 9.20) | 1.34 ms (1.26 to 1.59) | 308 |
| q8 | 5.05 ms (4.88 to 5.25) | 172 ms (166 to 190) | 41.0 ms (40.1 to 42.9) | 46,819 |
| q9 | 8.37 ms (8.04 to 9.80) | 383 ms (376 to 393) | 213 ms (205 to 216) | 210,950 |
| q10 | 0.38 ms (0.37 to 0.47) | 5.18 ms (4.77 to 5.48) | 1.05 ms (0.87 to 1.21) | 37 |
| q11 | 0.45 ms (0.43 to 0.54) | 3.52 ms (3.34 to 6.80) | 1.21 ms (0.91 to 1.26) | 224 |
| q12 | 0.59 ms (0.54 to 0.71) | 31.1 ms (29.0 to 33.1) | 9.78 ms (9.55 to 10.1) | 14,986 |
| q13 | 0.51 ms (0.45 to 0.74) | 16.1 ms (15.8 to 16.3) | 5.60 ms (5.30 to 5.79) | 8,856 |
| q14 | 8.00 ms (7.15 to 21.4) | 7.49 ms (7.26 to 8.06) | 7.26 ms (6.59 to 17.1) | 0 |

LUBM at 10 universities with RDFS:

| Query | Materialized | Demand | Demand, cached | Derived |
|---|---|---|---|---|
| q1 | 0.33 ms (0.32 to 0.44) | 1.44 ms (1.39 to 1.71) | 0.70 ms (0.57 to 0.75) | 0 |
| q2 | 0.70 ms (0.69 to 0.82) | 6.00 ms (5.33 to 6.17) | 1.99 ms (1.95 to 2.12) | 466 |
| q3 | 0.34 ms (0.32 to 0.35) | 1.94 ms (1.86 to 2.05) | 0.57 ms (0.55 to 0.61) | 0 |
| q4 | 0.41 ms (0.40 to 0.46) | 2.90 ms (2.69 to 3.28) | 0.85 ms (0.80 to 0.89) | 34 |
| q5 | 0.46 ms (0.45 to 0.49) | 4.49 ms (4.21 to 4.60) | 1.13 ms (1.09 to 1.14) | 1,029 |
| q6 | 9.59 ms (9.01 to 10.5) | 54.2 ms (51.9 to 59.0) | 47.0 ms (46.6 to 56.0) | 75,547 |
| q7 | 0.45 ms (0.37 to 0.61) | 3.38 ms (2.77 to 5.47) | 0.89 ms (0.75 to 1.06) | 60 |
| q8 | 4.80 ms (4.78 to 5.39) | 14.6 ms (13.7 to 16.2) | 8.82 ms (8.61 to 9.13) | 6,456 |
| q9 | 6.83 ms (6.67 to 7.42) | 50.2 ms (49.2 to 54.1) | 32.8 ms (32.1 to 33.9) | 23,967 |
| q10 | 0.36 ms (0.33 to 0.43) | 1.68 ms (1.51 to 1.80) | 0.69 ms (0.55 to 0.71) | 0 |
| q11 | 0.40 ms (0.32 to 0.47) | 1.35 ms (1.19 to 2.57) | 0.55 ms (0.51 to 0.83) | 0 |
| q12 | 0.31 ms (0.30 to 0.38) | 2.38 ms (2.28 to 2.44) | 0.62 ms (0.61 to 0.69) | 0 |
| q13 | 0.32 ms (0.29 to 0.83) | 1.23 ms (1.18 to 1.35) | 0.51 ms (0.49 to 0.54) | 0 |
| q14 | 9.58 ms (8.51 to 14.3) | 9.24 ms (8.60 to 10.1) | 7.69 ms (7.59 to 8.52) | 0 |

LUBM at 100 universities (13.4 million triples) with OWL 2 RL, three rounds, where a
materialization derives 10,907,029 facts:

| Query | Materialized | Demand | Demand, cached | Derived |
|---|---|---|---|---|
| q1 | 0.52 ms (0.44 to 0.59) | 2.65 ms (2.54 to 3.34) | 1.18 ms (0.97 to 1.35) | 0 |
| q2 | 189 ms (26.2 to 192) | 2,303 ms (2,264 to 2,337) | 2,691 ms (2,624 to 2,729) | 2,161,624 |
| q3 | 0.46 ms (0.35 to 0.48) | 2.62 ms (2.45 to 2.79) | 1.43 ms (0.79 to 25.2) | 0 |
| q4 | 0.51 ms (0.51 to 0.51) | 9.45 ms (9.43 to 34.6) | 1.94 ms (1.90 to 2.55) | 1,207 |
| q5 | 0.55 ms (0.49 to 0.69) | 24.2 ms (24.1 to 53.7) | 3.65 ms (3.50 to 4.07) | 3,890 |
| q6 | 127 ms (119 to 147) | 10,406 ms (10,370 to 10,430) | 5,347 ms (5,319 to 5,373) | 6,267,453 |
| q7 | 0.61 ms (0.56 to 0.81) | 8.10 ms (7.99 to 9.57) | 1.55 ms (1.37 to 1.92) | 308 |
| q8 | 27.4 ms (25.0 to 27.6) | 185 ms (182 to 206) | 42.5 ms (40.7 to 45.1) | 46,819 |
| q9 | 86.8 ms (85.1 to 88.4) | 11,132 ms (11,058 to 11,224) | 10,553 ms (10,536 to 10,576) | 6,268,532 |
| q10 | 0.53 ms (0.49 to 0.83) | 4.98 ms (4.89 to 7.91) | 1.30 ms (1.01 to 1.44) | 37 |
| q11 | 0.45 ms (0.44 to 0.64) | 3.71 ms (3.29 to 3.93) | 1.23 ms (1.11 to 1.28) | 224 |
| q12 | 0.69 ms (0.65 to 0.91) | 31.8 ms (29.2 to 84.1) | 10.2 ms (9.37 to 10.6) | 14,986 |
| q13 | 1.72 ms (1.52 to 2.07) | 122 ms (119 to 123) | 68.5 ms (67.5 to 73.6) | 124,475 |
| q14 | 93.8 ms (81.3 to 125) | 69.9 ms (68.0 to 114) | 98.2 ms (65.8 to 99.4) | 0 |

All 14 queries give the materialized answers under both profiles at 10 universities and
under OWL 2 RL at 100, so A2 holds on this data. A3 holds: query 1 at 100 universities
derives no facts beyond the schema, against 1% of 10.9 million allowed. The schema pass
itself derives about 600 facts once per snapshot, in 65 to 120 ms at 10 universities.

§13's target of 2 × for the selective queries (1, 3, 4, 5, 7, 8, 10, 11, 12 and 13) is not
met. With cached tables, RDFS takes 1.4 to 2.5 times the materialized time and OWL 2 RL
2.5 to 17 times. Deriving afresh takes 3.0 to 9.8 times for RDFS and 6 to 53 times for
OWL 2 RL. A query with a millisecond of work spends most of its time in the session's
setup, the program made for the query and the merge of the derived facts into the query's
view. Queries 2, 6 and 9 read millions of facts at 100 universities and take 12 to 128
times as long as over the materialization. That is what `auto` of Phase 2 is for. Item
190 holds the work on the per-query cost.

**Acceptance.** A1 holds at 2,000 nodes in `backward_rules_answer_on_demand`, and at
the example's 100,000 nodes `sparkles query --reasoning demand` returns the 99,990
ancestors of `ex:n10` in 7.4 s on the nsc instance, one iteration per node. A2 holds in
the differential test `demand_answers_like_materialization`, over random data and
queries for every profile and for Jena rules with builtins, `noValue` and backward rules,
and on LUBM as above. A3 holds as above, and `owl_rl_demand_follows_the_schema` checks it
in small. A6 holds in `budget_stops_a_runaway_rule`, A7 in the server test
`demand_reasoning_reads_the_protected_view`, A9's first part in
`tables_are_cached_per_snapshot`, and A10 in `rdfs_on_read_does_not_apply_under_demand`.
A4, A5, A8 and A9's second part belong to Phase 2.

**Review.** The Codex review of the phase found eight problems, each fixed with a
regression test in `crates/sparkles-reasoner/tests/demand.rs` that failed before the
fix. Imports were resolved before the caller's graph filter, which exposed the imports of
denied graphs in warnings (`imports_follow_the_graph_view`). The linear rewriting dropped
stored facts of the recursive predicate (`linear_recursion_reads_stored_facts`). The
schema pass skipped its lower stages (`schema_negation_reads_complete_lower_stages`).
Facts copied from cached tables bypassed the memory budget
(`cached_tables_are_charged_to_the_memory_budget`). Specialization ignored the deadline
(`specialization_stops_at_the_deadline`). List builtins asked for no derived lists
(`list_builtins_ask_for_derived_lists`). Negated type demands lost their class and
reported a false cycle (`negative_type_demands_keep_the_class`). DESCRIBE answered
without derived facts (`describe_is_refused_under_demand`). A second review of the fixes
found three regressions in them, each fixed with a test. Schema facts from the schema
pass's lower stages were left out of the schema's table
(`cached_schema_keeps_lower_stage_schema_facts`). Facts that several tables hold were
charged once per table (`overlapping_tables_are_charged_once`). A rule that reads one
list and builds another was refused as unstratified
(`distinct_list_heads_are_not_a_cycle`). A third review found that the overlay grew
before its charge was admitted, which now reserves first, and that the retry without
list stages accepted a `noValue` on a cycle through a list
(`negation_through_lists_is_refused`). A fourth review found that this check merged all
classes of `rdf:type`, which it now keeps apart (`negation_check_keeps_classes_apart`),
and a fifth that it left out the facts a `listForAll` tests
(`negation_through_list_for_all_is_refused`).

**Limitations.** Equality is not kept as canonical representatives, so an entity with
many aliases still costs a fact per alias and property in the equality stage.
`MAX_SEEDS` and the specialization caps are not tuned by measurement. Schema analysis
and program setup remain per-query costs under item 190. DESCRIBE is refused under
`reasoning=demand` under item 193.


### Explanation sessions, 2026-10-24

[F18 Phase 1](F18-explanations-and-entailment-diffs.md#outcome) runs demand sessions for a
single triple and exposes their facts to the well-founded derivation search. These
sessions keep the original linear-recursive rules and bypass result tables so the
search can see every intermediate premise. Why-not overlays also bypass the schema table,
because application rules can derive schema facts from a hypothesis.


### Instance-derived schema facts

Demand derives class and property declarations, schema relations and schema-term
`owl:sameAs` consequences that depend on derived instance facts. Valid cached schema
seeds remain separate from proven-complete relations. A conservative program analysis
and a finite abstract model determine which schema joins can be specialized. Unproven
relations remain in the ordinary demand program, which follows their derived premises.
The abstract model admits its workspace before allocating and falls back to the
conservative proof at its fact or binding cap. Input failures, cancellation, deadlines
and memory exhaustion fail the derivation.

The schema pass can derive instance facts on the way to schema facts, for example the
types of an instance whose class is a subclass of `owl:Class`. The query's program finds
those facts already held and does not derive them again. They therefore go out with the
next derivation of any demand. Only facts of the schema vocabulary wait for a demand that
can read them, and a debug assertion checks that no demand of the derivation could match
a withheld fact. Before this, a demand such as `?x rdf:type ex:C0` lost answers whenever
the schema pass had derived them.

An uncovered `listForAll` reads positive facts outside its explicit rule atoms. New
facts for those reads reconsider the rule, and a replay emits heads only when that
particular match used a new explicit fact or an actual bound list-member fact. This
prevents an old satisfied match from regenerating fresh witnesses. Programs containing
per-evaluation `makeTemp` generators use one ordinary demand derivation, with normal
list, equality and negative lower stages. Their result-table policy remains unchanged.
Stable `makeSkolem` terms retain their existing identity within a session.

The completeness model can stream visible inputs twice per query, even with warm
schema tables, and reserves a conservative workspace. `makeTemp` programs forgo staged
schema specialization and reuse. A replay may allocate a temporary generator term before
an old head is refused. Item 190 retains these setup and memory costs for measurement
and optimization. This correctness change supplies no current-head performance claim.

The acceptance checks cover fixed schema answers, snapshot and graph visibility,
cold and warm tables, `no_cache`, schema-term equality, analysis-cap fallback,
negative lower stages, coherent multihead relations and fresh-witness counts. The
explanation sample now requires every sampled materialized fact to have a demand
answer and retains its proof soundness and replay checks. The strengthened checks
include schema facts rather than allowing missing answers for W3C terms.
A schema-heavy random differential compares demand with OWL RL materialization on
small views that make W3C classes of classes and properties, schema predicates and
equality between schema terms follow from instance facts. Its queries have a five-second
deadline. A query past the deadline is counted and skipped, since equality between
schema terms can expand the demand program far beyond the closure (roadmap items 9
and 190).


### Bounded positive feedback evaluation

A cold demand session can evaluate the original rules over a temporary closure when
schema specialization expands a tiny positive feedback view into thousands of rule
copies. Eligibility requires at most 64 input triples and 64 distinct program and
input terms, at most 256 original rules, and an expansion of at least 4,096 rules
and 32 times the original rule count. A rule has at most 64 combined atoms, builtins
and heads. At least one input triple consists entirely of constants from the original
program. Conservative full-closure bounds must fit the remaining memory, derived-fact
and iteration limits before this route is selected. Other demand sessions keep their
existing evaluation route.

The admitted rules are positive and cannot construct terms or perform head actions.
Negation, fresh witnesses, arithmetic and string construction are excluded. Structural
list terms in input or rule heads also exclude the route. The temporary closure uses
the ordinary engine and the same cancellation, deadline, memory, derived-fact and
iteration controls. Its facts remain private to the captured view and session.
This can retain more intermediate facts than selective demand evaluation, trading
temporary memory for avoiding evaluation of the expanded program. It does not persist a
materialization or implement Phase 2's automatic cost selection.

Explanation search uses first-emission rounds to seed a minimum-depth proof. It then
checks alternative root instances against the complete closure, including independent
support that becomes available after the root was first derived. Distinct supported
premise sets of the same rule count as alternatives. A root-only cycle does not.
The alternative search retains consumed budgets when it continues from the initial
proof. A session retried after partial compact evaluation disables its incomplete
round metadata rather than treating that ordering as proof of completeness.

The default-budget why-not acceptance fixture retains every returned-hypothesis
rematerialization assertion and the existing test timeout. The repair passes a 250-case regression matrix and three supplemental proof-metadata
and eligibility controls. These checks establish bounded completion and explanation correctness.
They do not establish a current-head latency ratio or the selective-query cost target
in section 13. Item 190 retains matched Forge measurement, the temporary-closure
memory tradeoff and any measured control costs before performance acceptance.


### Equality stage table

When rules derive and read `owl:sameAs`, a demand program ends with equality stages that
derive `owl:sameAs` with the free adornment, whatever the query asks (§3.5). The first
session over a cacheable view keeps every derived fact its graph holds once these lower
stages finish in the derived-table cache. Later sessions over the same view add those
facts and skip the equality stages. The key is the view, the rules' digest and whether the
program rewrites linear recursion. Explanation sessions keep the rules' linear recursion
and so keep a table of their own. The kept facts include the premises of every fact the
equality stages derived, including generalized facts that RDF cannot state, such as an
`owl:sameAs` fact whose subject is a literal. An explanation over cached facts therefore
still finds well-founded trees, and a warm query derives what a cold one does. A program
that reads `now` neither reads nor fills the table, since each session reads its own
time. A session whose graph holds facts from outside the view, such as why-not hypotheses
or GeoSPARQL default geometries, neither reads nor fills the table. Like the schema table,
the equality table is a property of the view, and `no_cache` does not bypass it.

The table is charged to the derived-table cache's capacity, and the facts a session adds
from it are charged to the session's memory budget as they are read. The workspace that
builds the table is charged to the session's budget first. A table larger than the
cache's capacity, or one whose workspace does not fit the budget, is not kept, and the
session's answers do not change. This trades memory for time. A view's
table holds every fact the first session had derived when its lower stages finished,
which can be many more facts than a selective query derives. Tables whose facts hold a
blank node a rule made are not kept.

### Planning by predicate

The semi-naive engine indexes a program's rules by the constant predicates of their
atoms. After the first iteration it plans only the rules with an atom whose predicate has
a row among the iteration's new rows, together with the rules that have an atom of a
variable predicate or a builtin that reads facts outside its atoms. The predicates with
new rows come from the graph's predicate index rather than from the rows themselves. A demand program
guards nearly every rule with a magic predicate of its own, so most rules have no new
facts in a given iteration, and a full scan of the program dominated planning. The
index and each iteration's list of rules are charged to the run's memory budget while
they are held. The planned tasks are the same as without the index. A property test compares them on random
programs, graphs and row ranges. An iteration whose new rows include the base tier still
plans every rule, since the base tier's predicates are not listed, and so does a graph
with more predicates than the program has atoms, where a scan of the rules is cheaper.

### Local measurements of the equality table and rule index

These are diagnostic measurements on a shared 16-core development host whose one-minute
load ranged from 17 to 48 during the runs, with a median of 29. They are not published
results. Small effects and current-head claims need the quiet Forge host (item 190). Each
release build ran six interleaved rounds with the order rotated. The table gives the
median process CPU time per query in milliseconds. CPU time varied less than wall time
under this load. For the soundness fixture, it is the CPU time per ASK and explanation
pair. The fixture covers three 40-triple OWL RL views with 12 sampled inferred triples
each. The selective views hold 5,000 instances in an eight-class hierarchy, and the
equality variant adds 100 `owl:sameAs` links.

| Workload | Before | Equality table | With rule index |
|---|---|---|---|
| Soundness fixture, per ASK and explanation | 2,438 | 1,033 | 898 |
| Selective OWL RL with equality | 4,993 | 1,523 | 1,535 |
| Schema query with equality | 4,881 | 1,548 | 1,595 |
| Schema-heavy 9-triple view | 1,006 | 388 | 345 |
| Selective OWL RL without equality facts | 111 | 105 | 111 |
| Selective RDFS | 47 | 47 | 37 |
| Asserted answers under OWL RL | 12 | 14 | 14 |
| Tiny positive feedback view | 3,373 | 3,278 | 3,442 |

The equality table removes about 60 to 70% of the CPU time of warm queries over views
whose programs have equality stages. The rule index removes a further 10 to 20% on the
large demand programs of the soundness fixture and the schema-heavy view. Controls
without equality stages stay within the 10 ms resolution of the measurement. Wall times
of materialization moved by up to 20% in both directions between rounds, including for
the equality table, which does not touch the materializer. So this run does not resolve
the rule index's effect on materialization.

The equality table raised the derived-table cache's use from 8 KB to 579 KB for the
5,000-instance view with equality, from 349 KB to 430 KB over the three soundness
views, and from 8 KB to 19 KB for the schema-heavy view. In two interleaved development-build
runs of `every_inferred_triple_has_a_sound_demand_tree`, the test took 214 s and 387 s
before these changes and 107 s and 167 s after, with the host's one-minute load between
16 and 47.

