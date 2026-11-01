# Engine features and overheads

These focused measurements isolate query execution, planning, serialization and index features. Internal A/B results describe the measured feature and configuration rather than a general comparison between engines.

Read the [results](RESULTS.md), [run catalog](RUNS.json) and
[shared reference setup](../reference/README.md). The
[harness guide](../../BENCHMARKING.md) documents commands and output formats.

Two [rewrite gates](../../BENCHMARKING.md#rewrite-gates) record their baselines in [the results](RESULTS.md#rewrite-gate-baselines). These are the W3C suites under streaming execution and the instruction counts of vocabulary scans and WAL replay. A lane that changes an executor, a vocabulary reader or the WAL reader runs `mise run test:w3c:streaming` or `mise run bench:insn` and compares its record with the baseline through `scripts/bench-gate.py compare`.

The result report is a manual import. A linked raw record supports only the measurements
and metadata it contains. Missing build revisions, host details, durability settings or
answer evidence remain unknown and must be supplied by a new run before a broader claim
is generated. Existing raw records retain their original bytes and paths.
