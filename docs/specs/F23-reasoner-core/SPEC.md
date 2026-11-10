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
No code from another project is copied.

## Outcome

The quick-fixes phase is built. Roadmap items 485 and 486 shipped and item 487 was
measured and not adopted. The reasoner also records witnesses for F18 under item 416.
The core and convergence phases remain under
[item 456](../../ROADMAP.md).

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

