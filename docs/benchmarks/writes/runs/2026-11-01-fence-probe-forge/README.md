# Fence probe runs on forge

These runs measured the two shapes of a group commit fence outside the engine, with the
drive of `forge` in each of its states, for roadmap item 502. They ran on `forge` on
2026-11-01 under the host's heavy-job lock, with nothing else running. `runN-host-at-start.txt`
is the process list when each run took the lock.

The host is `forge`, an Intel Core i5-13500 with the SK hynix HFS512GEJ9X102N drive under
LUKS, LVM and ext4. `device.txt` holds the drive's power states, its autonomous power state
transition setting, its thermal counters and the block queue's cache and FUA settings, read
with `nvme-cli` after the runs.

`tools/fence-io-probe.rs` is the probe, built with `rustc --edition 2024 -O` on the laptop
and copied to `forge`. It preallocates two files of 256 MiB with written zeros and runs a
schedule of patterns, printing one line per second with the fences completed, their mean,
p50, p90 and largest time, and the device's writes, mean write time, flushes and mean flush
time from `/proc/diskstats`. Each fence writes the 4 KiB block that holds each file's
logical end, which advances by 300 bytes per fence, as a fence's records and terms do.

| Pattern | Fence |
|---|---|
| `wt2` | One `O_DIRECT` and `O_DSYNC` write in each file, in parallel, as a write-through fence writes its log records and terms |
| `wt2s` | The same two writes one after the other |
| `wt1` | One write-through write, as an ordinary commit without new terms |
| `fl2` | One buffered write in each file, then `fdatasync` of both files in parallel, as a flushing fence |
| `fl1` | One buffered write and `fdatasync` |

`commands.txt` holds the schedule of each run. Run 1 ran each pattern for 20 to 60 seconds,
and run 2 ran them after idle periods of 120 to 240 seconds. Runs 2 to 4 were cut by a
timeout of 900 or 1,200 seconds, which ended run 2 before its last two patterns. Run 3
alternated `wt2` and `fl2` every second for 600 seconds, and run 4 did so for 40 seconds
after each of three idle periods of 180 seconds, so that each `wt2` second and the `fl2`
second after it shared the drive's state.

`tools/probe-summary.py` classifies each second as fast when its fence p90 is under 0.8 ms,
slow when its fence p50 is over 1.2 ms, and mixed otherwise, and pairs each `wt2` second of
an alternating run with the `fl2` second after it. `summary.txt` is its output for each run
and for runs 3 and 4 together.

| State | Pairs | Write-through fence (ms) | Flushing fence (ms) | Paired ratio | Write-through faster |
|---|---:|---:|---:|---:|---:|
| Fast | 14 | 0.241 | 0.342 | 0.704 | 14 |
| Slow | 332 | 1.612 | 1.701 | 0.944 | 308 |

In the fast state a write-through write took 0.20 ms on the device, and a flushing fence's
writes took 0.13 ms and its flushes 0.24 ms. In the slow state a write-through write took
about 1.5 ms and a flush about 1.55 ms. Two write-through writes in parallel took no longer
than one in either state, while one after the other they took 0.46 ms in the fast state and
3.1 ms in the slow one. The drive spent most of every run in its slow state.
