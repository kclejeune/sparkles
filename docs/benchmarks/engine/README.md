# Engine features and overheads

These focused measurements isolate query execution, planning, serialization and index features. Internal A/B results describe the measured feature and configuration rather than a general comparison between engines.

The [results](RESULTS.md) are a
[manual report](../README.md#evidence-and-generated-reports). Read them with the
[run catalog](RUNS.json) and the [shared reference setup](../reference/README.md).
Commands and output formats are in the [harness guide](../../BENCHMARKING.md).

Two [rewrite gates](../../BENCHMARKING.md#rewrite-gates) record their baselines in [the results](RESULTS.md#rewrite-gate-baselines). These are the W3C suites under streaming execution and the instruction counts of vocabulary scans and WAL replay. A change to an executor, a vocabulary reader or the WAL reader runs `mise run test:w3c:streaming` or `mise run bench:insn` and compares the record with the baseline through `scripts/bench-gate.py compare`.
