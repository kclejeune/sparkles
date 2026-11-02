# Query threads on the hybrid cores of forge

This run measures where Sparkles' threads run on a processor with performance and
efficiency cores, for roadmap item 449. It covers BSBM BI at 10M and DBpedia's
`births-by-decade` on the 50M slice. It compares Sparkles `4bd70845` with `bf2b473c`,
which runs request threads on the performance cores, leaves one CPU free of query threads
for every four efficiency cores, maps expensive cursor batches over the query pool and
tries a slow automatic cursor eagerly once. The
[X06 Outcome](../../../../specs/X06-benchmark-coverage.md#query-threads-on-hybrid-cores)
describes the analysis.

## Host and build

The host is `forge`, an Intel Core i5-13500 with 20 threads and 16 GiB of RAM, on kernel
7.2.9. Logical CPUs 0 to 11 are its six performance cores with two threads each, and CPUs
12 to 19 are its eight efficiency cores. Every CPU ran the `powersave` governor with the
`balance_performance` energy preference, which each record's metadata and the
`preflight-*.txt` files record. Every measurement held `~/lanes/.heavy.lock`, and the final
measurements also held `~/lanes/.bench.lock`, so no build or other benchmark ran during a
timed request.

The base binary was built on wally from a `git archive` of `4bd70845` in `nix develop`
with `mbx build --release --locked -p sparkles-server --bin sparkles`. Its SHA-256 is
`7163daa09ebed4242050010f1085dc6ae301a7a00112e33096cb9109db32c2ee`. The final binary was
built the same way on the laptop from the lane's worktree at `bf2b473c`, with SHA-256
`bdb1ad85fd23bb46a2693f37f8cd2cd96ac773447c3fc99d196171abd95d74ea`. Both report rustc
1.99.1, GCC 15.3.0, clang 23.1.0 and LLD 23.1.1. The investigation's intermediate build of
`02d7e272` had SHA-256 `c8149d13d2ffc0a1f2b960d8f04f3cf320c5bb6895fdd05aebdfdc38938579e5`.
QLever is the pinned Nix store path of 0.5.48.

The BSBM data is the 10,095,035-triple corpus with SHA-256
`db77a8d62274566c0fcf6a85634741b0313551cc217b59f4b1f26d444fd205be`, the same as the
[Wave A run](../2026-10-31-wave-a-forge/README.md). The DBpedia slice is the 50M subject
slice of the [DBpedia reports](../../../dbpedia/RESULTS.md), loaded with
`scripts/bench-billion.sh load`.

## Method

`scripts/placement.py` starts one server process per binary and configuration in each
round, in rotated order with every second round reversed. A configuration names the
process's CPU affinity, its `RAYON_NUM_THREADS`, the CPUs that every thread but the query
pool's is pinned to after startup, and extra `serve` arguments. Each process answers every
BI instance and `births-by-decade` three times untimed and fifteen times timed, one
request at a time over a new connection, with result caches off. The runs of
`births-by-decade` alone time twenty requests. The client runs on CPU 19 unless
`CLIENT_CPU` says otherwise. The record keeps the client's wall time, the server's own
time from its access log, and the CPU time of every server thread by the kind of core it
last ran on and by whether it is a query pool thread, a runtime worker or another thread. `scripts/summarize.py` prints the pooled medians, the
medians of each process and the CPU split.

`scripts/exp10.sh` times `births-by-decade` with `execution=streaming` and
`execution=eager` in three rounds and prints the middle of the last twenty server times.
`scripts/births449.py` is the complete-HTTP timer of the
[births-by-decade run](../../../dbpedia/runs/2026-10-31-births-by-decade-forge/README.md)
for three engines: each round runs the base, the final build and QLever in rotated order,
each with three cold requests after evicting its store from the page cache and one warm
process of three untimed and twenty timed requests, with the client on CPU 11. The BSBM
passes ran `scripts/bench-bsbm.sh 10m` with `USECASE=bi`, `SKIP_LOAD=1`, `SKIP_QMPH=1`,
`WARMUP=10`, `RUNS=30`, `CLIENT_CPU=11` and `SERVER_MEM_MAX=12G`, as the Wave A run did.
`scripts/final.sh` ran the final measurements in one go.

## Investigation

The `investigation/` directories keep the records of the base binary's placement sweeps
and of the intermediate build, with each one's server-time summary.

| Directory | Configurations | What it shows |
|---|---|---|
| `exp1` | default, 12 threads, performance cores only, seven efficiency cores, 19 CPUs, six performance cores, 12 threads on 19 CPUs | `births-by-decade` is slow for whole processes on performance cores alone, so placement does not cause it. |
| `exp2` | default, 12 and 16 threads, performance cores only with 12 and 20 threads, seven efficiency cores and 19 CPUs, on `q3`, `q5` and `q8` | `q3` instance 3 takes 39.2 ms with 20 threads and 24.1 ms with 12 or 16, and its request thread uses more CPU. |
| `exp3` | 12, 14, 16 and 20 threads, and 16 and 20 threads with request threads pinned | Pinned request threads with 16 threads are fastest. Pinning with 20 threads and 16 threads without pinning are not kept. |
| `exp4` | 12, 14, 16 and 18 threads with pinned request threads | 16 and 18 threads lead. |
| `exp5` | the base and the intermediate build | The intermediate build removes the slow `births-by-decade` processes and keeps the `q3` gain, with 16 and 18 threads equal over all requests. |
| `exp6`, `exp8` | `births-by-decade` alone over twelve processes, with the client on CPU 19 and on CPU 11 | No slow process with the intermediate build. Eighteen threads are faster than sixteen with the client on a performance core. |
| `exp9` | `q1`, `q3` and `q6` with the client on CPU 11 | The `q3` and `q6` gains hold with the client on a performance core. |

`investigation/cursor-eager-wip1.txt` is the `exp10.sh` run of the intermediate build.

## Final measurements

`scripts/final.sh` ran on 2 November 2026 from 07:59 to 08:26 UTC with both locks held. Its
log is `final/final.log`, and each part's `preflight-*.txt` records the load, memory, disk
and governor before it started.

`final/placement` holds four rounds of the harness over every BI instance and
`births-by-decade`. Each round ran the base with its default twenty query threads, the
final build with its default placement and the final build with `--request-cpus all`.
The medians of the server's own time over every timed request of each template, in
milliseconds, were these.

| Template | Base | Final | Final with `--request-cpus all` |
|---|---:|---:|---:|
| `q1` | 11.07 | 9.66 | 10.82 |
| `q2` | 14.85 | 15.11 | 13.05 |
| `q3` | 30.19 | 18.44 | 26.60 |
| `q4` | 162.19 | 160.91 | 161.39 |
| `q5` | 20.29 | 18.81 | 18.68 |
| `q6` | 15.53 | 12.59 | 15.95 |
| `q7` | 45.43 | 43.19 | 43.72 |
| `q8` | 25.55 | 24.34 | 23.92 |
| `births-by-decade` | 5.06 | 3.76 | 4.42 |

The third `q3` instance took 23.1 to 24.1 ms in each final process and 32.7 to 47.6 ms in
each base process. The first took 7.1 to 8.5 ms against 17.3 to 19.1 ms. With
`--request-cpus all` the final build keeps the cursor changes, and the third `q3`
instance took 36.9 to 44.4 ms, so the gain comes from placement.

`final/births-processes` ran `births-by-decade` alone in twelve processes of each build
with the client on CPU 19. The base took a pooled median of 4.67 ms, with two processes at
15.4 and 15.5 ms. The final build took 3.62 ms, with every process between 2.94 and
4.14 ms. `final/cursor/summary.txt` gives the middle of the last twenty server times of
`births-by-decade` with `execution=streaming` and `execution=eager`. The base took 8.55 to
13.09 ms as a cursor and 2.12 to 2.24 ms eagerly, and the final build 3.16 to 3.27 ms and
1.78 to 1.84 ms.

`final/births/complete.json` is the complete-HTTP timer over twelve rounds. All three
engines returned the same 80 groups.

| Engine | Warm median | Warm mean | Cold median |
|---|---:|---:|---:|
| Sparkles `4bd70845` | 4.73 ms | 6.88 ms | 28.66 ms |
| Sparkles `bf2b473c` | 4.77 ms | 4.57 ms | 26.33 ms |
| QLever 0.5.48 | 16.78 ms | 16.14 ms | 35.14 ms |

Three base processes had warm medians of 13.16, 13.66 and 14.67 ms, and no final process
was above 5 ms. The warm medians of the two builds are equal because nine of twelve base
processes were fast, and the mean shows the slow ones.

`final/bsbm-base-first`, `final/bsbm-final-first` and `final/bsbm-final-second` are
BSBM BI passes with the published contract, one of the base and two of the final build,
each against QLever. The medians in milliseconds were these.

| Template | Base | QLever | Final, first | QLever | Final, second | QLever |
|---|---:|---:|---:|---:|---:|---:|
| `q1` | 12.40 | 50.48 | 12.17 | 49.13 | 12.15 | 49.92 |
| `q2` | 22.73 | 153.86 | 20.20 | 156.42 | 18.16 | 172.10 |
| `q3` | 31.46 | 29.18 | 25.41 | 27.62 | 24.25 | 27.49 |
| `q4` | 166.84 | 1,758.13 | 166.76 | 1,758.59 | 166.93 | 1,746.81 |
| `q5` | 30.98 | 95.25 | 25.45 | 95.15 | 30.18 | 95.77 |
| `q6` | 15.95 | 78.61 | 15.92 | 81.68 | 16.29 | 79.27 |
| `q7` | 50.17 | 48.34 | 50.97 | 54.06 | 49.07 | 52.73 |
| `q8` | 31.62 | 68.60 | 29.15 | 71.24 | 31.60 | 67.46 |

`q3` was 1.08 times slower than QLever with the base and 1.09 and 1.13 times faster with
the final build. Sparkles led QLever on every BI template in both final passes. Every
answer agreed with QLever's in all three passes, with `q4`, `q5` and `q8` compared under
the binary32 summation policy of `answer-validation.json`.

## Files

`scripts/` holds the harness as it ran. `investigation/*/records.json.gz` and
`final/*/records.json.gz` are the harness records compressed with gzip, unchanged
otherwise. `final/births/complete.json` is the record of the complete-HTTP timer, and
`final/bsbm-*/` holds each BSBM pass's results as `scripts/bench-bsbm.sh` wrote them,
without the raw answer bodies, whose hashes `answer-validation.json` keeps.
`final/cursor/` keeps the server logs of the cursor comparison.
