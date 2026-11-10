# Writes and updates results

This is a [manual report](../README.md#evidence-and-generated-reports). Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Comparison summary

The suite's churn of 5,000 single-triple commits runs at 778 and 935 Sparkles commits/s
  against 809 and 744 for unsynced Oxigraph at 1.05M, and at 681 and 684 against 715 at
  10.5M. Sparkles synchronizes before acknowledgement. With sixteen unpaced readers,
  Oxigraph completes about five times as many writes at 1.05M but 12 to 16 reads/s, against
  Sparkles' 2,101 to 2,198. The [focused write assessment](#serial-writes-with-reader-work)
  of 2026-10-25 predates the write-through fences of items 501 and 502.

## Writes and updates

Every Sparkles write is durable when it is acknowledged, because the WAL record and any
new vocabulary entries are synchronized first. The other engines use their defaults. TDB2
commits durably, QLever keeps updates in memory only, Fluree uses its default commit
path, and Oxigraph commits a RocksDB transaction that writes its WAL without
synchronizing it. Of these comparisons, only the one with TDB2 is between two commits that
are both synchronized.

### Durable commit latency

These serial single-triple commits alternate Sparkles and Oxigraph over 6 rounds, each
with a fresh copy of the store and a fresh server. Literal inserts add a new term to the
vocabulary, IRI inserts reuse known terms, and deletes remove triples.

| Serial commits, ms | Sparkles median | Sparkles p90 | Oxigraph median | Oxigraph p90 |
|---|---:|---:|---:|---:|
| 1.05M, literal insert | 1.126 | 1.345 | **0.613** | 1.255 |
| 1.05M, IRI insert | 1.080 | 1.273 | **0.627** | 1.298 |
| 1.05M, delete | 1.075 | 1.272 | **0.549** | 1.060 |
| 10.5M, literal insert | 1.284 | 2.781 | **0.926** | 1.318 |
| 10.5M, IRI insert | 1.200 | 2.702 | **0.929** | 1.360 |
| 10.5M, delete | 1.205 | 2.711 | **0.817** | 1.117 |

Oxigraph issued no device writes during these runs, while a run of 5,000 Sparkles commits
issued 14 device cache flushes.

These rounds ran before items 501 and 502 wrote the commit records and annotation records
through the device cache, and this serial workload has not been measured since, so the
table records the earlier build. The churn table below was measured after those items.

The comparative suite's `updates` mode applies the same 5,000 single-triple INSERT and
DELETE requests, one at a time, to every engine's store. No engine failed a commit.
`mise run bench 100000 .artifacts/bench --mode updates` reproduces it.

| Churn of 5,000 commits | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M: commits/s | **935** | 16 | 423 | 259 | 744 |
| 1.05M: latency p50 / p99 (ms) | **1.15 / 1.76** | 70.93 / 88.19 | 2.27 / 5.78 | 3.17 / 25.22 | 1.35 / 2.09 |
| 10.5M: commits/s | 684 | 26 | 387 | 361 | **715** |
| 10.5M: latency p50 / p99 (ms) | 1.36 / 3.19 | 19.75 / 90.27 | 2.48 / 6.30 | 1.69 / 6.46 | **1.35 / 2.14** |

*Sparkles and Oxigraph at 1.05M were measured on forge on 2026-11-02 with Sparkles at commit
98f1728d, Sparkles at 10.5M on the same day, and the other cells on 2026-10-18 and
2026-10-19. The [run record](runs/2026-11-02-competitive-writes-forge/README.md) also holds
a second pair at 1.05M, 778 against 809 commits/s, with Sparkles at commit b62d3722.*

The WAL preallocates space in steps from 64 KiB up to 4 MiB (`--wal-prealloc-kb`, `0`
disables it). Replay, readers, backups and quotas use the logical end of the last commit,
and recovery recognizes a torn tail.

### Serial writes with reader work

A bounded [write assessment](../2026-10-25-durable-write-assessment-forge.json)
keeps the durability distinction explicit. At 10.5M triples, two process medians for
literal inserts are 1.254 and 1.167 ms for Sparkles, against 0.625 and 0.594 ms for
unsynced Oxigraph. Direct synchronization averages about 0.25 ms in these Sparkles
processes. The remaining serial difference does not justify changing acknowledgement
ordering.

With one writer and sixteen unpaced star-join readers, Sparkles completes 275–279
writes/s and 232–245 reads/s at 10.5M, with write p99 of 7.37–7.46 ms. Oxigraph completes
585–827 writes/s with p99 of 2.80–4.08 ms, while no read finishes in the thirty-second
window. At 1.05M, Sparkles completes 373–388 writes/s and 2,463–2,644 reads/s, against
1,416–1,524 writes/s and 13.6–17.2 reads/s. These rates compare different completed reader
workloads. Deadline cancellations are separate from completed responses.

All 80,000 before-and-after churn state checks and Sparkles offline checks pass. Initial
mixed reader answer bags agree. This run supplies no separate mixed-write restart or
power-loss proof. The bounded profile supports retaining the durable path, with remaining
costs deferred under [item 290](../../ROADMAP.md).

### Concurrent durable commits

Group commit is the default, and `--no-group-commit` keeps the ordinary writer, which
synchronizes each commit on its own. The server ran on six CPUs and the client on four, and
six rounds alternated the two writers, each run in a fresh process on a fresh copy of the
store. Each cell is the median over the rounds. Every run completed with distinct,
contiguous receipts and the correct final state.

| 50,000 changes, transactions/s | 1.05M ordinary | 1.05M group commit | 10.5M ordinary | 10.5M group commit |
|---|---:|---:|---:|---:|
| Four writers | 3,399.8 | **6,173.8** | 3,375.6 | **6,160.0** |
| Sixteen writers | 3,342.8 | **18,119.2** | 3,332.0 | **18,304.4** |

| Request latency (ms) | 1.05M ordinary | 1.05M group commit | 10.5M ordinary | 10.5M group commit |
|---|---:|---:|---:|---:|
| Four writers, p50 / p99 | 1.159 / 1.383 | **0.631 / 0.954** | 1.168 / 1.381 | **0.633 / 0.952** |
| Sixteen writers, p50 / p99 | 4.738 / 5.867 | **0.813 / 1.467** | 4.752 / 6.083 | **0.793 / 1.445** |

*Measured on forge on 2026-10-17 at commit 758a6a25, with the same binary for both writers.*

With sixteen writers, no writer's share of the commits differed from the mean by more than
1.2% with group commit, and by 0.3% with the ordinary writer. A single writer gains
nothing from grouping, so a commit that finds no other writer waiting synchronizes as the
ordinary writer does. These serial runs send 5,000 single-triple commits.

| Serial commits, p50 / p90 (ms) | 1.05M ordinary | 1.05M group commit | 10.5M ordinary | 10.5M group commit |
|---|---:|---:|---:|---:|
| Literal insert | 0.426 / 0.461 | 0.427 / 0.459 | 0.429 / 0.463 | 0.421 / 0.458 |
| IRI insert | 0.426 / 0.458 | 0.425 / 0.459 | 0.422 / 0.460 | 0.425 / 0.464 |
| Delete | 0.425 / 0.459 | 0.430 / 0.459 | 0.423 / 0.456 | 0.425 / 0.456 |

The churn and mixed modes of `scripts/bench.sh` measured within 3% between the two writers
in the same session.

<a id="group-commit-on-indexed-datasets-on-forge"></a>

### Group commit on indexed datasets

Datasets with a full-text, spatial or embedding index also group their commits. These runs
apply the gates of [X09 section 17.9](../../specs/X09-group-commit-by-default.md#179-performance-gates)
with `scripts/bench-indexed-commit.py`, on native storage with no added delay. The server
ran on the six performance cores and their sibling threads, and the client on two
efficiency cores. Every cell is one index on one dataset of 1.05M or 10.5M triples, with
five blocks of four processes in the order ordinary, grouped, grouped, ordinary, so each
scenario has ten processes per setting. Every process started from a fresh copy of the
seed store, checked every acknowledgement and the index answers, and reopened cold to
check them again.

Every gate is met on `forge`. The campaigns of 2026-10-23 and 2026-10-24 met 35 of 36
serial checks, all six throughput gates, all ten memory gates and three of the four reader
gates. The p90 of spatial literal inserts at 10.5M missed there, and the spatial reader gate
at 1.05M was not established, because its ordinary writer did not keep the offered
schedule. The [rerun on forge](#rerun-on-forge) met both, with a rate calibrated for the
drive's slow state, and met the throughput and memory gates of the full-text and spatial
cells at 1.05M again. The [rerun on atlas](#rerun-on-atlas), a host with the same CPU and
drive model, found the slow state in more of its processes and supports the same verdicts.
With [write-through fences](#write-through-fences) the four writers of both cells at 1.05M
met their throughput gates on `forge` again, with the drive in its slow state.

*The serial, writer and memory cells were measured on forge on 2026-10-23 at commit
5b40d0ec, the mixed cells on forge on 2026-10-24 with the bounded full-text checkpoint, the
reruns on forge and atlas on 2026-10-31 at commit a202e8f2, and the
[write-through fences](#write-through-fences) on forge on 2026-10-31 at commit 3da475a9.*

#### Serial commits

The serial scenarios commit 2,000 single changes after 200 warmup
changes. A serial commit finds the group idle and takes the ordinary path, so both columns
measure the same code. The paired ratio is the median over the ten pairs of a grouped
process with its neighbouring ordinary process in the same block, and the gate asks for at
most 1.05.

| Index | Dataset | Change | p50 ordinary (ms) | p50 grouped (ms) | p50 paired ratio | p90 ordinary (ms) | p90 grouped (ms) | p90 paired ratio |
|---|---|---|---:|---:|---:|---:|---:|---:|
| Full-text | 1.05M | literal | 1.774 | 1.768 | 0.984 met | 1.874 | 1.966 | 1.008 met |
| Full-text | 1.05M | iri | 0.464 | 1.053 | 1.001 met | 1.106 | 1.723 | 0.973 met |
| Full-text | 1.05M | delete | 0.412 | 0.407 | 0.985 met | 0.431 | 0.427 | 0.990 met |
| Full-text | 10.5M | literal | 0.437 | 0.443 | 1.000 met | 1.188 | 0.483 | 1.007 met |
| Full-text | 10.5M | iri | 0.412 | 0.414 | 1.004 met | 0.458 | 0.446 | 0.986 met |
| Full-text | 10.5M | delete | 0.411 | 0.410 | 1.002 met | 0.432 | 0.428 | 0.990 met |
| Spatial | 1.05M | literal | 1.733 | 0.433 | 0.989 met | 1.822 | 1.163 | 0.973 met |
| Spatial | 1.05M | iri | 0.411 | 0.413 | 1.010 met | 0.456 | 0.456 | 1.001 met |
| Spatial | 1.05M | delete | 0.406 | 0.409 | 1.011 met | 0.431 | 0.428 | 1.005 met |
| Spatial | 10.5M | literal | 1.736 | 1.786 | 1.025 met | 1.846 | 1.950 | 1.077 **missed** |
| Spatial | 10.5M | iri | 0.409 | 0.414 | 1.012 met | 0.449 | 0.463 | 1.032 met |
| Spatial | 10.5M | delete | 0.412 | 0.411 | 0.996 met | 0.435 | 0.429 | 0.985 met |
| Embedding | 1.05M | literal | 0.473 | 0.441 | 0.978 met | 1.536 | 0.797 | 0.960 met |
| Embedding | 1.05M | iri | 0.410 | 0.415 | 1.002 met | 0.431 | 0.448 | 1.008 met |
| Embedding | 1.05M | delete | 0.411 | 0.410 | 0.996 met | 0.435 | 0.434 | 1.003 met |
| Embedding | 10.5M | literal | 0.381 | 0.376 | 0.987 met | 1.852 | 1.843 | 0.976 met |
| Embedding | 10.5M | iri | 0.411 | 0.412 | 1.010 met | 0.437 | 0.441 | 0.999 met |
| Embedding | 10.5M | delete | 0.415 | 0.417 | 1.011 met | 0.441 | 0.452 | 1.019 met |

The medians over ten processes follow the drive more than the writer. The mean time of a
commit synchronization was about 0.23 ms in some processes and between 1.2 and 1.7 ms in
others of the same cell and setting, and it often changed inside a block. Of the 360 serial
processes, 32 of 180 ordinary and 27 of 180 grouped ones had a mean synchronization above
1 ms, with a Fisher test p value of 0.57, which is why the full-text IRI inserts at 1.05M
have p50 medians of 0.464 and 1.053 ms and a paired ratio of 1.001. The engine overhead,
the mean request latency less the mean synchronization, differs by at most 3.6% between the
settings in 8 of 10 cells where it can be computed. It is 12.7% higher with grouping for
spatial literals at 10.5M, 0.240 against 0.213 ms, the cell that misses, and the reruns on
forge and atlas met both of its gates. Of the 36 paired
medians, 19 are above 1.0, with a median of 1.001 and a sign test p value of 0.43, so the
single-writer path shows no general cost.

#### Four writers

The writer scenario sends 50,000 changes from four clients. The paired
ratio takes the median over the ten pairs of a grouped process with its neighbour in the
same block, and the gate asks it to reach the required gain, which is 1.5 when the
ordinary path's mean commit synchronization takes 1.2 ms or longer and falls in proportion
to it below that. The peak RSS difference subtracts the ordinary process's sampled peak
inside the write interval from the grouped process's peak over the whole window, and the
limit is 64 MiB.

| Index | Dataset | Ordinary tx/s | Grouped tx/s | Paired ratio | Ordinary sync (ms) | Required gain | Peak RSS difference |
|---|---|---:|---:|---:|---:|---:|---:|
| Full-text | 1.05M | 926 (637–1,008) | 1,241 (1,207–1,503) | 1.496 met | 0.98 | 1.408 | 8.5 MiB met |
| Full-text | 10.5M | 795 (634–1,028) | 1,311 (1,154–1,543) | 1.675 met | 1.16 | 1.483 | -3.4 MiB met |
| Spatial | 1.05M | 860 (638–1,071) | 1,284 (1,199–4,106) | 1.522 met | 1.09 | 1.452 | 7.8 MiB met |
| Spatial | 10.5M | 929 (726–1,035) | 1,243 (1,118–3,634) | 1.593 met | 0.99 | 1.413 | 9.3 MiB met |
| Embedding | 1.05M | 650 (463–740) | 1,549 (766–1,816) | 2.382 met | 1.03 | 1.428 | 19.6 MiB met |
| Embedding | 10.5M | 608 (569–662) | 1,245 (1,151–1,256) | 2.059 met | 1.10 | 1.458 | 0.1 MiB met |

With four clients a fence carries a median of 1.9 to 2.0 commits in the text and spatial
cells, so a commit waits for about two synchronizations and the gain is bounded near 2. The
ordinary writer ran at 950 to 1,010 transactions/s in the first position of a block and at
700 to 740 in the last, after two grouped runs, so the gain depends on the neighbour. It is
1.26 to 1.60 against the first position and 1.56 to 2.26 against the last. The grouped
spatial processes at 3,634 and 4,106 transactions/s ran in the fast state of the drive,
where the mean synchronization was 0.44 ms, and barely move the medians.

#### Readers during writes

The mixed scenario paces four writers at 60% of the slowest
unpaced process, the same offered rate for both settings, and two readers run the index
query throughout, counting the matches of a text query or a spatial window. The medians
pool the ten processes of each setting, the ratios are the paired medians of grouped to
ordinary, and the gate asks for at most 1.10 with a schedule lag of at most 50 ms.

| Cell | Rate (tx/s) | Reader p50, ordinary and grouped (ms) | Reader p99, ordinary and grouped (ms) | Paired p50 and p99 ratios | Max schedule lag, ordinary and grouped (ms) | Gate |
|---|---:|---|---|---|---|---|
| Text at 1.05M | 414 | 4.85, 4.84 | 7.94, 7.72 | 1.001, 0.993 | 6.3–30.1, 4.2–12.1 | Met. |
| Text at 10.5M | 354 | 4.87, 4.83 | 8.00, 8.10 | 0.987, 1.008 | 3.7–6.4, 2.2–10.1 | Met. |
| Spatial at 1.05M | 583 | 1.04, 0.98 | 1.66, 1.35 | 0.947, 0.813 | 136.1–271.7, 8.0–11.7 | Not established. |
| Spatial at 10.5M | 444 | 1.03, 1.02 | 1.48, 1.45 | 0.999, 0.988 | 3.5–12.3, 4.0–8.8 | Met. |

No text process stalled. The ordinary spatial processes at 1.05M had no slow request, but
in 36 to 56 of their 86 seconds four queued commit synchronizations took about 7 ms, so
they sustained about 570 transactions/s against the offered 583. The unpaced calibration
averaged the drive's fast and slow states, and the reruns on forge and atlas calibrated the
rate for the slow state instead.
The text readers' paired ratios of 1.001 and 1.008 are within the gate and the spread of
the runs, and [roadmap](../../ROADMAP.md) item 170 measures them with more rounds.

#### Memory

Every cell stayed under the 64 MiB limit. The certified peak resident memory
difference was between −3.4 and 19.6 MiB for the writer scenarios and between 0.3 and
5.6 MiB for the mixed scenarios, where both settings run for the same time. The
certificate subtracts the lower sampled bound of the ordinary process, so it errs upward.

#### Rerun on forge

These campaigns ran on `forge` with a release build of commit `a202e8f2`, five blocks each
in the order ordinary, grouped, grouped, ordinary, while another lane shared the host under
its heavy-job lock. The [run record](runs/2026-10-31-indexed-group-commit-forge/README.md)
holds the results, the gate summaries and the process list at the start of each campaign.

| Cell | Measurement | Ordinary | Grouped | Paired ratio | Gate |
|---|---|---:|---:|---:|---|
| Spatial at 10.5M, serial literal | p50 and p90 (ms) | 0.432, 0.468 | 0.434, 0.480 | 1.002, 1.012 | Met. |
| Spatial at 10.5M, serial IRI | p50 and p90 (ms) | 0.487, 1.383 | 0.480, 1.374 | 1.001, 0.992 | Met. |
| Spatial at 10.5M, serial delete | p50 and p90 (ms) | 0.423, 0.453 | 0.424, 0.462 | 0.997, 1.005 | Met. |
| Spatial at 1.05M, four writers | tx/s | 1,439 (1,269–1,816) | 2,253 (2,011–2,814) | 1.564 | Met, with 1.257 required. |
| Full-text at 1.05M, four writers | tx/s | 1,524 (1,423–2,339) | 2,381 (2,002–2,755) | 1.564 | Met, with 1.240 required. |
| Spatial at 1.05M, readers at 478 tx/s | Reader p50 and p99 (ms) | 0.972, 1.394 | 0.988, 1.398 | 1.028, 1.003 | Met. |

The drive spent most of these campaigns in its fast state. The ordinary writers
synchronized in a median of 0.62 and 0.58 ms, which sets the required gains, and a fence
took 0.61 to 0.89 ms and carried 1.80 to 1.87 commits. The certified peak resident memory
differences were 3.5 and 1.3 MiB for the writers and −3.7 MiB for the readers. The reader
cell's calibration measured the unpaced ordinary writers at 1,535 and 1,538
transactions/s with a mean synchronization of 0.58 ms. At 1.6 ms per synchronization they
would sustain 597.4, and the cell offered 80% of that. Every process kept its schedule
within 3.3 to 8.5 ms. The reader p50 ratio of 1.028 is within the gate, and
[roadmap](../../ROADMAP.md) item 448 follows it up with the other small costs that the
gates accept.

#### Rerun on atlas

These campaigns ran on `atlas`, an Intel Core i5-13500 with the SK hynix drive of `forge`
under the same LUKS, LVM and ext4 stack, with a release build of commit `a202e8f2`. Each
cell had five blocks in the order ordinary, grouped, grouped, ordinary, and every process
probed the drive before it started. The [run record](runs/2026-10-31-indexed-group-commit-atlas/README.md)
holds the results, the gate summaries and the per-process device counters.

| Cell | Measurement | Ordinary | Grouped | Paired ratio | Gate |
|---|---|---:|---:|---:|---|
| Spatial at 10.5M, serial literal | p50 and p90 (ms) | 0.568, 0.641 | 0.561, 0.629 | 0.984, 0.973 | Met. |
| Spatial at 10.5M, serial IRI | p50 and p90 (ms) | 0.648, 2.018 | 0.646, 2.020 | 0.999, 1.000 | Met. |
| Spatial at 10.5M, serial delete | p50 and p90 (ms) | 0.541, 0.612 | 0.558, 0.619 | 1.016, 1.019 | Met. |
| Spatial at 1.05M, four writers | tx/s | 723 (648–888) | 1,654 (996–2,368) | 2.253 | Met, with 1.5 required. |
| Full-text at 1.05M, four writers | tx/s | 683 (631–908) | 1,249 (1,082–2,276) | 1.805 | Met, with 1.5 required. |
| Spatial at 1.05M, readers at 466 tx/s | Reader p50 and p99 (ms) | 1.654, 2.204 | 1.653, 2.187 | 0.999, 1.005 | Met. |

The certified peak resident memory differences were −1.4 MiB and 8.4 MiB for the
spatial and full-text writers and −2.2 MiB for the readers. The ordinary writers ran
with the drive in its slow state, with mean commit synchronizations of 1.26 and 1.32 ms.
Grouped processes whose fences took 0.6 to 0.74 ms committed 2,060 to 2,370
transactions/s, and those whose fences took 1.27 to 1.90 ms committed 1,000 to 1,370, a
median gain of 1.40 over their ordinary neighbours. A fence carried 1.70 to 1.99 commits.

The reader cell offered 80% of the rate that the slowest ordinary calibration process
would sustain with every synchronization at 1.6 ms, which is 466 transactions/s. Every
process kept its schedule within 11.1 to 21.0 ms. A first campaign at the same rate met
the lag in four blocks, and in its fifth every process of both settings saw device writes
of 7 to 12 ms and fell behind by up to 2.5 s. The stall hit both settings alike, which
points to I/O from outside the campaign, and the run record keeps it.

The fence's synchronization explained the smaller gain in the slow state. At that commit
a fence wrote with buffered writes and an `fdatasync` of the log and of the vocabulary,
which is two device cache flushes, while an ordinary commit wrote both files through
`O_DIRECT` and `O_DSYNC` with no flush. Fences now write through as well, which the
[write-through fence runs](#write-through-fences) measure. Idle, the drive takes 0.30 ms for a write and its flush and 0.24 ms for a
write-through write. In its slow state these take about 1.0 and 0.9 ms, and the fence's
two flushes in parallel take 1.3 to 1.5 ms. A fence that let the log's flush cover the
vocabulary measured no faster, as [PERFORMANCE-EXPERIMENTS.md](../../PERFORMANCE-EXPERIMENTS.md#writes)
records. Serial commits take the ordinary path in both settings, and `perf stat` counted
0.1% more user instructions per serial IRI insert with grouping and 0.3% more per geometry
insert, the admission checks of the group.

#### Write-through fences

A fence's leader now writes its prefix's records and terms through the `O_DIRECT` and
`O_DSYNC` handles when they fit in preallocated space. These campaigns ran on `forge` with
release builds of commit `8127961f`, whose fences write and flush, and of commit
`3da475a9`, whose fences write through. Each campaign had five blocks of four processes
at 1.05M, in the order A, B, B, A, under the host's heavy-job lock with nothing else
running. The [run record](runs/2026-10-31-write-through-fences-forge/README.md) holds the
results, the per-process counters of `/proc/diskstats` and the process list at the start
of each campaign.

| Cell | Measurement | Flushing fences | Write-through fences | Paired ratio |
|---|---|---:|---:|---:|
| Spatial, four grouped writers | tx/s | 1,233 (1,157–3,754) | 2,614 (2,458–2,681) | 2.053 |
| Spatial, four grouped writers | Fence time (ms) | 1.48–1.67 in six processes, 0.44–0.67 in four | 0.58–0.64 | 0.406 |
| Full-text, four grouped writers | tx/s | 1,436 (1,162–3,098) | 1,471 (1,280–2,459) | 1.020 |
| Full-text, four grouped writers | Fence time (ms) | 1.13–1.64 in nine processes, 0.54 in one | 1.10–1.48 in eight processes, 0.63 and 0.64 in two | 0.968 |
| Spatial, serial literal inserts | p50 and p90 (ms) | 0.442, 1.125 | 0.437, 0.476 | 1.005, 0.996 |

A flushing fence issued 1.00 to 1.14 device cache flushes per commit and a write-through
fence 0.001 to 0.005, so the fence no longer flushes the device. The spatial cell shows
the gain in the drive's slow state, where six flushing processes took 1.48 to 1.67 ms a
fence and committed 1,157 to 1,287 transactions/s, and every write-through process took
0.58 to 0.64 ms and committed 2,458 to 2,681. In the fast state four flushing processes
took 0.44 to 0.67 ms a fence and committed 2,566 to 3,754 transactions/s. Three of them
outran every write-through process, and their pairs have ratios of 0.69 to 0.73. These
campaigns kept only each process's means, so they cannot show how long the drive stayed in
each state. The reading that the data fit is that those flushing processes ran wholly in
the drive's fast state, while the device time of 0.47 ms per write-through write is the
mean of processes in which the drive spent part of the time in each state. The
[fence probes](#fences-in-each-drive-state) measured both shapes with the drive in each
state and found write-through fences faster in both. The fence histograms and device
samples of the [annotation record runs](#written-through-annotation-records) show
write-through processes that spend part of their time in each state. In the full-text cell
the write-through fences of eight processes took 1.10 to 1.48 ms, with device writes of
0.76 to 1.20 ms each, and the paired gain is 1.020. Serial commits take the ordinary path
with both builds and kept their latency. Their p90 medians differ because one flushing
process ran in the slow state, and the paired ratios compare neighbours. The peak resident memory medians were 192.8 and
228.7 MiB with write-through fences and 199.1 and 238.5 MiB with flushing ones, so the
records queued on the heap did not raise the peak.

The gate campaign compared grouped and ordinary commits of commit `3da475a9` with the
drive in its slow state, where an ordinary write-through commit synchronized in 0.95 to
1.47 ms. Four grouped writers committed 1,581 against 741 transactions/s on the spatial
dataset, a paired ratio of 2.135, and 1,433 against 726 on the full-text dataset, a paired
ratio of 1.927, both above the required 1.5. The certified peak resident memory
differences were 1.4 and 7.3 MiB, under the limit of 64 MiB.

#### Fences in each drive state

Probe runs on `forge` measured the two fence shapes outside the engine. A write-through
fence wrote one 4 KiB block in each of two files through `O_DIRECT` and `O_DSYNC` handles
in parallel, and a flushing fence wrote the same blocks with buffered writes and ran
`fdatasync` of both files in parallel. Two runs alternated the shapes every second, so that
neighbouring seconds shared the drive's state, and each pair counts in a state when both of
its seconds were in it. The [run record](runs/2026-11-01-fence-probe-forge/README.md) holds
the probe, its schedules, its output and the drive's power and thermal state.

| State | Pairs | Write-through fence (ms) | Flushing fence (ms) | Paired ratio | Write-through faster |
|---|---:|---:|---:|---:|---:|
| Fast | 14 | 0.241 | 0.342 | 0.704 | 14 |
| Slow | 332 | 1.612 | 1.701 | 0.944 | 308 |

In the fast state a write-through write took 0.20 ms on the device, and the two writes of a
fence overlapped, so the fence took no longer than one write. A flushing fence's writes
took 0.13 ms and its flushes 0.24 ms. In the slow state every write-through write and every
flush took about 1.5 ms. The drive spent most of each run in its slow state, and its fast
spells lasted 5 to 14 seconds, often after an idle period but not always. Its thermal
management counters stayed at zero. Write-through fences are faster in both states, so
fences keep writing through.

#### Written-through annotation records

A fence's leader now writes its commits' messages and digests to `annotations.bin` through
the `O_DIRECT` and `O_DSYNC` handle as well, instead of synchronizing the file with
`fdatasync`. These campaigns ran on `forge` with release builds of commit `4bd70845`,
whose fences synchronize the file, and of commit `4082759b`, whose fences write the records
through. The later commits of the item add one mutex acquisition and one atomic addition to
each fence for the quota's count of preallocated zeros. Every commit carried a commit
message. Each campaign had blocks of four processes at 1.05M, in the order A, B, B, A, under the host's heavy-job lock with nothing else
running. The [run record](runs/2026-11-01-annotated-fences-forge/README.md) holds the
results, the per-process counters of `/proc/diskstats` and the process list at the start of
each campaign.

| Cell | Measurement | Synchronized records | Written-through records | Paired ratio |
|---|---|---:|---:|---:|
| Spatial, four grouped writers | tx/s | 833 (613–857) | 1,735 (1,499–1,861) | 2.102 |
| Spatial, four grouped writers | Fence time (ms) | 2.31–3.24 | 0.95–1.23 | 0.427 |
| Full-text, four grouped writers | tx/s | 787 (622–860) | 1,772 (1,237–1,833) | 2.214 |
| Full-text, four grouped writers | Fence time (ms) | 2.28–3.16 | 0.95–1.53 | 0.407 |
| Spatial, serial literal inserts | p50 and p90 (ms) | 1.855, 4.691 | 0.692, 0.737 | 0.618, 0.627 |

A fence that synchronized the records issued 0.50 to 0.53 device cache flushes per commit,
one for each fence, and a fence that writes them through 0.001 to 0.005. The paired p50 and
p90 latency ratios of the grouped writers are 0.480 and 0.478 on the spatial dataset and
0.483 and 0.482 on the full-text dataset. The peak resident memory medians were 186.3 and
219.4 MiB with written-through records and 193.9 and 230.4 MiB with synchronized ones.
Serial commits take the ordinary path, which now writes its record through before the
commit's log records instead of synchronizing the file. They issued 0.005 to 0.007 cache
flushes per commit instead of 1.105 to 1.109, and their paired transactions/s ratio is
1.669. Their peak resident memory medians were 127.7 MiB with written-through records and
131.5 MiB with synchronized ones.

The fence times of a process fall in two groups. Every process with written-through
records had 17 to 57 percent of its fences under 0.5 ms and 42 to 80 percent between 1 and
2.5 ms, with at most 9.1 percent between the two. These are the fence times of the
[fence probes](#fences-in-each-drive-state) in the drive's fast and slow states. Samples of
`/proc/diskstats` every 100 ms through the third and fourth blocks of the full-text cell
show when the drive changed state. In three of the four sampled processes with
written-through records, a write took 0.20 ms on the device for one spell of 5.6 to 5.8
seconds that began within 6 seconds of the process's first commit, and about 1.5 ms for the
rest of the process. The fourth had a spell of 1.8 seconds and spent a third of its samples
between the two states. A process's mean fence time and device time per write therefore
lie between the two states and match neither.

#### Limits of these runs

A block carries over its state. The drive's flush time lasts for minutes and
crosses blocks, so ten processes are not ten independent draws, and the order of
ordinary, grouped, grouped and ordinary cancels a linear drift but not what the previous
process wrote. The embedding cell has no reader gate, and its worker called a
deterministic local provider.

<a id="bounded-full-text-checkpoint-on-forge"></a>

### Full-text checkpoint

The full-text index checkpoints in the background. A checkpoint synchronizes only the
files written since its last pass, from eight threads so that ext4 commits them together,
and repeats its unlocked pass up to four times while new files remain before it takes the
live writer lock for a final locked pass. Commits written faster than they can be
synchronized therefore still end in a locked pass. In this trace no tick needed the lock for
its I/O. The trace ran the mixed scenario of the full-text cell at 1.05M, one block of
an ordinary, two grouped and an ordinary process at a fixed 500 transactions/s and two
readers, with the debug target `sparkles::text::checkpoint`. A stall is a run of writes
slower than 25 ms that starts within 1 s of the last.

| Setting | Locked passes | Unlocked pass p50 (ms) | Stalls | Longest write (ms) | Max schedule lag (ms) | Write p99 (ms) | Reader p99 (ms) |
|---|---:|---:|---:|---:|---:|---:|---:|
| Ordinary | 0 | 27.7 | 0 | 19.5 | 31.0, 37.9 | 10.04, 9.41 | 7.69, 7.48 |
| Grouped | 0 | 31.8 | 0 | 18.6 | 10.1, 10.7 | 6.83, 6.70 | 7.51, 7.19 |

*Measured on forge on 2026-10-24. Each cell with two values gives the two processes of
that setting.*

A checkpoint that took the writer lock through a synchronization of every index file
stalled writes for up to about 170 ms every 5.3 s. The reader p99 of the current
checkpoint is 7.47 ms on average against 7.20 ms with the locked design, 3.8% higher over
four processes each, within the spread of these runs. The eight synchronizing threads may
compete with readers for the drive, and [roadmap](../../ROADMAP.md) item 170 follows it up.

### Reader tail of group commit

Four writers and two readers ran against the 10.5M dataset, six rounds per setting. In the
paced run both settings were held to 3,452 transactions/s, the most the ordinary writer
reaches. The paced run is the X09 acceptance gate, and the unpaced run shows the read cost
of the higher write rate that group commit allows.

| Measure | Unpaced, ordinary | Unpaced, group commit | Ratio | Paced, ordinary | Paced, group commit | Ratio |
|---|--:|--:|--:|--:|--:|--:|
| Transactions/s | 3,452.5 | 5,995.8 | 1.737 | 3,426.7 | 3,449.7 | 1.007 |
| Writer p50 (ms) | 1.126 | 0.634 | 0.563 | 1.119 | 0.712 | 0.636 |
| Reader p50 (ms) | 0.482 | 0.332 | 0.689 | 0.478 | 0.496 | 1.037 |
| Reader p99 (ms) | 4.639 | 5.039 | 1.086 | 3.980 | 3.986 | 1.002 |

*Measured on forge on 2026-10-16.*

### Concurrent writers with snapshot readers

Four writers insert 5,000 fresh quads while two readers each issue up to 25 queries per
second. Each read counts the inserted namespace and checks the count against the commit
snapshot named in the response. The state, the history and the reopened store are checked
afterwards.

| Four writers and two readers | Ordinary writer | Group commit |
|---|---:|---:|
| 1.05M, single-quad requests, transactions/s | 1,375–1,386 | **3,344–3,365** |
| 1.05M, writer p50 (ms) | 2.796–2.810 | **1.142–1.143** |
| 1.05M, reader p50 (ms) | 0.845–0.862 | **0.545–0.585** |
| 1.05M, 100-quad requests, effective changes/s | 74,231–75,187 | **118,627–123,253** |
| 10.5M, single-quad requests, transactions/s | 202–231 | **370–386** |
| 10.5M, writer p50 (ms) | 19.093–19.274 | **9.932–10.315** |
| 10.5M, reader p50 (ms) | 0.917–0.936 | 0.895–1.000 |
| 10.5M, reader p99 (ms) | **1.410–1.549** | 1.960–2.143 |

Readers keep their scheduled rate and see advancing snapshots throughout, so these runs
show reads proceeding under write load rather than saturated query throughput. The
100-quad runs at 1.05M last 41–67 ms and are short-burst observations.

<a id="http-write-snapshot"></a>

### Write throughput with one client

One client sends requests to a server on one logical CPU, starting from a compacted
1.05M store. Throughput counts effective changes, so it is not interchangeable with
transactions per second. Ranges span the fresh processes.

| One client | Changes per request | Effective changes/s | Request p50 (ms) | Request p99 (ms) |
|---|---:|---:|---:|---:|
| 1.05M, single-triple insert and delete churn | 1 | 1,046–1,062 | 0.856–0.870 | 1.536–1.609 |
| 1.05M, inserts of new literals | 100 | 53,979–59,414 | 1.493–1.515 | 3.964–7.118 |
| 1.05M, insert and delete batches | 100 | 54,972–58,586 | 1.516–1.548 | 2.857–3.981 |
| 10.5M, single-triple insert and delete churn | 1 | 320–494 | 2.154–2.351 | 5.989–7.733 |
| 10.5M, insert and delete batches | 100 | 45,788–46,123 | 1.613–1.622 | 16.070–16.364 |

On the same new-literal workload, two Oxigraph processes produced 34,641 and 58,853
effective changes per second, without synchronizing its WAL. The ranges overlap.

### Mixed load

Sixteen `star-join` readers run for 30 s with `oha` while one writer commits
single-triple `INSERT DATA` requests of new literals as fast as it can. Every request
opens a new connection. `mise run bench 100000 .artifacts/bench --mode mixed` reproduces it.

| Mixed load | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M: reads/s | **2197.9** | 80.6 | 522.9 | 36.5 | 15.7 |
| 1.05M: read p50 / p99 (ms) | **6.9 / 12.3** | 193.8 / 376.2 | 26.9 / 36.3 | 423.0 / 739.7 | 935.4 / 2027.4 |
| 1.05M: writes/s | 252.0 | 15.2 | 97.1 | 249.2 | **1276.9** |
| 1.05M: write p50 / p99 (ms) | 3.7 / 8.3 | 67.9 / 86.6 | 6.2 / 30.5 | 3.8 / 14.2 | **0.8 / 1.0** |
| 1.05M: failed reads / writes | 0 / 0 | 0 / 0 | 30034 / 1046 | 0 / 0 | 0 / 0 |
| 10.5M: reads/s | **197.1** | 5.7 | 74.7 | 3.4 | 0 |
| 10.5M: read p50 / p99 (ms) | **78.8 / 125.5** | 2704.1 / 3588.4 | 212.1 / 257.6 | 4284.9 / 6931.1 | — |
| 10.5M: writes/s | 210.5 | 18.1 | 47.5 | 237.8 | **565.1** |
| 10.5M: write p50 / p99 (ms) | 4.1 / 12.4 | 67.3 / 87.7 | 10.7 / 133.6 | 3.8 / 16.8 | **1.6 / 3.9** |
| 10.5M: failed reads / writes | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |

*Sparkles and Oxigraph at 1.05M were measured on forge on 2026-11-02 with Sparkles at commit
98f1728d, Sparkles at 10.5M on the same day, and the other cells on 2026-10-18 and
2026-10-19. The [run record](runs/2026-11-02-competitive-writes-forge/README.md) also holds
a pair at 1.05M with Sparkles at commit b62d3722, 2,101 reads and 246 writes per second
against Oxigraph's 12 and 1,264.*

Sparkles serves 4.2× QLever's reads at 1.05M and 2.6× at 10.5M. Oxigraph commits more
writes at both sizes while it serves far fewer reads. At 10.5M no Oxigraph reader finished
within 30 s. These forge runs kept every request on the performance cores of the hybrid
processor, which the placement below replaces, and [item 600](../../ROADMAP.md) measures
them again with it. On `atlas`, a host like `forge`, that placement committed 195 to 253
writes/s against 279 to 367 with `--request-cpus all`, as the [run record](runs/2026-11-02-changed-reads-and-placement-atlas/README.md)
shows. A separate run with the durable-commit harness above, with 16 readers on the
10.5M store, completed 299 new-literal commits per second at a median of 3.2 ms.

On a hybrid processor the server runs request work on the performance cores only while
at most one request per performance core is in flight. With sixteen readers it runs on
every CPU, and on atlas, an i5-13500 like forge, it answered 2,358.9 reads a second at
1.05M against 2,365.2 with `--request-cpus all` in five alternating pairs. The [hybrid
placement run](runs/2026-11-02-hybrid-mixed-atlas/README.md) records the measurements
and why confining every request to the performance cores lost about a third of the writes.

### Queries after updates

The `updates` mode applies the same 5,000 single-triple changes to each engine's store and
runs the query suite again. The changes stay in Sparkles' delta until a compaction folds
them into the base index. The reads that visit index blocks in parallel merge the delta
into the blocks it changes. At 10.5M the changed store takes 21.6 ms on `two-hop-count`,
8.1 ms on `contains` and 9.2 ms on `regex-iri`, against 17.5, 7.0 and 8.3 ms on the loaded
store in the full suite run. At 1.05M it takes 6.2 ms on `two-hop-count`, against
Fluree's 7.8 ms on its changed store and Sparkles' 5.1 ms on the loaded store. Before these
reads merged the delta, the changed stores took 8.2 and 35.8 ms on `two-hop-count` in the
same session.

In the full suite run on the changed stores, Sparkles was fastest on 27 of 28 queries at
both sizes, with Fluree ahead on `two-hop-count` before the change above. QLever's
changed-store `types-grouped` answer differs in value and is not ranked. Fluree's 10.5M run
on its changed store failed the same way as its run on the loaded store, with
`optional-chain` killed at the memory limit and `count-all` recorded as an error. That run
changed throughput from 239 to 217 q/s, against QLever's 85 q/s on its changed store.

*The four queries were measured on forge on 2026-11-02 with Sparkles at commits b62d3722 and
98f1728d, as the [run record](runs/2026-11-02-competitive-writes-forge/README.md) gives. The
full suite run was measured on forge on 2026-10-23 at commit 6699f27d, and its records for
[1.05M](../2026-10-23-synthetic-updates-1.05m-forge.json) and
[10.5M](../2026-10-23-synthetic-updates-10.5m-forge.json) have every sample.*

<a id="automatic-compaction-105m-triples"></a>

### Automatic compaction

This test sends 50,000 single-triple commits, 35,000 inserts and 15,000 deletes, to the
1.05M store, each as its own HTTP request.

| Query, median of 20 | With a 50,000-quad delta | After an automatic compaction |
|---|---:|---:|
| `types-grouped` | 5.8 ms | 1.9 ms |
| `predicate-counts` | 2.0 ms | 1.9 ms |
| `distinct-obj` | 7.8 ms | 2.2 ms |

That compaction took 2.0 s and held the writer lock for 5.7 ms. With `deltaRatio` 0.01,
two compactions ran during the 50,000 commits. They took 1.26 s and 1.44 s, carried 348
and 497 commits made during their builds into the new generation, and held the writer
lock for 3.4 ms and 3.3 ms. The commits made during a compaction had a p50 of 2.0 ms and
a p99 of 15.1 ms, against 2.7 ms and 21.0 ms in the run without compaction, so a
compaction does not slow the writes around it.

*Measured on the development laptop with other builds running, at a load average of about
38 on 16 cores, so the tails are noisy.*

<a id="other-measurements"></a>

### Validation on writes

Write-time validation checks only the focus nodes a write can affect. On 1.05M quads,
single-triple HTTP inserts take about the same time with validation off, in `warn` mode
and in `reject` mode, with or without 10,000 pending delta changes. Each mode had 20
samples, and its mean varies by several milliseconds between runs, so this test shows
incremental validation rather than a latency difference between modes.

| Single-triple insert, mean ms | off | warn | reject | full validation of the store |
|---|---:|---:|---:|---:|
| SHACL, plain write, no delta | 9.58 ± 1.13 | 6.76 ± 2.46 | 10.04 ± 2.05 | 478 to 485 |
| SHACL, write of a person, 10,000-change delta | 9.28 ± 2.06 | 8.90 ± 2.55 | 11.02 ± 2.49 | 488 to 489 |
| ShEx, plain write, no delta | 10.02 ± 2.25 | 10.63 ± 2.28 | 10.13 ± 2.26 | 1,091 to 1,432 |
| ShEx, write of a person, 10,000-change delta | 8.33 ± 2.84 | 11.37 ± 1.77 | 10.23 ± 2.69 | 1,046 to 1,398 |

<a id="spatial-index-commit-cost"></a>

### Spatial index commit cost

On the native store, a one-triple commit without geometry takes 0.025 ms with the spatial
index on and off, medians of 200. A commit of 1,000 `geo:asWKT` points takes 4.119 ms with
the index and 3.546 ms without it, medians of 20.

<a id="branches-and-merges-105m-triples"></a>

### Branches and merges

A release build served the 10.5M data. Each figure is one request's `time_total`, except
the queries, which are medians of 15 interleaved requests.

| Step | Time |
|---|---:|
| Create a branch of `main` at its head (four runs) | 19.5–32.7 ms |
| `star-join` on `main` | 38.3 ms |
| `star-join` on the new branch | 38.7 ms |
| Preview a merge of 10,000 inserted quads into a `main` with 10,000 of its own | 61.9 ms |
| That merge, committed | 114.9 ms |
| First request to the branch after a restart | 3.2 ms |

*Measured on the development laptop, which other builds kept at a load average of 35 to 85
on 16 cores.*

A new branch writes about 1.4 KB of files and takes about 70 KB on disk once its change
log opens, because it shares its upstream's index files until it compacts. A query on the
branch reads the same cached blocks as `main` and takes as long.

<a id="point-in-time-reads-105m-triples"></a>

### Point-in-time reads

A 1,000,000-triple base takes 50,000 single-quad commits in one generation, with history
retention covering every commit. Request times exclude process startup, and repeated rows
are medians of 20.

| Request | Time |
|---|---:|
| `COUNT(*)` at base + 25,000, first | 19.89 ms |
| The same, second / warm median | 1.04 / 0.95 ms |
| Selective read at base + 25,001, first / warm | 1.20 / 0.69 ms |
| `COUNT(*)` at the sealed base, first / warm | 18.12 / 0.67 ms |
| Sealed base, first after a restart (median of 3) | 14.40 ms |
| After a restart, at base + 25,000 | 21.39 ms |

A read starts from the nearest known state, found through a sparse WAL index, and replays
the commits in between, so a first read of an uncached state costs more than a repeat. In
the native store, a read of the middle of a 50,000-commit history takes 42.33 ms on first
use and 1.82 µs from the snapshot cache, a diff of one commit at the head 0.126 ms and of
100 commits 0.181 ms, and reopening with a replay of all 50,000 commits 221.51 ms.

### Diffs grouped by class

A base of 200,000 typed resources with one link each takes one commit of 1,000, 10,000 or
100,000 changes, half of them type triples of new resources and half links from those
resources to the base. `Diff::group_by_class` reads the classes of both ends of each
change, which here gives 30 groups. Times are the best of five, and
`cargo test --release -p sparkles-core --test diff_groups -- --ignored` runs it.

| Changes | Diff | Grouping by class |
|---:|---:|---:|
| 1,000 | 0.68 ms | 1.30 ms |
| 10,000 | 5.35 ms | 11.21 ms |
| 100,000 | 78.63 ms | 119.54 ms |

*Measured on a development laptop that was running other work, so the times are
approximate.*

Grouping costs about 1.2 µs per change on top of the diff, and it grows with the changes
rather than with the dataset.

<a id="backup-repositories-105m-triples"></a>

### Backup repositories

`mise run bench:backup` ran against the 10.5M database (299.8 MB) with a warm page cache
and an `fs` repository on the same ext4 file system. Each figure is the median and range
of 3 rounds, including process start.

| Case | Seconds | Size |
|---|---:|---|
| Full backup into an empty repository | 0.59 (0.56–0.61) | 299.8 MB logical, 281.1 MB stored, 509 MB/s |
| Incremental backup after 1,000 single-quad commits | 0.34 (0.33–0.34) | 55.2 KB added (3 new blobs, 29 reused) |
| Restore into a new directory, with the quick check | 0.81 (0.71–1.63) | 300.0 MB |
| Verify at level `data` | 0.21 (0.20–0.22) | 33 GETs |

An incremental backup stores only the appended tail of the update log and the files that
changed, so its size follows the writes since the last backup. S3, cold caches and
databases larger than memory were not measured.

### Incremental views

`crates/sparkles-core/tests/incr_bench.rs` measures Phase 1 of
[F13](../../specs/F13-incremental-views-and-provenance.md) on synthetic data in which each
person belongs to a team, has an age and knows two other people, and half the teams have a
label. The runs used a release build on atlas, an Intel Core i5-13500 with 20 threads,
with no other work on the host. The SNB, BSBM and fan-out benchmarks of the spec's §11 are
not built, and item 225 tracks them.

Each row below defines one view over an in-memory store, then makes 200 commits of one or
two quads each and times the round that brings the view up to date. A round includes
reading the changes, building the snapshot before them and the circuit's own work. The
query column is the median of 5 evaluations of the same query with the cache off.

| View | People | Query ms | Pass median ms | Pass p95 ms | Query / pass | State MiB |
|---|---:|---:|---:|---:|---:|---:|
| `team-sizes`, COUNT by team | 200,000 | 0.4 | 0.012 | 0.020 | 35 | 0.2 |
| `team-ages`, SUM and AVG by team over a join | 200,000 | 5.1 | 0.020 | 0.032 | 259 | 0.4 |
| `colleagues-known`, a three-pattern join | 200,000 | 9.8 | 0.193 | 0.313 | 51 | 0.0 |
| `labelled`, OPTIONAL label | 200,000 | 1.7 | 0.010 | 0.018 | 167 | 22.9 |
| `teams`, DISTINCT | 200,000 | 0.6 | 0.007 | 0.016 | 89 | 0.1 |
| `team-sizes` | 1,000,000 | 0.5 | 0.035 | 0.042 | 15 | 0.8 |
| `team-ages` | 1,000,000 | 27.1 | 0.096 | 0.115 | 282 | 1.8 |
| `colleagues-known` | 1,000,000 | 78.2 | 0.230 | 0.588 | 340 | 0.0 |
| `labelled` | 1,000,000 | 9.5 | 0.011 | 0.019 | 838 | 114.7 |
| `teams` | 1,000,000 | 4.0 | 0.008 | 0.017 | 492 | 0.5 |

The spec's target is a pass at least 100 times cheaper than evaluating the query again.
At a million people, four of the five views meet it. `team-sizes` misses it at both sizes
because the query engine answers a grouped COUNT over one pattern from the index in under
a millisecond. At 200,000 people, the join view and the two cheap queries miss it as well,
because the queries take only a few milliseconds there. `colleagues-known` stores no join
state, so each change probes the indexes, and its pass is the slowest. The `labelled`
view keeps its output rows, which is 115 MiB for 1M rows.

The commit path was measured on a persistent store of 50,000 people with two-quad
commits from one writer, using the default store options. Each figure is the median of
3,000 commits, in 3 rounds.

| Case | Commit median ms | Commit p99 ms |
|---|---:|---:|
| Nothing subscribed | 0.321–0.335 | 0.472–0.504 |
| One live query subscribed | 0.345–0.346 | 0.511–0.548 |

With nothing subscribed, a commit only checks an atomic counter, and its time matches a
build without views, which measured 0.329–0.336 ms. While a view or live query is
subscribed, each commit also copies its changes into the ring that passes read, which
adds about 4%. Item 226 tracks measuring it on forge and reducing it.

With 0, 10 and 100 views defined and a worker thread keeping them current, 2,000 commits
gave the following times. Each view had its own circuit.

| Views | Commit median ms | Commit p99 ms | Commits per second | Catch-up after the last commit ms |
|---:|---:|---:|---:|---:|
| 0 | 0.324 | 1.908 | 1,609 | 0 |
| 10 | 0.229 | 0.355 | 4,175 | 64 |
| 100 | 0.230 | 0.370 | 4,128 | 5,352 |

The writer does not wait for passes, so the commit time does not grow with the number of
views. The run with no views came first and paid for a cold page cache. With 100 views, the
single worker thread falls behind the writer and needs 5.4 seconds after the last commit to
catch up. Item 223 tracks passes on several threads.

On a dataset with RDFS on read or materialized inferences, a view evaluates its whole query
again with them, and item 221 tracks maintaining it incrementally. The test
`inferred_pass_against_query` in the same file adds a small schema to the data and, for the
materialized case, an inferred `rdf:type` per person. It then makes 100 commits that touch
each view and 100 that do not, and compares the round after a touching commit with a
one-off evaluation of the query. The query column below is the median of 5 one-off
evaluations with the same inference options and the cache off. Each figure is the median
of three runs. The ratio column takes the median of each run's ratio separately, so it
need not equal the quotient of the rounded time columns.

| View | People | Query ms | Round median ms | Round p95 ms | Median round / query | Round after an unrelated commit ms |
|---|---:|---:|---:|---:|---:|---:|
| RDFS, COUNT by team over a superproperty | 200,000 | 18.4 | 17.2 | 17.5 | 0.93 | 0.031 |
| RDFS, COUNT of a class reached by a domain | 200,000 | 6.0 | 6.1 | 6.2 | 1.01 | 0.041 |
| Materialized, COUNT by team | 200,000 | 2.0 | 2.1 | 2.2 | 1.07 | 0.011 |
| RDFS, COUNT by team over a superproperty | 1,000,000 | 215.4 | 195.6 | 209.3 | 0.93 | 0.044 |
| RDFS, COUNT of a class reached by a domain | 1,000,000 | 40.5 | 40.8 | 41.0 | 1.00 | 0.053 |
| Materialized, COUNT by team | 1,000,000 | 9.8 | 10.3 | 10.4 | 1.05 | 0.011 |

These runs used a release build in the Nix development shell on forge, an Intel Core
i5-13500, pinned to CPUs 0–11 with no other work on the host.

A round after a touching commit costs about one evaluation of the query. A round after a
commit outside the rewritten query's footprint skips the view and takes at most 0.053 ms
in these medians. A view of the materialized class count ran on the 34 commits that changed
the inferred graph and skipped the other 166. Its 95th percentile round took 3.8 ms at
200,000 people and 12.6 ms at 1,000,000 people, against one-off queries of 3.8 ms and
12.5 ms.

The persistent commit-path comparison remains inconclusive. It used the same forge host,
after SSD trimming, with matched release compilers and three A,B,B,A groups. Each process
made three rounds of 3,000 two-quad commits without a subscription, then three with a live
query subscribed. The medians of the unsubscribed rounds ranged from 0.272 to 1.611 ms
on the reference build and from 0.274 to 1.604 ms on the build with inferred views. The
ratio of group medians changed from 3.40 to 0.30 and 0.18 across the three groups, so the
run does not isolate a cost of the inference change. Item 265 tracks profiling this
latency variation and obtaining a stable comparison.

Syscall tracing of four more processes in A,B,B,A order, after SSD trimming on forge,
located the large variation inside writes to the WAL and delta vocabulary. Blocks of
3,000 aligned writes had median syscall times between about 0.24 and 1.55 ms. Both
builds switched between faster and slower writes within a process. Tracing changes the
timings, so these figures diagnose where the variation occurs rather than measure the
inference change's cost. Item 265 requires a comparison with matched durable-write
latency before attributing a difference to the engine.

#### Provenance annotations

The provenance annotations of
[F13 §5](../../specs/F13-incremental-views-and-provenance.md#provenance-annotations) were
measured with the same views and commit mix at 200,000 people, on an ephemeral Namespace
instance with 8 AMD EPYC vCPUs and nothing else running. The
[run record](runs/2026-11-01-provenance-annotations-nsc/README.md) holds the raw output,
the builds' checksums and the script. Each figure is the median of three runs.

The test `annotation_cost` attaches each view's query as an internal circuit that keeps
its rows, once without provenance and once with each annotation, and times 200 rounds.

| View | Provenance | Build ms | Pass median ms | Pass p95 ms | State MiB |
|---|---|---:|---:|---:|---:|
| `team-sizes` | none | 40.4 | 0.006 | 0.016 | 0.2 |
| `team-sizes` | count | 54.5 | 0.013 | 0.020 | 0.3 |
| `team-sizes` | lineage | 197.8 | 0.024 | 0.085 | 15.8 |
| `team-sizes` | why | 205.4 | 0.027 | 0.039 | 1.4 |
| `team-ages` | none | 36.7 | 0.013 | 0.027 | 0.4 |
| `team-ages` | count | 138.9 | 0.021 | 0.038 | 0.5 |
| `team-ages` | lineage | 377.2 | 0.038 | 0.061 | 31.5 |
| `team-ages` | why | 355.8 | 0.036 | 0.057 | 2.3 |
| `colleagues-known` | none | 9.2 | 0.126 | 0.244 | 0.0 |
| `colleagues-known` | count | 2,212.3 | 0.014 | 0.029 | 0.0 |
| `colleagues-known` | lineage | 2,195.8 | 0.014 | 0.025 | 0.0 |
| `colleagues-known` | why | 2,208.7 | 0.014 | 0.026 | 0.0 |
| `labelled` | none | 55.7 | 0.007 | 0.019 | 22.9 |
| `labelled` | count | 89.6 | 0.009 | 0.020 | 47.3 |
| `labelled` | lineage | 276.5 | 0.010 | 0.022 | 85.7 |
| `labelled` | why | 266.5 | 0.011 | 0.028 | 87.3 |
| `teams` | none | 17.6 | 0.006 | 0.014 | 0.1 |
| `teams` | count | 22.6 | 0.006 | 0.015 | 0.1 |
| `teams` | lineage | 94.5 | 0.007 | 0.016 | 8.0 |
| `teams` | why | 96.4 | 0.007 | 0.016 | 0.8 |

In these medians of three runs, an annotated pass was at most 0.025 ms slower than a
plain one. Single runs vary more, and one run of `team-sizes` with `lineage` measured
0.054 ms.
`count` keeps an integer beside each row of each state, which doubles the state of
`labelled` with its 200,000 output rows. `lineage` keeps every atom that some derivation
of a row uses, so a group row lists all of its members' quads, and its state is 4 to 80
times the plain state. `why` keeps at most 8 supports per row, and its state is 4 to 8
times the plain state. This memory buys exact deletions without evaluating the query
again.

An annotated build matches its patterns with index probes, since the query engine does
not report the quads behind a row. It takes 1.3 to 10 times as long as a plain build, and
240 times as long for the three-pattern join of `colleagues-known`, where the plain build
uses the engine's joins. Item 531 tracks faster annotated builds. The annotated passes
of `colleagues-known` are 9 times faster than its plain passes, because the annotated
path reads the pattern with the fewest estimated quads next while the plain path reads the
one with most bound positions. Item 530 tracks the same order for plain passes.

Views without annotations kept their cost within the noise of this host, except
possibly for one view. In three alternating runs of `pass_against_recompute`, the pass
medians of `team-ages`, `labelled` and `teams` were within 0.001 ms. Those of
`colleagues-known` ranged from 0.109 to 0.124 ms on the baseline and from 0.119 to
0.124 ms on the candidate. `team-sizes` measured 0.008 ms in two of the baseline's runs
and 0.010 ms in two of the candidate's. That is a difference of 2 µs in a path whose
plain operators did not change, and item 529 tracks checking it on forge. A query screen
on Namespace with 8 rounds compared the server of the baseline with that of `5368bd97`,
which precedes the two measured fixes, and the commits after it change only incremental
circuits. It gave a geometric mean of 0.987, with no query slower by 10% or more.
`types-grouped` measured 6.4% slower at 0.017 ms and `order-by-full` 2.0% slower, and
Item 529 covers both.

#### Property path views

`path_pass_against_recompute` in `crates/sparkles-core/tests/incr_bench.rs` measures the
property paths of [F13 §4.3](../../specs/F13-incremental-views-and-provenance.md#fixed-point-operator).
It adds a tree of units with a fan-out of 4 over a tenth of the people, with each person
in one unit, to the data of the other tests. `reach-one` is `:p0 :knows+ ?q`, which
reaches nearly every person, `unit-ancestors` is `?u :partOf+ ?a` over the whole tree,
`under-root` is `?u :partOf* :u0`, and `people-under` counts the people under each unit
through a join with `:partOf+`. Each view then takes 200 commits that cycle through five
kinds: a `knows` edge to a new person, the removal of a `knows` edge, a unit moved under
another unit, a new unit with a member, and an unrelated quad. Two commits in five
therefore change a view's path, and the median pass of a recomputed view mostly measures
the commits it skips.

The runs used one 8-vCPU Namespace instance at 200,000 people, with each process pinned
to 6 CPUs. The baseline is commit `0dc27215`, where paths with `+` and `*` are `recompute`
nodes, and the candidate maintains them with the fixed-point operator. Each figure is the
median over five processes of each build, which ran in alternating order, and every
view's rows matched its query evaluated again.

| View | Query ms | Recompute pass p95 ms | Maintained pass median ms | Maintained pass p95 ms | Build ms, recompute then maintained | State MiB, recompute then maintained |
|---|--:|--:|--:|--:|--:|--:|
| `reach-one` | 16.0 | 59.6 | 0.002 | 0.027 | 53 then 176 | 18.3 then 17.9 |
| `unit-ancestors` | 43.4 | 73.2 | 0.002 | 0.067 | 68 then 16 | 14.4 then 16.0 |
| `under-root` | 0.70 | 2.30 | 0.006 | 0.023 | 2.9 then 7.7 | 1.8 then 2.0 |
| `people-under` | 61.4 | 82.2 | 0.026 | 0.235 | 664 then 523 | 16.3 then 17.8 |

A pass that changes a path costs 0.02 to 0.24 ms where evaluating the query again costs
2.3 to 82 ms, between 100 and 2,200 times less. A deleted `knows` edge inside the large
component of `reach-one` keeps every person that another path still reaches, so its pass
checks only the walks recorded through the edge. The maintained state is up to 11% larger,
since the operator keeps each reachable pair with its record, about 40 bytes, beside the
rows that a recomputed view also keeps. This memory buys passes that follow only the
walks a change reaches.

The maintained build of `reach-one` takes 3.3 times as long as the recomputed one, and that
of `under-root` 2.6 times, because the operator records a derivation for each reachable
node where the query engine only walks. Builds of the other two views are faster, since
the recomputed build of `unit-ancestors` evaluates the path from every node. Item
548 tracks faster builds of the operator. The five plain views of
`pass_against_recompute` kept their pass medians and p95 within the range of the five
processes, and no metric of the other tests moved by more than the spread of the runs.

The [run record](runs/2026-11-01-property-paths-nsc/README.md) holds the raw output of
every process, the comparison of the two builds and the commands. It also holds three
runs of the overdeletion experiment that
[PERFORMANCE-EXPERIMENTS.md](../../PERFORMANCE-EXPERIMENTS.md#incremental-views) records.

### Aggregates and correlated EXISTS in views

The ignored test `aggregate_pass_against_recompute` of
`crates/sparkles-core/tests/incr_bench.rs` measures the operators of
[F13 §4.2](../../specs/F13-incremental-views-and-provenance.md#aggregates-and-correlated-exists)
on the people data of the other view tests, with the same 200 commits of one or two quads
each and the same columns. Every team has at most one label, so the `GROUP_CONCAT` and
`SAMPLE` of `team-labels` have one answer per snapshot and the maintained rows are
checked against the query. `knows-older` keeps the people who know someone older, which
is a correlated `EXISTS` with the outer age in a filter of its pattern. An age inserted for
a new person touches its `?q :age ?b` pattern, which names no outer variable, so that
commit evaluates the condition again for every row, and the p95 shows it while the
median does not.

AGG_TABLE

### Incremental view gate baseline

`mise run bench:incr` is the rewrite gate of incremental view maintenance, as
[BENCHMARKING.md](../../BENCHMARKING.md#incremental-view-maintenance) describes. Its
baseline ran on forge alone at commit 64391e7f, whose engine code is that of 04d49b57,
with 200,000 people and 12 Rayon threads in five processes. Every view's maintained rows
matched its query evaluated again at the same commit, and every check of the record
passed. Each figure is the median over the five processes, and the record keeps each
process's value and their range.

| View | Build ms | Query ms | Pass median ms | Pass p95 ms | Advance µs |
|---|--:|--:|--:|--:|--:|
| team-sizes | 50.3 | 0.389 | 0.013 | 0.023 | 4.28 |
| team-ages | 62.2 | 4.76 | 0.024 | 0.036 | 7.09 |
| colleagues-known | 13.2 | 9.53 | 0.196 | 0.285 | 192 |
| labelled | 89.3 | 1.82 | 0.011 | 0.021 | 4.62 |
| teams | 22.1 | 0.642 | 0.008 | 0.016 | 3.40 |

A pass after one commit costs between 0.008 and 0.2 ms, against 0.4 to 9.5 ms for
evaluating the query again. The rounds of views over inferred data took 17.5 ms for the
RDFS team sizes, 6.3 ms for the RDFS agents and 2.1 ms for the materialized team sizes,
about one evaluation of each query, and a commit outside a view's footprint cost 0.011 to
0.041 ms.

The commit timings are not a useful gate on forge. A commit's median latency was either
about 0.25 ms or about 1.6 ms, and which one a test got changed between processes and
between tests of one process, as item 265 found for the commit path. Four of the five
processes ran most commit tests at the slower latency, so the median of the 0-view commit
test is 1.60 ms with a range of 0.28 to 1.70 ms. Because a flagged metric needs the
candidate's whole range on the losing side, these wide ranges flag only a slowdown that
every process shows. Catching up 100 views after their commits took 2.0 to 4.8 s.

The [run record](runs/2026-11-01-incr-gate-forge-64391e7f/record.json) holds every value,
the answer fingerprints, the settings, the compilers and the build configuration, with
the raw lines and logs of the five processes and the
[commands](runs/2026-11-01-incr-gate-forge-64391e7f/run.sh.txt).
