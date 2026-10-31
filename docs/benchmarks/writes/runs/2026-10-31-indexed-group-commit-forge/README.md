# Group commit on indexed datasets, forge run

These campaigns judged the gates of
[X09 section 17.9](../../../../specs/X09-group-commit-by-default.md#179-performance-gates) that the
campaigns of 2026-10-23 and 2026-10-24 left missed or not established. They are the serial
spatial commits at 10.5M, four writers at 1.05M on the spatial and full-text datasets, and
the spatial readers under mixed load at 1.05M with a rate calibrated for the drive's slow
state. They ran on `forge` on 2026-10-31. The
[atlas run](../2026-10-31-indexed-group-commit-atlas/README.md) of the same day measured
the same cells and the fence cost.

The host is `forge`, an Intel Core i5-13500 with 20 logical CPUs and the SK hynix
HFS512GEJ9X102N drive under LUKS, LVM and ext4. `devices.json`, `filesystem.json` and
`cpu.json` in each campaign record the stack. Another lane measured on `forge` the same
day, so every campaign waited through `tools/runlocked.sh` until the host's heavy-job
lock was free and no other process used more than half a CPU, and then held the lock until
it finished. `host-at-start.txt` in each campaign is the process list when it started.

The engine binary is a release build of `mbx build --release -p sparkles-server --bin
sparkles` from a source archive of commit `a202e8f2`, built on `atlas` and copied to
`forge`, with the SHA-256
`437867bd4c39651dbd17a87a9399f10dc05c37f7bb94dc89d2fa43786b8e129b`. The `write-bench`
client has the SHA-256 `9735fda8378e95dcec3fc8c60baefc420644973e467996ba8f1a6fd539071da6`.
The harness is `scripts/bench-indexed-commit.py` of commit `c600c9ad`. `tools/chain.sh`
gives the three commands. Each campaign's log ends with `exit 0`. The copies of `tools/runlocked.sh` and `tools/chain.sh` here were later changed to return the harness status and stop the chain on a failure, which the runs did not need. The server ran on CPUs 0 to 11 and the client on CPUs 12 and 13.

Each cell had five blocks in the order ordinary, grouped, grouped, ordinary. Every process
started from a fresh copy of the seed store, probed the drive first, checked every
acknowledgement and the index answers live and after a cold reopen, and recorded the
server's metrics and `/proc/diskstats`. No process failed a check. The campaign
directories hold the harness's `results.json`, `gate-summary.json` and `completed.json`,
the seed and calibration records, `report.txt` from `scripts/bench-indexed-report.py` and
`processes.txt` from the atlas run's `tools/runsum.py`.

## Results

| Directory | Cell | Result |
|---|---|---|
| `f1-geo10m-serial` | Spatial at 10.5M, serial | Paired p50 and p90 ratios of 1.002 and 1.012 for literal inserts, 1.001 and 0.992 for IRI inserts, and 0.997 and 1.005 for deletes. All six checks met. |
| `f2-writers-1m` | Four writers at 1.05M | Paired ratios of transactions per second of 1.564 for the spatial and 1.564 for the full-text dataset, against 1.257 and 1.240 required. Peak resident memory differences of 3.5 and 1.3 MiB. Met. |
| `f3-geo1m-mixed` | Spatial readers at 1.05M | The rate was 477.9 transactions/s. Largest schedule lags of 3.3 to 8.5 ms ordinary and 3.6 to 8.5 ms grouped. Paired reader p50 and p99 ratios of 1.028 and 1.003. Peak resident memory difference of −3.7 MiB. Met. |

The drive spent most of these campaigns in its fast state. A serial commit synchronized in
about 0.25 ms. The ordinary writers synchronized in 0.35 to 0.72 ms, and the fences took
0.61 to 0.89 ms and carried 1.80 to 1.87 commits. The calibration of `f3` measured 1,535
and 1,538 transactions/s for the unpaced ordinary writers with a mean synchronization of
0.58 ms, which gives a slow-state capacity of 597.4 transactions/s at 1.6 ms, and offered
80% of it.
