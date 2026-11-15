# F23: Reasoner core

> **Status:** partially complete. The quick-fixes phase is built, and the
> [Outcome](#outcome) records it.
>
> **Roadmap:** [item 456](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase. The [implementation plan](PLAN.md) orders the remaining
> phases, fixes the representation and maintenance decisions and gives each phase its
> files, gates and routine and judgment portions.

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
evaluation and derivation records as its compact provenance, and the shared
incremental-maintenance core follows them, after item 547's LUBM measurements. The rule
frontends and the native n-ary Datalog core come after the tier, as the
[plan](PLAN.md#phases) orders them. RDFS on read becomes one rule profile only at
measured parity with the current rewrite.

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

**Columnar sorted derived tier with merge-join rule evaluation.** Replace the reasoner's row table and hash lists with immutable sorted per-predicate column runs plus a small mutable delta, and evaluate rule steps with merge or galloping joins. Derived facts cost about 135 bytes each and use u32 row ids, which caps the tier at about 4.29B rows. RDFox reaches about 40 to 60 bytes per fact. The [plan](PLAN.md#the-derived-tier-is-columnar-and-sorted) fixes its layout and proposes that item 9 Phase 2's derived layer be written from the tier's sorted runs, which the maintainer decides. Roadmap item 835 holds this phase.

**Compact provenance with derivation records and bounded witnesses.** Store one derivation record per derived fact, the rule and the body facts of the instance that first derived it, in columnar form, enabling deletion that checks only broken records, Why answers and reviewed-only visibility without re-derivation. Explanations are re-derived on demand. F13 §4.3 rules out counts for recursive maintenance because a fact in a cycle can support itself, and the [plan](PLAN.md#provenance-is-a-derivation-record-per-fact-with-counts-only-where-measured) keeps counts out of the design unless a measurement on non-recursive strata shows them faster than the record check. It depends on items 18, 285, 415 and 416, and roadmap item 836 holds this phase.

**Compiled or specialized rule kernels.** Generate native kernels for the fixed RDFS and OWL RL profiles, or JIT rule plans, to remove interpretive Step dispatch. Rules run through the interpreted Eval walk. Typical gains are 2 to 5 times per rule. It depends on item 9.

**Native n-ary Datalog core with existential rules and aggregates in recursion.** Make a general Datalog IR with n-ary IDB predicates, a restricted chase with MFA or joint-acyclicity checks, stratified negation and monotonic aggregates in recursion, with Jena rules as one frontend. Aggregates arrive in phases, with stratified aggregates first and monotone aggregates in recursion second, both after the columnar derived tier (*columnar sorted derived tier with merge-join rule evaluation*). The reasoner has one ternary relation (`demand.rs`:6). C24 §6.4 to §6.5 already plans n-ary relations encoded through makeSkolem tuple nodes and weakly acyclic existentials, so the remaining gap is a native n-ary representation, the restricted chase and recursive aggregates. The tier's runs are fixed-width tuples, so an n-ary relation is an order over more columns, and item 72's surface syntax is one frontend of the IR rather than a prerequisite. Roadmap item 839 holds this phase.

**SHACL-AF rules, SPIN translation, N3 rules and SWRL import.** Run `sh:TripleRule` and `sh:SPARQLRule` with `sh:order` and `sh:condition` as materialization with incremental maintenance, translate SPIN to SHACL-AF, and translate N3 and the DL-safe SWRL subset into the rule IR. SHACL-AF rules run natively. SWRL is limited to DL-safe rules, and SWRL, SPIN and N3 are translated to the rule engine rather than evaluated by their own engines. SHACL-AF and SPIN are absent from code and docs. TopBraid users depend on SHACL rules and Jena supports them. SWRL is common in bio ontology projects. SHACL-AF, SPIN and N3 need only the triple relation and follow the tier, and the SWRL import depends on item 71. Roadmap item 838 holds the SHACL-AF and SPIN phase.

### Convergence

**RDFS as one rule profile with the shared explainer.** Express Jena's RDFS as an rdfs-jena rule profile specialized against the closed schema, lower it to the UNION rewrite only when faster, and replace the hand-written RDFS-on-read explainer with the shared Search. RDFS is implemented three times plus a 598-line hand-written explainer. Item 53 deepens the duplication, and no gated benchmark exists for RDFS on read. It depends on items 53 and 190.

**Explanation routing after the RDFS merge.** Route explanations through one source chain once RDFS on read uses the shared explainer, and stop doubling table keys for explanation sessions. The routing chain tries materialized, then demand, then RDFS on read. It depends on items 18, 285 and 286.

**Entailment analysis in memory.** Diagnose over the derived tier plus the base in memory instead of copying inputs to a private store and materializing again. `analysis.rs` calls its own path the full-closure fallback. Wait for item 18 Phase 3 or F22 evidence that the cost matters. It depends on items 18 and 340.

**One incremental-maintenance core for rules and views.** Make the reasoner's rule engine over the derived tier a `Program` of item 417's fixed-point operator, which is a record-based backward/forward maintainer rather than a timestamped circuit, maintain the closure operators' relations inside it, emit the tier's change as a Z-set that views read, and share one Footprint type, so rules that read lists, and stratified noValue once the full run's negation semantics are decided, are maintained instead of falling back to a full run. The reasoner's backward/forward maintenance and the incr core are separate frameworks with recompute fallbacks on both sides, and footprints are computed four times. F13 §4.3 plans the reasoner plug-in. It depends on items 415, 417 and 547 and on the derivation records, and item 221 builds on its Z-set output. The [plan](PLAN.md#one-maintenance-core-is-the-record-based-backwardforward-maintainer) gives the design and the correctness argument, and roadmap item 837 holds this phase.

## Dependencies

* The gates of items 469 to 473, in particular the demand and RDFS-on-read benchmark of
  item 472, exist before the quick fixes land.
* The plan proposes writing item 9's derived layer from the columnar tier, and item 9's
  representatives supply the canonical equality that the equality rules read.
* Item 417's fixed-point operator and item 547's LUBM measurements come before the shared
  maintenance core, which also needs item 415 and the derivation records. Item 221 reads
  the core's Z-set output, and item 418's aggregates are maintained by the views side.
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

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: Differential Dataflow (McSherry et al. CIDR 2013), DBSP (Budiu et al. VLDB 2023), DDlog, RDFox transitive closure handling, Souffle eqrel, GraphDB sameAs optimization, Nuutila TC, VLog (Urbani et al. AAAI 2016), Nemo (Ivliev et al. KR 2022), Souffle B-tree and brie, Nemo, VLog, RDFox Datalog extensions, Souffle, RDFox dynamic planning, Souffle profile-guided scheduling, Green, Karvounarakis and Tannen PODS 2007, ProvSQL, RDFox counting, TopBraid SHACL-AF, EYE reasoner, W3C SWRL submission, Souffle (Jordan et al. CAV 2016) and Carac. The [plan](PLAN.md) also draws on the
backward/forward algorithm of Motik, Nenov, Piro and Horrocks (AAAI 2015), on Nemo's
public program conventions through item 791, and on the W3C SHACL Advanced Features note.
The evaluation order of the SHACL rules follows the behaviour of TopQuadrant's TopBraid
SHACL API (`RuleEngine`, `TripleRule`, `SPARQLRule` and the `shaclinfer` tool), whose source
is under the Apache License 2.0, and its rule tests are vendored with that licence under
`crates/sparkles-reasoner/tests/shacl-af/topbraid`. The SPIN translation follows the SPIN
W3C Member Submission. No code from another project is copied.

## Outcome

The quick-fixes phase is built. Roadmap items 485 and 486 shipped. The planner of item 487
was measured and not adopted, and item 939 orders rule bodies from sampled bindings
instead. The reasoner also records witnesses for F18 under item 416.
The columnar derived tier, the first phase of the core, is built under item 835. The
SHACL-AF and SPIN frontend of Phase 5 is built under item 838, with its maintenance in
item 958 and SPIN's RDF syntax in item 959. The rule IR of the n-ary core, the first
phase of item 839, is built, and the engine does not evaluate it. The other core and
convergence phases remain under [item 456](../../ROADMAP.md).

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
sort's second buffer, under the gate of half of 199.

The remaining cost at one thread was in `prp-symp`, which scans the first iteration's
derived facts, looks up the symmetric declaration of each fact's predicate in the base,
checks that the object is not a literal and looks up the head in the triple index. On
atlas, an Intel Core i5-13500 like the benchmark host, with the run pinned to one core,
the rule took 16.0 s against 9.95 s in the build before the tier. Both builds made the
same lookups and spent the same instructions, branch misses and cache misses at every
level on them, within a few percent. Counters sampled over the rule gave 741 cycles per
candidate against 427, with 1.1 cache misses in flight on average against 1.8. A
candidate's work, about 480 micro-operations, filled the core's reorder window, so the
index misses of one candidate no longer overlapped those of the next.

A scan now probes its candidates' heads in groups when the rest of the plan only checks
them. In forward chaining, when an atom's scan binds the last variables of the rule's one
triple head and every later step is an atom or a builtin whose variables are all bound,
the scan matches 16 candidates, forms their heads and probes the triple index for all of
them in a row. It then runs each candidate through the rest of the plan in order, and the
emit takes the group's answer when the head it forms is the one the group probed, so the
16 probes' misses overlap. A scan goes back to one candidate at a time when a group's
instances used fewer than half of its probes, as in `prp-key-1`, whose later steps reject
most candidates, or when the group probed fewer heads than a quarter of its candidates,
as in `rdf1`, whose heads repeat. The grouped path is built to give the facts, ids,
counters and stopping points of the one-at-a-time path. A test compares the derived facts
in order, their liveness, the iterations and the errors of both paths over random OWL 2
RL and RDFS programs, with and without an inferred-fact limit. `prp-symp` takes 7.0 s
with it at one thread.

Two further changes keep the tier's probes and plans level with the build before it. A
run's probe searches a coarse fence of every 64th fence key before the fence keys. Each
order counts the distinct keys its facts have had, dead ones included, as the hash lists
before it did. A merge counts the keys of its run that no older run holds, and a
compaction that drops dead facts keeps the keys it leaves without a fact, so the planner
reads the same counts however the facts fall into runs. The sum of the runs' counts,
which the tier read at first, had made `prp-key-1` choose a plan that scanned 44.5
million rows in the third iteration, and an estimate from sketches of the runs changed a
join order, and so a derived list link, with the split of a relation into runs. A scan
with one candidate is not grouped, since it has no probe to overlap, and roadmap item 937
carries the fallback across the short scans of one rule. Index entries with 32 hash bits,
and huge pages for the index tables, measured no faster and are recorded in
[PERFORMANCE-EXPERIMENTS.md](../../PERFORMANCE-EXPERIMENTS.md#reasoning).

On atlas, in four interleaved rounds against the build before the tier, OWL2Bench(5)
takes 100.8 to 105.4 s against 102.2 to 107.4 s at one thread and 50.2 to 52.7 s against
50.8 to 54.2 s at 20 threads, where the tier took 114.7 to 116.0 s at one thread before
the grouped probes. LUBM(100) takes 37.5 to 38.0 s against 37.3 to 37.8 s at one thread
and 16.1 to 16.7 s against 16.5 to 17.1 s at 20 threads. Its insertion of every hundredth
triple takes 2.43 to 2.46 s against 2.35 to 2.41 s and its deletion 3.30 to 3.34 s
against 3.23 to 3.27 s at 20 threads, 2% slower in every round, and roadmap item 938
investigates that cost with the one-thread run, 0.5% slower in every round. The [run
record](../../benchmarks/reasoning/runs/2026-11-13-item-866-atlas/env.txt) holds the
commands, host and builds. Both builds derive the same 52,966,139 and 10,907,379 facts.
The budget's peak stays at 4.6 GB, 87 bytes per derived fact, and the tier at 2,746 MB.
`prp-key-1` still takes 1.9 s against 0.9 s in the second iteration, which reads its
scan's keys through the fact rows, and the layout of item 862 compares keys in place. A
compaction policy that left an older run alone when it was under an eighth of the runs
behind it saved the third iteration's 1.25 s and was not kept, since the distinct-key
counts of that build summed over runs and the retained run changed a join order, and so
derived ids, in the reviewer's reproduction.

### Rule IR

`crates/sparkles-reasoner/src/ir.rs` holds the program form of the n-ary core. A program
is a set of rules over the triple relation and over declared relations, each with a name
and an arity, whose bodies hold positive and negated atoms and builtins and whose heads may
hold existential variables. The Jena frontend gives the IR of each compiled Jena rule as it
is, so a Jena rule has no relation, negated atom or existential. A check of the program
refuses an atom of the wrong arity, a variable that a head, a negated atom or a builtin
needs and that the body never binds, and builtins whose inputs no order of the body binds.

The strata follow the dependencies that demand's check for a negation on a cycle reads,
with declared relations and negated atoms added. Every head of a rule depends on every read
of the rule, list structure included. A rule with an existential or a `makeTemp` call is
one part with all its heads, since its heads share the nodes it makes, and any other rule
is evaluated in one part per head, each in its own stratum. A program that uses a
relation, a negated atom or an existential must stratify, its `noValue`s included, while a
program of Jena rules alone keeps Jena's reading of `noValue` and its strata are only
reported. The negative list builtins `listNotContains`, `listLength` and `listForAll` read
lists that a longer list can change, and the strata list the rules whose lists a head of
the program may give, which for `owl-rl` holds `cls-int1`. On the rule sets of the demand
tests that read `noValue` or lists and on 500 random programs the strata find a negation on a cycle exactly when
demand's check does, apart from rules that make nodes for several heads, whose heads the
strata keep together.

A program with an existential must be weakly acyclic, after Fagin, Kolaitis, Miller and
Popa. The positions of the triple relation are the subject and object of each constant
predicate, with the facts of a head with a variable predicate flowing to every constant
predicate, and list builtins and head actions read and give the positions of `rdf:first`
and `rdf:rest`. A builtin output other than a list member counts as a new value. The
analyses charge their graphs to the run's budget before they build them, from bounds of
the program's size, and walk their components without recursion. The maintainer chose
C24 §6.2's syntax subset in `parser.rs`, a Datalog-first restricted chase per stratum, the
storage of relations of arity three and more behind tail handles, and stratified `noValue`
in programs that use the IR, and null identity is still open.

`crates/sparkles-reasoner/src/ir/lower.rs` lowers a program to the engine's rules. A
relation fact is a row of the derived tier over the relation's key, a reserved id that no
input can name, so relation facts share the tier's id space, semi-naive ranges, hash
index, budget and limit of inferred facts, and the graph hides them from every atom and
`noValue` with a variable predicate. The columns after the first of a fact of arity three
or more are a tail, interned with the run's local terms, which a lowered body atom reads
through a builtin and a lowered head interns through another. A negated atom is a builtin
that is true when no fact before the iteration's end matches. `engine::run_program` runs
a program of Jena rules alone as before, one with relations and no negation in one
stratum, and one with a negation by strata, each to its fixpoint before the next, with the
limits of inferred facts and iterations counted across the strata and a warning for each
Jena rule whose `noValue` is read by strata. Rules that read lists are replayed when list
cells arrive, so a list cell can be an instance's only new premise. No frontend produces
relations yet, so only the reasoner's tests reach this path, and incremental updates,
demand, explanations and witnesses follow in later steps. On 8,000 random programs over
relations of arities zero to five, triple atoms, recursion and negated atoms, the runs
equal a naive stratified evaluator, and the decoded facts are the same at 1, 4 and 16
threads.

### SHACL rules and SPIN

Item 838 added the `shacl-af` profile in `crates/sparkles-reasoner/src/shacl_af.rs`, over
the rule parser and evaluator of `crates/sparkles-shacl/src/rules.rs` and the SPIN
translation of `crates/sparkles-shacl/src/spin.rs`. Rules are grouped by the shape's and
the rule's `sh:order`, and each group runs in rounds until a round adds nothing, or once
with `mode=one-pass`. A round evaluates every rule over an overlay of the run's snapshot
that holds the facts derived before it, so targets, conditions, node expressions and
SPARQL queries read derived facts as they read the input. A triple rule evaluates its node
expressions per focus node with the SHACL evaluator, and a SPARQL rule runs its CONSTRUCT
per focus node with `$this` pre-bound, as TopBraid's engine does. The terms the rules
compute are interned in the run's write transaction, so the overlay names them with store
ids, and a template blank node is one new node per rule, label and solution, which gives
such a rule a fixed point. A group past `max-rounds`, 100 by default, fails the run and
names its order, as decision 3 asks.

The plan compiled triple rules to engine rules with a `shConforms` builtin. Item 838 kept
one evaluator for both rule kinds instead, because a condition that reads facts the rule's
atoms do not match needs the rule evaluated again whenever those facts change, which the
semi-naive engine does not do for builtins, and because the computed terms of node
expressions and SPARQL functions need store ids that the engine's local terms do not
give the SHACL validator or the query engine. The general SPARQL bodies run in full
materializations now, since only their maintenance needs the recompute stratum of Phase 4.
An incremental run of the profile therefore materializes in full and says why, and item
958 holds the compiled form and its maintenance. The SHACL-AF 2017 node expressions map
onto the SHACL 1.2 expression tree, with a new `SparqlCall` node for `sh:SPARQLFunction`
calls over the combinations of their arguments. TopBraid's seven rule tests pass and are
vendored with their licence. SPIN rules run from `sp:text`, and SPIN's RDF syntax and
template calls are item 959. The Fuseki converter maps `ja:reasonerURL sh:Rules`, a
Sparkles convention, with `sh:shapesGraph` and `tosh:maxIterations`.

The example `shacl_af_bench` of the reasoner measures the evaluator. It makes N rectangles,
a third of them squares, and runs a triple rule that computes each area through a
`sh:SPARQLFunction`, a SPARQL rule whose condition finds the squares and, in a later
order, a triple rule that reads the squares' areas. Each order takes two rounds. On
stanley, a 16-thread Intel Core Ultra X7 358H laptop, with a release build of commit
6af0a6cf and one run per size, 1,000 rectangles took 42 ms, 5,000 took 218 ms and
20,000 took 897 ms, which is 10.4 to 11.2 µs per rectangle and round, linear in the
focus nodes. The work is the per-focus-node evaluation on one thread, with a query for
each function call and each SPARQL rule. That cost per focus node and round is the figure
the compiled semi-naive form of item 958 has to beat, since it evaluates only the facts
that are new in a round.

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

That planner kept the greedy estimate of every atom that holds a variable, and the greedy
order is wrong by orders of magnitude where the rule itself binds the predicate or the
class that an atom asks for. The greedy estimate of an atom whose predicate is a variable
bound earlier in the body is a quarter of an average predicate's rows, and its estimate of
`(?y rdf:type ?c)` with `?c` bound is the size of an average class. In `prp-key` the
ontology binds the keyed class and the key property, and the greedy order joined the
members of the class with themselves before it read a key value.

A probe that evaluates every atom order of each plan on one thread measured what the
greedy orders cost. It is the `plan-probe` feature of `sparkles-reasoner`, which
`SPARKLES_PLAN_PROBE=1` turns on in `examples/lubm.rs`. On a development laptop with one
pinned thread, at `e66d8b70`, it found the following.

| Workload | Rule evaluation in the greedy orders | In the fastest orders | Where the difference is |
|---|---|---|---|
| OWL2Bench(1), `owl-rl` | 494 ms | 158 ms | `prp-key` falls from 334 ms to 0.4 ms. |
| OWL2Bench(5), `owl-rl` | 30.7 s | 9.2 s at most | `prp-key` takes 21.6 s. Its longest plan outran the probe's budget. |
| LUBM(10) and LUBM(100), `owl-rl` | 0.89 s and 11.12 s | The probe found the same times. | The probe found no faster order. |
| LUBM(10), user rules with bodies of four to six atoms | 286 ms | 228 ms | Two rules gain, and their candidate counts fall far less than their times. |
| LUBM(100), the same rules | 4.14 s | 4.03 s | The probe did not try every order of the longest body. |

Item 939 orders rule bodies from sampled bindings (`engine/sampled.rs`). The planner keeps
a sample of up to 16 bindings of the steps placed so far, read from the facts the plan
reads. It takes an atom's estimate as the mean number of candidates the atom has under
those bindings, counted in the derived tier's orders and in the store's index, places the
atom with the fewest and extends the sample by that atom's facts. Its order replaces the
greedy order only when the sample puts its cost at half the greedy order's or less, and
only when it needs no order over the whole tier that the greedy order does not need.
Three gates keep the cost of planning low. A body of fewer than three atoms keeps the
greedy order. A body whose predicates are all constants is sampled only when the
statistics put its plan at two million candidates or more. A walk of the greedy order with
four bindings comes first, and a greedy order that it puts under 50,000 candidates is
kept without a search. `ReasonOptions::sampled_planning` is on by default, and `false`
keeps the greedy order of every body.

A run derives the same facts with either planner. The order of a body changes the work of
an evaluation and the order in which facts are derived, so fact ids can differ between
the planners. Two cases keep the greedy order because another order could change more
than that. A builtin checks a variable that is already bound by value, so that the
integer 3 passes for the decimal 3.0, where an atom matches the term itself. A rule in
which a builtin binds a variable that an atom also holds, or that another builtin also
binds, therefore keeps the greedy order. Every rule keeps it while the derived tier holds
a dead fact, which a deletion leaves when it removes a derived fact, since an order
counts a dead fact among its candidates until a compaction of its run drops it. Roadmap
item 944 covers both. Query-time reasoning and
the maintenance of witnesses plan greedily as before.

The sample is drawn at fixed places, so one snapshot plans the same way each time. In the
derived tier the places are evenly spaced candidates in id order, which do not depend on
how the facts fall into runs. In the store they are the starts of blocks along a scan.
The same quads in other blocks, after another sequence of loads or a compaction, can give
another sample and another order, which changes the time of a run and never its facts.

Planning is bounded. A read of the store ends with one block of the index at the latest,
whatever share of the rows belongs to graphs that the run does not read. Within that
block it has an allowance of 4,096 rows, counted in the pieces the scan delivers, which
are blocks, segments and rows of the snapshot's delta. The read stops before the piece
that follows the one that passed the allowance, so it can enter more rows than the
allowance by the size of one piece. A scan that merges the delta decodes a piece before
it delivers it, and it consumes deleted rows without delivering them, so it can decode
more than it delivers. A plan makes at most 2,048 probes, and each is charged 1 KiB to the run's
memory budget until the plan is made. A plan that needs more, that fails to read the
store or whose costs overflow keeps the greedy order. The planner's scans count in a
run's `base_scans`, so that count differs between the planners.

On atlas, with one binary at `a63916c0` and interleaved runs of the two planners, the
rules of OWL2Bench(5) take 29.9 s against 51.6 to 51.8 s at one thread and 9.5 to 9.7 s
against 12.5 to 12.8 s at 20 threads. Its whole materialization takes 80.6 to 80.8 s
against 102.5 to 102.8 s and 42.5 to 42.7 s against 45.5 to 49.2 s. The rules of
OWL2Bench(1) take 662 to 696 ms against 976 to 988 ms at one thread. No plan of LUBM
changes, and the rules of LUBM(100) take 14.51 to 14.55 s against 14.50 to 14.55 s at one
thread. On LUBM the planner changed no plan, by its count of changed plans, so the
difference there is taken to be the cost of planning. It shows on LUBM(10) at one thread,
where six of seven sampled runs took 930 to 936 ms and one 970 ms against 926 to 933 ms.
The median of the seven paired differences is 4 ms, 0.4%, and roadmap item 945 covers
that cost. The highest peak heap of the sampled runs of a workload is within 3.1 MiB of
that of the greedy runs under OWL 2 RL. With the long-body rules on LUBM(100) it is
15.3 MiB higher at one thread, 805.9 against 790.6 MiB, and 13.5 MiB higher at 20
threads. Every pair of `infer` runs derived the same facts by their digests, and every
pair of materializations inferred the same number. The
[results](../../benchmarks/reasoning/RESULTS.md#sampled-rule-body-planning) hold the
table. These are benchmark-worker measurements of `a63916c0`, and neither the later
commits of the item nor the benchmark host have repeated them.

The DL Horn rules of `sparkles-owl` are a calculus of their own and do not use this
planner. Bodies of constant predicates whose orders differ by less than a factor of two
in candidates keep the greedy order, and roadmap item 940 covers the time the probe
found there.

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

