# The cursor's conditional merge OPTIONAL, atlas, 2026-11-19

These are the records behind the table of conditional OPTIONALs in the
[memory results](../../RESULTS.md#automatic-exists) and the
[X05 outcome](../../../../specs/X05-streaming-query-execution.md#the-cursors-conditional-merge-optional-2026-11-19).
They are manual records, not bench-record files, so the [run catalog](../../RUNS.json)
does not list them. Their files keep the bytes the runs wrote, except that the plan
files keep only the timing line and the plan and drop the result rows. `SHA256SUMS`
lists the hashes of the three original TSV files of each timing directory and of the two
scripts, and each `samples.tsv.zst` compresses the `samples.tsv` that it lists. The
plans and queries have no listed hashes. The timing script `ab.sh` and the driver
`run.sh` are in `scripts.tar.zst`. `builds.txt` holds the compiler line of each build
log and `sha256sum` of the two binaries as taken on `atlas` after the run.

Both builds are release builds made on `atlas` with `mbx build --release -p
sparkles-server` and rustc 1.99.1. A is bc647108, which streams EXISTS inside larger
expressions and hashes every conditional OPTIONAL in the cursor, and B is ca1532ba,
which merges a conditional OPTIONAL in the cursor. B is the exact commit that was built
on `atlas`, and 426e8630 is its rebased counterpart on the branch, made when the change
was rebased onto a later main before it landed. Their sha256 hashes are
`fa5dfcac483004a9c673af6e9e09348e9eb002de8c73db5aa5d633e234c7e74f` for A and
`8c43879e2f6100cfa22f08669bedae30f5fbe4c505efc414f9eeeadc676b1ad3` for B. The commits
after ca1532ba change only the merge's charges, where its condition is held and the
order in which a spooled run drops a decoded frame. B also streamed an OPTIONAL whose
condition holds an EXISTS under automatic execution, which the change as merged does
not. The data is the synthetic people file `people-1000000.nt.zst` with sha256
`3f51783448371dd0f57391e7d3fdadae66442f0c32ae260c93a3a96b43b1cbd4`, which loads as
10,527,319 quads. Every server ran with one Rayon thread and `--result-cache-mb 0`.

| Directory | Host | Builds | Configuration |
|---|---|---|---|
| `atlas-ab` | `atlas`, a 13th-generation Intel Core i5-13500 with 16 GiB of memory and no other work | A and B | `ab.sh`, 3 A, B, B, A rounds, 7 timed and 2 warm-up requests in each mode, queries in `queries` |
| `atlas-nomerge` | `atlas`, as above | A and B | As `atlas-ab` with 2 rounds and `SPARKLES_DISABLE_OPTIMIZATIONS=merge_left_join`, so both modes hash |
| `plans` | `atlas`, as above | B | `sparkles query --time` once per query in eager, streaming and automatic execution |

Each `samples.tsv` holds one server execution time per timed request, read from the
`execMs` field of the native JSON answer. `ab.sh` wrote each `summary.tsv`, which gives
the medians per build and mode and states that every build and mode returned the same
sorted TSV answers, and each `answers.tsv` holds those hashes. `plans/merged.txt` counts
the plans of each file that name `[merge on`. The `knows-*` queries join each person to
the people they know, about 2.5 per person and at most 5, the highest fan-out on a merge
key in this data. The `class-*` queries hash in both modes, because the right side is
scanned on `?x`.
