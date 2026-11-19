# Server time of a root scan that shares its block, on atlas

This is a manual report of roadmap item 1063. The raw outputs are the summaries and
combined tables in this directory and the cycle profiles under `profile/`. They are not
part of the generated [run catalog](../../RUNS.json).

## Question

Item 1039 stopped charging a cursor scan's block between reads. Against `9acd6d74`, the
build before it, server execution time of `root-scan` rose 2.8% under streaming and 5.3%
under automatic execution on atlas, `export-500k` 2.9% and 3.1%, and the LIMIT of
`root-limit` 20% under streaming at 0.004 ms. A profile of `root-limit` put the cost in
the shared batch, in the weak handle the scan made on its block and dropped again at each
batch, and in taking the block back at each read. Item 1063 changes the shared path so that
a scan keeps no weak handle on a block it shares and reads it in place, and takes the
block back from the shared batch only when it switches to owned rows inside the block or
a batch declines the shared path.
This run asks how much of the cost the change recovers.

## Builds

| Arm | Commit | Contents |
|---|---|---|
| `pre` | `9acd6d74` | The build before item 1039. |
| `main` | `c0948014` | Item 1039 and the other merges of batches 30 to 32. |
| `fix` | `4e9185f1` | `main` with the change of item 1063. |

All three binaries were built on wally by `scripts/nsc-build.sh` with clang 21.1.8, GCC
16.2.0, LLD 23.1.1 and rustc 1.99.1, and each summary names its binary's SHA-256.

## Method

The host is atlas, a 13th Gen Intel Core i5-13500 with kernel 7.2.9, under its heavy
lock with no other work. Two stages each ran eight runs of `scripts/nsc-bench-remote.sh`
on 100,000 people from `scripts/gen-data.py`, 1,052,807 triples. The `main` stage compared
`main` with `fix`, and the `pre` stage compared `pre` with `fix`. Each stage ran the nine
queries under 0.3 ms at 12 rounds of A,B,B,A with 200 timed requests per query and server,
and `export-500k` at 12 rounds of 20 requests, under streaming and automatic execution, and
each in both arm orders. The server had one Rayon worker on CPU 2 and the client ran on
CPU 4. `scripts/x08-gates/combine.py` combines the two orders of a cell to cancel the bias
of the second position and applies the Intel rule, under which a query passes when the
upper end of its 95% interval is at most 1.02 or its median added time is at most 0.5 µs.
The combined tables keep that script's column names, so "pool off ms" is the time of the
base arm and "on/off" is the ratio of `fix` to that base.

Every run returned the same answers from both arms, which each summary states. The summaries carry the fixed title of the script that writes them, which names Namespace, but every run in this record ran on atlas. Another job
copied files to atlas and ran a 0.1 s test outside the lock during rounds 3 and 4 of
`summary-pre-small-streaming-ab.md`. Without those two rounds, `root-scan` reads 1.0240,
1.0208 to 1.0276, and `root-limit` 1.0314, 1.0026 to 1.0674, so they change no verdict.
`pre-small-streaming-without-rounds-3-4-intel.md` holds that combination.

## Results

Server execution time of `fix` against each base, with the 95% interval and the median
added time.

| Query | Mode | `fix` against `main` | `fix` against `pre` |
|---|---|---|---|
| `root-limit` | streaming | 0.965, 0.940 to 0.988, −0.14 µs | 1.039, 1.004 to 1.075, 0.14 µs |
| `root-limit` | automatic | 0.970, 0.893 to 0.994, −0.34 µs | 1.040, 1.011 to 1.088, 0.46 µs |
| `root-scan` | streaming | 0.982, 0.980 to 0.986, −1.55 µs | 1.024, 1.021 to 1.027, 1.98 µs |
| `root-scan` | automatic | 0.972, 0.970 to 0.976, −2.41 µs | 1.011, 1.007 to 1.014, 0.89 µs |
| `export-500k` | streaming | 0.983, 0.979 to 0.988, −6.60 µs | 1.005, 1.003 to 1.008, 1.95 µs |
| `export-500k` | automatic | 0.984, 0.981 to 0.986, −6.29 µs | 1.003, 0.999 to 1.006, 1.07 µs |

The change is faster than `main` on every named query in both modes. Against `pre`,
`root-limit` passes on its added time, `root-scan` passes under automatic execution and
`export-500k` stays within 1%, but `root-scan` under streaming fails the rule at 1.024.
Under the rule's own noise test, a 95% interval that reaches 1.0 or an added time of at
most 0.5 µs, the costs that remain are `root-scan` under both modes and `export-500k` under
streaming. The remainder is about 2 µs under streaming on both `root-scan` and
`export-500k`, although the export reads more blocks, and the point estimates under
automatic execution are about 1 µs on both, although the export's interval reaches 1.0.
That reads like a cost per request rather than per block, but two queries do not settle it.

Among the controls, `distinct-obj` and `predicate-counts` read 1.062 and 1.050 against
`main` under streaming, 0.30 and 0.31 µs, and 0.976 and 0.986 under automatic execution.
Their plans run an eager subplan under the cursor, and a profile of `distinct-obj` on
stanley lists no leaf of the scan code that the change touches among the 25 leaves it
prints per region, so the difference is not attributed to it. Against `pre`, `path-plus` under automatic execution reads 1.145,
1.43 µs, and `subquery-agg` 1.013, 2.41 µs, and 1.006, 1.14 µs, under streaming. All three
are flat against `main`, so item 1063 does not cause them. Item 1039's own comparison on
atlas, of `9acd6d74` with the tip of that item in six rounds of one arm order, read
`path-plus` 0.944 and `subquery-agg` 1.007 under automatic execution and `subquery-agg`
1.000 under streaming. That suggests most of these costs come from the other merges of
batches 30 to 32, but it does not isolate them. The `distinct-obj` classifications are in `profile/`.

## Profile of the scan

`profile/` holds LBR cycle profiles of `root-scan` under streaming on stanley, an Intel
Core Ultra X7 358H, with the same three binaries. Each pass started a server per build on
CPU 2 with one Rayon worker, sent 100 warm-up and 400 profiled requests from CPU 3, and
sampled user cycles every 20,011. Passes 1 and 3 ran `pre`, `main`, `fix` and passes 2 and 4
the reverse order. `classify.py.txt` assigns each sample to the first of the marks
`Operator>::next_shared` and `next_batch_at_most` found in its call chain, and the first
covers the time that a streaming cursor adds to execution time. The response writer takes
most samples and varies with the host's load, so the table divides the scan's samples
outside `block_passes` by those in `block_passes`, whose code and work the three builds
share.

| Build | Pass 1 | Pass 2 | Pass 3 | Pass 4 | Samples outside `block_passes` |
|---|--:|--:|--:|--:|--:|
| `pre` | 0.642 | 0.465 | 0.432 | 0.370 | 3,515 |
| `main` | 0.813 | 0.588 | 0.498 | 0.455 | 4,393 |
| `fix` | 0.558 | 0.550 | 0.369 | 0.439 | 3,409 |

`main` is above `pre` in every pass, and `fix` is above `pre` in two passes and below it
in two, so the ranges of `fix` and `pre` overlap. The leaves that item 1039 added, such as `Block::downgrade`, the drop of the weak
handle and the extra work of the shared batch, are gone or smaller in `fix`. On stanley, the
scan's cost in `fix` overlaps its cost in `pre` across the passes, and the profile does
not resolve a smaller difference. The profile does not reproduce the
remainder that atlas measures, and the cause of that remainder is open.
