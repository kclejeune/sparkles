# EXISTS inside larger expressions, atlas and the laptop, 2026-11-18

These are the records behind the table of EXISTS inside larger expressions in the
[memory results](../../RESULTS.md#automatic-exists) and the
[X05 outcome](../../../../specs/X05-streaming-query-execution.md#exists-inside-larger-expressions-2026-11-18).
They are manual records, not bench-record files, so the [run catalog](../../RUNS.json)
does not list them. Their files keep the bytes the runs wrote. The `SHA256SUMS` file lists the hash of each
original file, and each `samples.tsv.zst` compresses the `samples.tsv` that it lists.
The timing scripts `ab.sh` and `modes.sh` are in `scripts.tar.zst`, and the archived
`modes.sh` reads its data path from `DATA`.

Both builds are release builds of 8baa5686, the base, and c092dddf, built on the laptop
with `mbx build --release` and rustc 1.99.1. Their sha256 hashes are
`00ea3c7af8b0bc5947cedfeff23c783c34bb9d8189792d556da18f8cc4b74e53` for the base and
`035b24daf7342fd8353d1b7243712282057f3db7efe6a679c8ec678bf01dfff3` for c092dddf. The
data is the synthetic people file `people-1000000.nt.zst` with sha256
`3f51783448371dd0f57391e7d3fdadae66442f0c32ae260c93a3a96b43b1cbd4`, which loads as
10,527,319 quads. Every server ran with one Rayon thread and `--result-cache-mb 0`.

| Directory | Host | Builds | Configuration |
|---|---|---|---|
| `atlas-ab` | `atlas`, a 13th-generation Intel Core i5-13500 with 16 GiB of memory and no other work | The base as A and c092dddf as B | `ab.sh`, 3 A, B, B, A rounds, 7 timed and 2 warm-up requests in each mode, queries in `queries-ab` |
| `laptop-modes/base` | The laptop, with other builds running | The base | `modes.sh`, 1 round of eager and streamed servers, 5 timed and 2 warm-up requests, queries in `queries-modes` |
| `laptop-modes/c092dddf` | The laptop, with other builds running | c092dddf | `modes.sh`, 2 rounds, otherwise as the base |

Each `samples.tsv` holds one server execution time per timed request, read from the
`execMs` field of the native JSON answer. `ab.sh` wrote `atlas-ab/summary.tsv`, which
gives the medians per build and mode and states that every build and mode returned the
same sorted TSV answers, and each `answers.tsv` holds those hashes. The laptop figures
are approximate because of the competing load.
