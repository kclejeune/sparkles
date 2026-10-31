# F24: OWL classification

> **Status:** planned.
>
> **Roadmap:** [item 457](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

An OWL 2 EL classifier and a bridge to external DL reasoners over the F21 axiom model, with inferred-axiom export, justifications for any entailed axiom and repair suggestions.

## Problem and goal

SNOMED CT, GO, ChEBI and most OBO ontologies are in OWL 2 EL, and OWL 2 RL is
incomplete for classification. Ontology editors rely on the inferred class hierarchy,
unsatisfiable classes and justifications, which Sparkles does not compute.

The goal is classification and its explanations over the axiom model of
[F21](../F21-ontology-api.md).

## Scope

A consequence-based concurrent EL++ classifier writes the inferred hierarchy and the
unsatisfiable classes, and it is incremental on axiom changes. A bridge runs an external DL
reasoner on an exported ontology and writes its results back with provenance. Export of
inferred axioms, justifications and repairs work on the classifier's results.

This engine stays separate from the rule core of [F23](../F23-reasoner-core/README.md).
Both read the same axiom model, so the classifier never depends on the rule
representation.

## Candidate phases

**OWL 2 EL classifier.** Add a consequence-based concurrent EL++ classifier over the F21 axiom model that writes the inferred hierarchy and unsatisfiable classes, incremental on axiom changes. SNOMED CT, GO, ChEBI and most OBO ontologies are EL, and OWL 2 RL is incomplete for classification. Protégé users rely on the inferred hierarchy, unsatisfiability and DL Query. It depends on item 71.

**External OWL DL reasoner bridge.** Run HermiT, Openllet or Konclude as a sandboxed child process or through the P04 JVM binding on an exported ontology, and write back inferred axioms and unsatisfiable classes with provenance. It gives DL classification and consistency without building a tableau reasoner, as Protégé's reasoner plug-ins do. It depends on items 71 and 339.

**Inferred-axiom export and redundancy removal.** Export inferred subclass and equivalence axioms only, annotated as inferred, and remove redundant asserted subclass axioms. Every OBO release uses these ROBOT reason options.

**Justifications for any entailed axiom.** Compute minimal and laconic justifications for entailed axioms, not only for findings and unsatisfiable classes, rendered in Manchester syntax. Sparkles' explanations are strong but limited to facts and RL findings. It depends on item 18.

**Hitting-set repair suggestions.** Compute minimal axiom sets whose removal repairs all justifications of an unwanted entailment, ranked by impact. Support-fact removal preview exists for one support at a time. It depends on item 18.

## Dependencies

* Item 71 supplies the F21 axiom model, its mappings and profile checks.
* Item 339 supplies the durable jobs that run an external reasoner.
* Item 18 supplies the explanation machinery that justifications extend.

## Acceptance gates

* The classifier's hierarchy and unsatisfiable classes match a reference EL reasoner on
  SNOMED CT, GO and the ORE test ontologies, which the reasoning suite of item 452 runs.
* An incremental reclassification gives the same result as a full one.
* The external bridge runs in a sandboxed child process with a time and memory budget,
  and a failed run writes nothing.

## Non-goals

* A native tableau reasoner for OWL 2 DL. DL reasoning goes through the external
  bridge.

## Design constraints

Classification works on the F21 axiom model, so the separation from F23 holds in
both directions.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: ELK (Kazakov, Kroetzsch and Simancik JAR 2014), Snorocket, ROBOT reason, OWL API, HermiT, Openllet, Konclude, Protégé reasoner plugins, ROBOT, Protégé Explanation Workbench, OWL API BlackBox, Protégé debugger plugins and OWL repair. No code from another project is copied.

## Outcome

Nothing is built. [Item 457](../../ROADMAP.md) tracks the work.
