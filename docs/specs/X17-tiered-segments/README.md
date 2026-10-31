# Tiered segments

This spec covers immutable generations uploaded to S3-compatible storage after compaction, fetched as whole segments into a local disk cache and memory-mapped from there, with lazy dataset open and idle unload. It is planned, and
[roadmap item 455](../../ROADMAP.md) holds the work.

Read the [design](SPEC.md), its [candidate phases](SPEC.md#candidate-phases) and its
[acceptance gates](SPEC.md#acceptance-gates). Its [Provenance](SPEC.md#provenance) records
the design sources.
