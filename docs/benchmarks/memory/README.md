# Memory and disk

Memory measurements state the index, cache and execution settings that produced them. Internal tradeoff measurements do not imply that every configuration was compared against every peer.

The [results](RESULTS.md) are a
[manual report](../README.md#evidence-and-generated-reports). Read them with the
[run catalog](RUNS.json) and the [shared reference setup](../reference/README.md).
Commands and output formats are in the [harness guide](../../BENCHMARKING.md).

The [spill default and execution cost records](records/spill-item-93-2026-10-31/README.md)
hold the manual records of the forge runs for roadmap item 93. They list their builds,
configuration and file hashes, and the run catalog does not include them. The
[charged DISTINCT records](records/charged-distinct-2026-11-20/README.md) hold the forge
comparison of DISTINCT before and after its move onto charged buffers in the same way,
and the [conditional merge pair-condition records](records/conditional-merge-atlas-445a7f11/README.md)
hold the quiet `atlas` comparison of `merge_pair_conditions` in forced streaming.
