# F23: Reasoner core

> **Status:** partially complete. The quick-fixes phase is built, and the
> [Outcome](#outcome) records it.
>
> **Roadmap:** [item 456](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

The design covers specialized closure operators, complete OWL 2 RL rules, cost-based rule planning, a columnar derived tier, compact provenance, a native n-ary Datalog core, rule frontends and one incremental-maintenance core shared with views.

## Problem and goal

The reasoner evaluates transitive, symmetric and equivalence relations through
generic non-linear rules, so OWL2Bench under OWL RL fails at 5 universities with 213
million `prp-trp` candidates at 2. The OWL 2 RL rule file lacks `prp-key`, `cls-maxqc4`,
the cardinality-zero rules, the datatype rules and property chains longer than three.
Rule join order is greedy with independence estimates. Derived facts cost about 135 bytes
each in a row table with 32-bit row ids, and the reasoner has one ternary relation. Its
backward and forward maintenance and the incremental core of
[F13](../F13-incremental-views-and-provenance.md) are separate frameworks, each with
fallbacks to a full run.

The goal is a reasoner core that completes OWL 2 RL, scales its closures and derived
facts, and shares one maintenance core with views.

## Scope

Three quick fixes come first and need no structural change. These are the closure
operators, item 485, the missing OWL 2 RL rules, item 486, and cost-based rule-body
planning, item 487. The core then moves to a columnar derived tier with merge-join rule
evaluation and compact provenance. A native n-ary Datalog core with the rule frontends
follows. The shared incremental-maintenance core comes last, after item 417's LUBM
measurements. RDFS on read becomes one rule profile only at measured parity with the
current rewrite.

The phases of [F16](../F16-materialization-at-scale.md#outcome) and
[F17](../F17-query-time-reasoning.md#outcome) that stay with those specs are listed in
their Roadmap follow-ups. OWL classification is a separate engine in
[F24](../F24-owl-classification/README.md), which works on the axiom model of
[F21](../F21-ontology-api.md) rather than on this rule core.

## Candidate phases

### Quick fixes

**Specialized closure operators for transitive, symmetric and equivalence relations.** Detect transitive properties, subClassOf, subPropertyOf, sameAs and equivalence and evaluate them through SCC condensation with a linear or reachability closure and union-find for equivalence. prp-trp, scm-sco and eq-trans are generic non-linear rules. OWL2Bench under OWL RL fails at 5 universities with 213M prp-trp candidates at 2. It depends on item 9. Roadmap item 485 holds this phase.

**Missing OWL 2 RL rules.** Add prp-key (hasKey), cls-maxqc4, cardinality-zero rules, the dt-* datatype rules and property chains of any length. The rule file has none of prp-key, cls-maxqc4 or dt-* and only prp-spo2-2 and prp-spo2-3 for chains. Full OWL 2 RL is a conformance claim. Roadmap item 486 holds this phase.

**Cost-based rule-body planning that shares the SPARQL estimator.** Reuse characteristic sets and probes in rule planning, run DP for bodies of up to about six atoms, and replan when delta ratios shift. Rule join order is greedy with independence estimates (`engine.rs`:458-575). Roadmap item 487 holds this phase.

### Core

**Columnar sorted derived tier with merge-join rule evaluation.** Replace the reasoner's row table and hash lists with immutable sorted per-predicate column runs plus a small mutable delta, and evaluate rule steps with merge or galloping joins. Derived facts cost about 135 bytes each and use u32 row ids, which caps the tier at about 4.29B rows. RDFox reaches about 40 to 60 bytes per fact. It depends on item 9.

**Compact provenance with derivation counts and bounded witnesses.** Store a derivation count and one or a few witnesses per derived fact in columnar form, enabling fast deletion in non-recursive strata, Why answers and reviewed-only visibility without re-derivation. Explanations are re-derived on demand. F13 §4.3 rules out counts for recursive maintenance because a fact in a cycle can support itself, so counts apply to non-recursive strata only. It depends on items 18, 285, 415 and 416.

**Compiled or specialized rule kernels.** Generate native kernels for the fixed RDFS and OWL RL profiles, or JIT rule plans, to remove interpretive Step dispatch. Rules run through the interpreted Eval walk. Typical gains are 2 to 5 times per rule. It depends on item 9.

**Native n-ary Datalog core with existential rules and aggregates in recursion.** Make a general Datalog IR with n-ary IDB predicates, a restricted chase with MFA or joint-acyclicity checks, stratified negation and monotonic aggregates in recursion, with Jena rules as one frontend. Aggregates arrive in phases, with stratified aggregates first and monotone aggregates in recursion second, both after the columnar derived tier (*columnar sorted derived tier with merge-join rule evaluation*). The reasoner has one ternary relation (`demand.rs`:6). C24 §6.4 to §6.5 already plans n-ary relations encoded through makeSkolem tuple nodes and weakly acyclic existentials, so the remaining gap is a native n-ary representation, the restricted chase and recursive aggregates. It depends on item 72.

**SHACL-AF rules, SPIN translation, N3 rules and SWRL import.** Run `sh:TripleRule` and `sh:SPARQLRule` with `sh:order` and `sh:condition` as materialization with incremental maintenance, translate SPIN to SHACL-AF, and translate N3 and the DL-safe SWRL subset into the rule IR. SHACL-AF rules run natively. SWRL is limited to DL-safe rules, and SWRL, SPIN and N3 are translated to the rule engine rather than evaluated by their own engines. SHACL-AF and SPIN are absent from code and docs. TopBraid users depend on SHACL rules and Jena supports them. SWRL is common in bio ontology projects. It depends on items 71 and 72.

### Convergence

**RDFS as one rule profile with the shared explainer.** Express Jena's RDFS as an rdfs-jena rule profile specialized against the closed schema, lower it to the UNION rewrite only when faster, and replace the hand-written RDFS-on-read explainer with the shared Search. RDFS is implemented three times plus a 598-line hand-written explainer. Item 53 deepens the duplication, and no gated benchmark exists for RDFS on read. It depends on items 53 and 190.

**Explanation routing after the RDFS merge.** Route explanations through one source chain once RDFS on read uses the shared explainer, and stop doubling table keys for explanation sessions. The routing chain tries materialized, then demand, then RDFS on read. It depends on items 18, 285 and 286.

**Entailment analysis in memory.** Diagnose over the derived tier plus the base in memory instead of copying inputs to a private store and materializing again. `analysis.rs` calls its own path the full-closure fallback. Wait for item 18 Phase 3 or F22 evidence that the cost matters. It depends on items 18 and 340.

**One incremental-maintenance core for rules and views.** Run materialization as a recursive Z-set circuit on item 417's fixed-point operator with iteration timestamps, make the reasoner's backward/forward maintenance the circuit's deletion mode, and share one Footprint type, so noValue, lists, aggregates and blank-node rules are maintained instead of falling back to a full run. The reasoner's BFDR and the 12.9k-line incr core are separate frameworks with recompute fallbacks on both sides. Footprints are computed four times. F13 §4.3 plans the reasoner plug-in. It depends on items 221, 415, 417 and 418.

## Dependencies

* The gates of items 469 to 473, in particular the demand and RDFS-on-read benchmark of
  item 472, exist before the quick fixes land.
* Item 9 supplies the derived-fact layer and canonical equality that the closure
  operators and the columnar tier build on.
* Item 417's fixed-point operator and its LUBM measurements come before the shared
  maintenance core, which also needs items 415, 418 and 221.
* Item 72 defines the Datalog surface syntax that the n-ary core evaluates, and item 71
  supplies the axiom model that the SWRL import reads.
* Item 190's forge measurements come before the RDFS profile merge.

## Acceptance gates

* `bench:lubm`, `bench:reasoner`, the incremental differential tests and the demand gate
  of item 472 show no regression beyond 5% on forge.
* OWL2Bench under OWL RL completes at 5 universities and above once the closure operators
  land.
* The OWL 2 RL rule set covers every rule of the W3C OWL 2 Profiles §4.3 tables, each with
  a test.
* The derived tier keeps its memory per fact below the current 135 bytes, and its measured
  figure is recorded beside the RDFox reference of about 40 to 60 bytes.
* The shared maintenance core gives the same closure as a full run after every change in
  the differential tests, including deletions, blank-node rules and aggregates.

## Non-goals

* OWL 2 EL and DL classification, which [F24](../F24-owl-classification/README.md)
  holds.
* Evaluating SWRL, SPIN or N3 with their own engines. They are translated into the rule
  engine.

## Design constraints

Aggregates in the Datalog core arrive in two phases, stratified aggregates first
and monotone aggregates in recursion second, and both follow the columnar derived tier.
SHACL-AF rules run natively. SWRL is limited to its DL-safe subset, and SWRL, SPIN and N3
are translated to the rule engine.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: Differential Dataflow (McSherry et al. CIDR 2013), DBSP (Budiu et al. VLDB 2023), DDlog, RDFox transitive closure handling, Souffle eqrel, GraphDB sameAs optimization, Nuutila TC, VLog (Urbani et al. AAAI 2016), Nemo (Ivliev et al. KR 2022), Souffle B-tree and brie, Nemo, VLog, RDFox Datalog extensions, Souffle, RDFox dynamic planning, Souffle profile-guided scheduling, Green, Karvounarakis and Tannen PODS 2007, ProvSQL, RDFox counting, TopBraid SHACL-AF, EYE reasoner, W3C SWRL submission, Souffle (Jordan et al. CAV 2016) and Carac. No code from another project is copied.

## Outcome

The quick-fixes phase is built. Roadmap items 485 and 486 shipped and item 487 was
measured and not adopted. The reasoner also records witnesses for F18 under item 416.
The columnar derived tier, the first phase of the core, is built under item 835. The
other core and convergence phases remain under [item 456](../../ROADMAP.md).

### Columnar derived tier

`crates/sparkles-reasoner/src/tier` holds the derived facts. A fact gets a dense `u32`
id in the order of its first derivation, as the rows were numbered before, and its
subject, object and the index of its predicate live in one row of 24 bytes by id, of
which 4 are padding free for a later column, so the facts of one merge are an interval
of ids and the semi-naive ranges of the engine keep their meaning, and a hit in the
triple index checks its fact with one cache line. Each
predicate is a relation with its ids in ascending order, as its row list was before, and
two orders of its ids, by subject then id and by object then id. An order is a list of
immutable sorted runs, and a run keeps a fence key every eight entries, so a probe with a
bound subject or object searches the fence keys, which are few, and then the keys of at
most eight facts. A merge sorts its ids into each order as `(key, id)` entries of 16
bytes, by least significant digit over the digits in which the keys differ when the
batch is large and the pool has at most three threads, and by a parallel comparison sort
otherwise, and builds the run from the sorted entries. It appends one run per order and
compacts the older runs first, from the newest back while the next older run is at most
twice their size and further back until eight runs remain, and a relation with a quarter
of its facts dead is rebuilt without them. The engine sets a compaction floor at the start of each iteration's delta,
so a run the iteration reads is never merged with older ones, and a batch added between
runs lowers the floor to its first id. Orders over the whole tier by subject and by
object serve atoms with a variable predicate and are built on demand. A probe with a
bound predicate alone reads the predicate's id list, and a count of a predicate's facts
over an id range is a binary search in it. The triple index by hash stays, since the merge deduplicates its candidates through it and
`position` finds a fact by its triple in one lookup. The explicit flags of a maintained
closure are a bitmap of the tier beside the dead bitmap. `graph.rs` is a front over the
tier with the same interface, and the candidates of a probe are pieces of runs or id
ranges rather than a list of rows, filtered by the id range where a run is met in part.

The plan weighed two layouts. The first keeps each order as columns of subject, object
and id, 20 bytes per fact per order, with a position table of 8 bytes from id to place,
48 bytes in all. The second, which is built, keeps the fact by id once, 24 bytes in a row
that was three columns of 20 bytes until item 866, the predicate's id list, 4 bytes, and
each order as ids with fence keys, 5 bytes, 38 bytes in all, and pays an indirection for
each key it compares. The second was chosen because the memory gate of the phase is a
halving of the peak heap per derived fact and the candidate buffers, the term table and
the operators' workspace take a share of that peak the tier does not control, so the
smaller tier gives the gate room, and because the fence keys keep the indirect
comparisons of a probe to about three dependent reads. Measuring the first layout
against it is roadmap item 862, and a merge that deduplicates against the sorted orders
without the triple index is item 863. The ids still start after the base tier's facts,
so an input of more than `u32::MAX / 2` triples is still refused, and item 864 moves the
base tier off the id space.

The candidates of every probe are visited by ascending id, as the row lists gave them:
the id list of a predicate ascends, a probe with a bound subject or object finds ids
sorted by id within a run and the runs' id intervals ascend, and an id range ascends. The
facts an iteration derives therefore get the ids the previous tier gave them, and a
derived list link keeps its earliest value.

On wally, a 20-thread development host with 32 GiB, `sparkles infer --full` under
`owl-rl` on OWL2Bench with 5 universities derived 52,966,139 facts in six iterations with
both builds. The tier held them in 2,137 MB at the end, 40 bytes per derived fact with
the triple index and the cache of base facts, against about 135 bytes before, and RDFox
reports 40 to 60. The peak of the run did not fall with it. The budget's peak was
10.5 GB, 199 bytes per derived fact, and the peak resident set was 8.7 to 9.5 GiB against
9.0 to 10.0 GiB for the build before. The peak lies in the first iteration, where the
closure operators derive 52.4 million facts in one step: the step held 1.6 GB before the
merge of its outputs and the merge's buffers and probes of the base tier took the run to
its peak, so the tier is not where the memory of OWL2Bench(5) goes, which answers item
522's question. Roadmap item 865, below, bounds that step. On the quiet host the tier
build took 89 and 84 s at one thread against 77 and 70 s, and 41.0 and 42.0 s at 20
threads against 41.1 and 36.9 s. Its one-thread cost lies in the second iteration's
evaluation, 31.2 s against 24.4 s, and in the operators' step of the first, 18.2 s
against 13.2 s, which sorts the step's 52 million facts into the orders.

The closure operators merge their pairs into the tier in batches of four million (item
865). A batch enters the columns and the triple index as soon as it is full, so the next
batch is deduplicated against it, and a predicate's new facts are sorted into its orders
once when its closure is done, as one merge of the whole closure sorted them, so each
order still gets one run per closure and every fact keeps the id one merge would have
given it. The sink holds the graph while a predicate's closure runs, so no probe can read
the facts before they are indexed, and the probe roots assert that in debug builds. An
operator decides before it emits a pair whether it gives its rules back, an equivalence
by checking the literals of every class first and a guarded operator with a `notEqual`
filter by building each predicate's graph once for the check and once for the closure,
so a fallback has merged nothing, as it had when the step held every pair. The
columns double when a merge does not fit them, since a closure now arrives in many
merges. On OWL2Bench(5) the budget's peak fell from 10.5 GB to 4.2 GB, 80 bytes per
derived fact against 199, under the gate of half. The peak is now the sort of the
step's 52.4 million facts into the two orders at the end of the closure, 1.3 GB of keyed
entries and the two runs over the tier's 2.5 GB, and the intermediate peaks are the
doublings of the columns while a batch is merged. The tier ends at 2,467 MB, 47 bytes per
fact, since the columns hold the capacity of their last doubling. The peak resident set
of the whole `infer` command did not fall, 9.6 to 10.9 GiB against 9.4 to 10.0 GiB for
the build before, because the command commits the 53 million derived triples to the
store after reasoning, and a profile of the command finds that commit, which sorts the
quads on every thread, in most of its samples in both builds.

The tier was slower at one thread on OWL2Bench(5) when the batches landed: 88.6 and
84.5 s against 70.3 and 67.1 s, and 41.3 and 38.4 s against 38.3 and 34.3 s at 20 threads,
on a quiet wally. The per-rule times placed the cost in the second iteration's `prp-symp`,
14.8 s against 8.5 s, which rederives the facts of a symmetric closure and checks each
through the triple index, in the sort of a merge's ids into the orders, 5.9 s against
1.9 s for the hash lists it replaces in the first iteration's fill, and in the third
iteration's compaction, 1.3 s. Item 866 took these up. The fact is now one row of 24 bytes
in place of three columns, the sort keys `(key, id)` entries of 16 bytes and builds the
run from them in one pass, and a large batch sorts by least significant digit on one to
three threads. With these, on the same host in two interleaved rounds against the build
before the tier, the first iteration's merge takes 14.8 s against 15.5 s at one thread,
so the fill is faster than the hash lists were, the whole run takes 81.3 and 77.7 s
against 74.9 and 69.1 s at one thread and 39.0 and 37.6 s against 40.7 and 37.3 s at 20
threads, LUBM(100) takes 25.7 and 25.2 s against 24.8 and 24.7 s at one thread and 14.7
and 14.6 s against 14.3 and 14.1 s at 20 threads, and its insertion and deletion of every
hundredth triple at 20 threads take 2.2 and 3.0 s against 2.1 and 2.9 s. The budget's
peak is 71.5 bytes per derived fact with the comparison sort and 87.3 with the digit
sort's second buffer, under the gate of half of 199. The remaining cost at one thread
is in `Graph::position`, 13.1 s of samples against 7.5 s for the same lookups: the row
read costs the same in both builds, and the reads of the hash table's control bytes and
buckets cost two to three times as much, which item 866 still has to explain. A
compaction policy that left an older run alone when it was under an eighth of the runs
behind it saved the third iteration's 1.25 s and was not kept, since the planner's
distinct-key estimates sum over runs and the retained run changed a join order, and so
derived ids, in the reviewer's reproduction.

### Closure operators

`crates/sparkles-reasoner/src/closure.rs` evaluates transitive rules, their guarded form
`prp-trp`, and a symmetric rule paired with a transitive one, such as `eq-sym` with
`eq-trans`, as closure operators. After each iteration's merge an operator condenses the
relation with Tarjan's algorithm, computes each component's reachable set once in
reverse topological order, and releases a set once every component that reads it is
done. An equivalence closes its classes with union-find. With few new edges the operator
reads only the nodes that reach them, through the indexes. The rules stay in the
program, so explanations, incremental maintenance and query-time reasoning, which
evaluate them as joins, still name them.

The result equals the join evaluation. `tests/closure.rs` compares both on 300 random
ontologies under three profiles, on late edges, on literals with equal values in
filtered relations, on cliques and chains, and on programs whose result depends on the
order of derivation. The operators give their rules back to the join evaluator when a
filtered relation meets literals with equal values. A program with a rule that is not
monotonic, or one that reads lists and builds them in a rule head, evaluates every rule
as a join, and a run whose rules read lists and derive list facts through a property
starts again without operators. The operators stop at the candidate limit as they emit,
charge their buffers to the memory budget before they grow, and poll cancellation.

On forge with 20 threads at `58f5d9a9`, `examples/lubm.rs` materialized OWL2Bench with 2 universities
under OWL 2 RL in 1.43 s at a peak heap of 414 MiB, against 49.4 to 57.3 s and
1,362 MiB with the rules evaluated as joins, and with 1 university in 340 to 372 ms
against 4.7 to 5.0 s. Both runs derived the same facts. LUBM(100) took the same time
both ways, 2.6 to 2.7 s for OWL 2 RL and 1.3 s for RDFS, because its transitive relations
are small. With `sparkles infer`, OWL2Bench with 5 universities now completes in 44 s at 9.5 GiB
of RSS, where the joins ran out of 14 GiB after 13 minutes, and with 2 universities it
takes 5.0 to 5.3 s against 53 s. OWL2Bench with 10 universities exceeded the 14 GiB run limit with or without the
operators, and with them a 28 GiB limit on a development host. Where that memory goes
has not been profiled, and roadmap item 522 covers it.
[The reasoning results](../../benchmarks/reasoning/RESULTS.md#owl2bench) give every run.

### Bounded incremental deletion

Roadmap item 798 bounds the maintenance after a deletion by the cost of a full run,
until item 521 gives the incremental core closure operators. `incremental::Bound` counts
the rule instances that the deletion's backward searches and its consequence steps
match, and compares the charges of the run with their total when the deletion started.
Past four instances for each fact of the closure, at least 2^20, or past as many bytes
as the closure's own heap, at least 64 MiB, the update stops with a fallback and the run
materializes in full, which gives the same closure. The searches compare once every
1,024 instances and the consequence evaluations count their instances as they match
them, so the bound is a heuristic on sampled charges.

In a clique of a symmetric and transitive property every fact follows through every
member. Before the bound, deleting 24 of the 1,200 edges of a clique of 600 people
visited 184 million rule instances for 24 checked facts and held 5 GB of charges, where
the full run peaks at 79 MB. The deletion now stops after about 360,000 instances and
falls back, at an update peak of 101 MB. The consequences of deleted facts now keep
each head once, since a rule whose head does not depend on the deleted atom has one
head for millions of instances, and a builtin that binds many values, such as
`listMember`, ends its loop when the bound, a memory refusal or a cancellation stops its
evaluation.

On OWL2Bench(1), deleting 1% of the data triples, 18 of them edges of the
`hasSameHomeTownWith` clique of about 1,100 people, grew to 14.1 GiB before the
out-of-memory killer ended it. With the bound the deletion falls back on memory and the
run finishes in 1.4 to 3.4 s at 808 to 866 MiB of RSS on a development laptop, with a
closure equal to Nemo's, where a full run takes 1.7 to 4.0 s and 556 to 641 MiB
([deletion record](../../benchmarks/reasoning/runs/2026-11-09-item-798-delete-stanley/commands.txt)).

Item 815 brings the witnesses' preparation for a deletion under the bound, and item 816
reports the peak of an update that falls back.

### OWL 2 RL rules

`rules/owl-rl.rules` gained `prp-key` for keys of 1 and 2 properties, `cls-maxqc4`,
`scm-cls`'s `owl:equivalentClass` and `owl:Nothing` conclusions, `scm-op`, `scm-dp`, the
annotation-property axioms of `prp-ap` and the `dt-type1` axioms for the RL datatypes.
`tests/owl_rl_rules.rs` lists every rule of OWL 2 Profiles Tables 4 to 9 and checks each
one through an entailment, or, for a rule whose conclusion is `false`, through the
inconsistency check that reports it. The test cases follow the Profiles tables, because
the OWL 2 test-case repository was not reachable, and roadmap item 520 vendors
the official cases. `eq-ref`, `dt-type2`, `dt-eq` and `dt-diff` stay out of the
materialization, since each would derive a fact for every term or every pair of
literals, and roadmap item 518 covers them with a value operator. Chains of more
than 3 properties and keys of more than 2 would add thousands of rules to the demand
programs of query-time reasoning, which have a cap of 50,000 rules, and roadmap item
517 covers them in the n-ary core. The compact query session now admits 128
distinct terms, because the new axioms' constants passed the previous 64. Query-time
reasoning under OWL 2 RL is no slower for the larger demand programs. Four OWL2Bench
queries over 1 university, each in its own process, took 77 to 87 s against 230 to 240 s
before, and returned the same number of result lines. Entailment analysis evaluates the
whole rule set in each oracle run, so a justification with about 3,000 checks became
about 28% slower, a tradeoff that roadmap item 523 follows up. A work split
tries up to four atom steps for one that gives enough items, which keeps `prp-key`'s
first step from running on one thread.

### Cost-based planning

A planner that probed constant terms in the indexes, estimated subject stars from the
characteristic sets, ordered bodies of up to six atoms by dynamic programming and
replanned on factor-of-two changes was built in `58f5d9a9` and reverted in `17a67b5b`.
It derived the same facts, was slower on OWL2Bench with 1 university and no faster
elsewhere, and raised LUBM(100)'s peak heap by 50 MiB.
[PERFORMANCE-EXPERIMENTS.md](../../PERFORMANCE-EXPERIMENTS.md#reasoning) records the
measurement.

### Witnesses

Item 416 gives the reasoner bounded witnesses per fact with F13's provenance algebra,
off by default ([F18](../F18-explanations-and-entailment-diffs.md#reasoner-witnesses-item-416)).
They are computed after the closure, from the rule instances that the backward plans of
incremental maintenance find, and the engine's evaluation does not change. The closure
operators keep no provenance of their own. Their facts take witnesses from the instances
of the rules they evaluate, found as joins over the closure they built, so the result
does not depend on how the closure was computed. The compact provenance of the core phase
can later store these witnesses in columnar form, under roadmap item 556. With
witnesses off, LUBM and OWL2Bench materializations derived the same facts with the same
peak heap as the build before them. Their medians on a Namespace host ranged from 0.3%
faster to 7% slower and their means were within 3%, although the path without witnesses
only checks an option. Roadmap item 554 checks the difference on an Intel host.
The [reasoning results](../../benchmarks/reasoning/RESULTS.md#reasoner-witnesses) give
the measurements.

