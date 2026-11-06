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

**Inferred-axiom export and redundancy removal (items 634, 635 and 637, delivered).** Export inferred subclass and equivalence axioms only, annotated as inferred, and remove redundant asserted subclass axioms. Every OBO release uses these ROBOT reason options.

**Existential superclasses (item 636, delivered).** Assert the direct inferred superclasses of the form `ObjectSomeValuesFrom(p C)` for chosen properties, as `robot materialize` does, so that a release can carry the part-of and has-role parents that its classification derives.

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

* A native reasoner for OWL 2 DL in the phases above. Roadmap item 603 adds native DL
  classification on top of the EL classifier, which covers Horn ontologies and ALCHI
  without individuals, and the rest of OWL 2 DL goes through the external bridge until its
  later phases.

## Design constraints

Classification works on the F21 axiom model, so the separation from F23 holds in
both directions.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: ELK (Kazakov, Kroetzsch and Simancik JAR 2014), Snorocket, ROBOT reason, OWL API, HermiT, Openllet, Konclude, Protégé reasoner plugins, ROBOT, Protégé Explanation Workbench, OWL API BlackBox, Protégé debugger plugins and OWL repair. No code from another project is copied.

## Outcome

The OWL 2 EL classifier is delivered as roadmap item 580 and covers OWL 2 EL, apart
from the literals and the chains that item 663 follows, since item 587. Item 603 extends
it to Horn ontologies without individuals, as [the Horn rules](#the-horn-rules)
describe, and to ALCHI ontologies without individuals that need case analysis, as
[the disjunctive rules](#the-disjunctive-rules) describe. The export of inferred axioms and the removal of redundant ones are delivered
as the ROBOT-compatible commands of items 634, 635 and 637, and the existential
superclasses as `materialize`, item 636. The other phases remain under
[item 457](../../ROADMAP.md).

The bounded named-class read phase of items 588 and 622, and the availability route
of item 726, publish an immutable completed result beside the incremental saturation.
`GET /$/ontology/{ds}/classification` and the Rust, Python, Node.js and JVM read adapters
return availability, producer/source identity, consistency and bounded direct-parent,
equivalence, source/warning and exclusion pages without classifying on a miss. General
entity accessors, inferred axiom listings, property/individual entailments and matching
justifications remain in items 588, 622, 609 and F24's remaining phases. This phase adds
no implicit classification, materialization or explanation source.

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
number of threads. The classes are gathered in parallel chunks and put in IRI order by a
parallel sample sort, the nodes are numbered in one pass and filled in parallel chunks,
and the lookup from a class to its node is a table in IRI order that a binary search
reads, in place of a hash map. The IRIs are distinct, so every number of threads gives
one order (roadmap item 646).

**Normalization and messages on several threads.** A run of 16,384 axioms or more on
several threads normalizes chunks of the axioms in parallel, each into an interner of
its own that keeps an intersection whole until the operands have their final ids. The
chunks then merge in order into the run's interner, so that each name and expression
gets the id a normalization on one thread gives it, the binary intersections are folded
from the final ids and every thread count gives the same ids and taxonomy (roadmap
item 644). A run whose chunks meet data ranges or literals, whose ids sit inside their
values, normalizes on one thread. Each IRI is hashed once, held once in a table that
keeps its hash, and a class's or an individual's expression is found by its id in an
array (item 647). On several threads a worker buffers the messages it sends to other
contexts and delivers them grouped by destination with a radix sort, taking each
destination's lock once per batch, when the buffer fills, when another worker is idle
and before it waits, and a long activation shares the contexts it woke with idle
workers as it goes (item 645). The count of pending work is the shared queue's entries
plus the busy workers, so a context marked active costs no shared counter. A context's
subsumers stay a list of up to 12 members, searched eight at a time, before they become
a hash set (item 648). The rules are compiled twice, and a run whose index has no
individuals, universal roles, nominal self restrictions or functional data roles uses
the copy without their checks (item 665).

`Saturation` keeps the contexts over axioms that the caller keeps, reads them in place
on every run and takes the axioms added and removed since the last run. `Classifier`
keeps a copy of the axioms on top of it, for callers that add and remove axioms one at a
time. The classes of the signature come from the run's normalization, which marks each
class it interns, and a signature walk covers only the axioms that the normalization
leaves out. The index holds one record per expression, with the told subsumers, the
conjunctions and the existential restrictions that use the expression as spans of flat
arrays.

**Scope of the reasoning.** The classifier covers OWL 2 EL, apart from the literals,
datatypes and chains this paragraph names last. Besides `owl:Thing`,
`owl:Nothing`, intersections, existential restrictions, equivalences, disjointness, the
object and data property hierarchies, chains, transitive and reflexive properties,
domains, ranges and `ObjectHasSelf`, it reasons with individuals in class expressions
and assertions, the datatypes, literals and data ranges of the profile, functional data
properties, data property ranges, datatype definitions, keys, negative property
assertions and the top and bottom properties, with the rules of EL++ (Baader, Brandt and
Lutz, IJCAI 2005 and OWLED 2008) written from the papers. Item 587 added them, as the
next three paragraphs describe. A literal or a datatype that the classifier cannot
compare, which is an XML literal, an ill-typed literal, a datatype outside the OWL 2
datatype map or a number past 128 bits, is an atom that only equals itself, and its
axiom is reported as used in part. So is a property chain through the top object
property, whose links the classifier does not compose. An axiom with an expression
nested more than 256 deep takes no part, since no stand-in for a left-out expression is
sound on both sides of a subsumption. Axioms outside OWL 2 EL that the Horn rules do not
take are left out and reported with the rule the EL check names. Annotations and annotation axioms carry no logical
content. A classification can include the store's assertions, which makes it a
consistency check of the individuals.

**Individuals.** A context stands for an instance of its root, and its conclusions hold
wherever the root has an instance. Named individuals, `owl:Thing` and every context they
reach through existential links have an instance in every model, and these contexts are
real. A context that derives `{a}` registers with `a`'s context and
takes every subsumer `a` has and gets later. A real context that derives `{a}` also gives
`a` every subsumer it has, which merges the two. A link by `r` from a context with `{a}`
to one with `{a}` is a loop by `r`, a loop meets the range of its role, and a link by a
role under the bottom property derives `owl:Nothing`. What a context that is not real
knows of `a` holds only where its root has an instance. This is the reachability
condition of the completion rule CR6 of EL++, which Kazakov, Krötzsch and Simančík (KR
2012) show breaks goal-directed EL procedures. After the saturation, a context that is not
real, derives an individual and has a subsumer other than an intersection that the
individual lacks is a witness. A class whose instances reach no witness is classified
completely by the saturation, since merging a context with an individual whose
subsumers it already has changes no subsumer, link or loop of any context. Each named
class whose context reaches a witness through links is classified again in a world of
its own. A world is a copy-on-write layer over the saturated contexts in which the class
has an instance and every context it reaches is real. It keeps only the conclusions that
the saturated state lacks, runs on one thread and reaches the same fixpoint in any
order, and the worlds of different classes run in parallel. The classifier marks the
real contexts, in a second round of the saturation, and looks for witnesses only when a
context other than an individual's own derived the individual or the ontology has
individuals and keys or a universal role. Other ontologies, which include every one
without individuals, skip these steps and pay only for the rules of the first round. A
world is also where item 603's case
analysis can assume a disjunct over the same shared state, and the classes it needs come
from the same closure over links.

**Data.** The datatypes of OWL 2 EL form chains under `rdfs:Literal`, from `owl:real`
down to `xsd:nonNegativeInteger`, from `rdf:PlainLiteral` down to `xsd:NCName`, and
`xsd:dateTime` over `xsd:dateTimeStamp`, while `xsd:anyURI`, `xsd:hexBinary`,
`xsd:base64Binary` and `rdf:XMLLiteral` stand alone, and datatypes on different chains
have disjoint value spaces. Every value space is infinite, so a data range of the profile
is empty, one datatype or one value, and the classifier keeps it as an element of that
lattice. A literal's value is canonical. It is an exact rational for a number, a string,
a string with a lower-case language tag, decoded binary data, or for a date-time its
instant with its offset, since two date-times with different offsets are different
values (Structural Specification §4.7). Data values have no successors, so a data
restriction needs no context. The index gives a positive `∃d.E` the told subsumer
`owl:Nothing` when `E` meets an empty range of `d`, and every negative `∃g.F` with `d`
under `g` whose range `F` holds the value. A context keeps the meet of the ranges of its
data successors under each functional data property, and derives `owl:Nothing` when it
is empty and the negative restrictions it meets otherwise. The top data property links
every individual to every literal.

**Keys and negative assertions.** `NegativeObjectPropertyAssertion(p a b)` is `{a} ⊓
∃p.{b} ⊑ ⊥`, and the data form is `{a} ⊓ ∃d.{v} ⊑ ⊥`, which the rules above complete.
Keys (Structural Specification §9.5) apply between rounds of the saturation. Two named
instances of a key's class that share a named individual for each of its object
properties and a value for each of its data properties become one, and the saturation
resumes until no key merge and no fact of a universal role is new. Worlds apply them
too.

**Incremental runs.** The classifier keeps its contexts between runs. A run after
additions and removals clears the contexts that derived the left side of a changed
subsumption or part of it, the contexts whose links a changed existential restriction
can use, and every context that links to a cleared one, and saturates those again. The
other contexts drop their links from cleared contexts and recompute what they take from
the index, which is rebuilt from the axioms on every run. A change to the property
hierarchy, chains, reflexivity or ranges saturates everything again, and so does a
change to an axiom with an individual, a data property, a datatype or a key. So does any
change after a run that marked the real contexts, which merges, worlds, keys and the
facts of universal roles tie together in ways the invalidation does not follow, and a
run after a run that stopped before its saturation ended. A run that stops later keeps
its saturated contexts. Property tests check that an incremental run after random
additions and removals gives the taxonomy of a full run.

This reuse applies to eligible EL edits. Normalization with Horn or disjunctive axioms
currently clears the contexts and marks the run entangled, so a later edited source
rebuilds those runs. Reusing the completed result at the same commit is separate from
incremental maintenance. Item 753 delivers item 603's affected-module incremental DL
requirement, as [the follow-up](#incremental-native-dl-classification) describes.

**Bounds.** A run stops at its deadline, at a cancellation and when the estimated memory
passes the budget. The deadline and the cancellation are checked while the axioms are
normalized, while the property closure is built, between the stages and while the
taxonomy is built, as well as during the saturation and in the worlds. The estimate
counts the classifier's copy of the axioms, the normalized axioms, the index with
the property closure, which stops as soon as it passes the budget, the context slots and
roots, the stored conclusions, the messages waiting in the contexts' to-do lists and
the taxonomy with its links, which the budget checks before the taxonomy is built and
again before the links are stored. The worlds count their copies, stored conclusions and
waiting messages while they run, and the subsumers they keep once they end. The chunks
of a parallel normalization count their interners and axioms with room for what their
merge adds, the merge counts what it adds past that room, and a run whose chunks pass
the budget or a refused grant normalizes on one thread instead. The memory pool is
charged for the growth of the peak, once, from the normalization on.
A counting allocator measured each run's peak heap within 15% of the estimate on GO,
ChEBI lite and the largest ORE 2015 EL ontologies. A run that stops before its saturation ends
drops its contexts, since the next run starts again. In the handle the threads come from the
algorithm pool, the budget from the query's memory budget and the server's memory pool
grant, and a run past the budget answers `507` with `budget: "memory"`.

**Surfaces.** `Ontologies::classify` and `OntologyScope::classify` classify an open
ontology. `POST /$/ontology/{ds}/classify`, the `classify` operation of the shared JSON
surface, `sparkles ontology classify` and the Python, Node.js and JVM bindings run it. An
inferred read is refused, since a classification reads the asserted axioms. The dataset
keeps its last classification of an ontology for callers who read every graph, without
assertions, keyed by the graphs, imports and mode. A compatible classification at the
same commit answers from the retained result. At a later commit eligible EL additions
and removals reuse affected contexts, while Horn and disjunctive runs rebuild after
edits. Item 753 covers incremental native DL maintenance under item 603, and item 664
covers the EL individual, merge, key and world invalidation gaps. One kept classification
runs at a time, and a second caller waits for it under
its own deadline. The dataset keeps the saturation and the ontology it read, whose axioms
the open ontology shares, and at the next commit it passes the saturation the axioms
that differ between the two. The threads are taken before that, so a run that times out
while it waits for them leaves the kept state as it was. The kept state is dropped after
a run whose estimated size passes one gigabyte, and it costs memory while the dataset is
open, about 74 MB estimated for GO.

The kept publication records the bound store/branch identity, snapshot family, commit,
ordered normalized graphs, imports and mapping mode, with actual producer flags and
streaming input/content/source fingerprints. Partial-request and expansion policies
remain original producer metadata rather than compatibility keys. A later DL query can
change the saturation's expression inputs while the published taxonomy and its source
remain unchanged. Slot status/reference updates are short; classification, fingerprints,
estimates and name reads run outside its lock. An owned flight clears active state on
success, errors and unwind. Refusal before mutable work preserves the prior pair, while
a mutating failure discards it, and subsequent callers can start a fresh flight.
An idle exact legacy classify hit still reuses the completed publication without compute
work under an expired compute deadline. Bounded frame projection and GET reads check
their own read controls. After an actual stopped partial computation, immutable source
hashing and response serialization finish using result-local options that preserve graph
access without resetting shared cancellation flags. This read-only work can outlast the
compute deadline but never resumes classification or retains partial state. Completed
result serialization continues to honor its original cancellation and deadline.

The between-run estimate includes retained source strings, the copied excluded axioms
and taxonomy metadata. A DL query's retention decision includes its latest saturation
and the still-published older taxonomy. Partial or oversized outcomes drop both retained
owners. Old publications may remain live during a newer same-input run, and readers
may hold an evicted publication until their requests finish. This increases peak memory
beyond a single current estimate. The one-gigabyte retention cap is not a hard RSS cap
or a combined active-flight/reader admission limit.

Availability first validates the caller's access and requested history. Restricted graph
or triple views and synthetic snapshots receive a stable unsupported-scope miss without
observing shared state. A pinned read serves only its exact result ID, input and commit;
it does not wait on a newer flight or substitute another result. Otherwise a matching
publication takes precedence, then an exact active run, a same-input stale publication,
and absence. Source warnings form a typed virtual sequence whose complete digest and
counts agree with bounded page traversal. Page output and per-page labels are bounded,
while counts/order can scan retained vectors. Names use the pinned authorized snapshot
and complete IRI fallback for unproven historical prefixes.

Kept roots and direct-child pages expose the taxonomy's existing retained vectors without
adding another reverse index or downloading a whole taxonomy. Each bounded row includes
one representative, its equivalence count and direct child/parent counts. Nontrivial top
and bottom groups remain explicit. Entity status identifies a selected equivalent name's
representative without changing its asserted edit target. The same revision, publication,
source and access checks apply to hierarchy continuation and frame clauses. Materialized
property/individual views use a separate recorded-profile source and never advertise the
class-only publication as their producer. General classifier-backed entity/axiom reads
and matching arbitrary-axiom justifications remain open under items 588 and 622.

An optional explicit `projection=frame` on classify branches before legacy whole-node
and ancestor arrays. It preserves useful direct-parent/equivalent clauses even for an
isolated restricted computation, using its actual graph grant and already triple-masked
snapshot for names. Its bounded samples report totals and truncation, with no shared
continuation for a non-retained result. Legacy full-tree POST output remains available;
its multi-request clients pin the commit and compare exact input and complete taxonomy/
outcome fingerprints before appending, rather than comparing per-computation IDs.

**Files without a store.** `sparkles ontology classify FILE` and `sparkles ontology check
FILE` read a file of one graph straight into memory as `OntologyFile`
(`sparkles::handles::ontology::file`), with no store, and answer with the JSON, the
taxonomy, the diagnostics and the excluded axioms that a store read of the loaded file
gives (roadmap item 649). A file of triples in RDF/XML, Turtle, N-Triples or N3 is parsed
on several threads. Turtle and N-Triples are cut as the loader cuts them, and an RDF/XML
document whose root is `rdf:RDF` is cut between its top-level elements, each piece
parsed with the prolog and the root's start tag. A document with an `rdf:ID` or anything
the cut does not expect parses whole, and so does one with a piece that fails, so the
triples and the errors are those of a sequential parse. The triples are deduplicated as
a store's set is, the terminology is selected as an open ontology selects it, and the
triples go to the reverse mapping in the order of a store's scans, with the blank node
labels a store gives them. A functional-style document is parsed on several threads, cut
at the starts of its axioms, and its axioms stay as written without a mapping to
triples, but for the n-ary equivalences that the mapping writes as pairs. Its assertions
become the triples a store would stream. The file path answers only when it reproduces
the store's answer. A file that holds named graphs, a strict read of a functional-style
document, a request for an earlier commit or for inferences, and a file that fails to
parse go to the store path, which keeps reporting its errors. So do the triple files whose
answers depend on a store's own blank node labels or scan order, and the functional-style
documents whose triples the reverse mapping could read otherwise, such as one with an
anonymous individual, an undeclared entity, an `ObjectInverseOf` or two annotations that
the mapping writes as one triple. Library tests compare both paths on Turtle, RDF/XML and
functional-style files and on every case that goes to the store, and the
[file classification results](../../benchmarks/el-classification/RESULTS.md#file-classification)
compare the command line's answers on GO, ChEBI lite and the ORE 2015 sample.
`OntologyFile` holds the terminology, the assertions, the imports and the prefixes in
memory, so the ROBOT-compatible commands read their input through it, and
`read::Ontology::from_axioms` makes a command's output the next command's input
without a file.

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
Four test ontologies show what ELK misses when a property's range meets an individual,
two subsumptions and an inconsistency. Since item 587 the classifier derives them, as
HermiT, Konclude and Whelk do. Konclude 0.7.0-1138 gives the same taxonomy on all 298
ORE ontologies and ChEBI lite, and on GO it entails 14 more subsumptions between named
classes, with 7 more direct edges, all through
`InverseObjectProperties(part_of has_part)`, the GO axiom outside OWL 2 EL. Since item
603 the classifier takes that axiom with the Horn rules and entails the same 14
subsumptions, so its taxonomy of GO equals Konclude's and has 7 direct edges that ELK's
and GO's release do not. Whelk 1.2.1 agrees on GO, ChEBI lite and 296 ORE
ontologies and is incomplete on one. ROBOT 1.9.11's `reason`, `reduce`, `relax` and
`materialize` with ELK assert the classifier's direct hierarchy on GO and ChEBI lite, and
`sparkles ontology reason`, `reduce` and `relax` write the same outputs, as the
section below describes. The
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
3,514 to 1,686 MiB and 3,593 to 1,773 MiB. Item 587's rules left the largest ORE ontologies within 3% of the build
before, on either side, and ChEBI lite within 3%, and cost GO 2 to 3%. Items 644, 645,
648 and 665 then took ChEBI lite on eight threads from 912 to 393 ms, `ore_ont_868`
from 862 to 599 ms, `ore_ont_13482` from 588 to 368 ms and GO from 88 to 77 ms, and GO
on one thread from 288 to 279 ms, on forge against the build of commit 790cffae with the
same taxonomy. On eight threads ChEBI lite's saturation fell from 737 to 273 ms and
`ore_ont_868`'s index stage from 495 to 263 ms. The parallel normalization holds each
chunk's names until the merge and the outboxes hold up to 4,096 messages per worker,
which raises the peak resident set on eight threads from 911 to 988 MiB on ChEBI lite
and from 279 to 295 MiB on GO, while that of the ORE ontologies falls, 1,739 to
1,688 MiB on `ore_ont_868`. The chunks count against the memory budget, and a run whose
chunks would pass it normalizes on one thread. Three variants measured no faster and
are in [PERFORMANCE-EXPERIMENTS.md](../../PERFORMANCE-EXPERIMENTS.md), the outboxes on
one thread, outboxes that sort and drop repeated messages, and subsumer lists of up to
32 members, and so is a merge of the chunks' names through sharded tables, commit
208b8efc, reverted in 9ea7a425. The rules for individuals run in functions of their own, outside the
saturation's loop, since a first build that kept them inline, commit 68a683b5, made the
saturation of ontologies without individuals 3 to 19% slower on one thread in an
interleaved A/B on `atlas` ([record](../../benchmarks/el-classification/runs/2026-11-03-el-inline-atlas-68a683b5/record.json)).
Release builds compile `sparkles-owl` as one codegen unit, since with four the time of the
index stage moved by up to 7% with changes to code that EL runs never reach. One unit
classifies `ore_ont_3836` on one thread on forge in 543 ms against 559 ms and
`ore_ont_13482` in 868 against 902 ms, with 3.8% fewer instructions on `ore_ont_3836`, and
the crate takes 25 to 27 s to compile instead of 9 to 12 s ([code layout
results](../../benchmarks/el-classification/RESULTS.md#code-layout)). The [benchmark
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
individuals and datatypes was item 587, with item 605. The literals and datatypes the
classifier cannot compare and chains through the top property are item 663, and
incremental runs that follow merges, keys and worlds are item 664. Reads that
answer from the classification are item 588. DL queries answer from the kept classifier
through `Classifier::saturate` and `Classifier::describe`, as
[U12's DL Query outcome](../U12-ontology-workbench/SPEC.md#dl-query) describes.
Durable and automatic classification is item
589, after item 339. A kept classification with assertions is item 590,
the comparison and measurements against ELK were item 591, the speed and memory work
against ELK were items 602 and 592, and normalization on several threads, the
saturation's scaling and the cost of item 587 on GO were items 644, 645 and 665.
Cheaper contexts remain item 648, integer class ids in the taxonomy item 647, the
saturation's sensitivity to axiom order item 683 and the rest of the index stage
on several threads item 684.

### The Horn rules

Item 603 classifies Horn ontologies outside OWL 2 EL with the consequence-based calculus
for Horn-SHIQ of Kazakov (*Consequence-Driven Reasoning for Horn SHIQ Ontologies*, IJCAI
2009), written from the paper over the EL classifier's contexts. It covers inverse
properties, universal restrictions, at-most-one and at-least restrictions in Horn
positions, functional and inverse-functional properties, self restrictions, property
chains and transitive properties through the automata of Horrocks, Kutz and Sattler,
disjoint, asymmetric and irreflexive properties, and complements, unions and universal
restrictions that move to the other side of an axiom as their duals. An at-least
restriction of up to 64 becomes that many existential restrictions with fresh, pairwise
disjoint fillers, as the paper's Lemma 2 does. The rules run when one of them can fire
and the ontology has no individuals, functional data properties or top properties, and
not at all under `Limits::el_only`, the ELK mode, which leaves every axiom outside OWL 2
EL out as ELK does and gives ELK's taxonomy. With
any of these the axioms outside OWL 2 EL stay left out and reported, and Horn reasoning
with individuals is item 692. So do axioms that are not Horn, property hierarchies
that are not regular, automata of more than 65,536 states and data restrictions outside
OWL 2 EL. `Stats::tier` says which rules a run needed, `Stats::outside_el` counts the
axioms outside OWL 2 EL and those taken, and `Stats::conjunctions` counts the contexts
the rules created.

`Stats::conclusions` counts attempted rule applications, including duplicate messages,
rather than distinct stored consequences. The memory estimate charges a Horn successor
only when its role and target entry is new. Duplicate successor messages add no stored
charge. The estimate remains approximate, with waiting messages and dynamic contexts
accounted for separately.

A universal restriction on a successor's property extends the successor to the
conjunction of its classes and the restriction's filler, and at-most restrictions merge
successors and identify a predecessor with a successor. These conjunctions get contexts
of their own in a table beside the index, which the saturation's threads share. A context
extends its successors once at the end of its activation, with every universal it holds
by then, so that universals that arrive one after another create no context for each
conjunction on the way. OWL 2 RL and QL ontologies create no such context, since QL has
no universal or at-most restriction in a positive position and RL no existential one, so
they stay polynomial. A test checks that their contexts grow linearly.

The classifier's answers entail only what HermiT 1.4.5.519 entails on 640 generated and
hand-written OWL 2 DL ontologies, are the same on one and four threads, and equal
HermiT's on the 54 Horn ones that the rules take whole. On GO, whose
`InverseObjectProperties(part_of has_part)` the rules take, phase 3 created 2,577
contexts of conjunctions and derived 3.5 million conclusions where the EL rules derived
2.8 million. On the benchmark host GO classified in 325 ms on one thread and 95 ms on
eight, against 278 and 81 ms for the EL rules alone, 17 to 18% slower, and its peak
resident set rose from 195 to 248 MiB on one thread and from 288 to 331 MiB on eight
([record](../../benchmarks/el-classification/runs/2026-11-03-horn-forge-8966216c/record.json)).
The maintainer accepted this cost as the tradeoff for GO's complete taxonomy, and the
ELK mode keeps the EL rules' time and taxonomy. With item 693, GO derives 2.9 million
conclusions and classifies in 306 ms on one thread, against 320 ms for the rules of
phase 3 and 274 ms for the EL rules alone in the same A/B, 12% slower than the EL rules,
with a peak resident set of 223 MiB against 248 and 196 MiB. On eight threads it takes
91 ms against 87 and 92 ms, within the spread of the rounds
([record](../../benchmarks/el-classification/runs/2026-11-03-horn-cost-forge-320d4708/record.json)).
Item 693 follows the remaining cost. ChEBI lite's one `InverseObjectProperties` axiom
fires no Horn rule, so it runs the EL rules in the same time. The taxonomies of the 298
ORE 2015 EL ontologies and ChEBI lite are byte for byte those of the EL rules on one and
eight threads, and GO's taxonomy is the same before and after item 693.

A run with the Horn rules has a memory budget of its own. The budget is 64 times the
estimated bytes of the run's index and axioms, at least 1 GiB, and never more than the
caller's `Limits::max_memory`. A run that passes it classifies again by the EL rules
alone, as the ELK mode does, leaves the axioms outside OWL 2 EL out and reports them,
and sets `Stats::horn_over_budget`. The rules of phase 3 needed it on two ontologies of
the ORE 2015 DL sample derived from Uberon, ore_ont_9663 and ore_ont_14817. Their
transitive parthood properties are each other's inverses, and the contexts of
conjunctions composed the parthood chains again until the run passed 10 GiB, with most
of the memory in waiting messages. With the budget they classified in 4.2 s and 2.4 GB
on four threads on the laptop, with the EL rules' answer. Item 693's chain selection
below removed that composition, and both now finish with the Horn rules. On the
benchmark host ore_ont_9663 takes 4.0 s and 1.0 GiB on one thread and 0.89 s and 1.4 GiB
on eight, and ore_ont_14817 takes 2.2 s and 0.7 GiB and 0.46 s and 1.0 GiB. The EL rules
alone take 0.77 and 0.68 s on one thread, and Konclude 0.7.0 took 8.9 to 9.7 s and 3.5
to 4.2 GB on four threads on the laptop. The two taxonomies miss 33 and 27 of Konclude's
subsumptions, which follow from axioms that are not Horn, and give none that Konclude
does not entail. On one thread the Horn rules still derive 52.0 million conclusions on
ore_ont_9663 and 29.4 million on ore_ont_14817, where the EL rules derive 18.5 and 20.8
million, which item 693 follows. No file of the development set below and no case of the
differential test reaches the budget with item 693's rules, and the budget stays as a
safety net. The largest Horn run of the development set, ore_ont_9855, classifies in 1.5
s on one thread on the benchmark host against 4.4 s with the rules of phase 3, with a
peak resident set of 359 MiB.

A run with the Horn rules composes only the property chains that some negative existential
restriction needs, and carries every other property inclusion by the automata (item 693).
Two definitions are used here, both over the role hierarchy closed under inverses and the
told chains together with their mirror images, `r_n⁻ ∘ … ∘ r_1⁻ ⊑ s⁻` for
`r_1 ∘ … ∘ r_n ⊑ s`. A chain `s_1 ∘ s_2 ⊑ v` is left-linear when `v` is under `s_1`. A
property `s` is letter-closed when every chain whose result is under `s` has all its
operands under `s`, so that every path that implies `s` is a path of links by properties
under `s`.

A negative existential restriction `∃t.F ⊑ G` stays on composed links when the automaton
of `t⁻` has no transition by a property that a link runs under, which phase 3 calls not
linked. Its closure starts with `t`, and each chain whose result is under a property of
the closure adds the chain's left operand. It adds the right operand too, unless the chain
is left-linear and the right operand is letter-closed. The restriction is kept when every
chain of its closure has two operands. Every other negative existential restriction over a
property that chains reach becomes `F ⊑ ∀t⁻.G` and is unfolded by the automaton, as the
universal restrictions are. The run composes the chains of the kept closures and no
others.

- **Uberon.** The composition of the parthood chains, which the conjunction contexts
  repeated, is gone.
- **GO.** The regulation properties stay composed. Their automata would spread a state down
  every descendant of each regulated process.

The argument for completeness has three steps.

1. **The translation.** Replace every universal restriction and every negative existential
   restriction over a property that chains reach by the residual states of its automaton,
   and drop the chains. This preserves named subsumption.
   - A model of the ontology gives each state the elements whose accepted paths end in the
     filler.
   - A model of the translation, with its properties closed under the chains, satisfies
     each restriction by induction along the path that gave an added edge.
   - Simple properties gain no edge, so the at-most, self, disjointness, asymmetry and
     irreflexivity axioms, which take simple properties only, still hold.
2. **The calculus without composition is complete for the translation,** which has no
   chains left.
3. **Every conclusion of the run without composition is also derived with chain
   selection.**
   - A kept restriction's residual states move only backwards along links, by the rule for
     a universal restriction on an inverse. No link runs under their transitions, so the
     rule for a successor never applies to them.
   - Those states reach a context `P` only along a path of links from `P` to a context that
     holds `F`, whose properties spell a word that implies `t`.
   - By induction on how the word implies `t`, the kept chains compose a link from `P` to
     that context by a property under `t`. A left operand's links are complete, which the
     closure ensures. A letter-closed right operand contributes its links one at a time,
     each joined to the link built so far, which the chain being left-linear keeps under
     `s_1`.
   - The restriction's own EL rule then gives `G` at `P`.

Composed links are entailed and are never successors of the Horn rules. A restriction kept
on composed links and one unfolded by its automaton can share properties, and neither
takes a premise from the other. Two kinds of run compose every told chain and its mirror
image, and unfold only the linked restrictions, as phase 3 did:

- a run with a property under the bottom property, since closing the properties of a
  model could give that property an edge;
- a run with a negative existential restriction over a property that chains reach and
  whose inverse the run lacks, since that restriction cannot be unfolded.

Two further changes cut the cost of the Horn rules.

- **Merging by at-most restrictions.** At the end of its activation, a context merges
  every successor that an at-most restriction counts into one conjunction of their
  classes. Merging pairs created a context for each subset on the way.
- **Fillers no rule reads.** The rule for a successor passes over a filler that no rule
  takes as a premise and whose told subsumers every context holds. The rule for a
  universal restriction on an inverse still takes such a filler to the predecessors.

Item 693 also fixed a gap in completeness that phase 3 left. A chain
`r_1 ∘ … ∘ r_n ⊑ s` entails its mirror image `r_n⁻ ∘ … ∘ r_1⁻ ⊑ s⁻`, and the rules of
phase 3 composed the told chain only. With `r0 ∘ r2⁻ ⊑ r0` and `r2` the inverse of `r0`,
`r2` is transitive only through the mirror image, and `A ⊑ ∃r2.B` with `B ≡ ∃r2.C` did
not give `A ⊑ B`. The inputs affected were ontologies that the Horn rules take with a
chain that names an inverse, a chain of three or more operands over inverses, or a chain
whose mirror image's result sits under the inverse of a superproperty. A run with the
Horn rules now composes the mirror image of every chain that is not told itself. Runs by
the EL rules alone have no inverses and are unchanged.

A differential test found the gap. It generates small random Horn ontologies with chains,
inverses, transitivity and property hierarchies, compares the taxonomies of two builds on
one and eight threads, and shrinks each difference to a few axioms. Against the build of
phase 3, 6,000 cases gave this one difference. A scratch build with the mirror images
and with chain selection turned off by a switch served as the reference, and 6,000 cases
against it gave no difference, both for chain selection alone and for the final build. Chain selection changed the run's conclusion
count in 294 of 600 sampled cases, so about half the cases exercise it.

The development set of the ORE 2015 DL sample was compared on the laptop with four
threads and a 10 GiB cap, as an unpublished measurement. The classifier's taxonomy
equals the reference on 62 of the 206 files, 37 by the EL rules and 25 by the Horn rules.
It is sound on 137 files whose axioms it uses in part. 91 of these have axioms that are
not Horn, and 46 are Horn ontologies with individuals, functional data properties or top
properties, which item 692 covers. The comparison marks one file, ore_ont_15993,
as unsound because neither reference has an answer for it. Konclude read none of it and
HermiT refused its SWRL built-ins, and every subsumption the classifier reports is told
or follows from the file's told equivalences. Two files have no reference and the reader
refuses one. Three ran out of memory: ore_ont_9663 and ore_ont_14817, which the Horn
budget now classifies, and ore_ont_8737, whose 2.5 million existential restrictions take
the EL saturation past 10 GiB, item 695. The classification time is 0.16 of
Konclude's in geometric mean on the equal files and 0.23 on the sound ones, and never
above HermiT's. Two equal files and 11 sound ones classify slower than Konclude. The
largest gap is ore_ont_9855, an equal file at 2.5 s against 1.3 s.

With item 693's rules the same comparison gives 62 equal files and 139 sound ones, since
ore_ont_9663 and ore_ont_14817 now finish with the Horn rules and are sound, and only
ore_ont_8737 runs out of memory. Estimated from that run's times, scaled by the ratio of
item 693's build to the base's on `atlas`, the classification time is 0.15 of the faster
reference's in geometric mean on the equal files and 0.22 on the sound ones. It is 0.14
on the equal files that the Horn rules take, 0.16 on those of the EL rules, 0.30 on the
sound Horn files and 0.19 on the sound files with axioms that are not Horn. One equal
file and 11 sound ones are estimated slower than Konclude in one cold run, by up to 163
ms on ore_ont_8273, though warm runs of each beat Konclude's time, which item 709
follows. ore_ont_9855 takes 0.5 s on the laptop against Konclude's 1.3 s.

Item 709 measured the 12 small files on the benchmark host. A cold run of the smallest
of them on four threads spent most of its time starting helper threads that a saturation
of a few milliseconds does not need. The saturation now starts them once its first
thread has worked for 0.5 ms, or at once for 4,096 roots, and the parallel steps of the
normalization run on one thread below 4,096 axioms, while those of the taxonomy keep
chunks of 1,024 classes. Cold runs of the 12 files take 0.74 of the
time of the build before on four threads and 0.68 on eight, and a whole cold process of
each on four threads takes 1.9 to 21.8 ms, under Konclude's times on the laptop. The
item's estimates had come from the laptop while other work ran on it. Warm runs of five of
the files take 0.4 to 0.6 ms longer on eight threads, item 720. On one thread
ChEBI lite and `ore_ont_13482` classify 1.0 to 1.6% slower from code layout, item
722. Item 695 found that the peak memory of ore_ont_8737 is messages waiting in
to-do lists that repeat conclusions their contexts already hold. When the waiting messages
take more memory than the stored conclusions and 256 MiB, a delivery to a context whose
state is free drops the subsumptions and backward links that the context holds. The peak
resident set falls from 10.5 to 7.5 GiB on four threads on `atlas` and the run from 32.3
to 29.0 s, so the comparison's 10 GiB cap no longer stops it, while a run on one thread
still peaks at 9.4 GiB, item 721. Every ORE 2015 EL ontology, GO, ChEBI lite and
development file keeps its taxonomy byte for byte on one and eight threads, and
the [small files and memory results](../../benchmarks/el-classification/RESULTS.md#small-files)
give the numbers. Five variants that measured no better are in
[PERFORMANCE-EXPERIMENTS.md](../../PERFORMANCE-EXPERIMENTS.md).

The references disagree on two files of the set. On ore_ont_15516, Konclude's parsers all
failed at the first SWRL rule and it classified an empty ontology. HermiT found the
ontology inconsistent, which its rules cannot cause, since the properties in their heads
occur in no other axiom. The classifier leaves out the assertions about individuals and
is sound against HermiT. On ore_ont_9540, HermiT makes `Image_type` equivalent to
`Image_with_uji_office_door`, which the ontology does not entail. `Image_type` is a single
image whose shapes each have all 60 of the enumerated line-to-line points, so none is a
quadrilateral with exactly four, and the image has no office door in some model. HermiT's
second equivalence follows from the first, so Konclude's answer stands for both.

The functional-syntax reader refuses a document with a SWRL rule. A read with
`ofn_read::Options::skip_rules` passes over the rules and counts them, which the
classification comparison uses to report them as left out.

### The disjunctive rules

Item 603 classifies ontologies whose axioms outside OWL 2 EL need case analysis with the
consequence-based calculus of Bate, Motik, Cuenca Grau, Tena Cucala, Simančík and
Horrocks (*Consequence-Based Reasoning for Description Logics with Disjunctions and Number
Restrictions*, JAIR 2018), written from the paper over the EL classifier's contexts. It
covers ALCHI, which adds unions, complements and universal restrictions in every position
to the EL classifier's language, with inverse properties and property hierarchies.
Transitive properties and chains are taken through the automata of Horrocks, Kutz and
Sattler, which unfold the universal restrictions, negative existential restrictions and
ranges on the properties they reach. A run that does not count reads an at-least
restriction in a positive position and an at-most restriction in a negative one as the
existential restriction they imply, which keeps ALCHI's models. The rules run when an
axiom needs case analysis and the ontology has no individuals, data properties, self
restrictions, reflexive properties or top and bottom properties. Otherwise the Horn rules
take what they can, and the axioms that need case analysis stay left out and reported, as
do nominals and property hierarchies that are not regular. `Stats::tier` is `Disjunctive` for such a run, and
`Stats::clauses`, `dl_contexts`, `dl_stored`, `dl_tests` and `dl_testing` count its
clauses, the contexts that held them, the clauses they stored, the candidate subsumers it
tested and the time the tests took.

The axioms with two or more disjuncts become clauses, and the others stay told subsumers
of the EL index, so the EL rules still derive every unit. Each context keeps its clauses in
a store with indexes by maximal literal, and a new clause that a stored one subsumes is
dropped, while the stored clauses it subsumes are deleted. A marker `A → A` stays, since it
records a hypothesis a predecessor sent. All contexts share one total order on literals,
with fresh classes first, then named classes, then other atoms, and the literals over a
successor above those over the context's own element. Under a total order a context decides
its units and its satisfiability, and a named subsumer that only a case analysis gives
stays inside a clause. The run therefore takes the canonical model of each class's
context, built from its clauses with an empty body in the order of the literals, and tests
each named class that model makes true and that is not a unit. A test is a context whose
core is the class with the candidate's complement. All of a class's candidates are tested in
one context first, and when that context is unsatisfiable each is tested alone, since most
fail. A test that holds adds the subsumer to the class's units, and the saturation goes on
until no class has a new candidate. An earlier order kept the named classes incomparable,
so that every context answered for them at once, but on ore_ont_5438 it let clauses keep
many maximal literals and the run did not end within 300 s.

The Succ rule takes the cautious strategy of the paper by default. A unit existential
restriction uses the EL rules' successor, and a conditional one uses a hypothetical
successor of its own, whose core is empty and which takes the hypotheses as markers.
`Limits::expansion` set to `Expansion::Eager` makes the successor's core everything that
holds of it for certain, with a context of that conjunction when it is more than the EL
successor's core. Both strategies give the same answers on the fixtures and the random
tests.

`Limits::partial` makes a run of the disjunctive rules that its deadline, its cancellation
or its memory budget stops answer with what it proved instead of failing. The answer's
`Classification::complete` is false, `incomplete` lists the classes whose subsumers it may
miss, and `consistency` is `Consistent`, `Inconsistent` or `Unknown`. A stopped worker gives
back the conclusions it did not apply and delivers the messages it holds, and an inference
the stop cut off marks its context, so that every context with work left shows as open. A
class is complete when every context it reaches is closed, none of its candidates is left
untested and the ontology is consistent. Consistency is decided when `owl:Thing`'s context
is unsatisfiable, or when it and every context it reaches are closed, and when it is unknown
no class is complete. The taxonomy of a partial answer is built from its proved subsumptions
closed transitively, after the clause stores are freed. A partial answer that passes the
budget fails with the memory error, and the next run starts again. Runs of the other tiers
ignore the flag and fail on a stop as before, item 714. A class whose context the stop came
before is open too, and so is the consistency while `owl:Thing` has no context.

Phase 7's surfaces give the partial answer to every caller of a classification.
`ClassifyRequest::partial` and `ClassifyRequest::expansion` carry the flag and the
strategy, and the JSON operation, `POST /$/ontology/{ds}/classify`, the Python, Node.js
and JVM bindings and `sparkles ontology classify --partial --expansion` take them. Every
answer gives `tier` and `consistency`, and a partial one has `partial` true, `complete`
false and the open classes under `incomplete` with `incompleteCount`. The `complete` of
the JSON and of `Classified::complete` stays true only for an answer that left nothing out
and finished. The dataset keeps no partial answer. A request that writes its taxonomy,
asks for the functional-style answer or runs a ROBOT-compatible command refuses or ignores
the flag, since none of them can say that a taxonomy is partial. The command line takes
`--timeout`, notes a partial answer on stderr and exits with status 2 for one that is
otherwise clean. Each surface's tests classify a generated ALCHI ontology under deadlines
from a fiftieth of a full run's time upwards, and check that a partial answer is never
complete, that it lists its open classes and that its consistency agrees with the full run
or is unknown, which an inconsistent ontology gives when stopped early. The Workbench does
not ask for partial answers, item 719.

A run of the disjunctive rules has the Horn rules' memory budget, 64 times the estimated
bytes of its index and axioms, at least 1 GiB, and no more than the caller's. The clause
stores and the table of clauses that contexts send each other count against it, and the
case analysis of one inference derives its choices one at a time and checks the bounds as
it goes. A run without the partial flag that passes the budget classifies again with the
Horn rules, leaves the axioms that need case analysis out and reports them, and sets
`Stats::dl_over_budget`. ore_ont_3215 of the ORE 2015 DL sample, with 18,323 unions over a
transitive property, needs this, item 713.

Counting is part of the same rules and is off by default. `Limits::counting` turns it on,
and `el_classify --counting` does the same. A run that does not count leaves the axioms
that need counting out and reports them, and takes a minimum as the existential
restriction it implies, so it costs what the classifier cost before counting. The reason
is the cost on real ontologies, since no file of the ORE 2015 DL development set finishes
with counting and the five that attempt it spend 2 to 30 times the time of the run
without it before they give up where that run finishes, item 728. Turning counting on is the work of
that item. A run that counts when asked takes a number restriction that the Horn rules
cannot take, a functional or inverse functional property, disjoint properties or an
asymmetric property. The terms of a clause then include
equalities of the successor terms and of a successor with the predecessor, and the order
puts the literals of one function symbol above those of any smaller one, with the role
literal above the atom, the atom above an inequality and the inequality above an equality.
A minimum of two or more in a positive position becomes that many existential
restrictions with fresh fillers below the original and pairwise disjoint, which is
Kazakov's reading of the inequalities of the witnesses. A maximum in a positive position
becomes a fresh class that holds the at-most restriction, counted over the neighbours
of the qualified role that the filler defines. In a negative position a maximum or a
minimum above one is replaced by its dual. The rules are Eq, Ineq, Factor and the at-most
and qualified-role clauses of the paper's Table 1, with Succ and Pred reading the role
literals. A certificate that two successor terms differ, taken from a disjointness of
their atoms, cuts an equality between them from a clause whose body and head contain the
certificate's, so that the equalities of products of counting restrictions are not made.
An at-most clause that would equate more than 12 terms, make more than 16,384 subsets or
hold more than 65,536 literals in its products passes the work bound at once. A number
restriction on a role that is not simple declines the rules, as does an irreflexive role
that is not simple. A minimum above 64 is left out and reported, as in the Horn tier.

The equalities can make a context derive millions of conclusions that subsumption
discards, which the memory budget does not see. A run that counts when asked therefore has a bound
of 262,144 derived conclusions, kept or redundant, which `Limits::dl_work` sets. A run
that passes it on several threads starts again on one, since how many conclusions a
parallel run derives depends on the order in which the threads deliver them. When that
run passes the bound too, the run starts again without counting. That run leaves the
axioms that need counting out and reports them, as the classifier did before it counted,
with a bound of 4 million conclusions of its own, and sets `Stats::dl_counting_over` and
`Stats::dl_over_budget`. A run that passes the second bound falls back to the Horn rules
as before. A run that does not count has no bound on its conclusions, only the memory
budget. Of the 150 counting fixtures, all but three derive at most 190,000 conclusions on
one thread, `shiq-046` derives 1.0 million and needs a raised `Limits::dl_work`, and
`shq-041` and `shiq-028` pass any bound, item 729.

The bound has a limit that this section states plainly. Near it, the thread schedule can
decide whether a run answers in full or falls back, since the clauses stored at the
fixpoint vary with the schedule, for example 3,229 to 6,762 on `sriq-010` at four threads
against 3,253 on one. A fallback is always reported through the statistics and the axioms
left out and is never a different complete answer, so a caller learns from
`dl_counting_over` whether counting finished. A deterministic bound is item 727.

The generated `shq`, `shiq` and `sriq` fixtures equal HermiT 1.4.5.519 on every case that
Tier D takes whole with counting on, on one and four threads and under both strategies. Of 50 cases each,
Tier D takes whole 49 of `shq`, 48 of `shiq` and 16 of `sriq`, where the others name
individuals or pass the bound. Random tests of counting ontologies check that minimums of
two answer as the existential restrictions they imply and that a functional property
answers as a maximum of one on every class. No development file of the ORE 2015 DL
sample is taken whole with counting. Its counting files fall back when counting is on,
ore_ont_6485 after 205 ms on one thread against 7 ms without counting, and ore_ont_6952
and ore_ont_10140 after 1.4 and 1.7 s against 0.6 and 0.5 s. The cost is not counting alone, since the same ontologies
with every exact cardinality read as an existential restriction do not finish on the base
either, item 728. The normalization of a clause with a universal or a maximum
beside a class or an existential restriction keeps the class in the clause's body, which
classifies BioTop in 56 ms on one thread and 27 ms on four against 940 and 350 ms before,
item 712.

The classifier's answers equal HermiT 1.4.5.519's on the generated and hand-written
`alc`, `alch` and `shi` fixtures that the rules take whole, and are the same on one and
four threads and under both strategies. A random test of 20,000 small Horn ontologies with
inverses, chains and transitive properties gives the disjunctive rules' answer and the Horn
rules' the same taxonomy, and a random test of ALCHI ontologies stops each run at every
point of its cancellation and checks that each partial answer is part of the full one and
whole for every class of its signature that it reports complete. The deadline tests stop
runs before every class has a context. On the development set of the ORE 2015 DL
sample, the disjunctive rules take nine files of the `dl` and `dl-i` strata whole, and
their taxonomies equal the reference on all nine. Eight files of the strata have
individuals or data properties and stay sound, and ore_ont_3215 passes the budget. Of the 13
development files that classify slower than Konclude without them, the disjunctive rules
take only ore_ont_5940, whose taxonomy is now complete and faster than Konclude's. ore_ont_12141, BioTop,
classifies slower than Konclude, item 712. The taxonomies of the 298 ORE 2015 EL
ontologies, GO, ChEBI lite and every development file that the disjunctive rules do not
take are byte for byte those of the Horn tier on one and eight threads. The saturation's
arms for the disjunctive rules compile away in other runs. They first made the one worker's
activation too large to inline, and GO and ChEBI lite classified 0.9% and 1.4% slower on
one thread on the benchmark host. Each rule set now has a worker of its own that inlines
its activation, and with `sparkles-owl` in one codegen unit GO classifies on one thread in
0.99 of the time of the build before and ChEBI lite in 0.98, with the same taxonomies, as
the [code layout results](../../benchmarks/el-classification/RESULTS.md#code-layout) give.

### ROBOT-compatible commands

**The commands.** `sparkles ontology reason`, `reduce` and `relax` (items 634 and 635)
follow ROBOT 1.9.11's `ReasonOperation`, `ReduceOperation` and `RelaxOperation` as their
source is written, with the inferred axiom generators of OWL API 4.5.29 that ROBOT calls,
so that a release pipeline gets the same axioms. The options take ROBOT's names, short
names, values and defaults. `reason` and `reduce` read a boolean as ROBOT's
`OptionsHelper` does, where `true` or `yes` in any case is true and any other value is
false, and `relax` takes exactly `true` or `false`, as ROBOT's `getBooleanValue` does.
ROBOT's error messages and its exit status 1 are kept. The transformations live in
`sparkles_owl::transform` and work on a `Document` of a header and a set of axioms, with
the classification passed in as a function, so they use the classifier without changing
it. `sparkles::handles::ontology::transform::WorkingOntology` reads a file through
`OntologyFile` or a database with `--loc`, runs the classifier with the caller's
threads, deadline and budgets, and writes functional-style syntax, RDF/XML, Turtle and
N-Triples, gzipped when the name ends in `.gz`.

**What each command does.** `reason` validates as ROBOT's `ReasonerHelper.validate` does.
An inconsistent ontology, an unsatisfiable class or an unsatisfiable object property
fails, and the last is found with ROBOT's probe for ELK, a fresh class under
`ObjectSomeValuesFrom(p owl:Thing)` for each property, added to the same classification
and left out of every answer. `--dump-unsatisfiable` writes the syntactic bottom-locality
module of the unsatisfiable classes, which holds every justification, as ROBOT's `BOT`
extraction does. The subclass, equivalence and class assertion generators work over the
classes and individuals of the ontology's signature. `--reasoner ELK`, and `emr`, which
ROBOT builds on ELK, classify by the OWL 2 EL rules alone (`ClassifyRequest::el_only`,
which sets `Limits::el_only`), so that the output is ROBOT's with ELK. `materialize`
takes ELK by default, as ROBOT's does. Without `--reasoner` in `reason` and `reduce`, and
with `hermit`, `jfact`, `whelk` or `structural`, which run it with a
warning, the classifier also takes the Horn axioms outside OWL 2 EL, so on GO `reason`
adds the 14 subsumptions that `InverseObjectProperties(part_of has_part)` entails. A class the taxonomy does not hold,
such as a declared obsolete class, is a direct subclass of `owl:Thing`, as ELK answers,
and class assertions come from a second classification with a fresh class equivalent to
`ObjectOneOf(a)` for each individual. The generators whose queries ELK does not answer
add nothing and log a warning, as in ROBOT. The duplicate, `owl:Thing`, external-entity
and tautology exclusions, the `is_inferred` annotation, the new ontology modes and the
removal of redundant subclass axioms follow ROBOT, which removes only the unannotated
form of an axiom. For the axioms between named entities that the generators write, the
axioms the empty ontology entails are exactly ROBOT's structural tautologies, so
`--exclude-tautologies all` needs no other reasoner. `reduce` by default classifies the
subclass axioms and the property characteristics alone, with each anonymous class
expression of a subclass axiom named by a fresh class equivalent to it, as ROBOT does,
so it removes redundant existential axioms with the classifier's taxonomy of named
classes.
`--named-classes-only` keeps the subclass axioms between named classes that are direct
edges of the whole ontology's taxonomy. `relax` adds the subclass axioms for the
existential restrictions, cardinality restrictions above zero and named conjuncts of
equivalences, and of complex superclasses with `--include-subclass-of`.

**Materialize.** `sparkles ontology materialize` (item 636) follows ROBOT 1.9.11's
`MaterializeOperation` and the expression materializing reasoner 0.1.3 that ROBOT wraps
around ELK. It validates the ontology as `reason` does, then asserts for each class with
a class axiom, owl:Nothing aside, the members of its direct superclass nodes in a
taxonomy that has a class equivalent to `ObjectSomeValuesFrom(p B)` for each chosen
property p and filler B, each such class read back as its restriction. It leaves out a
superclass with owl:Thing or the class itself in its signature and an axiom the ontology
already holds without annotations, as ROBOT does, so it also asserts direct named
superclasses that are not asserted. The properties come from `--term` and
`--term-file`, read as ROBOT reads term lists, with the 282 prefixes of ROBOT's default
JSON-LD context. An expanded term is kept when it is a URN or a URL that OpenJDK 17's
`java.net.URL` parses, which ROBOT checks, so a full IRI with a malformed port or
host is left out with ROBOT's warning. When none is given, or none is an IRI, every object
property of the signature is taken. `--create-new-ontology true` adds nothing, since
ROBOT's operation adds its axioms to a new ontology that the command never saves, and
`-s` and `-a` are accepted and ignored. The saturation derives an existential
superclass as a link to a context whose subsumers hold the filler, but whether the
restriction is direct depends on what the restriction is itself subsumed by, which only
a context of its own derives. So the restrictions are named by fresh classes and
classified together with the ontology and the validation's property probes, as `reduce`
names expressions, and the classifier is unchanged. A filler that occurs only where it
could be given no instances, such as the subclass of a subclass axiom or an operand of a
disjointness, gets no fresh class, since no class can then be below the restriction. An
ontology with individuals, a reflexive property, `ObjectHasSelf` or the top property
names every class, as ROBOT does. A property test checks on random EL ontologies that
the pruning changes no output. It keeps about 41% of GO's classes and 16% of ChEBI
lite's as fillers. Without the pruning, `materialize` with all nine of ChEBI lite's
properties ran out of a 12 GiB memory cap, and with it the run takes 5,084 MiB on the
benchmark host. Each named restriction costs memory, so a run with more properties holds
more than `reason`, about 280 MiB for each further property on ChEBI lite
([materialize results](../../benchmarks/el-classification/RESULTS.md#materialize)).

**Chains.** Each command ends in a hidden list of the arguments that follow it, which is
parsed again as the next command, so that `sparkles ontology reason --input x.owl relax
reduce --output y.owl` runs in one process with the ontology in memory (item 637).
`classify` and `check` can join a chain and end it with status 1 where they would exit
with it alone. A chain can also start from `classify FILE` or `check FILE`, which answer
from the file as they do alone before the rest of the chain reads it. FILE is their
first positional argument and the next command is the second, so the options after the
next command belong to it. A chain from a database starts with `reason`, `reduce`,
`relax` or `materialize` and `--loc`. `--input` on a later command is ROBOT's chained input error, and
`--output` on any command writes the ontology after it. A malformed command line exits
with status 2, as every `sparkles` command does, where ROBOT documents only a non-zero
status.

**Comparison with ROBOT.** `scripts/elk-compare/ofn-axioms.py` compares two
functional-style files after expanding prefixes and sorting the operands of n-ary
constructors. On GO and ChEBI lite the outputs of `reason`, `reduce`, `relax` and the
chain `reason relax reduce` with `--reasoner ELK` equal ROBOT 1.9.11's with ELK 0.6.0 axiom for axiom,
annotations included: 622,357 axioms of GO's reasoned ontology and 1,682,146 of ChEBI
lite's, of which 96,847 and 424,904 are logical. On the benchmark host `reason` took 3.45 s
on GO and 7.74 s on ChEBI lite from the RDF/XML release, against `robot reason`'s 14.04 s
and 30.63 s, and the chain took 3.87 s and 10.27 s. The run record of the
[ROBOT comparison](../../benchmarks/el-classification/RESULTS.md#robot) keeps the
comparison and the timings.

**Limits.** The commands read no dataset on a server (item 685), write no OWL/XML,
Manchester, OBO or OBO Graphs JSON (item 686), and refuse
`--prevent-invalid-references true` (item 687). `reason --reasoner emr` asserts
named superclasses only, as ELK does, not the class expressions that ROBOT's expression
materializing reasoner adds, which item 691 follows.

### Roadmap follow-ups

#### Incremental native DL classification

Item 753 delivers item 603's affected-module maintenance for axiom insertions and
deletions. It first covers supported Horn and disjunctive constructs and extends
eligibility with later phases of item 603. Dependencies must cover normalized rules,
Horn conjunction contexts, disjunctive test contexts and taxonomy consequences,
including consequences whose last support was removed. An edit with untracked effects
must fall back to fresh full classification, including global role or chain changes,
tier transitions and new constructs without complete dependency tracking. An affected
module can be the whole ontology, and case analysis retains its exponential worst case.

Item 590 owns assertion retention and item 664 owns EL individual, merge, key and world
invalidation. Coordinate their dependency records before making those changes eligible
for native DL reuse. Terminology-only maintenance can ship with a safe full fallback
before those items finish. Item 589 owns durable automatic scheduling after item 339.

A completed incremental run must equal a fresh full run at the same revision and
options in subsumptions, equivalence groups, direct taxonomy edges, unsatisfiable
classes, consistency, completeness and excluded-axiom reports. Partial results remain
sound and preserve the existing incomplete-class and unknown-consistency contract.
Retained state, invalidation scratch space and replacement state count against the
applicable memory budgets. Deadlines and cancellation must preserve safe recovery and
truthful completeness. Reuse must match the source history and producer options, and
publication must preserve revision identity and reject stale writes.

Acceptance compares fixed and randomized insertion and deletion histories with fresh
full runs after every edit, plus reference checks on supported fragments. Cover lost
supports, equivalence and unsatisfiability changes, consistency changes, global effects,
tier transitions and full fallback, together with memory stops, cancellation, deadlines,
interrupted-run recovery and concurrent source changes. Bounded runs may stop at
different points but must remain sound and report completeness truthfully.

Measure cold classification, fresh full runs and retained small insertions and deletions
on identified Horn and disjunctive workloads with checked answers. Include normalization,
invalidation and taxonomy costs, fallback edits, retained bytes and peak memory.
Acceptance requires a measured benefit for localized edits and a documented memory
tradeoff. Existing full-classification gates still apply, and accepted slowdowns get a
roadmap follow-up. This does not promise cheap updates for arbitrary OWL 2 DL.
