# Instruction and cache counts of four queries with unchanged plans on atlas

This is a manual report of roadmap item 703. The raw outputs are the four JSON files in
this directory, which `perfstat.py.txt` wrote and `run-703.sh.txt` started. They are not
part of the generated [run catalog](../../RUNS.json).

## Question

The [estimation comparison on forge](../2026-11-03-estimation-ab-forge-e8e0e4cd/README.md)
ran the synthetic `f-regex-iri-join` 1.06 times and `f-expr-range-join` 1.11 times as long
on the candidate build with the same plans, and its rounds ran `f-expr-range-join` in
either 1.7 to 2.3 ms or 2.7 to 3.3 ms on both builds. An earlier pair of builds ran
`two-hop-count` and DBpedia's `births-by-decade` 1.15 and 1.13 times as long. Item 703
asks whether such differences come from code layout or from different code, by counting
the instructions and cache misses of each request.

## Method

The host is atlas, a 13th Gen Intel Core i5-13500 whose CPUs 0 to 11 are the six
performance cores with their hyperthreads and 12 to 19 the efficiency cores, with kernel
7.2.9, the powersave governor, perf 7.3.1 from nixpkgs and `perf_event_paranoid` 2. The
three binaries were built on stanley by `scripts/nsc-build.sh` with clang 21.1.8 and rustc
1.99.1. `base` is `71d11ad0`, `main` is `321508fe` and `tip` is `ce993ece`. The tip differs
from `main` in the planner code of items 702 and 874 only, so it is the control whose
execution code is the same as `main`'s and only placed differently. The synthetic store has 100,000 people from
`scripts/gen-data.py`, 1,052,801 quads, and the DBpedia store is the 8.2M-quad slice of
the X15 measurements, both loaded by `base`.

`perfstat.py` starts a fresh server per binary and round, in the arm order and then in
reverse, with the result cache off and `?nocache=true`, warms each query with three
requests, and attaches `perf stat -p <server>` to the server around each block of 30
requests. Three blocks per query use three event sets that fit the performance core's
counters without multiplexing, and every count ran 100% of the time. The first set is
`instructions`, `cycles`, `cache-misses`, `branch-misses`, `mem_load_retired.l3_miss` and
`baclears.any`. The second is `idq.dsb_uops`, `idq.mite_uops`,
`frontend_retired.any_dsb_miss` and `dsb2mite_switches.penalty_cycles`. The third is
`icache_data.stalls`, `icache_tag.stalls`, `L1-icache-load-misses` and
`iTLB-load-misses`. The counts are user-space counts of the
whole server process divided by the 30 requests, so they cover planning, execution and the
response of a request. An idle server costs 2,600 instructions per second, which does not
register at this scale. Each block also records the median `planMs` and `execMs` that the
server reports. A structural fingerprint of the plan, the operators and descriptions
without estimates and runtime annotations, confirms that every arm ran the same plan
shape for every query. Six rounds, so 12 server processes per arm.

Two configurations. The first pins the server to CPU 2 with `RAYON_NUM_THREADS=1` and the
client to CPU 4, with an idle-priority spinner on each, as the lane's A/B method does. The
second gives the server CPUs 0 to 11 with twelve Rayon threads and the client CPU 12, with
spinners on every server CPU, which is the configuration of the forge record above.

## Results with one thread

Medians over the 12 processes, counts per request.

| Query | Arm | execMs | Instructions | Cycles | IPC | Cache misses | DSB misses |
|---|---|---|---|---|---|---|---|
| `f-regex-iri-join` | base | 3.739 | 64.80M | 16.46M | 3.94 | 42.2K | 185K |
| `f-regex-iri-join` | main | 3.626 | 64.81M | 15.90M | 4.08 | 35.3K | 188K |
| `f-regex-iri-join` | tip | 3.616 | 64.81M | 15.92M | 4.07 | 39.2K | 199K |
| `f-expr-range-join` | base | 0.736 | 37.70M | 13.02M | 2.89 | 131K | 110K |
| `f-expr-range-join` | main | 0.739 | 37.71M | 12.38M | 3.05 | 130K | 126K |
| `f-expr-range-join` | tip | 0.682 | 37.71M | 11.96M | 3.15 | 105K | 105K |
| `two-hop-count` | base | 2.613 | 14.15M | 11.24M | 1.26 | 84.8K | 58.5K |
| `two-hop-count` | main | 2.563 | 14.16M | 11.00M | 1.29 | 84.7K | 56.1K |
| `two-hop-count` | tip | 2.579 | 14.16M | 11.10M | 1.28 | 70.2K | 59.7K |
| `births-by-decade` | base | 1.729 | 18.55M | 7.81M | 2.38 | 56.4K | 66.4K |
| `births-by-decade` | main | 1.743 | 18.55M | 7.89M | 2.35 | 57.7K | 65.3K |
| `births-by-decade` | tip | 1.722 | 18.56M | 7.82M | 2.37 | 60.1K | 66.3K |

The instructions per request agree across the three binaries to within 0.04% on every
query, and within one binary the 12 processes agree to within 0.1%. The decoded-icache
coverage is 96% to 97% of the uops in every arm, and the DSB misses, icache stalls and
iTLB misses of the three binaries lie within each other's spreads. The one steady
difference between binaries is `f-regex-iri-join`, where `main` and `tip` run 3% fewer
cycles than `base` in every process, with 7,000 fewer cache misses per request and the
same instructions, so the newer binaries place their data better for that query. No
query runs slower on a newer binary.

`f-expr-range-join` has two modes in every arm. The processes of the first two rounds
took 0.62 to 0.65 ms with 66K to 88K cache misses and 4.6K to 6.6K L3 load misses per
request, and the processes of the later rounds 0.70 to 0.81 ms with 106K to 153K cache
misses and 8.0K to 11.6K L3 load misses, at the same 37.7M instructions. The mode changed
at the same time for all three binaries, `tip`'s last process was fast again, and
`f-regex-iri-join` and `two-hop-count` stepped up by 1.5% and 3% at the same point. The
modes are therefore the state of the host's shared cache and memory while the processes
ran, and not a property of a binary. `f-expr-range-join` is the most exposed of the four
queries because it has the most cache misses per request.

## Results with twelve threads

Medians over the 12 processes, with the range of the per-process medians of `execMs`.

| Query | Arm | execMs | Range | Instructions | Cycles | IPC | DSB coverage | DSB misses |
|---|---|---|---|---|---|---|---|---|
| `f-regex-iri-join` | base | 7.794 | 7.16 to 8.74 | 67.22M | 30.07M | 2.24 | 90.9% | 833K |
| `f-regex-iri-join` | main | 7.495 | 7.12 to 9.34 | 67.17M | 29.61M | 2.27 | 90.8% | 817K |
| `f-regex-iri-join` | tip | 7.575 | 7.26 to 8.28 | 67.17M | 29.63M | 2.27 | 90.0% | 891K |
| `f-expr-range-join` | base | 3.033 | 1.75 to 3.84 | 38.99M | 22.48M | 1.73 | 64.8% | 2.04M |
| `f-expr-range-join` | main | 2.768 | 1.74 to 3.74 | 38.97M | 21.23M | 1.84 | 71.4% | 1.55M |
| `f-expr-range-join` | tip | 2.261 | 1.73 to 3.31 | 38.95M | 21.32M | 1.83 | 72.6% | 1.52M |
| `two-hop-count` | base | 6.882 | 6.45 to 7.45 | 16.24M | 17.29M | 0.94 | 92.0% | 216K |
| `two-hop-count` | main | 6.459 | 6.38 to 7.42 | 16.24M | 17.43M | 0.93 | 89.8% | 272K |
| `two-hop-count` | tip | 7.285 | 6.43 to 7.44 | 16.27M | 17.48M | 0.93 | 90.5% | 265K |
| `births-by-decade` | base | 3.568 | 2.89 to 3.88 | 19.69M | 11.30M | 1.74 | 89.9% | 244K |
| `births-by-decade` | main | 3.685 | 3.17 to 3.79 | 19.72M | 11.33M | 1.74 | 87.2% | 309K |
| `births-by-decade` | tip | 3.510 | 2.90 to 3.71 | 19.69M | 11.29M | 1.74 | 85.2% | 364K |

With twelve threads the four queries take 2 to 4 times as long as with one thread, with 4%
to 15% more instructions per request, 1.4 to 1.7 times the cycles, half the instructions
per cycle and 4 to 15 times the DSB misses, since the threads share the cores' decoded
caches and spend their time in the pool. The per-process medians of `f-expr-range-join`
spread from 1.7 to 3.8 ms in every arm, which is the forge record's two modes: a
ratio of medians over a few rounds of this configuration is not a measurement of the
binaries. The instructions still agree across the binaries to within 0.2%.

## Conclusion

The execution differences between these binaries under identical plans are not
differences of code: the binaries retire the same instructions per request to within 0.04%
with one thread and 0.2% with twelve. What differs is cache behaviour, which the host's
state moves more than the binary does, and the twelve-thread configuration's own spread.
No hot loop runs slower on a newer binary, so there is nothing to align or split. The
twelve-thread cost of short queries is roadmap item 878.
