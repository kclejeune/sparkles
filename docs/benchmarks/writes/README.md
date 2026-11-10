# Writes and updates

Write measurements separate serial latency, group commit, concurrent readers and writers, changed-store queries and maintenance work. Synchronization before acknowledgement, writes to the operating system cache and memory-only writes remain distinct durability classes.

The [results](RESULTS.md) are a
[manual report](../README.md#evidence-and-generated-reports). Read them with the
[run catalog](RUNS.json) and the [shared reference setup](../reference/README.md).
Commands and output formats are in the [harness guide](../../BENCHMARKING.md).

The incremental-view gate of the [rewrite gates](../../BENCHMARKING.md#rewrite-gates) records its baseline in [the results](RESULTS.md#incremental-view-gate-baseline). A change to an incremental-view path runs `mise run bench:incr` on forge and compares the record with that baseline through `scripts/bench-gate.py compare`.
