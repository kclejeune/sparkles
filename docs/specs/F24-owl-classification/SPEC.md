# F24: OWL classification

> **Status:** partially complete. The OWL 2 EL classifier is delivered as roadmap item 580.
>
> **Roadmap:** [item 457](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.
>
> **User docs:** [OWL 2 ontologies](../../reasoning/README.md#owl-2-ontologies) and
> [API: Ontology](../../API.md#ontology).

The design covers an OWL 2 EL classifier and a bridge to external DL reasoners over the F21 axiom model, with inferred-axiom export, justifications for any entailed axiom and repair suggestions.

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

**OWL 2 EL classifier (item 580, delivered).** Add a consequence-based concurrent EL++ classifier over the F21 axiom model that writes the inferred hierarchy and unsatisfiable classes, incremental on axiom changes. SNOMED CT, GO, ChEBI and most OBO ontologies are EL, and OWL 2 RL is incomplete for classification. Protégé users rely on the inferred hierarchy, unsatisfiability and DL Query. It depends on item 71.

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

* A native reasoner for OWL 2 DL in the phases above. DL reasoning goes through the
  external bridge until roadmap item 603 adds native DL classification on top of the EL
  classifier.

## Design constraints

Classification works on the F21 axiom model, so the separation from F23 holds in
both directions.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: ELK (Kazakov, Kroetzsch and Simancik JAR 2014), Snorocket, ROBOT reason, OWL API, HermiT, Openllet, Konclude, Protégé reasoner plugins, ROBOT, Protégé Explanation Workbench, OWL API BlackBox, Protégé debugger plugins and OWL repair. No code from another project is copied.

## Outcome

The OWL 2 EL classifier is delivered as roadmap item 580. The other phases remain under
[item 457](../../ROADMAP.md).

### The OWL 2 EL classifier

**The calculus.** `sparkles_owl::classify` follows the consequence-based calculus of
ELK (Kazakov, Krötzsch and Simančík, JAR 2014), written from the paper. Class
expressions are interned once, with intersections made binary. A context stands for an
instance of its root expression and holds the root's subsumers, the backward links from
contexts with an existential restriction it fills, the forward links that property
chains need and the role loops of `ObjectHasSelf` and reflexive properties. Each rule
reads the state of the context it runs in and sends conclusions to other contexts as
messages. Worker threads take active contexts from a shared queue, and a context is
processed by one worker at a time. Property chains are binarized, and transitivity is the
chain of a property with itself. A range is pushed into the filler of each existential
restriction on the property, which is complete under the range restriction of OWL 2 EL
(Profiles §2.2.6). A chain that breaks that restriction is left out. The taxonomy groups
the classes by mutual subsumption and computes direct superclasses by transitive
reduction, by greedy minimization or, for a class with more than 32 direct superclasses,
by marking the classes above each candidate.

**Scope of the reasoning.** The classifier covers `owl:Thing`, `owl:Nothing`,
intersections, existential restrictions, equivalences, disjointness, the object and data
property hierarchies, chains, transitive and reflexive properties, domains, ranges and
`ObjectHasSelf`. Individuals in class expressions, data ranges and literals are treated
as atoms, which is sound and incomplete, so such axioms are reported as partial. So are
assertions on a property with a range, since each range gives the individual a context
of its own that nominal reasoning would merge, and axioms on the top and bottom object
and data properties, which the classifier treats as ordinary properties. Keys, negative
property assertions, functional data properties, data property ranges, datatype
definitions and axioms with an expression nested more than 256 deep take no part and
are reported as partial too. No stand-in for a left-out expression is sound on both
sides of a subsumption, so the whole axiom goes. Axioms outside OWL 2
EL are left out and reported with the rule the EL check names. Annotations and
annotation axioms carry no logical content. A classification can include the store's
assertions, which makes it a consistency check of the individuals.

**Incremental runs.** The classifier keeps its contexts between runs. A run after
additions and removals clears the contexts that derived the left side of a changed
subsumption or part of it, the contexts whose links a changed existential restriction
can use, and every context that links to a cleared one, and saturates those again. The
other contexts drop their links from cleared contexts and recompute what they take from
the index, which is rebuilt from the axioms on every run. A change to the property
hierarchy, chains, reflexivity or ranges saturates everything again, and so does a
change to an axiom with a data property or datatype, whose generated rules for literals
and data ranges the invalidation does not follow, and a run after a run that stopped
before its saturation ended. A run that stops later keeps its saturated contexts. Property tests check that an incremental run after random
additions and removals gives the taxonomy of a full run.

**Bounds.** A run stops at its deadline, at a cancellation and when the estimated memory
passes the budget. The deadline and the cancellation are checked while the axioms are
normalized, while the property closure is built, between the stages and while the
taxonomy is built, as well as during the saturation. The estimate counts the classifier's copy of the axioms, the index with
the property closure, which stops as soon as it passes the budget, the stored
conclusions and the messages waiting in the contexts' to-do lists, and the memory
pool is charged for the growth of its peak. A run that stops before its saturation ends
drops its contexts, since the next run starts again. In the handle the threads come from the
algorithm pool, the budget from the query's memory budget and the server's memory pool
grant, and a run past the budget answers `507` with `budget: "memory"`.

**Surfaces.** `Ontologies::classify` and `OntologyScope::classify` classify an open
ontology. `POST /$/ontology/{ds}/classify`, the `classify` operation of the shared JSON
surface, `sparkles ontology classify` and the Python, Node.js and JVM bindings run it. An
inferred read is refused, since a classification reads the asserted axioms. The dataset
keeps its last classification of an ontology for callers who read every graph, without
assertions, keyed by the graphs, imports and mode. A classification at the same commit
answers from it, and one at a later commit gives the kept classifier the axioms that
changed. One kept classification runs at a time, and a second caller waits for it under
its own deadline. The kept state is dropped after a run whose estimated size passes one
gigabyte, and it costs memory while the dataset is open, about 115 MB estimated and
190 MB resident for GO.

**Writing the taxonomy.** A run with `target` writes the taxonomy to a named graph that it
does not read, through the handle's edit, in one commit. It adds the taxonomy's axioms
and removes the subsumptions and equivalences between named classes that the graph held
and the taxonomy no longer has. Provenance is written on the graph's ontology header,
not on each axiom as the MCP tool of F21 writes it, so that an unchanged axiom keeps no
stale activity and a run writes only the axioms that changed. The header carries
`prov:wasGeneratedBy` a `urn:uuid:` activity, `prov:generatedAtTime`,
`prov:wasDerivedFrom` each named graph read and an `rdfs:comment` with the commit read,
and a later run replaces them. The write reads the head under the writer lock, and when
the terminology read has changed since the classification it answers a conflict. The
taxonomy written is the full direct hierarchy, asserted and inferred, as ELK writes one.
Inferred-only export and redundancy removal belong to the export phase.

**Comparison with a reference.** The classifier's direct hierarchy of GO (release
2026-07-26, 51,986 classes) equals the asserted named hierarchy of GO's release, which
GO's pipeline computes with ELK and reduces, with 57,456 direct subsumptions on each
side and no difference. On ChEBI lite (release 256, 238,150 classes) every asserted
subsumption between named classes is entailed and every direct subsumption the
classifier gives is asserted. `scripts/el-oracle.py --asserted` makes these
comparisons. ELK 0.6.0, run through the OWL API by `scripts/elk-compare/elk.sh`, gives
the same taxonomy as the classifier on GO, ChEBI lite and all 298 ontologies of the ORE
2015 EL classification sample, with the same unsatisfiable classes and the same
inconsistent ontology. `scripts/el-oracle.py` found no difference to explain, including
on the 18 ORE ontologies with axioms the classifier uses in part and the 31 where ELK
reports possible incompleteness. HermiT 1.4.5.519, a complete OWL 2 DL reasoner, gives
the same taxonomy on the 36 ORE ontologies where either EL reasoner uses axioms in part.
Four test ontologies show what both EL reasoners miss when a property's range meets an
individual, two subsumptions and an inconsistency, which the classifier reports as axioms
used in part and item 587 covers. Konclude 0.7.0-1138 gives the same taxonomy on all 298
ORE ontologies and ChEBI lite, and on GO it entails 14 more subsumptions between named
classes, with 7 more direct edges, all through
`InverseObjectProperties(part_of has_part)`, the GO axiom outside OWL 2 EL that the
classifier leaves out and reports. Whelk 1.2.1 agrees on GO, ChEBI lite and 296 ORE
ontologies and is incomplete on one. ROBOT 1.9.11's `reason`, `reduce`, `relax` and
`materialize` with ELK assert the classifier's direct hierarchy on GO and ChEBI lite, and
items NEW-ITEM-1 to NEW-ITEM-4 add ROBOT-compatible `sparkles ontology` commands for the
outputs that Sparkles does not write. The
reasoners are packaged in `nix/reasoners` from checksum-pinned releases and run through
`scripts/elk-compare/elk.sh`. On the benchmark host the classifier classifies GO, ChEBI
lite and the 8 largest ORE ontologies faster than Konclude and Whelk, and HermiT confirms
the 7 GO edges that come from the inverse property. Konclude takes 1.3 s on GO, 4.2 s on
ChEBI lite and 12.2 s on the largest ORE ontology, and `sparkles ontology classify` runs
3.1 to 3.3 times as fast as `robot reason` end to end
([results](../../benchmarks/el-classification/RESULTS.md#reference-reasoners)). SNOMED CT is not available to the project and was not used, so the acceptance
gate's SNOMED CT comparison remains open.

**Measurements.** On forge, GO classifies in 410 ms and ChEBI lite in 2.97 s on four
threads, at peak resident sets of 438 MiB and 1.66 GiB that include the axioms read.
Saturation scales with the threads, and indexing and the taxonomy, which run on one
thread, take most of the time from four threads on. Keeping each worker's activations
on a stack of its own instead of a shared queue took ChEBI on eight threads from 6.0 s
to 2.8 s. Beside ELK 0.6.0 on forge, the classifier classifies GO in 354 ms against
ELK's 553 ms on eight threads, and ChEBI lite in 4.0 s against 6.7 s on one thread. ELK is
1.16 times as fast on ChEBI lite on eight threads, 2.5 s against 3.0 s, because the
classifier's indexing and taxonomy stages run on one thread. With the OWL API's load
counted, ELK takes 1.3 s and 6.1 s on eight threads. The classifier's peak resident set is
363 MiB and 1.6 GiB, and ELK's is 2.4 GiB and 4.3 GiB with a 4 GiB heap. The [benchmark
report](../../benchmarks/el-classification/RESULTS.md) has the numbers and the records.

**Testing.** Tests in `crates/sparkles-owl/tests/classify.rs` cover worked examples of
each construct, the reports of excluded axioms, the bounds, incremental runs, a
comparison with an independent naive completion procedure for EL++ on random ontologies
of up to twenty axioms, and incremental against full runs on random changes. The library
tests cover the kept classification, the refusals, the taxonomy write with its
provenance and its replacement, the budgets, assertions and the JSON operation. Route
tests cover the answer, the functional-style form, dry runs and the caller's grants, a
CLI test classifies a file and a database, and each binding's tests classify and write a
taxonomy.

**Roadmap follow-ups.** An MCP tool is item 586. Complete reasoning with
individuals and datatypes is item 587. Reads that answer from the classification
and DL Query are item 588. Durable and automatic classification is item
589, after item 339. A kept classification with assertions is item 590,
the comparison and measurements against ELK were item 591, and a classifier that
shares the ontology's axioms and builds its index and taxonomy in parallel is item
592.
