# Writes and updates results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Comparison summary

The focused [write assessment](../writes/RESULTS.md#serial-writes-with-reader-work)
  puts new-literal commits near 1.2 ms for Sparkles and 0.6 ms for unsynced Oxigraph at
  10.5M. Sparkles synchronizes before acknowledgement. With sixteen readers, Oxigraph
  completes 2.1–3.0× as many writes but no reads in the thirty-second 10.5M window,
  against Sparkles' 232–245 reads/s. The separate suite churn table records 878 and
  909 Sparkles commits/s at 1.05M and 10.5M under its own contract.

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

The comparative suite's `updates` mode applies the same 5,000 single-triple INSERT and
DELETE requests, one at a time, to every engine's store. No engine failed a commit.
`mise run bench 100000 .artifacts/bench --mode updates` reproduces it.

| Churn of 5,000 commits | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M: commits/s | **878** | 16 | 423 | 259 | 771 |
| 1.05M: latency p50 / p99 (ms) | **1.20 / 2.06** | 70.93 / 88.19 | 2.27 / 5.78 | 3.17 / 25.22 | 1.25 / 2.07 |
| 10.5M: commits/s | **909** | 26 | 387 | 361 | 715 |
| 10.5M: latency p50 / p99 (ms) | **1.10** / 2.74 | 19.75 / 90.27 | 2.48 / 6.30 | 1.69 / 6.46 | 1.35 / **2.14** |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-18 and 2026-10-19.*

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

*The serial, writer and memory cells were measured on forge on 2026-10-23 at commit
5b40d0ec, the mixed cells on forge on 2026-10-24 with the bounded full-text checkpoint, and
the reruns on forge and atlas on 2026-10-31 at commit a202e8f2.*

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

The fence's synchronization explains the smaller gain in the slow state. A fence writes
with buffered writes and an `fdatasync` of the log and of the vocabulary, which is two
device cache flushes, while an ordinary commit writes both files through `O_DIRECT` and
`O_DSYNC` with no flush. Idle, the drive takes 0.30 ms for a write and its flush and 0.24 ms for a
write-through write. In its slow state these take about 1.0 and 0.9 ms, and the fence's
two flushes in parallel take 1.3 to 1.5 ms. A fence that let the log's flush cover the
vocabulary measured no faster, as [PERFORMANCE-EXPERIMENTS.md](../../PERFORMANCE-EXPERIMENTS.md#writes)
records. Serial commits take the ordinary path in both settings, and `perf stat` counted
0.1% more user instructions per serial IRI insert with grouping and 0.3% more per geometry
insert, the admission checks of the group.

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
| 1.05M: reads/s | **2491.3** | 80.6 | 522.9 | 36.5 | 15.0 |
| 1.05M: read p50 / p99 (ms) | **6.1 / 10.9** | 193.8 / 376.2 | 26.9 / 36.3 | 423.0 / 739.7 | 1014.2 / 1855.0 |
| 1.05M: writes/s | 374.2 | 15.2 | 97.1 | 249.2 | **1272.6** |
| 1.05M: write p50 / p99 (ms) | 2.3 / 6.4 | 67.9 / 86.6 | 6.2 / 30.5 | 3.8 / 14.2 | **0.8 / 1.0** |
| 1.05M: failed reads / writes | 0 / 0 | 0 / 0 | 30034 / 1046 | 0 / 0 | 0 / 0 |
| 10.5M: reads/s | **244.1** | 5.7 | 74.7 | 3.4 | 0 |
| 10.5M: read p50 / p99 (ms) | **64.6 / 84.3** | 2704.1 / 3588.4 | 212.1 / 257.6 | 4284.9 / 6931.1 | — |
| 10.5M: writes/s | 265.8 | 18.1 | 47.5 | 237.8 | **565.1** |
| 10.5M: write p50 / p99 (ms) | 3.5 / 8.0 | 67.3 / 87.7 | 10.7 / 133.6 | 3.8 / 16.8 | **1.6 / 3.9** |
| 10.5M: failed reads / writes | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-18 and 2026-10-19.*

Sparkles serves 4.8× QLever's reads at 1.05M and 3.3× at 10.5M. Oxigraph commits more
writes at both sizes while it serves far fewer reads. At 10.5M no Oxigraph reader finished
within 30 s. A separate run with the durable-commit harness above, with 16 readers on the
10.5M store, completed 299 new-literal commits per second at a median of 3.2 ms.

### Queries after updates

The `updates` mode applies the same 5,000 single-triple changes to each engine's store and
runs the query suite again. The changes stay in Sparkles' delta until a compaction folds
them into the base index. At 10.5M the delta changes `two-hop-count` from 17.5 to
36.9 ms, `contains` from 7.0 to 15.6 ms and `regex-iri` from 8.3 to 18.0 ms, changes
`star-join` from 36.7 to 39.0 ms, and changes throughput from 239 to 217 q/s, against
QLever's 85 q/s on its changed store. On the changed stores Sparkles is fastest on 27 of
28 queries at both sizes. QLever's changed-store `types-grouped` answer differs in value
and is not ranked. Fluree is ahead on `two-hop-count` by 1.10× at 1.05M, 7.8 against
8.6 ms, and by 1.06× at 10.5M. Fluree's 10.5M run on its changed store failed the same way
as its run on the loaded store, with `optional-chain` killed at the memory limit and
`count-all` recorded as an error.

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d. The records for
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
