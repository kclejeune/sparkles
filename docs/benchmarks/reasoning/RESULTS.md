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
| OWL2Bench, 2 universities, `owl-rl` | 25.0 s | 1,892 MiB | 4,914,915 |
| OWL2Bench, 5 universities, `owl-rl` | killed at 28 GiB after 12 min 50 s | | |

*Measured on a Namespace instance at commit 0e0fa5b4.*

[F16's Outcome](../../specs/F16-materialization-at-scale.md#outcome) has the method and the
comparison with the reasoner that preceded it.

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
