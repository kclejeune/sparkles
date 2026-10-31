# X15: Join algorithms and estimation

> **Status:** planned.
>
> **Roadmap:** [item 454](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

The design covers correlation-aware cardinality estimation, worst-case-optimal joins, scalable join enumeration, adaptive re-optimization, OPTIONAL reordering, a reachability index and factorized intermediate results.

## Problem and goal

Outside subject stars and tiny inputs, the planner estimates a join as the product of
its inputs divided by the larger distinct count, scaled by a constant, and assumes the
patterns are independent. Exhaustive join ordering stops at 16 inputs, larger groups stay
greedy, and planning time grows with the square of the patterns. Cyclic patterns such as
triangles, the WatDiv C templates and LUBM `q2` run through binary joins and bespoke
probes, although the sorted permutations already form tries. OPTIONAL, UNION and MINUS
keep their algebra order apart from filter pushdown, and property paths rebuild their
adjacency for every query.

The goal is a planner and a set of join algorithms that keep intermediate results small on
the workloads where Sparkles loses or relies on special cases.

## Scope

The phases below share the estimator. The worst-case-optimal join and factorized
results build on the n-ary join node of item 84, and scalable enumeration replaces the DP
limit that item 84 also raises. Each phase lands in the unified executor of
[X14](../X14-unified-executor/README.md).

## Candidate phases

**Correlation-aware cardinality estimation.** Add characteristic pairs for chains, object-side characteristic sets, sampling-based estimates for subplans larger than 1024 rows, and an optional pessimistic upper-bound estimator for cyclic and multi-key joins. Outside subject stars and tiny inputs the planner uses a*b/max(d)*0.7 with independence, and keyprobe is capped at 1024 rows and 32 keys.

**Worst-case-optimal multiway join with a hybrid planner.** Add a leapfrog triejoin or Free Join operator over the sorted permutations, used for cyclic subgraphs of a BGP and of rule bodies, with a cost rule that picks it over binary joins. No WCOJ exists in code or docs. Cyclic BGPs such as triangles, WatDiv C templates and LUBM q2 rely on binary joins and bespoke probes. The permutations are already tries. It depends on item 84.

**Reachability and transitive-closure index for property paths.** Keep an SCC condensation with interval, 2-hop or GRAIL labels for hot transitive predicates, shared with the reasoner's closure operator (*specialized closure operators for transitive, symmetric and equivalence relations*). It needs a versioned on-disk format and maintenance at compaction or through item 417. Property paths build hash adjacency per query and traverse per start node (`exec.rs`:7556-7600). Large hierarchies such as GO, SNOMED and dependency trees repeat the same closure work. It depends on items 17 and 417.

**Bidirectional evaluation and cardinality estimates for bound-bound paths.** Evaluate p+ and p* from both ends when both are bound, and estimate path cardinalities from SCC statistics. Bidirectional search exists only in `pathsearch.rs`, not in SPARQL property paths.

**Adaptive re-optimization and cardinality feedback.** Re-plan at pipeline breakers when observed rows diverge from estimates, and keep observed join sizes per snapshot keyed by plan fingerprint for repeated templates and for item 32's plan cache. Executed-plan feedback is reported only after a query finishes, and no misestimate is ever corrected mid-query. It depends on items 27 and 32.

**Scalable join enumeration for large groups.** Use DPhyp or DPccp up to about 50 relations and linearized or IKKBZ-seeded DP beyond, and collapse stars into one n-ary star operator before ordering. Exhaustive DP stops at 16 inputs and 2^15 splits, rounds use 2^13, and larger groups stay greedy. Planning time grows with the square of the patterns. It depends on item 84.

**Well-designed OPTIONAL reordering and null-rejection rewrites.** Reorder left joins for well-designed patterns, turn OPTIONAL into inner joins under null-rejecting filters, and make the DP aware of outer-join hyperedges. OPTIONAL, UNION and MINUS follow algebra order apart from filter pushdown and specific rewrites (`plan.rs`:7-9). BSBM and DBpedia workloads are OPTIONAL-heavy.

**Factorized intermediate results.** Represent many-to-many join results in factorized form so that intermediate sizes stay near the input sizes. No factorized representation exists. It pays off after the WCOJ work of *worst-case-optimal multiway join with a hybrid planner*. It depends on item 84.

## Dependencies

* The executor merge of [item 453](../../ROADMAP.md) lands first.
* Item 84 supplies the n-ary join node that the worst-case-optimal join, scalable
  enumeration and factorized results use.
* Items 27 and 32 supply live operator counts and the plan cache that adaptive
  re-optimization feeds.
* The reachability index needs a versioned on-disk format and maintenance at compaction or
  through the fixed-point operator of item 417.

## Acceptance gates

* The forge gate of X14 applies to every phase: no query 5% or more slower at 12 threads
  and no query under 0.3 ms slower on an Intel host.
* `plan_fidelity_tests.rs` records estimated against actual rows, and a new estimator
  must not raise the error on any recorded plan.
* Cyclic workloads, namely triangles, the WatDiv C templates and LUBM `q2`, are the
  acceptance cases of the worst-case-optimal join, with answers equal to the binary-join
  plans.
* The reachability index refuses an unknown format version and gives the same answers as
  per-query traversal after compaction and after incremental changes.

## Non-goals

* Plan hints and forced join orders, which U04 §11.1 keeps out of scope.
* Distributed query planning, which the sharding phase of X16 would need first.

## Design constraints

The estimator and the rule-body planner of [F23](../F23-reasoner-core/README.md)
share one implementation. A planner change that a measurement shows to be no faster is
recorded in `docs/PERFORMANCE-EXPERIMENTS.md` with the scale at which it could still
pay off.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: Gubichev and Neumann EDBT 2014, Leis et al. CIDR 2017, Cai et al. SIGMOD 2019, G-CARE VLDB 2022, LpBound 2025, Veldhuizen ICDT 2014, Umbra (Freitag et al. VLDB 2020), Free Join (SIGMOD 2023), RDFox, Kuzu, MillenniumDB, EmptyHeaded, GRAIL (Yildirim et al. VLDB 2010), PLL (Akiba et al. SIGMOD 2013), 2-hop labelling (Cohen et al.), Virtuoso transitive option, Virtuoso, QLever path search, Kabra and DeWitt 1998, Perron et al. ICDE 2019, IBM LEO, PostgreSQL AQO, Umbra, Moerkotte and Neumann SIGMOD 2008, Neumann and Radke SIGMOD 2018, Perez, Arenas and Gutierrez TODS 2009, Moerkotte and Neumann outer-join hyperedges and Olteanu and Schleich factorized databases. No code from another project is copied.

## Outcome

Nothing is built. [Item 454](../../ROADMAP.md) tracks the work.
