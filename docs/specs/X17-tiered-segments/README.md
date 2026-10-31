# Tiered segments

Immutable generations uploaded to S3-compatible storage after compaction, fetched as whole segments into a local disk cache and memory-mapped from there, with lazy dataset open and idle unload.

Status: planned. [Roadmap item 455](../../ROADMAP.md) holds this work.

Read the [design](SPEC.md), its [candidate phases](SPEC.md#candidate-phases) and its
[acceptance gates](SPEC.md#acceptance-gates). Its [Provenance](SPEC.md#provenance) records
the design sources.
