# Unified executor

This spec covers one morsel-driven pull executor in which eager execution is a consumption policy, with one memory-accounting model, one kernel per join and grouping algorithm, and one execution-policy function for every entry point. It is implemented in part. Its
first stage of deletions and the policy items 480 to 483 of its third stage have shipped,
as its [Outcome](SPEC.md#outcome) records. The format cutoff of item 479 and the commit
publish tail of item 484 remain, and [roadmap item 453](../../ROADMAP.md) holds the rest
of the work.

Read the [design](SPEC.md), its [candidate phases](SPEC.md#candidate-phases) and its
[acceptance gates](SPEC.md#acceptance-gates). Its [Provenance](SPEC.md#provenance) records
the design sources.
