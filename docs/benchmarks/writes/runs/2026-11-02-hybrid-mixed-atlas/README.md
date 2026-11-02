# Request placement under mixed load on atlas

This run measures where Sparkles runs request work on a hybrid processor while readers
and a writer share the server, for roadmap item 583. The
[X06 section](../../../../specs/X06-benchmark-coverage.md#request-placement-under-concurrent-load)
describes the analysis and the change.

## Host and builds

The host is `atlas`, an Intel Core i5-13500 with 20 threads and 16 GiB of RAM, the same
processor as `forge`. Logical CPUs 0 to 11 are its six performance cores with two threads
each, and CPUs 12 to 19 are its eight efficiency cores. Every CPU ran the `powersave`
governor with the `balance_performance` energy preference. Every measurement held
`~/lanes/.heavy.lock` and `~/lanes/.bench.lock`, so no build or other benchmark that takes
the locks ran at the same time. The final measurements also waited until no compiler,
test runner or test binary ran on the host and the one-minute load was under 2, since
other work that does not take the locks ran on atlas that day. A pass that such work
overlapped was stopped and run again.

| Directory | Commit | Built | What it holds |
|---|---|---|---|
| `b62d3722` | `b62d3722`, item 449 | on atlas | The diagnosis of the slowdown. |
| `9449f2c4` | `9449f2c4`, an experiment build | on wally | The placement and update experiments, and the load sweep. |
| `2f872143` | `2f872143`, the first fix | on the laptop | The first acceptance, which found the inherited threads. |
| `f167fc8f` | `f167fc8f`, the final placement | on atlas | The final acceptance, with `p449` the `9449f2c4` build in its default placement, which is item 449's. |

Builds on atlas and wally ran `mbx build --release -p sparkles-server --bin sparkles` in
`nix develop`, and the laptop build ran the same command without it. The binaries have
these SHA-256 digests.

| Commit | SHA-256 |
|---|---|
| `b62d3722` | `9b64590ed735903c9872875d5b2366dd25ff81c2df6023333b8850ea59cb7257` |
| `9449f2c4` | `9ea44010651f141cca706f6b12ad401460414a8e5e99bedc4ea8c663426aba0b` |
| `2f872143` | `417b40f9604232cba8c0a8c967187c1913dcbc694bec00b84a4d62927fb5c4d9` |
| `f167fc8f` | `cff334499fffbf56d73502c3e64817ebaa278ea807fee2bc94485a564abb0b37` |

The change's last commit after `f167fc8f` touches only a test and a feature gate.

The mixed load uses the 1,052,807-triple store of `scripts/bench.sh` at 100,000 people,
and the single-client check uses the BSBM BI 10M store, generated with
`scripts/bench-bsbm.sh 10m` and loaded with `sparkles load`, whose `dataset.nt` has the
SHA-256 `db77a8d62274566c0fcf6a85634741b0313551cc217b59f4b1f26d444fd205be` of the item 449
run.

## Method

`bench-mixed/` holds `scripts/bench.sh` runs with `MODE=mixed`, sixteen `star-join`
readers through `oha` and one writer of new literals for 30 s on a fresh copy of the
store, in five alternating pairs of the default placement and `--request-cpus all`.
`scripts/mixab.py` runs the same load with CONC readers and one writer, rotated over the
configurations in each round, and records `bench.sh`'s figures together with the run and
run-queue wait time of every server thread from `/proc/<pid>/task/*/schedstat` and the
busy share of the performance and efficiency CPUs from `/proc/stat` over the window.
`scripts/mixsum.py` prints the medians. `scripts/placement583.py` is the single-client
harness of the [item 449 run](../../../bsbm/runs/2026-11-02-hybrid-placement-forge/README.md)
with the DBpedia query left out, and `scripts/summarize.py` is that run's summary.
`scripts/affsnap.sh` printed the CPUs of every server thread that was not free to run on
every CPU, 15 s into each default run. `scripts/phase*.sh` ran each part.

## Diagnosis

`b62d3722/mixed-c16` ran item 449's placement (`auto`), every CPU (`all`), item 449's
placement with 20 query threads (`auto20`) and every CPU with 18 (`all18`), three rounds
each with sixteen readers.

| Configuration | Reads/s | Writes/s | P busy | E busy | Request thread run-queue wait |
|---|---:|---:|---:|---:|---:|
| `auto` | 2,004.7 | 241.3 | 95% | 33% | 131 s |
| `all` | 2,354.0 | 359.6 | 79% | 92% | 30 s |
| `auto20` | 2,006.1 | 241.7 | 95% | 34% | 131 s |
| `all18` | 2,356.9 | 367.0 | 78% | 92% | 28 s |

The request threads waited for the twelve logical CPUs of the performance cores while the
efficiency cores idled, and the query pool's size changed neither rate.

`9449f2c4/mixed-c16` adds two experiments over five rounds. `adapt8` placed request work
by the requests in flight, as the final change does with a limit of eight, and answered
2,360.4 reads and 367.8 writes a second against 2,346.0 and 362.0 for `all`. `upd` kept
item 449's placement and ran each update on every CPU, and answered 1,995.3 reads and
309.0 writes a second. `9449f2c4/mixed-c2`, `-c4` and `-c8` swept the readers.

| Readers | `auto` reads/s | `all` reads/s |
|---:|---:|---:|
| 2 | 527.5 | 404.9 |
| 4 | 1,024.7 | 964.0 |
| 8 | 1,603.8 | 1,652.4 |
| 16 | 2,017.6 | 2,346.0 |

The write rates at two to eight readers take one of two values in each run, such as 400
or 800 a second, in both configurations, so the medians of three runs do not rank them.

## First acceptance

`2f872143` placed request work on the performance cores while at most six requests were
in flight. Its sixteen-reader runs answered as many reads as `--request-cpus all`, but
three of five `bench.sh` runs in the default placement wrote 277.5 to 318.1 times a second
against 354.5 to 360.2 with every CPU. `affinity.log` found the block decoders, the change
log writer and the vocabulary sync worker on the performance cores alone. They start with
the first request that needs them, which on a fresh server runs alone, and inherited its
CPUs. The final placement starts them on every CPU.

## Final acceptance

`f167fc8f/bench-mixed` is the five alternating pairs.

| Pair | Default reads/s | `all` reads/s | Default writes/s | `all` writes/s |
|---:|---:|---:|---:|---:|
| 1 | 2,733.6 | 2,595.9 | 380.6 | 367.8 |
| 2 | 2,358.9 | 2,365.2 | 334.6 | 266.7 |
| 3 | 2,348.7 | 2,357.7 | 363.9 | 286.0 |
| 4 | 2,357.1 | 2,362.1 | 364.5 | 318.5 |
| 5 | 2,368.7 | 2,386.4 | 302.9 | 232.3 |
| Median | 2,358.9 | 2,365.2 | 363.9 | 286.0 |

The default placement answered 0.993 to 1.053 of the reads of every CPU in each pair, and
1.035 to 1.304 of the writes. The first pair ran right after an idle host and was faster
in both configurations. The writer took about 2.4 ms per commit in some runs and 3.2 to
3.9 ms in others, in both configurations, and the slower runs fell on `all` more often
in this pass, so the default's higher write median is not a gain of the placement.
`affinity.log` found no thread confined to the performance cores in two of the default
runs, and in the third one runtime worker whose last request ran while at most six were
in flight.

`f167fc8f/mixed-c16` answered 2,381.3 reads and 366.0 writes a second in the default
placement against 2,367.4 and 361.2 with every CPU, with the CPUs as busy and the request
threads' run-queue waits of 28 and 29 s. `f167fc8f/mixed-c4` answered 1,019.1 reads a
second against 976.5.

`f167fc8f/single-client` ran four rounds of the single-client harness with the final
build and with `p449`, each in its default placement and with `--request-cpus all`. The
medians of the server's own time in milliseconds were these.

| Template | Final | Final, `all` | `p449` | `p449`, `all` |
|---|---:|---:|---:|---:|
| `q1` | 11.35 | 13.40 | 13.25 | 13.77 |
| `q2` | 15.60 | 12.91 | 14.91 | 12.94 |
| `q3` | 18.59 | 27.78 | 18.64 | 28.08 |
| `q4` | 159.06 | 158.48 | 158.70 | 159.66 |
| `q5` | 19.72 | 18.40 | 19.73 | 18.77 |
| `q6` | 11.54 | 13.96 | 11.76 | 16.85 |
| `q7` | 44.31 | 44.10 | 43.33 | 43.25 |
| `q8` | 23.94 | 23.75 | 24.86 | 24.08 |

The third `q3` instance took 22.2 to 23.5 ms in each final process and 21.5 to 24.2 ms
with `p449`, against 30.0 to 49.7 ms with every CPU, so one client keeps item 449's gain.
`q2` and `q5` take one of two times in each process, which item 575 covers.

## Files

`scripts/` holds the harnesses and the scripts that ran each part. Each commit's
directory holds the records of `mixab.py` as `mixed-c<readers>/records.json`, the
single-client records as `single-client/records.json.gz` with their `meta.json`, the
`bench.sh` results as `bench-mixed/mixed-<n>.json`, and the log of its part.
