# Leaf cursors script rerun on forge

This manual record reruns the text, geo and vector scripts of the
[first leaf cursors A/B](../2026-11-18-leaf-cursors-ab-forge-9df856f3/README.md) in three
rounds on forge. It uses the same two binaries as that record, A at bccc5f7b and B at
9df856f3, whose SHA-256 sums `SHA256SUMS` lists and the driver checked before it started.
The record of the first pass gives their compilers, build command and `.text` sizes.

`queue.sh.txt` waited until the units queued ahead of it had left the host and then held
forge's benchmark lock and heavy lock for the whole rerun in a 13 GiB scope. `queue.log`
gives the times, with the locks held from 05:14:07 to 08:39:07 Eastern on 2026-11-20, and
`uptime-before.txt` the load when the locks were taken. `rerun.sh.txt` ran the first
pass's grid, which is `scripts/bench-text.sh`, `scripts/bench-geo.sh` and
`scripts/bench-vector.sh` at 100,000 documents, points and vectors of 384 dimensions, in
eager and streaming execution with 12 threads and with one, from a `git archive` of
`scripts/` at dd177ccb. Rounds 1 and 3 ran the arms as A,B,B,A and round 2 as B,A,A,B,
each arm in a fresh server, over one work directory per script that every round reused.
All 144 steps exited with status 0, and `exit.txt` holds the driver's exit status.

`results.tar.gz` keeps each round's `steps.log`, every step's log and every run's timing
JSON, summary and answer fingerprints. The fingerprints agree across every round, arm,
mode and thread count of each script.

`analysis.md` and `analysis.json` are the output of `analyze_rounds.py.txt` over those
files. For each of the 88 configurations it gives the ratio of the pooled medians of
client wall time, the ratio of each of the six A,B pairs, the per-round ratios and the
largest same-binary spread. A configuration is slower when every pair is above 1. By that
rule 80 configurations read as noise, 5 as faster and 3 as slower, which are streamed geo
`q9-star` with 12 threads, eager vector `q-ef-64` with one thread and eager geo
`q6-touching-counties` with one thread. The verdicts are in the
[synthetic results](../../RESULTS.md#leaf-cursors), and the
[q9-star rerun](../2026-11-20-leaf-cursors-q9-star-forge-cde29b37/README.md) settles the
first of them.
