# X16: Storage consolidation

> **Status:** implemented in part (Phases 1, 2, 3, 4, 5 apart from `imbl`, and items 846 and 1062)

This spec covers a first-release format cutoff, budgeted readers through one Control trait, a shared commit publish tail, one load plan, partitioned vocabularies with per-range generations, and the remaining replica, clone and format-policy work. Phases 1, 2, 3, 4, 5 apart from `imbl`, and items 846 and 1062 have shipped, and
[roadmap item 455](../../ROADMAP.md) holds the remaining work. The unified WAL commit
path of Phase 6 has shipped. Item 833 tracks its remaining writes benchmark, and item
1152 tracks the remaining `imbl` maps.

Read the [design](SPEC.md), its ordered [plan](SPEC.md#plan), its later
[candidate phases](SPEC.md#candidate-phases), its [decisions](SPEC.md#decisions)
and its [acceptance gates](SPEC.md#acceptance-gates). Its [Provenance](SPEC.md#provenance)
records the design sources.
