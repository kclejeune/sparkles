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
messages. Each worker takes root contexts in chunks from a shared cursor, activates up
to 1,024 of them in a round and then processes the contexts that its messages woke, in
the order they woke, before it takes more roots. A context is processed by one worker at
a time, and workers count their conclusions and memory locally and add them to the
shared totals in batches. Property chains are binarized, and transitivity is the
chain of a property with itself. A range is pushed into the filler of each existential
restriction on the property, which is complete under the range restriction of OWL 2 EL
(Profiles §2.2.6). A chain that breaks that restriction is left out. The taxonomy groups
the classes by mutual subsumption and computes direct superclasses by transitive
reduction, by greedy minimization or, for a class with more than 32 direct superclasses,
by marking the classes above each candidate. The subsumers, the equivalence classes and
the direct superclasses are computed in parallel chunks from the saturated contexts
alone, with the classes numbered in IRI order, so the taxonomy is the same for every
number of threads.

`Saturation` keeps the contexts over axioms that the caller keeps, reads them in place
on every run and takes the axioms added and removed since the last run. `Classifier`
keeps a copy of the axioms on top of it, for callers that add and remove axioms one at a
time. The classes of the signature come from the run's normalization, which marks each
class it interns, and a signature walk covers only the axioms that the normalization
leaves out. The index holds one record per expression, with the told subsumers, the
conjunctions and the existential restrictions that use the expression as spans of flat
arrays.

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
taxonomy is built, as well as during the saturation. The estimate counts the classifier's copy of the axioms, the normalized axioms, the index with
the property closure, which stops as soon as it passes the budget, the context slots and
roots, the stored conclusions, the messages waiting in the contexts' to-do lists and
the taxonomy with its links, which the budget checks before the taxonomy is built and
again before the links are stored. The memory pool is charged for the growth of the peak.
A counting allocator measured each run's peak heap within 14% of the estimate on GO,
ChEBI lite and the largest ORE 2015 EL ontologies. A run that stops before its saturation ends
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
its own deadline. The dataset keeps the saturation and the ontology it read, whose axioms
the open ontology shares, and at the next commit it passes the saturation the axioms
that differ between the two. The threads are taken before that, so a run that times out
while it waits for them leaves the kept state as it was. The kept state is dropped after
a run whose estimated size passes one gigabyte, and it costs memory while the dataset is
open, about 74 MB estimated for GO.

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
items 634 to 637 add ROBOT-compatible `sparkles ontology` commands for the
outputs that Sparkles does not write. The
reasoners are packaged in `nix/reasoners` from checksum-pinned releases and run through
`scripts/elk-compare/elk.sh`. On the benchmark host the classifier classifies GO, ChEBI
lite and the 8 largest ORE ontologies faster than Konclude and Whelk, and HermiT confirms
the 7 GO edges that come from the inverse property. Konclude takes 1.3 s on GO, 4.2 s on
ChEBI lite and 12.2 s on the largest ORE ontology, and `sparkles ontology classify` runs
3.1 to 3.3 times as fast as `robot reason` end to end
([results](../../benchmarks/el-classification/RESULTS.md#reference-reasoners)). SNOMED CT is not available to the project and was not used, so the acceptance
gate's SNOMED CT comparison remains open.

**Measurements.** On forge, beside ELK 0.6.0 and with the harness of item 591, the
classifier classifies GO in 280 ms on one thread and 83 ms on eight, against ELK's
1,216 and 608 ms, and ChEBI lite in 2.1 s and 0.84 s, against 6.7 s and 2.5 s. On the
largest ORE 2015 EL ontologies it is 2.5 to 11.5 times as fast as ELK on one thread and
5.1 to 13.3 times as fast on eight, and `ore_ont_868`, of 981,151 classes, classifies in
1.9 s and 1.0 s against 11.7 s and 5.5 s. Its peak resident set is below ELK's on every
run, 252 MiB for GO and 930 MiB for ChEBI lite on eight threads. Items 602 and 592 made
these changes. Before them the classifier copied each axiom twice on the way in and once
more for its signature, built the taxonomy on one thread with hash maps keyed by
expression, sent every activation through a shared queue and left out the taxonomy from
its estimate. An interleaved A/B on `atlas` against the build of commit aa0a41a6 took
ChEBI lite on eight threads from 2,955 to 1,009 ms, `ore_ont_868` from 8,226 to 1,393 ms
and `ore_ont_1559` from 8,099 to 781 ms, and the peak resident set from 1,337 to 804 MiB,
3,514 to 1,686 MiB and 3,593 to 1,773 MiB. Normalization and interning still run on one
thread and take most of a run on eight threads on the ORE ontologies, which item
644 follows. The [benchmark
report](../../benchmarks/el-classification/RESULTS.md) has the numbers and the records.

**Testing.** Tests in `crates/sparkles-owl/tests/classify.rs` cover worked examples of
each construct, the reports of excluded axioms, the bounds, incremental runs, a
comparison with an independent naive completion procedure for EL++ on random ontologies
of up to twenty axioms, incremental against full runs on random changes for both
`Classifier` and `Saturation`, the same taxonomy on one, two and eight threads for an
ontology of 3,000 classes, and an estimate that bounds a run with its taxonomy. Unit
tests of the taxonomy check that the estimate counts every link of a wide hierarchy,
that an inconsistent ontology's taxonomy asks the budget and that every stage polls the
deadline. The library
tests cover the kept classification, the refusals, the taxonomy write with its
provenance and its replacement, the budgets, assertions and the JSON operation. Route
tests cover the answer, the functional-style form, dry runs and the caller's grants, a
CLI test classifies a file and a database, and each binding's tests classify and write a
taxonomy.

**Roadmap follow-ups.** An MCP tool is item 586. Complete reasoning with
individuals and datatypes is item 587. Reads that answer from the classification
are item 588. DL queries answer from the kept classifier through `Classifier::saturate`
and `Classifier::describe`, as [U12's DL Query outcome](../U12-ontology-workbench/SPEC.md#dl-query)
describes. Durable and automatic classification is item
589, after item 339. A kept classification with assertions is item 590,
the comparison and measurements against ELK were item 591, the speed and memory work
against ELK were items 602 and 592, and normalization and interning on several threads
are item 644.
