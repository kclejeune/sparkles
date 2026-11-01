# Written-through annotation records, forge run

These campaigns measured the fences of roadmap item 501, which write a prefix's annotation
records through the `O_DIRECT` and `O_DSYNC` handle of `annotations.bin` instead of
synchronizing the file with `fdatasync`, against the fences before it, with four grouped
writers whose commits all carry a commit message. They ran on `forge` on 2026-11-01.

The host is `forge`, an Intel Core i5-13500 with 20 logical CPUs and the SK hynix
HFS512GEJ9X102N drive under LUKS, LVM and ext4. `devices.json`, `filesystem.json` and
`cpu.json` in each campaign record the stack. Every campaign waited through
`tools/runlocked.sh` until the host's heavy-job lock was free and no other process used
more than half a CPU, and the harness then held the lock until it finished.
`host-at-start.txt` in each campaign is the process list when it started, and `exit.txt`
holds the harness's exit status, 0 for each.

The engine binaries are release builds of `mbx build --release -p sparkles-server --bin
sparkles` from source archives, built on `wally` and copied to `forge`. Commit `4bd70845`,
whose fences synchronize `annotations.bin` with `fdatasync`, has the SHA-256
`b7a2a18b869854cbaebe76a8e9672d97c8cc298349b60fa5e551248b1780028f`. Commit `4082759b`,
whose fences write the records through, has the SHA-256
`4d6e7d2eca39609fc667f6b84c061a3df1c277206334dbdc9f61565c791d1d61`. The later commits of
the item cut the preallocated zeros at close, take a failed annotation write back and count
the zeros in the quota, which adds one mutex acquisition and one atomic addition to each
fence. The `write-bench` client has the SHA-256
`7b95d22ad87f3ea8d2d496b3c6ea51d94b313d4dae316c5605b59b577b1d8433`. The harness is
`scripts/bench-indexed-commit.py` of commit `d96dd323`, whose `--message` option makes the
client send a commit message with every commit. `tools/chain.sh` gives the two campaign commands.
The server ran on CPUs 0 to 11 and the client on CPUs 12 and 13.

Each campaign had blocks of four processes in the order A, B, B, A, where A is the grouped
path of `4bd70845` and B the grouped path of `4082759b`, through the harness's
`--compare-binary`. Every process started from a fresh copy of the seed store, probed the
drive first, checked every acknowledgement and the index answers live and after a cold
reopen, and recorded the server's metrics and `/proc/diskstats`. No process failed a check.
The campaign directories hold the harness's `results.json` and `completed.json`, the seed
records, `processes.txt` from `tools/runsum.py`, `report.txt` from `tools/abpairs.py` and
`fences.txt` from `tools/fencehist.py`.

## Results

| Directory | Cell | Result |
|---|---|---|
| `m1-writers-1m` | Four grouped writers at 1.05M with commit messages, five blocks | Spatial: 833 (613–857) against 1,735 (1,499–1,861) transactions/s, a paired ratio of 2.102, and a paired fence time ratio of 0.427. Full-text: 787 (622–860) against 1,772 (1,237–1,833), a paired ratio of 2.214, and a fence time ratio of 0.407. |
| `m2-serial-1m` | Serial spatial literal inserts with commit messages at 1.05M, three blocks | Paired p50 and p90 ratios of 0.618 and 0.627 and a paired transactions/s ratio of 1.669, with 1.105 to 1.109 cache flushes per commit before and 0.005 to 0.007 after. |

In `processes.txt`, `group` is the mean fence synchronization and the number of fences,
`size` the commits per fence, `w/c` and `fl/c` the device writes and cache flushes per
commit, `wms/w` and `fms/f` the device time per write and per flush, and `probe` the
drive's write and flush times before the process started. The fences of `4bd70845` issued
0.50 cache flushes per commit, one for each fence, and those of `4082759b` 0.001.

`fences.txt` divides each process's fences by their synchronization time, from the
server's histogram. Every process of `4bd70845` had no fence under 0.5 ms, up to 44
percent between 0.5 and 1 ms and 54 to 82 percent over 2.5 ms. Every spatial process of
`4082759b` had 37 to 57 percent of its fences under 0.5 ms and 42 to 61 percent between 1
and 2.5 ms, with at most 4.3 percent between, and every full-text process 17 to 56 percent
under 0.5 ms and 44 to 80 percent between 1 and 2.5 ms, with at most 9.1 percent between. Those are the fence times of the
[fence probes](../2026-11-01-fence-probe-forge/README.md) in the drive's fast and slow
states, so a process's mean fence time lies between the two and matches neither.

`dsample-text.txt` holds the write and flush counters of `nvme0n1` from `/proc/diskstats`
every 100 ms from the last process of the full-text cell's second block to the end of its
fourth block, taken by `timeout 1500 python3 tools/dsample.py nvme0n1 1490`, which was
started by hand. `spells.txt` is the device time per write in each sample of each process
from `tools/dspells.py`, with block numbers that count from 0 as the directory names do.
In three of the four sampled processes of `4082759b`, a write took 0.20 ms on the device
for one spell of 5.6 to 5.8 seconds that began within 6 seconds of the process's first
commit, and about 1.5 ms for the rest of the process. The fourth had a spell of 1.8
seconds and a third of its samples between the two. The processes of `4bd70845` wrote
buffered blocks, whose device time per write of 0.1 to 0.8 ms does not show the state.
