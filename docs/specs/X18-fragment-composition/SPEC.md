# X18: Fragment composition

> **Status:** planned.
>
> **Roadmap:** [item 458](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

The design covers immutable fragment generations that index each source version once, and datasets composed by a k-way merge of fragment vocabularies and sorted permutations.

## Problem and goal

Loading a large public dataset into several local datasets parses and sorts it each
time, and a source update rebuilds every dataset that uses it.

The goal is to index a source version once and compose datasets from those fragments, so
that a source update re-merges one fragment.

## Scope

A fragment generation is an immutable index of one source version. A composed dataset
is a k-way merge of fragment vocabularies and sorted permutations through ID remap
tables, reusing the compaction merge. The multi-base overlay that mounts fragments without
copying is deferred until fragment composition is measured.

## Candidate phases

**Fragment generations and merge composition.** Index each source version once into an immutable fragment generation and compose datasets by a k-way merge of fragment vocabularies and sorted permutations with ID remap tables, re-merging one fragment on source updates. Loading large public datasets becomes a linear merge without reparse and sort. The merge needs 8 bytes per term per fragment for remap tables, and the composed dataset costs full disk again. It depends on item 14.

**Multi-base overlay for graph import by reference.** Mount immutable fragments as named graphs without copying, with a merged ID map built once per mount (about 8 bytes per term of the smaller side), so queries, statistics and access control see the union. One copy of a very large source could serve many datasets. Measure whether fragment merge time is acceptable first. It depends on items 9 and 14. This phase is deferred until the condition under Design constraints holds.

## Dependencies

* The storage consolidation of [item 455](../../ROADMAP.md) lands first.
* The resource catalog of [C30](../C30-resource-catalog/README.md) supplies the source
  versions.
* Item 14 supplies the per-block merge.

## Acceptance gates

* A composed dataset answers every query exactly as a dataset loaded from the same
  sources does.
* The merge uses 8 bytes per term per fragment for its remap tables, and the composed
  dataset costs its full size on disk. Both tradeoffs are recorded with measurements.

## Non-goals

* A vocabulary shared across datasets, which the per-server vocabulary decision of C30
  rules out.

## Design constraints

The multi-base overlay is built only after fragment composition ships and
measurements show that duplicated storage matters.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: QLever multi-input, LOD Laundromat and Generalizes F09 virtual bases. No code from another project is copied.

## Outcome

Nothing is built. [Item 458](../../ROADMAP.md) tracks the work.
