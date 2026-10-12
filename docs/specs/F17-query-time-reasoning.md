# F17: Query-time reasoning

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 is on-demand derivation for a query through the
> demand (magic-sets) transformation of the rule program, Jena backward rules, tables
> cached per snapshot, budgets and EXPLAIN. Phase 2 adds the cost-based choice between
> materialized and derived facts, dataset settings, OWL 2 QL query rewriting, tables kept
> across commits that do not affect them, the MCP tool argument, the UI and the Fuseki
> converter mapping. Phase 3 keeps tables current with the incremental core of F13 and
> derives equality on demand.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

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
too. F13's `count` provenance gives the maintenance its weights, and `why` provenance on a
table is what F18's explanations read for derived answers.

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

## Outcome

Not built. See [ROADMAP](../ROADMAP.md) item 19.
