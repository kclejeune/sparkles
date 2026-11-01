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
