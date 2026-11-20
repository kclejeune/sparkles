# Leaf cursors A/B on forge

This manual record compares the first part of item 829 with its base on forge, which ran
each comparison alone under the host's heavy lock. A is main at bccc5f7b and B is
9df856f3, whose engine code is the part-1 tip. Both are release builds of
`mbx build --release -p sparkles-server --bin sparkles` made on stanley with GCC 16.2.0,
Clang 21.1.8, rustc 1.99.1 and LLD 23.1.1, as `readelf -p .comment` shows for both. The
build commands did not set `MBX_LEARNED_INCREMENTAL=false`, so whether Boxington's learned
incremental reuse took part is not recorded. Their `.text` sections are 112,734,445 and
112,706,605 bytes. Their SHA-256 sums are
`af1f600cafe20755e0dbc6db5d677a39be0fc82a164453d1957725f7a028ba24` for A and
`5e384d0f108d17bc2fd0f2d4cdae8cdc8abbfdcd520f084f63af3e7ffd3bec14` for B.

## The 31-query matrix

`streaming` and `eager` each hold one cell of the synthetic matrix at 100,000 people,
about 1.05M triples, with one Rayon thread. Each cell ran `scripts/nsc-bench-remote.sh` in
eight rounds of A,B,B,A with three warm-up and 20 timed requests per query and server, the
server pinned to CPU 2 and the client to CPU 4 with idle spinners, and the execution mode
of the cell sent with every request. `stage.sh.txt` staged the cells and
`run-forge.sh.txt` ran them. Each cell keeps its manifest, settings, environment, queries,
samples, answers and the summary of `scripts/nsc-bench-summary.py`. Both builds returned
the same sorted answers to all 31 queries in both cells, and steal was zero.

## The text, geo and vector scripts

`scripts` holds the A/B of `scripts/bench-text.sh`, `scripts/bench-geo.sh` and
`scripts/bench-vector.sh` at 100,000 documents, points and vectors of 384 dimensions, in
eager and streaming execution with 12 threads and with one, which `scripts/ab.sh.txt` ran
from a checkout of e8056367. Each configuration ran the base, the tip, the tip and the
base again, each in a fresh server. `results.tar.gz` keeps every run's timing JSON,
summary and answer fingerprints with each step's log, and `steps.log` gives the order and
start of the steps. The text and geo fingerprints agree across all 16 runs of each script.
The vector fingerprints agree within each mode and differ between the modes only in the
recorded request path. The geo script's step without the spatial rewrites exited with
status 1 in the 12 runs outside the one-thread streaming configuration, at the base and
the tip alike, after its indexed and scan timings were written. Its `geo-index` call
found the store still held by the server that the script had just stopped, because the
script waited only for that server's port to close, and the script discarded the call's
message. The script now waits
for the server to exit, which leaves the recorded timings as they are.

## Analysis

`analysis.md` is the output of `analyze.py.txt` over the fetched copies of these files.
For each matrix query it gives the median of the per-round ratios of median execution
times, a 95% percentile bootstrap over the rounds, the median added time and the spread of
each build between its server processes. For each script query it gives the ratio of the
pooled medians, the ratio of each A,B,B,A pair and the same-binary ratios of the two
servers of each build. Its last column is a first-pass flag from the interval or from the
two pairs alone, before the same-binary spread and the confirmation runs are weighed, and
the verdicts are in the synthetic results.

## Confirmation

`confirm` holds the runs that settle the first pass's flags, which `confirm/stage.sh.txt`
staged from the matrix binaries and stores and `confirm/run-forge.sh.txt` ran under the
heavy lock, with `confirm/driver.log` giving their order. Each run used
`scripts/nsc-bench-remote.sh` with one Rayon thread, the server pinned to CPU 2 and the
client to CPU 4 with idle spinners, five warm-up requests and the settings in its
`run.env`. `small-eager-ab` and `small-eager-ba` compare A and B in both orders over the
matrix queries under 0.3 ms and the ones over it that the first pass flagged, in twelve
rounds of 200 timed requests, and the `small-streaming` pair does the same in streaming
execution. The `vec` pairs do the same over the vector script's store and queries in
twelve rounds of 50 requests. `cj-es` and `cj-se` run B on both sides, eager against
streaming in both orders, in twelve rounds of 100 requests. Each run keeps its queries,
samples, answers, environment and exit status, and every server of every run returned
the same answers. `confirm/combined` holds the output of `scripts/x08-gates/combine.py`
over each pair's server time, and `confirm/combined-wall` its output over the samples'
`wall_ms` column, curl's transfer time, which splits between about 1 and 2 ms for many
short queries, and `confirm/explain` the plans of the two `CountJoin` queries in each
mode from B on stanley over the same data.

## Instruction counts

`insn-stanley` holds `mise run bench:request-insn` over A and B on stanley, an Intel
Core Ultra X7 358H, which is not forge's hardware. It counted the confirmation's synthetic
and `CountJoin` queries in eager and streaming execution, in eight rounds of A,B,B,A with
50 counted requests per query and server, on the store that the task loads from the
synthetic data at 100,000 people. `report.md` is the task's report, and `samples.tsv.gz`
and `manifest.json` its samples and settings. The verdicts are in the synthetic results.

