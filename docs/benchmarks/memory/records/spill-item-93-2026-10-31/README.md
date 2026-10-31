# Spill default and execution costs, forge, 2026-10-31

These are the records behind the forge measurements in the
[X08 section for item 93](../../../../specs/X08-disk-spill-and-server-memory.md#spill-execution-costs-and-the-default-item-93-2026-10-31)
and the [memory results](../../RESULTS.md#batch-kernels-and-disk-spill). They are manual
records, not bench-record files, so the [run catalog](../../RUNS.json) does not list them.
Their files keep the bytes the runs wrote. The `SHA256SUMS` file lists the hash of each
original file. Each `.zst` file compresses an original that it lists, either `samples.tsv`
or `matrix.sh`.

All runs used forge, which has a 13th Gen Intel Core i5-13500 with 20 logical CPUs and
15.6 GiB of memory, on kernel 7.2.9. Each ran under the heavy lock with no other job on
the host. The data is the synthetic people file `people-1000000.nt.zst` with sha256
`3f51783448371dd0f57391e7d3fdadae66442f0c32ae260c93a3a96b43b1cbd4`, which loads as
10,527,319 quads.

| Directory | Builds | Configuration |
|---|---|---|
| `default10-offon` | `sparkles-890c60ef`, sha256 `5a26691a…`, on both sides | `serve --spill off` as A and `--spill on` as B, 8 rounds, 20 timed and 3 warm-up requests, one Rayon thread, one shared store |
| `default10-onoff` | the same binary | The same run with the sides swapped |
| `inv-item85` | `sparkles-5453f9d0`, sha256 `6db286d6…`, as A and `sparkles-c88c5969`, sha256 `f7e94f79…`, as B | Default `serve` flags, 16 rounds, 80 timed and 10 warm-up requests, one Rayon thread, a store loaded by each build |
| `matrix` | `modeprobe-d65eaf40` and `modeprobe-8ca35daa`, whose hashes are in `provenance.txt` | The `matrix.sh` script, eight Rayon threads, batches of 4,096 rows, three timed rounds per run, two runs per binary and cell in the order A, B, B, A |

The three A/B directories ran `scripts/nsc-bench-remote.sh` on forge itself, as it is at
the base of this change, and `scripts/nsc-bench-summary.py` wrote their `summary.md`
files. These show the medians and the per-round ratios, and each states that A and B
returned the same sorted TSV answers. In `matrix`, `matrix.sh.zst` holds the script as it
ran. The first line of each log that starts with `metrics` holds the digests of the cursor and eager rows, the budget, the peak
and the bytes written, and `summary.md` is the output of `matrix-summary.py`. Each
`-files-after.txt` file lists what a spilled run left in its directory, and all are empty.
