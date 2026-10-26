# Bounded preparation of read views

This design adds request-scoped limits while preparing historical snapshots and
permission-filtered views. It preserves the selected state and the caller's exact
policy when replay, caches or derived masks need substantial work.

The capability is specified under roadmap item 395. No phase has shipped.

Read the [design and acceptance](SPEC.md). Its [Outcome](SPEC.md#outcome) records the
delivery boundary, and its [Provenance](SPEC.md#provenance) records the design sources.
