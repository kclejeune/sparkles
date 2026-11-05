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

The first phase of *correlation-aware cardinality estimation* is built, and the other
phases are not. [Item 454](../../ROADMAP.md) tracks the remaining work.

* **Subject sets with a remainder.** A bulk load or compaction keeps 32,768 subject
  characteristic sets instead of 10,000 and counts the subjects outside them as one
  remainder, with its subjects and, per predicate or class, its holders and triples. A
  star's estimate adds the remainder, in which the items are taken to be independent, so
  the 95% coverage rule applies only to statistics without a remainder.
* **Node sets.** The same builds count the characteristic sets of nodes over both
  directions, out-items, in-items and classes, 32,768 sets with a remainder, in one pass
  over SPO and OPS on the build's threads. A partial compaction updates both families for
  the nodes its changes touch, and freezes the kept signatures once a remainder holds
  anything. A variable that is the object of `?x p ?v` with a constant predicate other
  than `rdf:type` is estimated from the node sets for every join on it. When an input
  also restricts such a variable by a constant object, a subquery or a VALUES table, a
  join over its outgoing items alone is estimated from the subject sets, which take the
  restriction to be independent, and other joins keep the estimate from distinct values.
  This is the characteristic-pair idea reduced to the join node. Pairs keyed by both ends' sets are not built.
* **Same sets at any thread count.** The node pass keeps the same sets at any thread
  count, so a load of data without blank nodes writes the same `stats.json` at any
  thread count. A load numbers blank nodes in the order its parser threads meet them, so
  their identifiers in the statistics can differ between loads, as they did before.
  When the data has at most 2^20 distinct node signatures, the node pass counts
  them all exactly. Otherwise its first pass also fills a count-min sketch of the nodes
  per signature, and a second pass counts exactly the 2^20 signatures with the most nodes
  by the sketch, with ties broken by a stable hash of the signature. None of the
  benchmark stores has that many signatures, so none pays for the second pass.
* **Several keys.** A join on several keys divides by the key tuples of the larger side,
  from its distinct counts with exponents 1, 1/2, 1/4 and 1/8, at most its rows when each
  side has one input binding every key.
* **Zero hits in a key sample.** A sample that finds no rows, without covering every
  distinct value of the small input, keeps a share of `1 / (k + 2)` of the `k` values
  probed instead of an empty join.
* **Format.** The member `correlations` of `stats.json` has its own version, 1, the
  derived kind `planner-statistics`. A member of another version is refused with a
  recorded reason that `sparkles check` reports, and the planner estimates without it.
* **Switches.** `set_remainders`, `node_sets` and `key_backoff` turn the parts off.

The q-error of a plan node is `max(est, act) / min(est, act)`, both at least one row,
over the nodes that ran to completion. `scripts/bench-estimates/qerror.py` recorded every
join node of these workloads on the base build and on this change, on the laptop.
The SELECT answer fingerprints agree. The check did not read graph triples or quads,
so its matching fingerprints for the six BSBM Explore graph queries do not establish
RDF graph equality. This limitation leaves the recorded node estimates and timings
unchanged.

| Workload | Joins | Geometric mean | 90th percentile | Largest |
|---|---|---|---|---|
| Synthetic, 1.05M triples | 34 | 1.39 to 1.37 | 2.7 to 2.7 | 42 to 42 |
| WatDiv at scale 100 | 260 to 246 | 8.80 to 3.98 | 661 to 59 | 1,222 to 666 |
| BSBM explore, 10M | 136 to 135 | 1.89 to 1.86 | 5.6 to 5.6 | 18 to 18 |
| BSBM BI, 10M | 130 | 2.17 to 1.78 | 12.5 to 6.5 | 41 to 31 |
| LUBM(10) with OWL RL | 26 | 3.52 to 1.81 | 32 to 4.1 | 3,016 to 9 |
| DBpedia, 8.2M-triple slice | 22 | 4.84 to 1.96 | 78 to 7.7 | 1,478 to 78 |

The recorded plans of `plan_fidelity_tests.rs`, 21 queries over stars, object joins,
chains, a triangle, joins on two keys, a class variable, a subquery's variable and small
inputs, have no join whose q-error rises against the three switches off, and five fall.
The largest join q-error of a query falls on 54 of the 234 recorded queries and rises on
13, two of them by less than 2%. On the five WatDiv `F1` queries it rises from 88 to 103
at most, since the inputs of an empty join are now estimated
exactly and the empty join itself keeps the estimate from distinct values. On `F4-2` it
rises from 3 to 9 and on LUBM `q12` from 6.2 to 8.8, after plan changes that move a join
estimated from distinct values. On BSBM BI `q1-2` it rises from 4.1 to 6.8, and on two
DBpedia queries of six people from 2 and 2.5 to 6 and 5, where the remainder takes a
category's people to be as likely as others to have a label and a class.

The node pass takes 0.2 s of a 5 to 6 s WatDiv load and 0.7 s of a 10 to 20 s DBpedia
load on the laptop, and the peak memory of a load stays within 5% of the base build's.
`stats.json` grows from 2.5 MB to 16.3 MB on WatDiv and from 5.0 MB to 44.3 MB on DBpedia.
On atlas, an Intel host like forge, the DBpedia slice opens in 0.30 s instead of 0.07 s
and the server holds 214 MB instead of 133 MB once ready. WatDiv opens in 0.13 s instead
of 0.10 s and holds 156 MB instead of 127 MB. These are medians of five opens, polled
every 20 ms. Items 700, 704 and 705 would reduce these costs.

Both families keep 32,768 sets. Smaller caps, measured on the same stores, lose much of
the gain on WatDiv, where 16,384 sets raise the geometric mean of the join q-error by
10% and 8,192 by 19%. The 90th percentile is lower at 16,384 than at 32,768 because the
plans, and so the joins measured, differ.

| Workload | Sets | Geometric mean | 90th percentile | Largest | `stats.json` | Open | Memory once ready |
|---|---|---|---|---|---|---|---|
| WatDiv at scale 100 | Base build | 8.80 | 661 | 1,222 | 2.5 MB | 0.10 s | 127 MB |
| WatDiv at scale 100 | 8,192 | 4.72 | 49 | 666 | 4.4 MB | 0.07 s | 133 MB |
| WatDiv at scale 100 | 16,384 | 4.36 | 31 | 666 | 9.3 MB | 0.10 s | 144 MB |
| WatDiv at scale 100 | 32,768 | 3.98 | 59 | 666 | 16.3 MB | 0.13 s | 156 MB |
| DBpedia, 8.2M-triple slice | Base build | 4.84 | 78 | 1,478 | 5.0 MB | 0.07 s | 133 MB |
| DBpedia, 8.2M-triple slice | 8,192 | 3.36 | 15 | 78 | 7.1 MB | 0.07 s | 141 MB |
| DBpedia, 8.2M-triple slice | 16,384 | 2.06 | 6.0 | 78 | 16.7 MB | 0.14 s | 157 MB |
| DBpedia, 8.2M-triple slice | 32,768 | 1.96 | 7.7 | 78 | 44.3 MB | 0.30 s | 214 MB |

Against 32,768 sets, the largest join error of WatDiv `F1-2` rises from 17 to 49 at 8,192
sets, of `F4-2` from 9 to 58 and of DBpedia's `film-director-optional` from 1.0 to 15. At
16,384 sets that of WatDiv `F4-1` rises from 2.1 to 7.0 and of DBpedia's `births-by-decade`
from 1.0 to 2.1.

On forge at 12 threads, with interleaved rounds of fresh servers and matching SELECT
answer fingerprints, the geometric mean of the query times against main without these
estimates is 0.968 on WatDiv, 0.933 on BSBM BI, 0.946 on LUBM, 1.000 on BSBM explore, 1.028 on the synthetic
workload and 0.993 on DBpedia. This timing check also omitted RDF graph output from its
fingerprints. Its six BSBM Explore graph markers are unverified, as the
[manual run note](../../benchmarks/engine/runs/2026-11-03-estimation-ab-forge-e8e0e4cd/README.md)
explains. The plans of 41 queries change. WatDiv's `C` queries take
0.20 to 0.45 times as long, BSBM BI `q1-3` 0.23 times, LUBM `q2` 0.51 times and DBpedia's
`costar-birthplace` 0.63 times. WatDiv `S4-2` and `S4-4` take 1.6 times as long, since an
accurate estimate leads the planner to a 704-key index join that the cost model prices
too low. A cost term for keys spread at random brought their base plan back, but the 64
queries whose plans it changed took 1.15 times as long, since keys close together cost a
third of what spread keys cost and the planner cannot tell them apart. The
[experiment](../../PERFORMANCE-EXPERIMENTS.md#query-execution) is recorded, and item
701 prices probes by how their keys spread. Planning takes 1 to 90 microseconds
longer on WatDiv `F4` and on short synthetic, WatDiv and DBpedia queries, 5% to 10% of
their time, which item 702 follows up. Two synthetic queries with unchanged plans
run 1.06 and 1.11 times as long in execution in eight rounds, and item 703 follows
them up. Interleaved runs on atlas, an Intel host like forge, found the same slowdowns of
`S4-2`, `S4-4` and `F4`. The
[engine results](../../benchmarks/engine/RESULTS.md#estimation-timing-on-forge) list every
changed plan.

The first query of a fresh server over a store that holds the correlation statistics pays
for them once. On atlas at 1.05M triples, with one thread, the first run of the synthetic
`expr-bind-group` took a median of 1.19 ms of server time over five fresh servers,
against 0.98 ms without these estimates. Its plan was the same, it ran about the same instructions, and its later
runs took the same time. The three switches off left it unchanged, and a store without
the member did not show it. The server's history of quick texts compared that cold
cursor run with the eager run after it and kept most fresh servers eager for their whole
life, so the Namespace screen of the batch read the query at 1.465 of main. The history
now gives the slower mode a second run before a quick text settles, as the
[X06 outcome](../X06-benchmark-coverage.md#query-threads-on-hybrid-cores) describes, which
removes that effect. The cold cost of the first query stays, and item 708 looks for it.
The [synthetic results](../../benchmarks/synthetic/RESULTS.md#execution-mode-of-quick-texts-in-fresh-servers)
give the measurements.

Sampling of sources above 1,024 rows and of subplans, degree caps and full
characteristic pairs are deferred to items 698, 699 and 697. The
measurements above leave the far-end restrictions of a link and the overestimates of
empty joins as the largest remaining errors, which samples of large sources would not
change.
