# DBpedia

The DBpedia workload measures a 50M slice and the full English dataset. Cold-cache, warm-cache and complete HTTP response measurements remain separate contracts, and unresolved answer differences are excluded from rankings.

Read the [results](RESULTS.md), [run catalog](RUNS.json) and
[shared reference setup](../reference/README.md). The
[harness guide](../../BENCHMARKING.md) documents commands and output formats.

The result report is a manual import. A linked raw record supports only the measurements
and metadata it contains. Missing build revisions, host details, durability settings or
answer evidence remain unknown and must be supplied by a new run before a broader claim
is generated. Existing raw records retain their original bytes and paths.
