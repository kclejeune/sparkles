# U12: Ontology workbench

> **Status:** planned.
>
> **Roadmap:** [item 457](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

The design covers an ontology editor in the web UI with class, property and individual trees, entity frames, a SKOS thesaurus editor, a DL Query tab and an editable schema diagram.

## Problem and goal

The web UI browses schemas and edits instance data through shape-driven forms, but it
cannot edit an ontology. Property hierarchies are not shown, SKOS concept schemes have no
editor, and there is no way to ask which classes satisfy a class expression.

The goal is an ontology workbench in the web UI over the F21 axiom model.

## Scope

The workbench edits entities, axioms and annotations through the axiom model of
[F21](../F21-ontology-api.md) and shows asserted and inferred trees side by side from
[F24](../F24-owl-classification/README.md). Term editing reuses the forms of
[U02](../U02-shape-driven-forms/README.md) for the common case.

## Candidate phases

**SKOS and SKOS-XL thesaurus editor.** Edit concept schemes, top concepts, collections, broader, narrower and related links, multilingual labels, SKOS-XL labels and mapping relations in a tree. SKOS is more common than OWL in enterprise taxonomies. No support exists. It depends on item 21.

**Ontology editor UI.** Add editable class, object, data and annotation property and individual trees, an entity frame with axioms, an annotation editor with language tags and annotated axioms, a header editor, an axiom-level usage view, side-by-side asserted and inferred trees, IRI policy hookup, create, rename and delete, a session undo stack, and live refresh of open views from the change feed, all writing previewed commits on a branch. Real-time co-editing stays with the U05 §1 open question. Protégé and WebProtégé users expect these views. Property hierarchies are missing entirely. It depends on items 21 and 71.

**DL Query tab.** Evaluate a Manchester class expression for sub- and superclasses, equivalents and instances, first under an RL approximation through demand reasoning, then under the EL or DL classifier. Protégé users expect it. It depends on item 71.

**Editable schema diagram.** Draw the schema diagram from declared axioms as well as observed data, with restrictions as labelled edges, an optional VOWL notation and SVG, Mermaid and PlantUML export, and turn it into a modeling canvas that adds classes, properties and restrictions by drawing. Stardog Designer and Gra.fo offer the same canvas. It depends on item 31.

## Dependencies

* Item 71 supplies the axiom model, and the classifier of F24 supplies inferred trees.
* Item 21 supplies the shape-driven forms, and item 31 the schema diagram this extends.
* The resource catalog of [C30](../C30-resource-catalog/README.md) supplies IRI minting
  and vocabulary resources.

## Acceptance gates

* Every edit is a previewed commit through the existing write path, with the same
  access checks and conflict handling as other writes.
* Round trips through the axiom model leave unrelated triples unchanged.

## Non-goals

* A desktop application. The workbench is part of the web UI.

## Design constraints

The ontology-editing exclusions of U02 §1 and U07 §1 are lifted for this spec.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: VocBench, PoolParty, EDG Taxonomy, Protégé, WebProtégé, Protégé DL Query, Stardog Designer and OntoGraf. No code from another project is copied.

## Outcome

Nothing is built. [Item 457](../../ROADMAP.md) tracks the work.
