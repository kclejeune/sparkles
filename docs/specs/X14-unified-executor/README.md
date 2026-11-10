# Unified executor

This spec covers one morsel-driven pull executor in which eager execution is a consumption policy, with one memory-accounting model, one kernel per join and grouping algorithm, and one execution-policy function for every entry point. It is implemented in part. Its
first stage of deletions, the policy items 480 to 483 of its third stage and the
accounting model and mode-neutral kernels of its fourth stage have shipped, as its
[Outcome](SPEC.md#outcome) records. The format cutoff of item 479 and the commit publish
tail of item 484 remain with [X16](../X16-storage-consolidation/README.md), the
hierarchical budget of item 675 and the decoding twins of item 678 close stage 4, and
[roadmap item 453](../../ROADMAP.md) holds the rest of the work. The [plan](PLAN.md)
orders the remaining stages into phases, items 825 to 829 first, with their files, gates,
measurements and the decisions that are open.

Read the [design](SPEC.md), its [candidate phases](SPEC.md#candidate-phases), its
[acceptance gates](SPEC.md#acceptance-gates) and the [plan](PLAN.md) for the remaining
stages. Its [Provenance](SPEC.md#provenance) records the design sources.
