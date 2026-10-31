# Memory and disk

Memory measurements state the index, cache and execution settings that produced them. Internal tradeoff measurements do not imply that every configuration was compared against every peer.

Read the [results](RESULTS.md), [run catalog](RUNS.json) and
[shared reference setup](../reference/README.md). The
[harness guide](../../BENCHMARKING.md) documents commands and output formats.

The result report is a manual import. A linked raw record supports only the measurements
and metadata it contains. Missing build revisions, host details, durability settings or
answer evidence remain unknown and must be supplied by a new run before a broader claim
is generated. Existing raw records retain their original bytes and paths.

The [spill default and execution cost records](records/spill-item-93-2026-10-31/README.md)
hold the manual records of the forge runs for roadmap item 93. They list their builds,
configuration and file hashes, and the run catalog does not include them.
