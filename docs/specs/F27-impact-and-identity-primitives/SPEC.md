# F27: Impact and identity primitives

> **Status:** planned.
>
> **Roadmap:** [item 463](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

The design covers generic entity-resolution primitives, tier and concentration metrics, decaying weighted reachability, recommendation explanations and survivorship rules.

## Problem and goal

Applications built on Sparkles, such as entity resolution and supply-chain analysis,
need similarity functions, candidate pairs, impact propagation and golden-record views.
Matching logic and domain adapters stay application work, so the engine must supply the
generic primitives.

The goal is a set of domain-neutral primitives in the engine.

## Scope

The primitives are blocking keys and string similarity functions with a candidate-pair
service, tier depth and concentration metrics, impact propagation with decay weights and
path witnesses, explanations behind a score and survivorship rules for entity
projections.

## Candidate phases

**Generic entity-resolution primitives.** Add blocking keys, Jaro-Winkler, token-set and n-gram similarity functions and a candidate-pair service that produces possible-match proposals. Entity representation is the third priority, and matching logic stays application work, so the engine must supply the primitives. It depends on items 81, 396 and 407.

**Tier and concentration metrics.** Package tier depth from a root, HHI concentration and path share through a node as generic stored-query templates or algorithms. It depends on item 340.

**Decaying weighted reachability and impact propagation.** Add `algo:impact` with multiplicative or decay weights, max-product or noisy-OR aggregation, top-k reached and path witnesses. It generalizes BFS and SSSP to probability semirings, and F07 has no product or decay semantics. It depends on items 8 and 81.

**Recommendation explanations.** Return path witnesses and shared-neighbour contributions behind a score. It explains a score to users and agents.

**Survivorship rules for entity projections.** Select attribute values by source rank, recency or evidence for golden-record views. A01 representatives assert no values. It depends on items 81 and 396.

## Dependencies

* Items 396 and 407 supply the entity representation and matcher fixtures, item 81 the
  evidence records, item 8 the algorithm library and item 340 maintained analyses.

## Acceptance gates

* Each primitive is generic, with tests on fixtures from at least two domains.

## Non-goals

* Domain adapters and matching policies, which remain application work.

## Design constraints

The engine supplies generic primitives only.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: Fellegi-Sunter, Senzing and Stardog public docs, Semiring path algebra, noisy-OR and MDM practice. No code from another project is copied.

## Outcome

Nothing is built. [Item 463](../../ROADMAP.md) tracks the work.
