# SP²Bench

scripts/bench-sp2b.sh runs the SP²Bench queries over the generated publication data. Result size, independent answer checks and incomplete queries affect which engines can be ranked.

Read the [results](RESULTS.md), [run catalog](RUNS.json) and
[shared reference setup](../reference/README.md). The
[harness guide](../../BENCHMARKING.md) documents commands and output formats.

The result report is a manual import. A linked raw record supports only the measurements
and metadata it contains. Missing build revisions, host details, durability settings or
answer evidence remain unknown and must be supplied by a new run before a broader claim
is generated. Existing raw records retain their original bytes and paths.
