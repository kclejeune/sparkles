# F26: Temporal paths

> **Status:** planned.
>
> **Roadmap:** [item 463](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

Time-respecting paths and temporal projections over valid time, custody-chain checks and algorithm results across time slices with change-point detection.

## Problem and goal

Graph algorithms and path search run at one snapshot, and valid time from
[C27](../C27-valid-time-profiles.md) has no path or projection semantics. Contact tracing
and custody chains need edges valid at a time and paths whose steps follow each other in
time.

This spec defines those semantics before any implementation.

## Scope

Temporal projections select edges valid at a time or within an interval and feed
`algo:` services. Earliest-arrival paths respect the order of edge validity. Custody
checks verify time-ordered chains over PROV-O and report gaps. Algorithms can run across
commits or valid-time slices and flag change points.

## Candidate phases

**Metric time series and change-point detection.** Run an algorithm across commits or valid-time slices, emit (t, node, value) and flag change points. Algorithms run at one snapshot. It depends on items 52 and 341.

**Provenance lineage closure with custody gaps.** Verify time-ordered custody chains over PROV-O and report gaps. Biosecurity provenance needs it. It depends on items 81 and 341.

**Time-respecting paths and temporal projections.** Select edges valid at t and compute earliest-arrival paths, with a valid-time filter for algo: projections. The semantics get their own spec before implementation. Contact tracing and custody chains need it. It depends on item 341.

## Dependencies

* Item 341 supplies valid time and observation history.
* Item 8 supplies the algorithm library and its projections.

## Acceptance gates

* Temporal path results match a reference implementation on fixtures with overlapping
  and open intervals.
* Projections respect the caller's view, including protections on validity metadata.

## Non-goals

* Stream windows over event time, which the operations workstream holds.

## Design constraints

The semantics are specified here before implementation starts.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: CUSUM, Bayesian online change-point detection, PROV-O and Wu et al. earliest-arrival paths. No code from another project is copied.

## Outcome

Nothing is built. [Item 463](../../ROADMAP.md) tracks the work.
