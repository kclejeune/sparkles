# BSBM Explore `q7` and `q8` and BI `q3` at 10M on forge

This run pairs Sparkles with Oxigraph on BSBM Explore `q7` and `q8`, and with QLever
0.5.48 on BI `q3`, over the 10M corpus of 10,095,035 triples. It compares Sparkles
`d65eaf40` with `47bec6f2`, which adds the probed OPTIONAL, comparisons decided on ids
and ORDER BY keys taken from a BIND. It also times every BI template with two sizes of
the query thread pool.

## Host and build

The host is `forge`, an Intel Core i5-13500 with 20 threads and 16 GiB of RAM, on kernel
7.2.9 with the `powersave` governor. Logical CPUs 0 to 11 are its six performance cores
with two threads each, and CPUs 12 to 19 are its eight efficiency cores. Each pass held
`~/lanes/.heavy.lock`, so no other build or benchmark ran during a load or a timed
request. Every directory's `preflight.txt` records the load and memory before the pass,
and `trim.txt` the `fstrim` that preceded it.

Both Sparkles binaries were built on forge in `nix develop` with
`mbx build --release --locked -p sparkles-server --bin sparkles` and a placeholder
`ui/build/index.html`. Their SHA-256 values are in `inputs.sha256` and in each record's
manifest. QLever and Oxigraph are the pinned Nix store paths in the manifests.

## Method

Every pass ran `scripts/bench-bsbm.sh 10m` with `SKIP_QMPH=1`, `WARMUP=10`, `RUNS=30`,
`CLIENT_CPU=11`, `SERVER_MEM_MAX=12G` and `SPARKLES_ALLOW_GPL_BENCHMARK=1`, once with
`USECASE=explore` and `QUERIES="explore-q7 explore-q8"` and once with `USECASE=bi`.
The first pass loaded every store, and the later passes reused them with `SKIP_LOAD=1`.
Each engine served alone during its answer check and its timings. Each template has
three fixed instances, so a pooled median covers ninety requests.

The `base` pass timed `d65eaf40` with Sparkles first. The `tip-first` pass timed
`47bec6f2` with Sparkles first, and the `tip-second` pass timed it with the other engine
first. In all three passes both engines return the same rows and value fingerprints for
every instance, as each phase's `answers.json` records.

## Result

| Template, pooled median ms | Sparkles `d65eaf40` | Sparkles `47bec6f2`, first | Sparkles `47bec6f2`, second | Reference, first | Reference, second |
|---|---:|---:|---:|---:|---:|
| Explore `q7`, Oxigraph | 5.21 | 3.78 | 3.83 | 4.09 | 4.03 |
| Explore `q8`, Oxigraph | 5.79 | 3.75 | 3.63 | 3.96 | 3.93 |
| BI `q3`, QLever | 41.40 | 31.49 | 33.03 | 28.95 | 29.46 |

The reference columns come from the `47bec6f2` passes. In the `base` pass, Oxigraph
took 4.11 and 3.91 ms and QLever 31.09 ms. Sparkles now leads Oxigraph on `q7` and `q8`
in both passes. On BI `q3`, QLever is 1.09 and 1.12 times faster. Sparkles leads on the
first and third instances and trails on the second, with a mean of 32.7
against 30.6 ms in the first pass.

## Thread pool size

The `threads-12` and `threads-20` passes timed `47bec6f2` alone on every BI template,
and on Explore `q7` and `q8`, with `RAYON_NUM_THREADS` set to 12 and 20. Twenty is the
default on this host, and twelve is the number of logical CPUs on its performance cores.

| BI template, pooled median ms | 12 threads | 20 threads |
|---|---:|---:|
| `q1` | 12.54 | 11.83 |
| `q2` | 17.35 | 17.90 |
| `q3` | 22.65 | 34.37 |
| `q4` | 146.13 | 150.35 |
| `q5` | 23.17 | 30.68 |
| `q6` | 15.96 | 16.20 |
| `q7` | 49.31 | 49.75 |
| `q8` | 32.55 | 29.10 |

Twelve threads make `q3` 34% faster and `q5` 24% faster, and `q8` 12% slower. The
other templates move by 6% or less, and Explore `q7` and `q8` stay between 3.68 and
3.91 ms. On atlas, a host with the same processor, the slower side of the `q3` plan's
hash join changes from one server process to another, and each side then takes twice
its usual time. This fits a sequential part of an operator that runs on an efficiency
core. The default pool size stays at the number of logical CPUs, because no single
size is faster on every template.

These two passes ran Sparkles alone in the results directory of the earlier passes. Their
records also hold the QLever and Oxigraph entries of the `tip-second` pass, which were
not measured again, and only their Sparkles entries belong to these passes.

## Files

`base/`, `tip-first/`, `tip-second/`, `threads-12/` and `threads-20/` each hold the
pass's `preflight.txt`, `inputs.sha256` and `trim.txt`, and an `explore/` and a `bi/`
directory with its `record.json` and `manifest.json`. The first three passes also keep
their hyperfine output, answer fingerprints and load and memory summaries. The records
are registered in `../../RUNS.json`.
