# EXISTS in BIND and OPTIONAL, Namespace, 2026-11-01

These are the records behind the table of EXISTS in BIND and OPTIONAL conditions in the
[memory results](../../RESULTS.md#automatic-exists) and the
[X05 follow-ups](../../../../specs/X05-streaming-query-execution.md#item-7-follow-ups-2026-11-01).
They are manual records, not bench-record files, so the [run catalog](../../RUNS.json)
does not list them. Their files keep the bytes the runs wrote. The `SHA256SUMS` file lists
the hash of each original file, and each `samples.tsv.zst` compresses the `samples.tsv`
that it lists.

Both runs used `scripts/nsc-bench.sh` on an 8-vCPU AMD EPYC Namespace instance with
16 GiB of memory, each on a fresh instance. The binaries were built on the laptop with
`scripts/nsc-build.sh`, and `manifest.txt` gives their hashes and compilers. The data is
the synthetic people file `people-1000000.nt.zst` with sha256
`3f51783448371dd0f57391e7d3fdadae66442f0c32ae260c93a3a96b43b1cbd4`, which loads as
10,527,319 quads. The queries are in `queries`.

| Directory | Builds | Configuration |
|---|---|---|
| `auto` | 4bd70845 as A and ec6cd326 as B | Automatic execution, 3 rounds, 10 timed and 3 warm-up requests, one Rayon thread, `--result-cache-mb 0` |
| `modes` | ec6cd326 on both sides | `execution=eager` as A and `execution=streaming` as B, otherwise as `auto` |

`scripts/nsc-bench-summary.py` wrote each `summary.md`, which shows the medians and the
per-round ratios and states that A and B returned the same sorted TSV answers.

`describe-first-bytes.txt` holds a manual timing of DESCRIBE on the laptop, with the
same data and the 015fdd80 release binary, whose DESCRIBE code ec6cd326 keeps. Other
lanes' builds ran on the laptop at the time, so its figures are approximate.
