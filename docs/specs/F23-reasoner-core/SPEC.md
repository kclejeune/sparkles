# F23: Reasoner core

> **Status:** planned.
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

Nothing is built. [Item 456](../../ROADMAP.md) tracks the work.
