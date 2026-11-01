# LUBM and reasoning

The LUBM measurements cover query timing over a materialized closure and the cost of producing it. The report also covers larger materializations and incremental reasoning under their separate contracts.

Read the [results](RESULTS.md), [run catalog](RUNS.json) and
[shared reference setup](../reference/README.md). The
[harness guide](../../BENCHMARKING.md) documents commands and output formats.

The reasoning gate of the [rewrite gates](../../BENCHMARKING.md#rewrite-gates) records its baseline in [the results](RESULTS.md#query-time-reasoning-latency). A change to the reasoner's query path, derivation on demand or RDFS on read runs `mise run bench:reasoning-latency` on forge and compares the record with that baseline through `scripts/bench-gate.py compare`.

The result report is a manual import. A linked raw record supports only the measurements
and metadata it contains. Missing build revisions, host details, durability settings or
answer evidence remain unknown and must be supplied by a new run before a broader claim
is generated. Existing raw records retain their original bytes and paths.
