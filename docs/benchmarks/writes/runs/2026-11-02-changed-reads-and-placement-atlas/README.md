# Changed-store reads and request-thread placement, atlas run

These runs measured the server time of `two-hop-count` on a changed store with the
parallel reads of changed blocks, and the write rate under mixed load with each placement
of request threads. They ran on `atlas` on 2026-11-02. `atlas` has the same Intel Core
i5-13500 and drive model as `forge`, with CPUs 0 to 11 on the six performance cores and
CPUs 12 to 19 on the eight efficiency cores. The server logs `hybrid processor: request
threads run on the performance cores performance=12 efficiency=8 query_threads=18` there
with the default `--request-cpus auto`. Each run held the host's heavy-job lock, but other
work ran on the host during the server-time runs, whose first line gives the load average.

The binaries are release builds of `mbx build --release -p sparkles-server` in the Nix
development shell on `atlas`.

| Name in the output | Source | SHA-256 |
|---|---|---|
| `base-b62d3722` | main before the parallel reads of changed blocks | `9b64590ed735903c9872875d5b2366dd25ff81c2df6023333b8850ea59cb7257` |
| `cand-0af790e5` | the first version, which split a changed block at every delta key | `c676cb64da37d64090e38f480f991d516c81ff5c0d8a8482f016be0b84db7321` |
| `cand2` | a copy of every column of a changed block with the delta merged in | `b34bf89ba7f23c5ac0106a01a91ed76634baf34cfb72aa0223ba4e75c53d3d57` |
| `cand3` | the copy of only the requested columns, the tree of commit `98f1728d` | `744102123ab0b47e0b898fb174f9c7f6b2067983d0ad4045cacdfcff46b1622d` |

## Server time on the changed store

The 1.05M store is `scripts/gen-data.py 100000` loaded by `base-b62d3722`, and its changed
copy took the 5,000 commits of `churn-5000.ru` from `scripts/bench-writes.py` through the
server. `scripts/ab2.sh` starts each binary in turn on both stores and sends `two-hop-count`
60 times to each with `curl`, and reports the medians of the access log's `exec_ms` after
the first ten requests. The loaded-store times of 1.6 and 1.7 ms are single runs, and every
other run of every binary took 2.9 to 3.2 ms on that store. `scripts/ab3.sh` does the same
for `star-join` and `predicate-counts` on the loaded store, with the server on CPUs 0 to 11
and `curl` on CPU 12. The access log records these times to 10 µs.
`changed-reads-server-time.txt` holds every output.

| Binary | Loaded store (ms) | Changed store (ms) |
|---|---:|---:|
| `base-b62d3722` | 1.7 to 3.2 | 7.1 to 10.7 |
| `cand-0af790e5` | 2.9 to 3.1 | 5.4 to 6.1 |
| `cand2` | 2.9 to 3.0 | 4.2 to 4.5 |
| `cand3` | 1.6 to 3.1 | 4.0 to 4.2 |

On the loaded store `star-join` took 1.38 to 1.44 ms with `cand3` and 1.45 to 1.55 ms with
`base-b62d3722`, and `predicate-counts` took at most 0.01 ms with both.

## Mixed load with each placement

`scripts/mix-abab.sh` runs the `mixed` mode of `scripts/bench.sh` for Sparkles alone on the
loaded 1.05M store with `cand3`, alternating `SPARKLES_ARGS="--request-cpus auto"` and
`"--request-cpus all"`, five pairs with `auto` first. One earlier block ran the same
commands in the order `auto`, `all`, `all`, `auto`. Each run is 30 s of 16 unpaced
`star-join` readers and one writer of new literals, on a fresh copy of the store. `request-cpus-abab.txt` and
`request-cpus-abba.txt` hold the outputs. No read or write failed.

| Placement | Writes/s | Write p50 (ms) | Reads/s |
|---|---|---|---|
| `auto` | 253.4, 239.1, 197.7, 222.9, 194.8 | 3.73 to 4.89 | 2,031 to 2,284 |
| `all` | 366.8, 358.7, 309.1, 314.0, 278.8 | 2.39 to 3.56 | 2,348 to 2,499 |

The paired ratios of `auto` to `all` are 0.64 to 0.71 for writes and 0.86 to 0.91 for
reads. The A,B,B,A block gave 213.6 and 241.2 writes/s with `auto` and 373.6 and 355.5
with `all`.
