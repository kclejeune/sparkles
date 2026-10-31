# F28: ML and probabilistic integration

> **Status:** planned.
>
> **Roadmap:** [item 463](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

Integration with ML pipelines, model registries and probabilistic engines through Croissant, Parquet, MLflow, Hugging Face Hub, ONNX, KServe, Triton, Feast, OpenLineage, PSL and ProbLog.

## Problem and goal

Graph features, projections and embeddings are useful inputs to ML pipelines, and
assessments are natural inputs to probabilistic reasoning. Sparkles has no way to hand
them to standard tools or to bring results back with their lineage.

The goal is integration with the standard tools, not a reimplementation of them.

## Scope

Exports write graph features, projections and embeddings as MLCommons Croissant
datasets and Parquet. Models are registered and resolved in the MLflow model registry or
the Hugging Face Hub, scored with ONNX in the engine or through KServe or Triton, and
features sync with Feast. Collaborative filtering exports bipartite projections to an
external trainer and imports its vectors. Probabilistic reasoning exports assessments to
PSL or ProbLog and imports propagated beliefs as assessments. OpenLineage records the
lineage of everything written back.

## Candidate phases

**Probabilistic reasoning through external engines.** Export C25 assessments and their evidence to established probabilistic engines such as PSL or ProbLog, import propagated beliefs back as assessments with method and lineage recorded through OpenLineage, under an explicit opt-in. C25 and F22 exclude probabilistic reasoning as a native feature, so this is integration with existing engines. It depends on items 81 and 340.

**ML pipeline integration through industry standards.** Integrate with existing ML tooling rather than reimplementing it: export graph features, projections and embeddings as MLCommons Croissant datasets and Parquet, register and resolve models in the MLflow model registry or Hugging Face Hub, score ONNX models in-engine or call KServe or Triton endpoints, sync features with Feast, and emit OpenLineage events so predictions written back keep their lineage. F14 excludes native ML pipelines, so this integrates with the standard registries, formats and serving stacks. It depends on items 26 and 340.

**Collaborative filtering through external trainers.** Export a bipartite projection as a Croissant dataset or Parquet for an external ALS or BPR trainer, import the resulting vectors through ONNX or the MLflow registry into the vector index, and record lineage with OpenLineage. It depends on items 8 and 34.

## Dependencies

* Item 340 supplies maintained analyses and their run records, item 81 assessments,
  item 26 local models, item 8 projections and item 34 vector indexes.

## Acceptance gates

* Everything written back carries its method and lineage, and an import refuses data
  whose lineage it cannot record.
* Probabilistic integration runs only under an explicit opt-in.

## Non-goals

* Training infrastructure, a native model catalog or a native probabilistic reasoner.

## Design constraints

ML and probabilistic work is integration only, through the named standards and
tools.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: PSL, ProbLog, OpenLineage, MLflow model registry, ONNX Runtime, MLCommons Croissant, Hugging Face Hub, KServe, Triton, Feast, implicit ALS, Spark MLlib, MLflow and ONNX. No code from another project is copied.

## Outcome

Nothing is built. [Item 463](../../ROADMAP.md) tracks the work.
