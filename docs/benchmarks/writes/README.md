# Writes and updates

Write measurements separate serial latency, group commit, concurrent readers and writers, changed-store queries and maintenance work. Synchronization before acknowledgement, writes to the operating system cache and memory-only writes remain distinct durability classes.

Read the [results](RESULTS.md), [run catalog](RUNS.json) and
[shared reference setup](../reference/README.md). The
[harness guide](../../BENCHMARKING.md) documents commands and output formats.

The incremental-view gate of the [rewrite gates](../../BENCHMARKING.md#rewrite-gates) records its baseline in [the results](RESULTS.md#incremental-view-gate-baseline). A change to an incremental-view path runs `mise run bench:incr` on forge and compares the record with that baseline through `scripts/bench-gate.py compare`.

The result report is a manual import. A linked raw record supports only the measurements
and metadata it contains. Missing build revisions, host details, durability settings or
answer evidence remain unknown and must be supplied by a new run before a broader claim
is generated. Existing raw records retain their original bytes and paths.
