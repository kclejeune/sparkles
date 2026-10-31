# Group commit on indexed datasets, atlas run

These campaigns measured the cost of a group commit fence, the serial spatial commits at
10.5M, four writers at 1.05M and the spatial readers under mixed load at 1.05M, which the
[X09 Outcome](../../../../specs/X09-group-commit-by-default.md#phase-4) reports. They ran on
`atlas` on 2026-10-31.

The host is an Intel Core i5-13500 with 20 logical CPUs, 15 GiB of RAM and Linux 7.2.9. Its
drive is the SK hynix HFS512GEJ9X102N that `forge` also has, under LUKS, LVM and ext4 as on
`forge`, with a volatile write cache and FUA support. `devices.json`, `filesystem.json` and
`cpu.json` in each campaign record the stack. Every campaign took the host's heavy-job lock
before it started, and no build or benchmark of another lane ran while it held the lock.

The engine binary is a release build of `mbx build --release -p sparkles-server --bin
sparkles` from a source archive of commit `a202e8f2`, built on `atlas` before timing, with
the SHA-256 `7eef8bf5fb57bcf877e7520a86d0ecb81fe6427bc3e44e81a2ebb88c72c90e6c`. The
`write-bench` client has the SHA-256
`775cc8f5f81d730674cc6db0ae79b80eee6e95aeafac6e7c7f83c97d7ae65ea5`. The harness is
`scripts/bench-indexed-commit.py` of commit `d54506b7`, whose SHA-256 each `manifest.json`
records. Campaign `e1` ran with the harness of `a202e8f2`. The server ran on CPUs 0 to 11
and the client on CPUs 12 and 13.

Every process started from a fresh copy of the seed store, probed the drive with 64 writes
and `fdatasync` calls first, checked every acknowledgement and the index answers live and
after a cold reopen, and recorded the server's metrics and `/proc/diskstats` before and
after its measured interval. A process that failed a check would have made the campaign
fail, and none did.

The campaign directories hold the harness's `results.json`, `gate-summary.json` and
`completed.json`, the seed and calibration records, and the summaries below. The per-process
directories with metrics, client output and logs are kept outside the repository.

## Campaigns

| Directory | What it measured |
|---|---|
| `e1-main-geo100k` | One A, B, B, A block of serial literal inserts and four writers on the spatial dataset at 1.05M, as a first look at the fence. |
| `ab1-flush-100k` | Three blocks of four writers at 1.05M on the spatial and the full-text dataset, comparing the grouped path of `a202e8f2` (A) with that of `06f16cf9` (B), which let the WAL's `fdatasync` flush the new terms. `--compare-binary` ran it, so it has no gate. |
| `g1-geo10m-serial` | Five blocks of serial literal, IRI and delete commits on the spatial dataset at 10.5M under the gates of X09 section 17.9. |
| `cpu-geo10m-iri`, `cpu-geo10m-geo` | Three rounds of `perf stat` user instructions and cycles per serial commit at 10.5M, in the order ordinary, grouped, grouped, ordinary, with `tools/cpuab.sh`. |
| `g2-geo1m-mixed` | Five blocks of the spatial mixed cell at 1.05M with the rate calibrated for the drive's slow state, with `--slow-sync-ms 1.6`. |
| `g3-writers-1m` | Five blocks of four writers at 1.05M on the spatial and the full-text dataset under the gates. |
| `g4-geo1m-mixed` | Five blocks of the spatial mixed cell at 1.05M again, at the rate that `g2` calibrated, given with `--write-rate 466.05150521597045`. |

`processes.txt` comes from `tools/runsum.py` and gives, for every process, the transactions
per second, the latency, the mean synchronization time of each path, the commits per fence,
the device writes and flushes per commit, the drive probe and the device time per write and
per flush. `pairs.txt` comes from `tools/abpairs.py`, `report.txt` from
`scripts/bench-indexed-report.py` and `cpu-summary.txt` from `tools/cpusum.py`.
`drive-probes.txt` holds two runs of `tools/syncprobe2.py`, which timed the synchronization
shapes of a fence and of an ordinary commit on the idle drive.

## Results

The fence ran two device flushes, one per file, while the ordinary path ran none, as
`fl/c` in `e1-main-geo100k/processes.txt` shows. The single-flush build of `ab1` halved the
flushes and gave paired transactions-per-second ratios of 1.076 for the spatial and 0.927
for the full-text dataset, with fences as long as before, so it was not kept.

All six serial checks of `g1` were met. The paired ratios of grouped to ordinary are 0.984
and 0.973 at p50 and p90 for literal inserts, 0.999 and 1.000 for IRI inserts, and 1.016
and 1.019 for deletes. The instructions per commit of `cpu-summary.txt` are 0.1% higher
with grouping for IRI inserts and 0.3% higher for geometry inserts.

The calibration of `g2` measured 907 transactions/s for the unpaced ordinary writer with a
mean synchronization of 0.989 ms, which gives a slow-state capacity of 583.6 and 582.6
transactions/s for the two ordinary calibration processes at 1.6 ms. The offered rate was
80% of the smaller, 466.05 transactions/s. Blocks 0 to 3 kept the schedule within 10 to
20 ms in both settings, with reader p50 and p99 paired ratios of 1.000 and 1.012. In block 4
every process, ordinary and grouped, saw device writes take 7 to 12 ms on average and fell
behind its schedule by 658 to 2,496 ms, so the harness reported a schedule lag failure for
the cell. The stall was the same for both settings and began between processes, which
points to I/O from outside the campaign, so `g4` repeated the cell at the same rate.

Every process of `g4` kept its schedule, with a largest lag of 11.3 to 21.0 ms for the
ordinary and 11.1 to 13.6 ms for the grouped processes, and every gate of the cell was met.
The paired reader p50 and p99 ratios were 0.999 and 1.005, and the certified peak resident
memory difference was −2.2 MiB.

Both cells of `g3` met the throughput and memory gates. The paired ratios of transactions
per second were 2.253 for the spatial dataset and 1.805 for the full-text dataset, against
a required gain of 1.5, and the certified peak resident memory differences were −1.4 and
8.4 MiB.
