# Write-through group commit fences, forge run

These campaigns measured the fences of roadmap item 446, which write a prefix's log
records and new terms through the `O_DIRECT` and `O_DSYNC` handles instead of
synchronizing both files with `fdatasync`, against the fences before it, and checked the
throughput and memory gates of
[X09 section 17.9](../../../../specs/X09-group-commit-by-default.md#179-performance-gates)
for four writers at 1.05M. They ran on `forge` on 2026-10-31.

The host is `forge`, an Intel Core i5-13500 with 20 logical CPUs and the SK hynix
HFS512GEJ9X102N drive under LUKS, LVM and ext4. `devices.json`, `filesystem.json` and
`cpu.json` in each campaign record the stack. Every campaign waited through
`tools/runlocked.sh` until the host's heavy-job lock was free and no other process used
more than half a CPU, and the harness then held the lock until it finished.
`host-at-start.txt` in each campaign is the process list when it started, and `exit.txt`
holds the harness's exit status, 0 for each.

The engine binaries are release builds of `mbx build --release -p sparkles-server --bin
sparkles` from source archives, built on the laptop and copied to `forge`. Commit
`8127961f`, whose fences write and flush, has the SHA-256
`7fa2222d797147792721acd0a6ebb4f550833d67233866b49e9e62516e90fa9f`. Commit `3da475a9`,
whose fences write through, has the SHA-256
`5f645168675418701e04ebad630e372ff1fde7efc9c2c4c5fce33991165c6dec`. The later commits of
the item change only test code and the memory ordering of two counters, which compiles to
the same instructions on x86-64. The `write-bench` client has the SHA-256
`09f1991ab56a64d25e8c2fe6f93d71bb3a915f3ab8d8799327cbd0608707d5c9`. The harness is
`scripts/bench-indexed-commit.py` of commit `8127961f`, which the item did not change.
`tools/chain.sh` gives the three commands. The server ran on CPUs 0 to 11 and the client on
CPUs 12 and 13.

Each campaign had blocks of four processes in the order A, B, B, A. In `ab1` and `ab2`, A
is the grouped path of `8127961f` and B the grouped path of `3da475a9`, through the
harness's `--compare-binary`. In `g1`, A is the ordinary path and B the grouped path of
`3da475a9`. Every process started from a fresh copy of the seed store, probed the drive
first, checked every acknowledgement and the index answers live and after a cold reopen,
and recorded the server's metrics and `/proc/diskstats`. No process failed a check. The
campaign directories hold the harness's `results.json` and `completed.json`, the seed
records, `processes.txt` from `tools/runsum.py`, and `report.txt` from `tools/abpairs.py`
for `ab1` and `ab2` and from `scripts/bench-indexed-report.py` with `gate-summary.json` for
`g1`.

## Results

| Directory | Cell | Result |
|---|---|---|
| `ab1-writers-1m` | Four grouped writers at 1.05M, five blocks | Spatial: 1,233 (1,157–3,754) against 2,614 (2,458–2,681) transactions/s, a paired ratio of 2.053, and a paired fence time ratio of 0.406. Full-text: 1,436 (1,162–3,098) against 1,471 (1,280–2,459), a paired ratio of 1.020, and a fence time ratio of 0.968. |
| `g1-writers-1m` | Ordinary against grouped at 1.05M, five blocks | Spatial: 741 against 1,581 transactions/s, a paired ratio of 2.135. Full-text: 726 against 1,433, a paired ratio of 1.927. Both against 1.5 required. Certified peak resident memory differences of 1.4 and 7.3 MiB. Met. |
| `ab2-serial-1m` | Serial spatial literal inserts at 1.05M, three blocks | Paired p50 and p90 ratios of 1.005 and 0.996. |

In `processes.txt`, `group` is the mean fence synchronization and the number of fences,
`size` the commits per fence, `w/c` and `fl/c` the device writes and cache flushes per
commit, `wms/w` and `fms/f` the device time per write and per flush, and `probe` the
drive's write and flush times before the process started. The fences of `8127961f` issued
1.00 to 1.14 flushes per commit and those of `3da475a9` 0.001 to 0.005. In `ab1`, six
spatial processes of `8127961f` ran with the drive in its slow state, with fences of 1.48
to 1.67 ms, and four in its fast state, with fences of 0.44 to 0.67 ms and 2,566 to 3,754
transactions/s. Every spatial process of `3da475a9` had fences of 0.58 to 0.64 ms, whose
device writes took about 0.47 ms each. The drive was in its slow state through `g1`, where
an ordinary commit synchronized in 0.95 to 1.47 ms.
