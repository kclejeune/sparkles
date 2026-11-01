# Unified executor

This spec covers one morsel-driven pull executor in which eager execution is a consumption policy, with one memory-accounting model, one kernel per join and grouping algorithm, and one execution-policy function for every entry point. It is implemented in part. Its
first stage of deletions has shipped, as its [Outcome](SPEC.md#outcome) records, and
[roadmap item 453](../../ROADMAP.md) holds the rest of the work.

Read the [design](SPEC.md), its [candidate phases](SPEC.md#candidate-phases) and its
[acceptance gates](SPEC.md#acceptance-gates). Its [Provenance](SPEC.md#provenance) records
the design sources.
