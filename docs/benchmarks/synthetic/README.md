# Synthetic suite

The synthetic workload generates people, organisations, documents and typed literals with scripts/gen-data.py. scripts/bench.sh compares the same 28 SPARQL queries over HTTP, with separate warm, cold, changed-store and mixed-load modes.

The [results](RESULTS.md) are a
[manual report](../README.md#evidence-and-generated-reports). Read them with the
[run catalog](RUNS.json) and the [shared reference setup](../reference/README.md).
Commands and output formats are in the [harness guide](../../BENCHMARKING.md).

The [Horn classification and kept-frame screen](RESULTS.md#horn-classification-and-kept-frame-screen-leads)
records unresolved per-query timing leads separately from the peer comparisons.

The eager-against-cursor matrix of the [rewrite gates](../../BENCHMARKING.md#rewrite-gates) records its baseline in [the results](RESULTS.md#eager-against-cursor-matrix). A change to an executor path runs `mise run bench:executors` on forge and compares the record with that baseline through `scripts/bench-gate.py compare`.

The timed native checker accepts successful, complete SELECT and ASK answers whose
result kind matches the query. SELECT counts must match the returned rows. ASK counts
are one for either boolean value. Eager metadata may omit status, while an explicit
status must be complete. Missing metadata, errors and truncated or stopped responses
fail the check. A display `send` cap that leaves rows out is not a complete benchmark
answer; a SPARQL LIMIT remains part of the query. Unordered LIMIT results retain the
existing count, schema, distinctness and input-membership checks. A run's recorded
harness identity determines which validation it performed.

[Generated record tables and charts](RECORDS.md) supplement this report with medians
recomputed from the explicitly selected records in [report.json](report.json). Every
selection fixes its host, suite, scale, binary revision and timing settings. The report
shows carried-over peer measurements and failed or unqualified answers. It makes no
claim about the speed of a different build.
