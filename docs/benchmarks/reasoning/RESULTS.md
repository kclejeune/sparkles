# LUBM and reasoning results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
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
operators it also ran out of a 28 GiB limit on a 32 GiB development host, wally. Where
that memory goes has not been profiled, and roadmap item 522 covers it. LUBM(100)
gains nothing, because its transitive relations are short and its time goes to the
other rules.

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
2,654 MiB that a closure keeping them needs. Changes of 1,000 triples stay within 1.2 times
their time with the larger closure, while changes of 100 triples at 16 threads and undos at
1 thread take 1.3 to 2.3 times as long. [Roadmap](../../ROADMAP.md) item 113 follows up those
slowdowns.

[F16's follow-ups](../../specs/F16-materialization-at-scale.md#roadmap-follow-ups-2026-10-18) give
the ranges, the other sizes and the 1-thread runs.

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
triples equal the reference's closures of the same data, and so does the closure after
inserting the change into OWL2Bench(1). Deleting the change from OWL2Bench(1) grew to
14.1 GiB in 136 s until the out-of-memory killer ended it, against less than 800 MiB for a
full run, and [roadmap](../../ROADMAP.md) item 798 covers it. Those memory figures come
from the development laptop and are not benchmark measurements.

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
