# X16: Storage consolidation

> Phases 1 through 5, the unified WAL commit path of Phase 6, and items 846, 1062 and
> 1152 have shipped.

This spec covers a first-release format cutoff, budgeted readers through one Control
trait, a shared commit publish tail, one load plan, partitioned vocabularies with
per-range generations, and the remaining replica, clone and format-policy work.
[Roadmap item 455](../../ROADMAP.md) tracks the remaining work. Item 833 tracks Phase 6's
Intel writes benchmark.

Read the [design](SPEC.md), its ordered [plan](SPEC.md#plan), its later
[candidate phases](SPEC.md#candidate-phases), its [decisions](SPEC.md#decisions)
and its [acceptance gates](SPEC.md#acceptance-gates). Its [Provenance](SPEC.md#provenance)
records the design sources.
