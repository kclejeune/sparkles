# Leaf cursors q9-star rerun on forge

This manual record times geo `q9-star` at 12 threads on forge for three builds. The
[script rerun](../2026-11-20-leaf-cursors-scripts-rerun-forge-9df856f3/README.md) read the
streamed query slower in all six pairs of 9df856f3 against bccc5f7b, and this run
separates item 829's own cost on current main from main's changes since bccc5f7b.

The builds are base at bccc5f7b, the binary of the earlier records, main at 5f3f69c2,
the merge base of the tip with main, and tip at cde29b37. All three are release builds of
`mbx build --release -p sparkles-server --bin sparkles` made on stanley with mbx 1.29.1,
rustc 1.99.1, LLD 23.1.1, GCC 16.2.0 and Clang 21.1.8, without
`MBX_LEARNED_INCREMENTAL` set. Their `.text` sections are 112,734,445, 113,218,477 and
113,151,149 bytes. `SHA256SUMS` lists the binaries and scripts, and `run.sh.txt` checked
them on forge before it started.

`run.sh.txt` held forge's benchmark lock and heavy lock in a 12 GiB scope from 11:18:46
to 11:38:32 Eastern on 2026-11-20, as `queue.log` shows, with the load in
`uptime-before.txt`, and `exit.txt` holds its exit status. `q9.sh.txt` ran six rounds,
streaming and then eager execution in each, from a `git archive` of `scripts/` at
cde29b37. Each arm is one `scripts/bench-geo.sh` run at 100,000 points with
`QUERIES=geo-q9-star`, 25 timed requests and its own servers, over one store that the
first arm loaded. The arm order rotates the builds through the positions, so that over
six rounds each build holds each position pair twice. With `PLAIN_QUERIES=geo-q9-star`
the server without the spatial rewrites times the query as well, as a second process per
arm, and its plan is the same. `cpusample.sh.txt` read the CPU of every server thread
every 0.25 s beside each arm.

`results.tar.gz` keeps each round's step logs, timing JSON, answer fingerprints and CPU
samples. Every arm returned the same answers. `explain.tar.gz` keeps the executed plans of
the four builds bccc5f7b, 9df856f3, 5f3f69c2 and cde29b37 in both modes and of the server
without the rewrites, all at 100,000 points on stanley, and all of them are the same plan,
a project over a star join, an index join and a merge join of a type scan with the spatial
scan.

`summary.md` and `summary-plain.md` are the output of `analyze_q9.py.txt` for the indexed
server and the server without the rewrites, with `analysis.json` and `analysis-plain.json`
beside them. For each pair of builds the analysis gives the ratio of pooled medians with a
95% bootstrap interval over server processes, the twelve per-position pairs and each
build's same-binary spread. The verdicts are in the
[synthetic results](../../RESULTS.md#leaf-cursors).

`placement.md` is the output of `cpus_q9.py.txt` with the P cores 0 to 11 of forge's
i5-13500, and gives each server's share of its query pool's CPU time on P cores beside
its median. `placement_corr.py.txt` correlates the two over the 36 servers of each mode
and server kind, and `placement-corr.txt` gives the coefficients, which lie between -0.11
and 0.03. The P-core share shows no relation to a server's median. With at most two
servers per cell in the fast mode, under 22 ms, the samples cannot say what causes that
mode.
