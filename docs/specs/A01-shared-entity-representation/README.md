# Shared entity representation, convergence and evolution

A01 defines roadmap item 396. Phase 1 provides versioned profile resources and
independent acceptance fixtures. Offline replay and managed integration remain.

The application profile separates source descriptions from domain entities and makes
identity resolution, qualified relationships and changes reproducible. It reuses C25
claims and review decisions and C27 temporal records. It introduces no new storage
format, logical equality machinery or managed-write authority.

Phase 1 provides versioned vocabulary, shapes, fixed fixtures and eight competency
queries through `sparkles::shared_entity::profile(1)`.
Phase 2 adds bounded offline reference replay and selected adapter mappings. Phase 3
integrates the profile with the actual shared evidence, temporal, durable execution
and analysis runtimes after their required contracts pass acceptance.

Read the [design and acceptance](SPEC.md), its [Outcome](SPEC.md#outcome) and
[Provenance](SPEC.md#provenance). Current runtime capabilities are described in the
[evidence guide](../../ai/README.md#evidence-profile) and
[valid-time guide](../../ai/README.md#valid-time-profile).
