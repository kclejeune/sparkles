# Synthetic suite

The synthetic workload generates people, organisations, documents and typed literals with scripts/gen-data.py. scripts/bench.sh compares the same 28 SPARQL queries over HTTP, with separate warm, cold, changed-store and mixed-load modes.

Read the [results](RESULTS.md), [run catalog](RUNS.json) and
[shared reference setup](../reference/README.md). The
[harness guide](../../BENCHMARKING.md) documents commands and output formats.

The eager-against-cursor matrix of the [rewrite gates](../../BENCHMARKING.md#rewrite-gates) records its baseline in [the results](RESULTS.md#eager-against-cursor-matrix). A lane that changes an executor path runs `mise run bench:executors` on forge and compares its record with that baseline through `scripts/bench-gate.py compare`.

The result report is a manual import. A linked raw record supports only the measurements
and metadata it contains. Missing build revisions, host details, durability settings or
answer evidence remain unknown and must be supplied by a new run before a broader claim
is generated. Existing raw records retain their original bytes and paths.

[Generated record tables and charts](RECORDS.md) supplement this report with medians
recomputed from the explicitly selected records in [report.json](report.json). Every
selection fixes its host, suite, scale, binary revision and timing settings. The report
shows carried-over peer measurements and failed or unqualified answers. It makes no
claim about the speed of a different build.
