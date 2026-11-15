# LUBM and reasoning results

This is a [manual report](../README.md#evidence-and-generated-reports). Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Comparison summary

Over the 24.3M triples of the OWL 2 RL closure, which Sparkles
  materializes in 17.8 s, paired `q2` means are 26.8 ms for Sparkles and 60.6 ms for
  QLever. The query guard preserves all fourteen answers.

## LUBM and reasoning

<a id="lubm100"></a>

### LUBM(100)

LUBM's generator, UBA 1.7, made 13,876,262 triples for 100 universities. Sparkles'
reasoner materialized the OWL 2 RL closure, 10,907,029 derived triples, in 17.8 s at a
peak RSS of 3,967 MiB, and every engine then loaded and queried the closure of 24,312,703
triples, so no query needs reasoning at query time. All 14 queries returned the same
answers on all five engines. Times are mean ± σ in ms. The paired Sparkles and QLever
`q2` cells combine sixty samples across both engine orders, with ten warm-ups before
each thirty-request process. The other cells use ten runs after two warm-ups.
`mise run bench:lubm 100` reproduces the run.

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| q1 | **3.5 ± 0.1** | 7.5 ± 0.8 | 7.0 ± 0.7 | 3.6 ± 0.2 | 959.2 ± 7.0 |
| q2 | **26.8 ± 3.5** | 1274.3 ± 27.3 | 60.6 ± 6.9 | 2856.2 ± 25.9 | 4295.6 ± 19.5 |
| q3 | **3.4 ± 0.1** | 6.9 ± 0.5 | 8.4 ± 1.0 | 3.7 ± 0.2 | 2388.5 ± 28.5 |
| q4 | **3.6 ± 0.1** | 7.4 ± 0.3 | 11.7 ± 1.3 | 4.0 ± 0.1 | 286.4 ± 20.1 |
| q5 | **3.7 ± 0.2** | 8.5 ± 0.5 | 7.5 ± 0.5 | 6.8 ± 0.2 | 3166.5 ± 17.2 |
| q6 | **72.7 ± 0.7** | 944.7 ± 56.6 | 647.1 ± 4.0 | 407.6 ± 9.7 | 1701.9 ± 11.3 |
| q7 | **3.7 ± 0.1** | 6.6 ± 0.4 | 11.9 ± 0.7 | 3.9 ± 0.1 | 4.8 ± 0.2 |
| q8 | **10.3 ± 0.2** | 67.9 ± 18.3 | 153.5 ± 7.2 | 33.3 ± 1.1 | 9527.7 ± 33.4 |
| q9 | **67.0 ± 5.0** | 4446.1 ± 82.9 | 221.6 ± 9.2 | 7273.9 ± 50.2 | 17660.2 ± 29.5 |
| q10 | **3.5 ± 0.1** | 6.0 ± 0.6 | 6.7 ± 0.2 | 4.3 ± 1.4 | 2975.5 ± 14.7 |
| q11 | **3.8 ± 0.1** | 7.4 ± 0.5 | 5.5 ± 0.3 | 4.6 ± 0.2 | 147.4 ± 6.2 |
| q12 | **3.7 ± 0.1** | 7.9 ± 0.4 | 7.8 ± 0.5 | 5.9 ± 0.2 | 30.5 ± 1.9 |
| q13 | **4.1 ± 0.2** | 8.4 ± 0.5 | 11.3 ± 0.6 | 5.9 ± 0.2 | 7.1 ± 1.2 |
| q14 | **58.1 ± 0.8** | 561.3 ± 64.9 | 494.5 ± 4.8 | 328.6 ± 12.2 | 1370.7 ± 29.2 |

*The Sparkles and QLever `q2` cells come from the
[secondary-probe record](../2026-10-25-lubm-secondary-probes-forge.json), with
Sparkles at `187f3170`. The [LUBM record](../2026-10-23-lubm-100-forge.json)
keeps the other cells, measured with Sparkles at `6699f27d`, and the materialization
costs. Both records use a closure with the same SHA-256.*

The secondary-key probe avoids enumerating incompatible students within each
university's range. An A/B/B/A comparison reduces the `q2` median by about 89%, and
Sparkles beats QLever in both peer orders. All fourteen guarded answers match the
published QLever fingerprints. The guard accepts small increases on `q10` and `q13`,
tracked under [roadmap](../../ROADMAP.md) item 291.

In the recorded cold runs, the median of 3 restarts with the engine's files
evicted, Sparkles is fastest on all 14 queries and answers 0.09 s after a restart.

Sparkles loads the closure in 7.57 s, against 18.96 s for QLever, 20.03 s for Fluree,
48.23 s for Oxigraph and 60.38 s for TDB2, into the smallest index, 236 MiB. Its load
peaks at 5,158 MiB, the highest after TDB2's 6,299 MiB, against QLever's 1,778 MiB. The
RDFS closure derives 5,527,675 triples in 11.3 to 12.7 s.

### Materialization at scale

These runs materialize closures that forge's 16 GiB cannot hold, so they ran on a
Namespace instance with 16 vCPUs and 32 GiB, each materialization alone in a cgroup of
28 GiB without swap, with `--max-inferred 0` and 16 Rayon threads. They are development
numbers, not forge numbers.

| Run | Time | Peak RSS | Derived triples |
|---|---:|---:|---:|
| LUBM(100), `rdfs` | 8.6 s | 3,125 MiB | |
| LUBM(1000), `rdfs`, 138,278,680 input triples | 90.9 s | 15,187 MiB | 55,069,413 |

*Measured on a Namespace instance at commit 0e0fa5b4.*

[F16's Outcome](../../specs/F16-materialization-at-scale.md#outcome) has the method and the
comparison with the reasoner that preceded it.

<a id="owl2bench"></a>

### OWL2Bench and closure operators

Closure operators evaluate transitive and equivalence rules
([F23](../../specs/F23-reasoner-core/SPEC.md#closure-operators)). The table compares
`sparkles infer --full --max-inferred 0` with the operators against the build before them,
which evaluates those rules as joins. Each run materialized a fresh copy of a store loaded
from the same N-Triples on forge with 20 threads, alone and in a cgroup of 14 GiB without
swap. Times include writing the inferred graph. The ranges cover three interleaved rounds,
and a run that failed was not repeated.

| Run | Joins | Joins, peak RSS | Operators | Operators, peak RSS | Derived triples |
|---|---:|---:|---:|---:|---:|
| OWL2Bench, 1 university, `owl-rl` | 5.30 to 5.61 s | 766 to 788 MiB | 1.31 to 1.39 s | 675 to 709 MiB | 1,400,688 |
| OWL2Bench, 2 universities, `owl-rl` | 53.2 to 54.1 s | 1,526 to 1,568 MiB | 5.01 to 5.34 s | 1,296 to 1,396 MiB | 4,915,635 |
| OWL2Bench, 5 universities, `owl-rl` | killed at 14 GiB after 13 min 38 s | | 43.8 to 44.9 s | 9.0 to 9.5 GiB | 52,966,139 |
| OWL2Bench, 10 universities, `owl-rl` | killed at 14 GiB after 34 s | | killed at 14 GiB after 15 s | | |
| LUBM(100), `owl-rl` | 16.1 to 16.2 s | 4,152 to 4,178 MiB | 15.5 to 15.6 s | 4,158 to 4,184 MiB | 10,907,379 |
| LUBM(100), `rdfs` | 11.9 to 12.1 s | 3,076 to 3,137 MiB | 11.8 to 12.1 s | 3,083 to 3,120 MiB | |

*Measured on forge with the base build at 04d49b57 and the operators at 29f54c39. The
[record](../2026-11-01-reasoning-closures-forge.json) keeps every run.*

The operators derive the same facts as joins. The derived counts above are those of the
operators' build, whose OWL 2 RL rule set also gained the missing rules of the Profiles
tables, which add 720 facts to each OWL2Bench run and 350 to LUBM(100). Within one build,
`examples/lubm.rs` derived the same facts, with the same order-free digest, with and
without the operators. OWL2Bench(10) exceeded the 14 GiB run limit with or without the operators, and with the
operators it also ran out of a 28 GiB limit on a 32 GiB development host, wally. The
profile of roadmap items 835 and 865 found where the memory of the 5-university run goes
and bounded it, as the next paragraph records, and item 522 covers the 10-university run. LUBM(100)
gains nothing, because its transitive relations are short and its time goes to the
other rules.

On wally, a 20-thread development host with 32 GiB, `sparkles infer --full --profile
owl-rl --max-inferred 2e8` on OWL2Bench with 5 universities, two interleaved rounds per
thread count against the build before the columnar derived tier (88538906), derived
52,966,139 facts in six iterations with every build. The derived tier at 2ced03ad held
them in 2,137 MB, 40 bytes per fact with the triple index, and the reasoner's budget
peaked at 10.5 GB, 199 bytes per fact, in the closure operators' first step. With the
operators' batched merges at f6264c44 the budget peaked at 4.2 GB, 80 bytes per fact, and
the tier ended at 2,467 MB. The whole command's peak resident set stayed between 9.4 and
10.9 GiB for every build, since the commit of the derived triples to the store follows
the reasoning. Wall times at one thread were 70 and 67 s before the tier, 89 and 84 s with
it and 89 and 85 s with the batches, and at 20 threads 38 and 34 s, 41 and 42 s, and 41
and 38 s. With item 866's row of 24 bytes, 16-byte sort entries and digit sort, in two
further rounds against the same base build, the wall times were 81 and 78 s against 75 and
69 s at one thread and 39.0 and 37.6 s against 40.7 and 37.3 s at 20 threads, with the
budget's peak at 4.6 GB, 87 bytes per fact, and the tier at 2,745 MB. LUBM(100) under
`owl-rl` took 25.7 and 25.2 s against 24.8 and 24.7 s at one thread, 14.7 and 14.6 s
against 14.3 and 14.1 s at 20 threads, and the insertion and deletion of every hundredth
of its triples at 20 threads 2.2 and 3.0 s against 2.1 and 2.9 s.

With the grouped head probes and exact distinct-key counts of item 866 at 75e7fb61, on
atlas, an Intel Core i5-13500 like the benchmark host, in four interleaved rounds against
the build before the tier with one thread pinned to one core or 20 threads, OWL2Bench(5)
took 100.8 to 105.4 s against 102.2 to 107.4 s at one thread and 50.2 to 52.7 s against
50.8 to 54.2 s at 20 threads. The tier before the grouped probes had taken 114.7 to
116.0 s at one thread on the same host, and the rule `prp-symp`, whose probes of the
triple index made most of that difference, took 7.0 s against 9.8 s. LUBM(100) under
`owl-rl` took 37.5 to 38.0 s against 37.3 to 37.8 s at one thread and 16.1 to 16.7 s
against 16.5 to 17.1 s at 20 threads. The insertion of every hundredth of its triples at
20 threads took 2.43 to 2.46 s against 2.35 to 2.41 s and the deletion 3.30 to 3.34 s
against 3.23 to 3.27 s, about 2% slower in every round, and the one-thread run 0.5%
slower in every round, which roadmap item 938 investigates. The
[run directory](runs/2026-11-13-item-866-atlas/run.sh.txt) holds the commands, the
[host and builds](runs/2026-11-13-item-866-atlas/env.txt), the
[timings](runs/2026-11-13-item-866-atlas/results.txt), the
[trace lines](runs/2026-11-13-item-866-atlas/trace-lines.txt) with each run's derived count,
and the [earlier rounds](runs/2026-11-13-item-866-atlas/earlier-rounds.txt) of the tier
before the grouped probes. Both builds derived 52,966,139 and 10,907,379 facts, the budget's peak on
OWL2Bench(5) stayed at 4.6 GB and the tier ended at 2,746 MB. These are benchmark-worker
measurements, and the benchmark host has not rerun them.

### Sampled rule-body planning

Roadmap item 939 orders rule bodies from sampled bindings. On atlas, an Intel Core
i5-13500 like the benchmark host, one build of `examples/lubm.rs` at `a63916c0` ran each
workload with the greedy orders and with the sampled orders in interleaved rounds, with
one thread pinned to one core or 20 threads. The rules' time is the time of `infer`,
which derives the closure in memory, and the materialization's is that of `materialize`
on a fresh copy of the database.

| Workload | Threads | Greedy orders | Sampled orders | Pairs |
|---|---|---|---|---|
| OWL2Bench(5), rules | 1 | 51.6 to 51.8 s | 29.9 s | 3 |
| OWL2Bench(5), rules | 20 | 12.5 to 12.8 s | 9.5 to 9.7 s | 5 |
| OWL2Bench(5), materialization | 1 | 102.5 to 102.8 s | 80.6 to 80.8 s | 2 |
| OWL2Bench(5), materialization | 20 | 45.5 to 49.2 s | 42.5 to 42.7 s | 3 |
| OWL2Bench(1), rules | 1 | 976 to 988 ms | 662 to 696 ms | 7 |
| OWL2Bench(1), rules | 20 | 403 to 451 ms | 319 to 355 ms | 7 |
| LUBM(100), rules | 1 | 14.50 to 14.55 s | 14.51 to 14.55 s | 6 |
| LUBM(100), rules | 20 | 2.85 to 2.91 s | 2.82 to 2.89 s | 6 |
| LUBM(100), materialization | 1 | 40.7 to 41.3 s | 40.7 to 40.8 s | 3 |
| LUBM(100), materialization | 20 | 18.4 to 18.8 s | 18.5 to 18.7 s | 3 |
| LUBM(10), rules | 1 | 926 to 933 ms | 930 to 970 ms | 7 |
| LUBM(10), rules | 20 | 236 to 291 ms | 213 to 317 ms | 7 |
| LUBM(100), long-body user rules | 1 | 5.69 to 5.74 s | 5.68 to 5.70 s | 4 |
| LUBM(100), long-body user rules | 20 | 1.08 to 1.11 s | 1.08 to 1.14 s | 5 |
| LUBM(10), long-body user rules | 1 | 375 to 383 ms | 376 to 417 ms | 7 |
| LUBM(10), long-body user rules | 20 | 147 to 169 ms | 152 to 176 ms | 7 |

Every pair of `infer` runs derived the same facts, with equal digests. Under OWL 2 RL
they are 52,966,139 facts on OWL2Bench(5), 1,400,688 on OWL2Bench(1), 10,907,379 on
LUBM(100) and 1,038,100 on LUBM(10), and the long-body rules derive 416,960 on LUBM(100)
and 39,667 on LUBM(10). `materialize` prints no digest, and every pair of its runs
inferred the same number of facts. The planner counts the plans that take a sampled
order. On the same databases that count is zero in every iteration of the four LUBM
workloads and four on OWL2Bench(1), as the
[plan counts](runs/2026-11-14-item-939-atlas/plan-counts.txt) record, and `prp-key` is
the rule that reads a key's value before it joins the keyed class with itself. The
difference on LUBM is therefore taken to be the cost of planning. On LUBM(10) at one
thread six of the seven sampled runs took 930 to 936 ms and one took 970 ms. The median
of the seven paired differences is 4 ms and their mean 9 ms, and roadmap item 945 covers
this cost. The highest peak heap of the sampled runs of a workload is within 3.1 MiB of
that of the greedy runs under OWL 2 RL, where single pairs at 20 threads differ by up to
13.7 MiB. With the long-body rules it is 805.9 against 790.6 MiB on LUBM(100) at one
thread, 804.7 against 791.2 MiB at 20 threads, where one pair differs by 19.6 MiB, and
95.6 against 90.2 MiB on LUBM(10) at 20 threads. The
[run directory](runs/2026-11-14-item-939-atlas/run.sh.txt) holds the commands, the
[host and build](runs/2026-11-14-item-939-atlas/env.txt), the
[rule file](runs/2026-11-14-item-939-atlas/long-bodies.rules), the timings of the
[first](runs/2026-11-14-item-939-atlas/results-part1.txt) and the
[second](runs/2026-11-14-item-939-atlas/results-part2.txt) part of the run and their
[summary](runs/2026-11-14-item-939-atlas/summary.txt). These are benchmark-worker
measurements of `a63916c0`. The item's later commits, which make the planner keep the
greedy order in more cases and bound its reads, and the benchmark host have not repeated
them.

### Small incremental runs

Each run materializes the closure, then changes 1,000 triples, undoes the change and makes
it again, each time from the closure kept in memory. The data is LUBM-style data with 100
universities from the generator of `examples/lubm.rs`, 13.1 million triples in index format
3, not the UBA data above. The table gives the medians of six interleaved rounds at 16
threads, each run alone on a fresh copy of the dataset.

| Run, 16 threads | Change | Undo | Change again | Full run | Kept closure |
|---|---:|---:|---:|---:|---:|
| RDFS | 18 ms | 13.5 ms | 12 ms | 14.7 s | 927 MiB |
| OWL 2 RL | 40.5 ms | 44.5 ms | 25 ms | 20.8 s | 1,244 MiB |

*Measured on forge on 2026-10-23 at commit 77789822. The kept closure sizes come from the
format 2 dataset at commit 534a7ad3.*

The kept closure leaves out the explicit facts, so it needs 41% and 47% of the 2,267 and
2,654 MiB that a closure keeping them needs.

`churn` of `examples/lubm.rs` makes many changes over one kept closure. Each change goes to a university that no
earlier change touched and is followed by its undo, the change again and a second undo. A process makes 40 changes of 100 triples, 20 of 1,000 and 6 of 10,000 on the
same dataset. The table gives the medians of three interleaved rounds, each process alone
on a fresh copy of the dataset, at one thread and on a reasoning pool of 16 threads.

| Run | Change | Undo | Change again | Second undo |
|---|---:|---:|---:|---:|
| RDFS, 100 triples, 1 thread | 8.0 ms | 2.7 ms | 2.3 ms | 2.1 ms |
| RDFS, 1,000 triples, 1 thread | 19.4 ms | 14.0 ms | 12.7 ms | 11.7 ms |
| RDFS, 10,000 triples, 1 thread | 130 ms | 127 ms | 106 ms | 105 ms |
| OWL 2 RL, 100 triples, 1 thread | 12.0 ms | 10.7 ms | 6.2 ms | 8.7 ms |
| OWL 2 RL, 1,000 triples, 1 thread | 48.6 ms | 83.3 ms | 37.2 ms | 69.3 ms |
| OWL 2 RL, 10,000 triples, 1 thread | 379 ms | 957 ms | 366 ms | 823 ms |
| RDFS, 100 triples, pool of 16 | 5.5 ms | 2.8 ms | 2.1 ms | 2.1 ms |
| RDFS, 1,000 triples, pool of 16 | 11.4 ms | 10.6 ms | 9.5 ms | 9.9 ms |
| RDFS, 10,000 triples, pool of 16 | 79.3 ms | 88.0 ms | 77.4 ms | 78.0 ms |
| OWL 2 RL, 100 triples, pool of 16 | 8.8 ms | 11.3 ms | 5.8 ms | 10.2 ms |
| OWL 2 RL, 1,000 triples, pool of 16 | 27.2 ms | 53.4 ms | 24.3 ms | 48.4 ms |
| OWL 2 RL, 10,000 triples, pool of 16 | 210 ms | 411 ms | 200 ms | 367 ms |

*Measured on atlas, a benchmark worker with the benchmark host's hardware, on 2026-11-14
at commit 1590064b. The benchmark host has not rerun these.*

On this data and host, RDFS undos and repeated changes take 0.85 to 1.08 times their time
with the larger closure at one thread and 0.67 to 1.05 times on the pool. OWL 2 RL changes take 1.2 to 2.3 times as
long at one thread and 0.9 to 1.7 times on the pool, which [roadmap](../../ROADMAP.md)
item 941 follows up. The first change of 100 RDFS triples to a university takes 2.6 times
as long at one thread, which is item 942. The
[run directory](runs/2026-11-14-item-113-atlas/run.sh.txt) holds the commands, the
[host and builds](runs/2026-11-14-item-113-atlas/env.txt), the
[medians with their ranges](runs/2026-11-14-item-113-atlas/results.txt) and
[every step's quartiles](runs/2026-11-14-item-113-atlas/steps.txt).

The reasoner's maintenance runs as a program of the fixed-point operator of incremental
views (roadmap item 837). On the same data and host, the build with the operator against
the build before it gives the same inferred counts after every step. Its full runs take
0.99 to 1.01 times as long. Its changes, undos and repeated changes take 0.99 to 1.00
times as long by the geometric mean of each profile at one thread and on the pool, and
every such step is within 1.06 times with ranges that overlap the earlier build's. The
first change of each size after the closure is kept, one sample per process, takes 1.05
times as long by the geometric mean of the OWL 2 RL steps on the pool, within ranges that
overlap, which item 964 follows up. Changes of 100,000 triples to the closure of OWL 2 RL
at 20 threads, read back from the dataset when undone, take 17.9 to 18.8 s against 17.8 to
18.0 s and undo in 5.8 to 5.9 s in both builds. The
[run directory](runs/2026-11-14-item-837-atlas/run.sh.txt) holds the commands, the
[host and builds](runs/2026-11-14-item-837-atlas/env.txt), the
[medians with their ranges](runs/2026-11-14-item-837-atlas/results.txt) and the
[large changes](runs/2026-11-14-item-837-atlas/keep-938.txt).

[F16's follow-ups](../../specs/F16-materialization-at-scale.md#roadmap-follow-ups-2026-10-18) give
the ranges, the other sizes and the 1-thread runs.

### The rule plug-in against the reasoner's maintenance

`plugin` of `examples/lubm.rs` runs the RDFS rules without axioms or builtins two ways over
the same dataset, through the reasoner with a kept closure and through `RuleProgram` on the
fixed-point operator, which keeps a record for every fact and deletes by its one-step
backward check. Each round removes `K` triples of a university and adds `K` new ones, then
undoes the change. The reasoner's time includes the commit of its inferred graph, and the
operator's includes the read of the change. Both closures had the same size after every
step. The table gives the medians over the rounds.

| Data | Change, reasoner | Change, operator | Undo, reasoner | Undo, operator |
|---|---:|---:|---:|---:|
| LUBM(1), 100 triples, 6 rounds | 31.8 ms | 133 ms | 23.0 ms | 347 ms |
| LUBM(1), 1,000 triples, 6 rounds | 11.3 ms | 677 ms | 14.4 ms | 1,526 ms |
| LUBM(10), 100 triples, 10 rounds | 4.7 ms | 705 ms | 4.6 ms | 1,325 ms |
| LUBM(10), 1,000 triples, 10 rounds | 10.9 ms | 7,136 ms | 9.6 ms | 15,063 ms |

*Measured on stanley, a development laptop shared with other builds, on 2026-11-14 at
commit 4a5bcac9. The reasoner's small runs vary by several times between processes on
this host.*

The operator is 4 to 1,600 times slower, and the undo, which deletes the added triples'
consequences through records, costs it about twice its change. The reasoner keeps 57 to
82 bytes per derived fact. The operator holds the explicit triples beside the derived
ones, 1.63 million facts for 339,224 derived on LUBM(10), at 206 to 266 bytes per fact,
or about 1,000 bytes per derived fact. The reasoner's maintenance therefore moves onto
the operator as an exact program over its own derived tier and evaluation, not through
`RuleProgram`. The [run directory](runs/2026-11-14-item-547-stanley/run.sh.txt) holds the
commands, the [host and build](runs/2026-11-14-item-547-stanley/env.txt) and each run's
output, such as [LUBM(10) with 1,000 triples](runs/2026-11-14-item-547-stanley/l10-k1000.txt).

### Reasoner witnesses

The witnesses of item 416 are off by default
([F18](../../specs/F18-explanations-and-entailment-diffs.md#reasoner-witnesses-item-416)).
These runs used `examples/lubm.rs` on an 8-core AMD EPYC Namespace instance with 16 GB of
memory, which gives approximate numbers rather than published ones. The base build is
0dc27215 and the witness build is 25f927df, with the same compiler. Each run materialized
a fresh copy of the dataset with 8 threads, and the LUBM-style data comes from the
generator of `examples/lubm.rs`. The first table gives the medians of ten interleaved
rounds with witnesses off.

| Run, witnesses off | Base | Witness build | Ratio | Derived triples |
|---|---:|---:|---:|---:|
| LUBM, 10 universities, `rdfs` | 1,574 ms | 1,594 ms | 1.013 | 565,906 |
| LUBM, 10 universities, `owl-rl` | 1,286 ms | 1,350 ms | 1.050 | 1,233,449 |
| OWL2Bench, 1 university, `owl-rl` | 866 ms | 927 ms | 1.070 | 1,400,688 |
| OWL2Bench, 2 universities, `owl-rl` | 3,338 ms | 3,327 ms | 0.997 | 4,915,635 |

Both builds derived the same facts with the same peak heap, 390, 268 and 992 MiB. The
means of the same runs differ by 3% or less, and an earlier instance with five rounds
measured 1.015, 1.064, 1.082 and 0.933. The path without witnesses only checks an
option, so these differences are not explained by the work it does, and roadmap item
554 checks them on an Intel host.

The second table gives single runs of the witness build with a bound of 8, each limited
to 11 GB of address space.

| Run, bound 8 | Run | Witnesses | Facts | Truncated | Kept witnesses | Witness heap | Peak heap |
|---|---:|---:|---:|---:|---:|---:|---:|
| LUBM, 1 university, `rdfs` | 1.2 s | 1.1 s | 74,203 | 28,429 | 246,063 | 58 MiB | 238 MiB |
| LUBM, 1 university, `owl-rl` | 5.2 s | 5.0 s | 132,492 | 89,285 | 729,859 | 190 MiB | 458 MiB |
| LUBM, 10 universities, `rdfs` | 15.9 s | 13.5 s | 677,738 | 248,201 | 2,139,973 | 574 MiB | 1,416 MiB |
| LUBM, 10 universities, `owl-rl` | 51.1 s | 48.8 s | 1,233,449 | 828,320 | 6,763,141 | 1,753 MiB | 3,286 MiB |
| OWL2Bench, 1 university, `owl-rl` | ran out of memory | | | | | | |

The facts include the explicit facts that a rule can derive, which count as their own
witness. Most of the time goes to finding each fact's rule instances and multiplying
their witnesses, and many OWL 2 RL facts are truncated because one of their premises is.
Roadmap item 555 covers the time and item 556 the memory.

The incremental runs change 1,000 triples of the LUBM data with 10 universities under
`rdfs`, undo the change and make it again, each time from the closure kept in memory.

| Run, `rdfs` | Without witnesses | With witnesses, bound 8 |
|---|---:|---:|
| Full run | 1.5 s | 14.6 s |
| Settle after reading the closure back | 0.23 s | 12.0 s |
| Change | 12 ms | 5.3 s |
| Undo | 10 ms | 4.5 s |
| Change again | 8 ms | 6.6 s |
| Kept closure | 83 MiB | 849 MiB |

The reasoning latency harness of `mise run bench:reasoning-latency` ran on the same
instance with 12 threads, five rounds and three repetitions, first for the base build and
then for the witness build. Query-time reasoning does not use witnesses. The witness build
measured a geometric mean of 0.938 of the base over the 100 timings, with every answer
check passing. Five timings were 10% or more slower, all of them queries under 0.6 ms.
They are `owl-rl` materialized `q4`, `q5` and `q12` at 0.08 to 0.12 ms, `rdfs` on read
`q13` at 0.08 ms and `rdfs` demand-cached `q7` at 0.56 ms against 0.50 ms. Roadmap item
554 rechecks them on an Intel host.

A change evaluated about 230,000 facts again, a third of the closure, because it reaches
many truncated facts. Reading the closure back from the dataset builds the witnesses
again, since they are not stored. The
[run directory](runs/2026-11-01-witnesses-nsc/run.sh.txt) holds the commands, the
[host](runs/2026-11-01-witnesses-nsc/env.txt), every
[materialization line](runs/2026-11-01-witnesses-nsc/mat.txt) and the latency lines of the
[base](runs/2026-11-01-witnesses-nsc/latency-base.jsonl) and
[witness](runs/2026-11-01-witnesses-nsc/latency-witnesses.jsonl) builds.

### OWL 2 RL reference agreement

The [OWL 2 RL reference](README.md#the-owl-2-rl-reference) runs the OWL 2 RL/RDF rules on
Nemo 0.10.1. Each row compares the triples that `sparkles infer --profile owl-rl`
derived with those of the reference on the same stored data. The aligned program adds
Sparkles' RDFS and OWL vocabulary additions to the W3C rules, and its derived graph must
be equal. The W3C program alone must derive nothing that Sparkles lacks.

| Input | Base triples | Derived by Sparkles | Aligned difference | Derived by the W3C program | Missing from Sparkles |
|---|---:|---:|---:|---:|---:|
| LUBM(1) | 100,836 | 85,098 | 0 | 67,268 | 0 |
| OWL2Bench(1) | 54,815 | 1,400,688 | 0 | 1,395,689 | 0 |
| 66 W3C OWL 2 RL test cases | | 28,004 | 0 | 3,946 | 0 |

*Checked on stanley, a development laptop, with Sparkles built at 233f0362. The
[run directory](runs/2026-11-09-rl-reference-agreement-stanley/commands.txt) holds the commands, the [host](runs/2026-11-09-rl-reference-agreement-stanley/env.txt) and
each comparison's counts.*

The test cases are those of the approved W3C OWL 2 test cases that list the RL profile and
have an RDF/XML premise and no imports, 66 of 70. The closures that incremental
maintenance left after inserting and after deleting a change of 1% of LUBM(1)'s data
triples equal the reference's closures of the same data, and so do the closures after
inserting the change into OWL2Bench(1) and after deleting it. The deletion passes the
bound on incremental maintenance, because its `hasSameHomeTownWith` closure is a clique of
about 1,100 people, and the run materializes in full. In three rounds on 16 threads it
took 1.4 to 3.4 s at a peak RSS of 808 to 866 MiB, against 1.7 to 4.0 s and 556 to
641 MiB for a full run. The [deletion record](runs/2026-11-09-item-798-delete-stanley/commands.txt)
holds the runs. They come from the development laptop while it ran other builds, and are
not benchmark measurements.

## Query-time reasoning latency

`mise run bench:reasoning-latency` is the rewrite gate of derivation on demand and RDFS
on read, as [BENCHMARKING.md](../../BENCHMARKING.md#derivation-on-demand-and-rdfs-on-read)
describes. Its baseline ran on forge alone at commit 53220ef6, whose engine code is that
of 04d49b57, with 12 Rayon threads. The data was 10 LUBM-style universities, about 1.3
million asserted triples. The rdfs profile inferred 565,906 triples in 2.3 s and owl-rl
1,233,089 in 1.7 s. Every timed run of every mode returned the materialized answer of its
profile to all 14 queries. Each figure is the median of 15 timed runs in milliseconds, from five rounds that
visit the modes in a rotated order.

| Query | rdfs materialized | rdfs demand | rdfs demand, cached | RDFS on read | owl-rl materialized | owl-rl demand | owl-rl demand, cached |
|---|--:|--:|--:|--:|--:|--:|--:|
| q1 | 0.064 | 42.6 | 0.331 | 0.380 | 0.042 | 2,017 | 1,517 |
| q2 | 0.633 | 49.5 | 3.87 | 14.4 | 3.01 | 2,212 | 2,989 |
| q3 | 0.060 | 43.7 | 0.394 | 2.50 | 0.053 | 1,995 | 1,535 |
| q4 | 0.164 | 44.1 | 1.35 | 0.653 | 0.139 | 2,060 | 2,549 |
| q5 | 0.142 | 48.6 | 1.42 | 9.09 | 0.130 | 2,058 | 1,556 |
| q6 | 0.915 | 106 | 51.5 | 1.63 | 1.19 | 2,344 | 1,204 |
| q7 | 0.116 | 44.3 | 0.644 | 2.63 | 0.093 | 2,012 | 2,173 |
| q8 | 2.67 | 127 | 76.9 | 12.2 | 2.96 | 2,043 | 2,599 |
| q9 | 7.84 | 96.4 | 49.6 | 7.28 | 8.60 | 2,565 | 2,986 |
| q10 | 0.063 | 42.1 | 0.424 | 1.70 | 0.058 | 2,029 | 1,537 |
| q11 | 0.061 | 43.3 | 0.360 | 0.109 | 0.098 | 2,005 | 1,544 |
| q12 | 0.059 | 43.6 | 0.740 | 0.167 | 0.168 | 2,049 | 2,247 |
| q13 | 0.039 | 41.1 | 0.300 | 0.109 | 0.081 | 2,025 | 1,549 |
| q14 | 0.929 | 41.6 | 0.456 | 1.34 | 0.905 | 2,244 | 1,203 |

Derivation on demand with the rdfs profile costs about 44 ms per query when it makes its
tables again, and a few tenths of a millisecond to tens of milliseconds with them cached.
RDFS on read stays within a few milliseconds except on the queries that read broad
classes, `q2`, `q5` and `q8`. Under owl-rl every query takes about two seconds on demand,
including `q1`, which needs no inference. The cached mode does not help. The
process-wide table cache, whose default capacity is 256 MiB, filled to 266 MB at this
size, and it missed about 85 times in every round, so tables were evicted between
queries. These times are far
above the 2.5 ms that spec F17 recorded for `q1` at the same size on an nsc instance at
d80c0ce7, before the schema-completeness changes. [Roadmap](../../ROADMAP.md) item
Item 532 investigates the per-query cost. A server answering `reasoning=demand` on a
smaller copy of this data took more than a second for `q1` as well, so the cost is not an
artifact of the harness.

The [run record](runs/2026-11-01-reasoning-latency-forge-53220ef6/record.json) holds every sample,
the answer fingerprints, the settings, the compilers and the build configuration, with the
[raw lines](runs/2026-11-01-reasoning-latency-forge-53220ef6/metrics.jsonl), the
[log](runs/2026-11-01-reasoning-latency-forge-53220ef6/output.log) and the
[commands](runs/2026-11-01-reasoning-latency-forge-53220ef6/run.sh.txt). Item 190's
equality-stage fixtures are not part of this benchmark, and item 533 adds them.

### Reuse of tables across commits

The `table_reuse` example of the reasoner measures the first `reasoning=demand` query
after a commit, with and without the reuse of tables across commits that spec F17 §6.2
describes. It ran on atlas, which has the benchmark host's hardware, with nothing else
running and 20 Rayon threads. The data was 10 LUBM-style universities. Before each timed
query the example commits one triple, either a literal of a predicate the rules do not
read, the unrelated kind, or the type of a new graduate student, the relevant kind. Every
answer matched the same state read through a view that no cache serves. The build before
reuse is commit 65c570db and the build with it is 84dc3c11. Four runs of five rounds
alternated the builds, the earlier build first and last. Each figure is the mean of the
two runs' medians for a build, in milliseconds, for the `owl-rl` profile.

| Query | unrelated, before | unrelated, reuse | tables read | relevant, before | relevant, reuse |
|---|--:|--:|--:|--:|--:|
| q1 | 229 | 64.9 | 2 of 10 | 226 | 225 |
| q2 | 391 | 146 | 6 of 30 | 374 | 386 |
| q3 | 243 | 2.63 | 2 of 10 | 249 | 248 |
| q4 | 240 | 10.5 | 5 of 25 | 250 | 258 |
| q5 | 327 | 52.8 | 2 of 10 | 306 | 321 |
| q6 | 2,259 | 1,785 | 1 of 5 | 2,263 | 2,199 |
| q7 | 233 | 9.71 | 4 of 20 | 232 | 226 |
| q8 | 2,318 | 731 | 9 of 25 | 2,325 | 2,327 |
| q9 | 1,052 | 755 | 6 of 30 | 1,040 | 1,038 |
| q10 | 226 | 6.07 | 6 of 10 | 237 | 234 |
| q11 | 265 | 2.23 | 6 of 10 | 257 | 257 |
| q12 | 333 | 87.7 | 12 of 20 | 366 | 343 |
| q13 | 253 | 25.2 | 2 of 10 | 254 | 260 |
| q14 | 240 | 0.63 | 5 of 5 | 250 | 236 |

After an unrelated commit, the session reads the schema's table and the completeness
proof of the anchor's commit, and the query's own tables where the anchor's view already
holds them, which the tables column counts over the five rounds. That takes most queries
from about 250 ms to between 0.6 and 90 ms. Queries 2, 6, 8 and 9, whose own derivations
dominate, gain less. After a relevant commit every table is derived again, and each query
took between 6% less and 5% more time than before reuse. A build that timed the reuse
steps put the anchor lookup and the walk over the differing quads at 8 µs on average per
session, specialization's extra work below a millisecond and anchoring below 0.1 ms. The
`rdfs` profile reads every fact through `rdfs4`, so it never reuses, and its queries were
within 6% of the earlier build except `q10`, whose times spread from 47 to 77 ms in both
builds. The [run record](runs/2026-11-14-item-19-table-reuse-atlas/env.txt) holds the
four logs, the build identities and the commands.
