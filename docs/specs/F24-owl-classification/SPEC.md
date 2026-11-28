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

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: ELK (Kazakov, Kroetzsch and Simancik JAR 2014), Snorocket, ROBOT reason, OWL API, HermiT, Openllet, Konclude, Protégé reasoner plugins, ROBOT, Protégé Explanation Workbench, OWL API BlackBox, Protégé debugger plugins and OWL repair. The survey of OML models also reads the OML v2 specification and the Apache-2.0 source and documentation of openCAESAR's `owl-adapter` and `owl-tools`. No code from another project is copied.

## Outcome

The OWL 2 EL classifier is delivered as roadmap item 580 and covers OWL 2 EL, apart
from the literals and the chains that item 663 follows, since item 587. Item 603 extends
it to Horn ontologies without individuals, as [the Horn rules](#the-horn-rules)
describe, and to ALCHI ontologies without individuals that need case analysis, as
[the disjunctive rules](#the-disjunctive-rules) describe. The export of inferred axioms and the removal of redundant ones are delivered
as the ROBOT-compatible commands of items 634, 635 and 637, and the existential
superclasses as `materialize`, item 636. The other phases remain under [item 457](../../ROADMAP.md).

Item 719 adds partial-classification controls and display to the Ontology Workbench. Both the explicit inferred-tree run and the class frame accept a partial-answer toggle, a microsecond-precision bounded deadline in seconds sent as the `timeout` query parameter (blank omits the override and uses the server default) and the `cautious` or `eager` expansion strategy on `POST /$/ontology/{ds}/classify`. When a partial answer returns, the Workbench displays the partial status with its tier, open-class count and any excluded-axiom or unmapped-triple gaps, marks classes in the `incomplete` set as open in the inferred class tree (including accessible row names) and marks the selected class from `entity.incomplete` or `frame.incomplete` in the class frame, while rendering `unknown` consistency distinctly from `consistent` and `inconsistent` verdicts. Partial tree responses keep only their first page with a single-page partial truncation notice distinct from the 50,000-node browser cap, and open-class list sample caps are reported on both partial and finished incomplete runs. Both the explicit tree and the class frame reject unrequested partial answers, and the class frame also rejects any partial projection that advertises shared retention.

Integer, decimal and rational literals use exact arbitrary-precision canonical values,
including values beyond 128 bits. Construction and copied storage obey the classifier's
resource controls, and large values share immutable storage until their last owner
releases it. XML namespace conformance is checked without canonicalizing XML values.
On the default classification path, two-link OWL 2 EL property chains ending in
`owl:topObjectProperty` now derive their named-domain consequences when the ontology
stays within OWL 2 EL without datatype-checker components, under a 4,096-row bound, the
classifier's memory budget and shared-world teardown refund ordering under pooled paid
admission. Explicit EL-only mode (`Limits::el_only`, `ClassifyRequest::el_only` and
`--reasoner ELK`) and rooted runs are unchanged, and the chain axioms remain reported as
used in part. General composition through the top object property, canonical XML values,
ill-typed literal semantics and datatypes outside the supported map remain in item 663.
Item 692 runs the Horn
rules on ontologies with individuals, functional data properties and top properties, as
[The Horn rules with individuals](#the-horn-rules-with-individuals) describes, and the
shared hierarchical budget remains in item 675.

Item 694 exposes EL-only (ELK) classification across the Rust API, HTTP routes, CLI and
Python, Node.js and JVM bindings and keys the dataset's kept classification cache by
`el_only`. `POST /$/ontology/{ds}/classify`,
`GET /$/ontology/{ds}/classification`, `POST /$/ontology/{ds}/dl-query` and
`sparkles::handles::ontology::wire::call` accept `elOnly`, `sparkles ontology classify`
(both standalone and chained) accepts `--el-only`, Python's `Dataset.ontology.classify`,
`kept_classification` and `dl_query` accept `el_only`, Node.js's `dataset.ontology.classify`,
`keptClassification` and `dlQuery` accept `elOnly`, and JVM's `ClassifyQuery`,
`KeptClassificationQuery` and `DlQuery` expose `elOnly`. DL queries with `elOnly` run the
EL classifier in EL-only mode against the dataset's EL-only kept saturation, report
`reasoning.elOnly`, and refuse `reasoning=rl` or query expressions outside OWL 2 EL.
`KeptCache` stores default (`el_only = false`) and EL-only (`el_only = true`) entries in
separate slots so neither mode clobbers or mis-serves the other across classifications,
incremental updates, kept reads and DL queries, while evicting the other slot if combined
retained bytes exceed the one-gigabyte retention cap.

Item 867 enables at-least restrictions above one beside individuals in the Horn rules,
using Kazakov's Lemma 2 unfolding into fresh, pairwise disjoint subclasses of the filler
up to `MAX_AT_LEAST`. For Horn ontologies with individuals, nominals, at-most-one
restrictions, keys, top properties and OWL 2 EL data axioms, the unfolding preserves
satisfiability and all named-class subsumptions, including named-class unsatisfiability,
by a direct-power argument over `I^κ` in which every tuple whose coordinates all satisfy
`≥n r.C` in `I` has at least `2^κ` distinct successors to colour with the fresh classes.
When a non-Horn finite data range enters through the datatype checker, the Horn saturation
stays sound because the checker emits only valid Horn implications and reports unresolved
data covers as used in part. All 50 `horn-sroiq` fixtures in the `dl603` suite now
classify whole alongside the 50 `horn-shiq` fixtures with exact HermiT 1.4.5.519 taxonomy
parity, together with new hand fixtures covering satisfiable and unsatisfiable
restrictions, nominals, functionality, at-most restrictions, inverses, subproperties,
keys, self restrictions, anonymous individuals, EL datatypes, `DifferentIndividuals`,
asymmetric roles, non-Horn data covers and incremental updates.

Item 972 is the second phase of [the datatype checker](#the-datatype-checker). A data
property whose component has a universal restriction, a cardinality, an enumeration of
several values, a facet, a union, a complement, a datatype outside OWL 2 EL or a
`DisjointDataProperties` axiom has its restrictions, ranges, assertions and functional
axioms taken as the checker's atoms, and the Horn tier decides them in each context by
the theory rule, as [the EL and Horn tiers](#the-datatype-checker) paragraph describes.
An ontology whose data stays within OWL 2 EL builds no atoms and runs the EL lattice as
before. A value outside an `xsd:int` range, two values of a functional `xsd:double`
property and a string in an `xsd:boolean` range now make an ontology inconsistent, as
they do under HermiT. Three other answers change. In a run with a checker component, a
`DatatypeDefinition` outside OWL 2 EL that the checker converts and a
`DisjointDataProperties` axiom over checker properties are used and no longer reported as
left out. A cover found in a context that the run finds unsatisfiable, or in a world whose
class has no instance, is not reported. `Stats::tier` reads `Disjunctive` for a run whose
only axioms outside the Horn rules are data unions, enumerations of several values,
complements on the left or higher data cardinalities, which the Horn tier now decides. A
cover over the values of a property that gives different classes and a key over a checker
property are reported as used in part, while a universal or maximum data restriction on
the left is reported as left out. Item 768 takes them in Tier D and the rooted run.
On 26 of the wiring tests' ontologies HermiT 1.4.5
gives 37 of the 38 answers that the tests pin, and it derives the subsumption whose cover
a world reports as used in part. The other answer is a
`NegativeDataPropertyAssertion` of `"3"^^xsd:integer` beside the assertion of
`"3"^^xsd:int`, which HermiT finds consistent, while it finds the same negation written as
a class assertion of a complement inconsistent, as the shared numeric value space of
OWL 2 requires. Across the 222 non-held-out census files whose default run previously left
out data axioms, phase 2 keeps every file on the same tier, leaves more axioms out on none
and takes 43,014 more axioms across 195 files. Twenty of those files now leave no axiom
out and report nothing as used in part, one file reports a cover over three axioms as used
in part, and nine files move from consistent to inconsistent, all nine confirmed
inconsistent by HermiT 1.4.5. On the thirteen Kepler16b OML probes, the functional
`xsd:double` probe and the `xsd:boolean` range probe move from consistent to inconsistent
as in HermiT, so the classifier now agrees with HermiT on eleven of the thirteen probes
and misses only the SWRL rule and enumeration probes. Item 1303 records that one object
axiom needing case analysis sends an otherwise eligible ontology whose data properties
occur only outside OWL 2 EL to Tier D. Tier D leaves those data axioms out.

Item 1256 changes the invalidation of a run kept by the EL rules. It recomputed what
every kept context takes from the index and read every subsumer of every context with
hash sets, which on `ore_ont_13482` on `wally` took about 105 ms of a 433 ms index stage
to clear 3 to 39 of its 659,580 contexts. Kept contexts now keep their propagations,
renumber their disjointness sets only when the run's sets differ from the last run's,
and the two remaining reads of every context use bit sets. The invalidation admits its
bit sets and the normalized premises of the changed axioms before it starts, and its lists
of parts, of the walk and of the cleared contexts before each grows. It also admits the
larger disjointness maps that kept contexts need when they number their sets again,
before any context changes, and an unsatisfiable context keeps no map. On `wally` with four
threads, in alternating runs of 7885a660 against 3afa8c52 that each passed the check against
a fresh run, the kept index stage of `ore_ont_13482` fell from 423.5 to 347.6 ms and its edit
from 485.9 to 408.1 ms, 0.68 of a run that starts again, and the edit of `ore_ont_9499` fell
from 146.6 to 138.6 ms. Item 590 still waits for item 1254, since on `ore_ont_13482`
the index build and the checks before it are 0.47 of a run
that starts again, while item 664 shipped ahead of item 1254 over the index-comparison path.

Item 664 extends incremental OWL 2 EL classification to edits involving individuals,
nominals, assertions, keys, context merges, universal roles and conditional worlds. Plain
EL terminologies without individuals continue to take the bitset invalidation path of
item 1256, adding only an edit-signature check and an inactive modification-bit guard on
the shared saturation path. Strictly EL runs that enter Phase 2 record an acyclic
`real_parent` forest for `real` reachability, pre-broadcast grounded supports for derived
global universal facts, per-merge causal context and real-reachability dependencies for
`HasKey` merges and per-world context, expression, key, named-individual and
datatype-checker dependencies for cached conditional worlds. Incremental invalidation
compares the old and new indexes, propagates through backward existential edges, nominal
members, real-context nominal merges, the `real_parent` forest and `HasKey` dependencies,
re-seeds kept contexts that retain an alternative real predecessor and reuses unaffected
cached conditional worlds while replaying sticky datatype-checker rows. A kept run
restarts cleanly when a previously broadcast global universal fact loses all grounded
supports, drops optional tracking to a fresh next run when a tight memory budget or
shared pool refuses the dependency tables, and uses `Interner::met_individuals` to filter
removed individuals out of live nominals and `Saturation::describe`. Targeted and
randomized differential tests in `crates/sparkles-owl/tests/el_incremental_individuals.rs`,
`tests/classify.rs` and `tests/incremental_sweep.rs` verify exact taxonomy, consistency
and `describe` parity against fresh full runs and confirm that unaffected contexts and
conditional worlds are not recomputed.

Item 804 delivers the sixth phase's rooted run as the option `Limits::rooted`, off by
default, as [nominals in the disjunctive rules](#nominals-in-the-disjunctive-rules) and
[item 804's departures](#item-804s-departures-from-the-journals-rules) describe. With it,
ontologies with individuals and nominals take the case analysis through a root context,
with four recorded departures from the journal's rules, a work bound past which the run
gives its default answer, and 45 of the 50 `shoi` fixtures exact against HermiT. The five
fixtures that passed the bound were item 822, and the default is item 823.

Item 805 delivers keys over object properties and the universal object property in rooted
runs, as [nominals in the disjunctive rules](#nominals-in-the-disjunctive-rules) describes.
A key becomes class axioms that mark the values of each named individual with a universal
restriction, in place of the fresh property per individual that the section first
designed, which took most generated ontologies with keys past the work bound. The
universal property becomes a symmetric transitive property with a hub individual. The
fresh names carry a reserved prefix, an ontology that uses the prefix is not rewritten,
and no fresh name reaches a taxonomy, a partial answer, a description or the report of
the axioms left out. On 392 generated ontologies with four individuals and keys or the
universal property, checked against HermiT 1.4.5 on one and on four threads with the same
answers, no run reports a subsumption HermiT does not. 274 take the disjunctive rules and
finish within the bound, and all agree with HermiT but one, a gap of the rooted run that
the ontology shows without keys or the universal property too and item 823 records. 109
pass the bound and give their default answer, and 9 take the default paths. ore_ont_14924 states the universal property only in a dropped tautology, so its
run is rooted now. It passes the bound and gives the default answer, which is HermiT's,
in 0.3 to 1 s against 16 ms without the option on a shared laptop. ore_ont_15803 does the
same in 1.7 s against 0.7 s on four threads, with the same taxonomy as before item 805.
Item 1061 follows that cost.

Item 1265 closes a crash of the keys that the rewriting declines. When an ontology's only
individuals were in axioms outside OWL 2 EL, the disjunctive rules took the run, the
normalization made it rooted, and a key left in the EL part marked the contexts real in
rules that do not take those marks, so the run panicked. A run of Tier D is now rooted only
when the EL part has no keys, as the check for individuals in the EL part already required,
and such a run answers as a run without the option. Each reason the rewriting declines a
key, its bound, data, the bottom property, a construct without a reduction and the reserved
prefix, has a regression test. The rooted taxonomies and statistics of 198 fixture and
generated ontologies, 62 of them with keys, are unchanged. A rooted run that completed
before gives the same answers, except that some such runs now take axioms they used to
leave out. In one case a retained key and an individual only in a key outside OWL 2 EL
gave a rooted run with an empty root, which left out a minimum cardinality. The run is no
longer rooted, takes the cardinality and derives the subsumption that the run without the
option derives, which a regression test checks.

Item 1304 keeps the disjointness of members that the rewriting merges. Two members such
as `∃top.A` and `≥1 top.A`, or `owl:Thing` and `≥0 top.X`, have one image, and the set of
images held one member, which the normaliser drops, so a rooted run lost the disjointness
and could answer a class satisfiable or an inconsistent ontology consistent while it
reported itself complete. The members are equivalent, so their disjointness makes the
image empty. The rewriting now adds `D ⊑ ⊥` for each image `D` of more than one member,
keeps the disjointness of the distinct images when two or more remain, and keeps the union
of a `DisjointUnion` over the set of images. Intersections, unions and equivalences keep
the set of images, since a repeated member does not change their meaning. A rewritten
axiom can equal an axiom that the ontology states unchanged or the rewriting of another
axiom, so the report of the axioms left out now names every original axiom that a left-out
rewritten axiom stands for, and names each original once for each kind of gap.

Item 1314 preserves repeated operands in disjointness and inequality axioms at parsing and
model construction. `DisjointClasses`, `DisjointUnion`, `DisjointObjectProperties`,
`DisjointDataProperties` and `DifferentIndividuals` now store their operands in a sorted
`Multiset<T>` instead of a `BTreeSet<T>`, so structural equality, ordering, hashing and
serialized output remain order-independent while duplicate occurrences are kept across the
functional-style, RDF and JSON codecs, Manchester's four binary disjointness and inequality
forms and public constructors. Manchester continues to render `None` for `DisjointUnion`
and n-ary forms longer than two and carries no axiom annotation round-trip. Equivalences,
unions, intersections, `SameIndividual` and `HasKey` keep `BTreeSet` deduplication.
Classification forces repeated class operands empty in `DisjointClasses` and
`DisjointUnion`, marks repeated properties empty in `DisjointObjectProperties` and
`DisjointDataProperties`, and derives inconsistency for repeated individuals in
`DifferentIndividuals` with rooted rules off and on, subject to each tier's existing role
and datatype restrictions such as top and bottom data properties in the datatype checker
and counting for `DisjointObjectProperties` in Tier D. `dl::clauses`, Horn normalization,
Tier D normalization and `dl_reduce` reduce repeated class operands to emptiness
constraints beside surviving distinct members so Horn ontologies do not escalate to Tier D,
property disjointness emits each self-pair once over distinct roles under the run's memory
and cancellation limits, retained-axiom estimates include repeated operand storage, and
round-tripping, annotations and original-axiom diagnostics preserve the original operands.
Item 1313 keeps eligible item-805 rewritten results when the rewriting turns every non-Horn
axiom into Horn or OWL 2 EL axioms, such as a one-image `DisjointUnion` or two complemented
disjointness members that merge to one image. Previously `Saturation::run` required Tier D to
have taken the rewritten ontology and discarded the lower-tier answer, reporting the original
axiom outside the supported fragment. The run now keeps the rewritten result whenever the
rewritten ontology has no remaining disjunctive axioms and leaves out no axiom generated by
the rewriting, restores any unchanged exclusions, computes the original axioms' outside-EL
counts, chain range checks and tier from the original ontology, and keeps every internal
fresh class and the hub individual out of the taxonomy, descriptions and reports. When Tier D
does not take the rewritten ontology, runs that remain disjunctive or leave out a rewritten
axiom, such as under a non-regular role hierarchy, a `DisjointUnion` over a
non-simple self role or an at-least restriction above the Horn bound of 64, still start again on
the original axioms. Direct parser regressions in `tests/dl_nominals.rs` cover each kept,
chain-range statistics and rejected case without injecting unrelated disjunctions.

Item 822 adds a fifth departure, [the constant of a restriction into a
singleton](#item-822s-departure-the-constant-of-a-restriction-into-a-singleton), and item
1070 makes the context of `owl:Thing` a query context. With both, 48 of the 50 `shoi`
fixtures are exact against HermiT in every configuration, and shoi-005, 027 and 032 finish
with 21,485, 12,666 and 30,789 derived conclusions where they passed the bound before.
The 45 fixtures that finished before derive about half as many conclusions as before at
the median, with one, shoi-033, rising from 404 to 528. shoi-022 and 042 still pass the
bound for other causes, which the departure's section attributes. Item 822 is complete
for its scope, and shoi-022 joins the duplication of ground reasoning that item 1061
follows, while item 1075 follows the disjunctive equalities of shoi-042. Of the 393 generated
ontologies of item 805 that run, 339 now finish within the bound on one and on four threads
with the same answers, where 274 did before, 44 pass it where 109 did, and 10 take the
default paths. All agree with
HermiT but g2-150, whose answer holds one subsumption that HermiT 1.4.5 does not report,
since HermiT does not apply a key over the universal property. HermiT derives that subsumption once the key's property is a named one with the
same values. The gap of `owl:Thing` that item 805 found is closed.

Item 1086 closes the taxonomy and class-description gap when a run places a class in the
top node without also recording every class above it as a subsumer of `owl:Thing`.
Taxonomy construction now closes the set of top-equivalent classes upward transitively
over satisfiable subsumers, covering multi-hop chains, equivalence classes in any IRI
order and paths through hidden internal classes from item 805's rewriting without leaking
internal names, breaking parent and child links or altering inconsistent ontologies.
`Saturation::describe` applies the same upward closure so direct descriptions agree with
the taxonomy.

Item 1220 first turned off the selection of ground body literals in rooted runs and
narrowed the restriction on ground inferences in Join, since each could make a run that
reported itself complete miss an entailed subsumption or an unsatisfiable class. A Join
whose premise on the side of the head has no ground literal in its body is exempt from the
restriction, whatever its residual head. Generated ontologies found both losses, and with
the two changes no generated or fixture run that finished within the bound, reported itself
complete and left no axiom out differed from HermiT, apart from g2-150, whose difference
is HermiT's. That is evidence and not a proof, and a complete rooted answer is not a
certificate of completeness while item 823 is open. shoi-005 and shoi-027 then passed the bound and gave their default answer, so 46 of the
50 `shoi` fixtures were exact against HermiT, and 14 more of the 393 generated ontologies
with keys or the universal property passed the bound.

Item 1220's repair then turned the selection back on. Hyper over `x` takes selected
clauses as partners of an atom over `x`, and Pred from a successor over a successor term
`f_e(u)` of a constant takes them as partners of an atom over `f_e(u)`, so two selected
clauses of one context meet in both inferences. The Join exemption stays. Generated
ontologies built around both losses, with bodies of two to four literals, delayed and
disjunctive support, residues on edges, successor-term hypotheses, missing premises and
renamed twins, find the losses in many runs with the selection and without the repair and,
without counting, in none with the repair, apart from g2-150. With counting, one generated
ontology loses subsumptions with the repair, with the selection off and with both
restrictions off, so that loss is not the selection's, and it is item 1229. Every fixture and generated run that the
run without the selection finishes within the bound still finishes, with counting on and
off. The repair is adopted on that measured gate, and its completeness is not proved, so
item 1220 stays open for the proof. Against the run before item 1220, 48 of the 50 `shoi`
fixtures are exact again, and two runs pass the bound that finished before, s2-g2-011 on
one thread and on four and a version of shoi-022 with its individuals merged on four
threads, which are items 1227 and 1228. `SPARKLES_ROOTED_SELECT_GROUND=0`,
`SPARKLES_ROOTED_SEL_PARTNERS_X=0` and `SPARKLES_ROOTED_JOIN_EXEMPT=0` turn each part off
for measurement, and reproducing the original uncut selection-off control after item 1250
also sets `SPARKLES_ROOTED_GROUND_BODY_CUT=0`.

Item 1229 records a loss of runs that count and of rooted runs. Pred takes each clause
that a successor sends with the restrictions that reach the successor when it arrives, and
drops the clause when the successor sends it again over a later link. A unit existential
restriction that the context derived later took no Pred with the clauses its successor
had already sent. Under the eager strategy two restrictions can share a successor, and the
second restriction's edge can come after the successor's clauses did, when the context
replayed Pred only with the literal that made the edge, so a clause whose body lacks that
literal missed the new restriction in the same way. With counting Pred's conclusion can
name the restriction's own successor term in a role atom, and in a rooted run in an
equality with a constant, so such a run could report itself complete and miss an entailed
subsumption. Without counting or the root, a conclusion names the successor term only
through premises whose own arrival replays Pred, and runs that force the replay on in that
case change no taxonomy. A run that counts or is rooted now replays every clause that the
successor already sent with the late restriction, for a new unit, for a unit whose earlier
edge went to another successor, and for a new edge, whose replay of the literal that made
it is then left out for that successor as already included. The repair is adopted on a measured gate and
item 1229 stays open for the proof that it covers every order of arrival. On generated and
fixture ontologies built around the loss and on the selection's sets, on one and four
threads with both strategies, every run that answers in full, with nothing left out
and no fallback, agrees with HermiT wherever HermiT gives a taxonomy, and the cautious and
eager runs that both answer in full give the same taxonomy.
On the runs that answer in full with and without it, and where the run without it agrees
with HermiT, the replay adds 0.29% to the derived
conclusions of rooted runs that count, 0.47% to rooted runs without counting and 0.90% to
runs that count without the root, because it charges its products before the redundancy check. Three
generated ontologies that finished close to the bound now exceed it and give the answer
without counting or without the rooted rules, with the same taxonomy as before at a raised
bound, which item 1232 follows. `SPARKLES_COUNTING_LATE_UNIT=1` gives the earlier containment, in
which a rooted run classifies again without counting or without the rooted rules when such
a restriction or edge arrives, and `=0` turns both off, for measurement.

Item 1231 records a loss of rooted runs. A fact that holds in a context only under the
queried class and concerns an individual reached from another individual must reach the
contexts registered for that individual, and its consequences there must come back to the
context, and the departures above keep a context's conditional ground literals in that
context.
A rooted run therefore looks after its saturation for such a fact that cannot reach a
registered context, and when it finds one it classifies again without the rooted rules and
sets `Stats::dl_rooted_declined`. The check exempts four kinds of context without a proof,
and it withdraws between 2 and 38 complete answers per generated corpus, so it is partial
containment, and `Limits::rooted_decline` turns it off for measurement.

`Limits::rooted_marker_repair`, off by default, repairs the delivery. A context that holds
a ground literal as a condition tells the root, which sends the literal as a marker to the
contexts registered for its individuals that the root reaches, also to a context that
registers or becomes reachable later. The root returns its clauses whose bodies hold such
literals to the contexts that hold them. Enabling `Limits::rooted_marker_repair` defaults
to mode `129` (`MREPAIR_ON | MREPAIR_CF`), in which both return arrival paths exclude any
holder premise whose body holds a literal mentioning neither `x` nor `y`, namely a constant
literal or a constant-successor `f_e(u)` literal. On initial return arrival the holder
records the returned clause under every literal of its body before filtering its candidate
premises, and on late premise arrival the holder registers itself with the root before
excluding an ineligible premise from replay. The sends and the returns count against the
work bound. `SPARKLES_ROOTED_MARKER_REPAIR=1` selects the unfiltered return rule that
resolves against all live non-marker holder premises on both arrival paths, `=0` turns the
repair off, and an explicit environment value takes precedence over test overrides and
`Limits::rooted_marker_repair`. With the diagnostic `SPARKLES_ROOTED_MARKER_DIAG=1`, a run
reports in `Stats::dl_marker_deficits` the markers and returned products that its quiescent
contexts still owe; when mode `129` is active, the second deficit measures only the
eligible filtered return products, while mode `1` measures unfiltered products. In the
initial unfiltered measurements (`SPARKLES_ROOTED_MARKER_REPAIR=1`) with the check off, the
two generated corpora built around the loss give 1,168 and 262 runs that differ from HermiT
without the repair and none with it. Twelve fixtures built for particular orders of arrival
fail without it and pass with it, although no fixture isolates a returned clause that the
root subsumes later or a return followed by a late premise that it misses. No run that
finishes on the other generated corpora, with or without counting and on one and four
threads, differs from HermiT apart from g2-150. With the repair off, every corpus gives the
same answers and the same counts of derived conclusions as before, and every finished run
on the corpora owes no deficit with the repair. Unfiltered repair derives 1.02 to 1.13
times the conclusions on the larger generated corpora, up to 1.41 times on corpora of small
files, up to 1.28 times with counting and 1.4 to 1.65 times on the two built around the
loss, which now derive the missing entailments, so a few runs near the bound pass it and
give their default answer.

In the original uncut selection-off control (`SPARKLES_ROOTED_SELECT_GROUND=0` and
`SPARKLES_ROOTED_GROUND_BODY_CUT=0`, the setting of item 1220's earlier measurements),
unfiltered returns (`SPARKLES_ROOTED_MARKER_REPAIR=1`) derive more. On one purpose-built
ontology they add 24% to the derived conclusions, and on the generated ontology
g1-051_p0012 the run passes the work bound at 4 million conclusions and the memory budget
of 1 GiB at 400 million, against 455,967 conclusions without the repair. Filtering returns
alone with `SPARKLES_ROOTED_MARKER_REPAIR=129` on that uncut control loses no answer on the
corpora, takes the purpose-built ontology's cost from 24% to 10.5% and finishes ten runs
that unfiltered repair takes past the bound, but g1-051_p0012 still passes the bound of 4
million and finishes only at 63.2 million conclusions under a bound of 400 million. Its
growth is in Pred over successor terms of constants and in Join, and g1-051_p0121 derives
1.42 million conclusions against 1.17 million with unfiltered repair alone.

Item 1250 decouples the non-root ground-body cut from full ground-body selection and makes
mode `129` the default when `Limits::rooted_marker_repair` is enabled. Each stored clause
in a non-root context carries `Stored::gbody` alongside `Stored::sel`, while root clauses
keep both flags false. A non-root clause whose body holds a literal mentioning neither `x`
nor `y` sets `Stored::gbody` whenever either full selection or the decoupled cut is active
and sets `Stored::sel` only when full selection is active. `Stored::gbody` cuts non-local
ground Hyper, role-universal inferences with constant or `f_e(u)` fillers, head-side Join,
and purely constant or `f_e(u)` binary Eq and Factor even when
`SPARKLES_ROOTED_SELECT_GROUND=0`. Mixed non-local Eq and Factor, including query-form
`x ≈ u` equalities, remain gated by `Stored::sel`, and Hyper over `x` and `f_e(u)` Pred
partners remain gated by `Stored::sel` and `SPARKLES_ROOTED_SEL_PARTNERS_X`.
`SPARKLES_ROOTED_GROUND_BODY_CUT` is on by default and `=0` disables the decoupled cut;
reproducing the original uncut selection-off control therefore sets
`SPARKLES_ROOTED_SELECT_GROUND=0`, `SPARKLES_ROOTED_GROUND_BODY_CUT=0` and
`SPARKLES_ROOTED_SEL_PARTNERS_X=0`, together with `SPARKLES_ROOTED_MARKER_REPAIR=1` for
unfiltered returns. With the decoupled cut active under `SPARKLES_ROOTED_SELECT_GROUND=0`
and `SPARKLES_ROOTED_SEL_PARTNERS_X=0`, the full 19-axiom `g1-051_p0012` finishes complete
within the default 524,288 rooted work bound with zero marker deficits and matches HermiT
1.4.5's taxonomy (`C1` unsatisfiable and `C0` equivalent to `owl:Thing`) across repair off,
default filtered mode `129` and unfiltered mode `1`: on one thread cautious with default
mode `129` it takes 251,217 derived conclusions, 32,307 stored clauses and 10.9 MB peak
estimated memory, and on four threads eager it takes 336,298 derived conclusions, 41,450
stored clauses and 12.6 MB peak estimated memory, against 232,622 conclusions (10.2 MB)
with repair off and 262,033 conclusions (10.9 MB) with mode `1` on one thread cautious. The
repair's state costs 8 bytes per context with the repair off, which item 1247 removes, and
a synthetic family whose conditions nest two or more deep grows with the repair's sends,
which is item 1251. The repair's completeness is not proved, and the check stays on by
default.

The repair keeps the resource contract of the rest of Tier D, and the contract is
cooperative. A run charges each conclusion that it derives to the work bound, and several
of the repair's scans also charge one conclusion for every 16 entries that they read.
Item 1280's meter, described below, counts the reads of the repair's other preparation
and of the rooted rules' premise lists. The redundancy test of each derived conclusion
against the stored clauses is not counted, here as in the rest of Tier D. The run checks the work bound, the memory budget, the deadline and a cancellation at
cooperative checkpoints, which come after every 256 choices of a product, after every 64
sends of a loop and whenever a worker adds its counts of applied conclusions to the run's
totals. Earlier builds added retained index growth to the memory estimate only after
allocation, buffered rooted initialization into one uncharged `Found` collection per
constant, and left non-ground product tables uncharged on the default path, so a counting
allocator measured the heap 6 to 31% above the charged estimate on the three files named
below and 1.04 GB above the 1 GiB budget on `ore_ont_8480`.

Item 1291 closes those gaps across Tier D. Root initialization streams directly through
`derive` with pre-admitted literal interning and store growth instead of buffering
constants times `owl:Thing` in `Found`. `Store` tracks exact incremental owned bytes across
its clause vector, literal slices, queue, predecessor list and index maps, pre-admitting
clause construction copies, `maximal_rooted` scratch and reallocation or rehash overlap
before mutating its containers. `DlSt`, `RootSt`, `MRep` and their index maps and vectors
are pre-admitted before allocation, `MRep::regs` empties replayed slots in place to stay
append-only and credits freed vectors after replay, and `Lits::lit_admitted` revalidates
growth bounds under the interner mutex while keeping caller callbacks outside the lock.
Committed Tier D growth stays inside the saturation's single memory counter so concurrent
checks never double-count a transfer, and temporary reservations in root initialization,
product enumeration and marker-repair handlers are guarded against callback unwinds.
Non-ground `fold` and ground products pre-admit candidate buffers, retained union payloads,
table rehashes, step-list growth and `prune` scratch, keep drained buffers charged until
emitted when switching to streaming at 4,096 unions, and account for live union payloads
beside any table slack that M-return reserved before item 1280 removed that reservation. The discretionary Horn and Tier
D memory allowance now uses a deterministic `.len()`-based logical input size independent
of thread count while `Limits::max_memory` and `Limits::charge` continue to observe full
container capacities, and `Saturation::retained_bytes` includes `DlRun::lits`. Counting-allocator
regression tests cover store mutations, backward subsumption, refusal without growth,
concurrent literal and ledger boundaries, non-ground and ground products, marker replay,
unwind cleanup and multi-chunk thread invariance. Remaining estimation boundaries outside
item 1291 are the upfront 32-slot `Segs` directory, `Table::push` segment allocations,
un-admitted `Lits::lit` calls outside root initialization, which remain post-counted in
`DlRun::bytes`, and the reads of premise-list preparation, which item 1280 meters as
described below.

Item 1075 delivers demand-driven binary case splitting over disjunctive constant
equalities in rooted runs behind the default-off `Limits::rooted_eq_split` switch, as
[item 1075](#item-1075-binary-splitting-over-disjunctive-equalities-of-constants)
describes. When enabled on full classifications, branches that reach the probe threshold
or derive a pure constant-equality disjunction at depth one and below fork into equality
and disequality children, intersect their terminal leaves' closed subsumptions and public
instances under one shared work bound and pre-admitted memory ledger, and retain a compact
`SplitAnswer` for `describe` queries. On `shoi-042` it completes within the 524,288 bound
across all thread, expansion and step configurations with zero taxonomy differences against
the checked-in HermiT oracle and exact `describe` instances, and across all 50 `shoi`
fixtures it completes within the bound with zero taxonomy differences against HermiT.
Enabling the switch by default waits on benchmark-host qualification under item 823.

Before item 1280, a returned clause reserved a bound on the tables of its product that is
not ground before the product ran, both on initial arrival and when a late premise replayed
saved returns. The bound took one union per choice of the product, which can exceed the
unions a step keeps by more than a thousand times, and on a product of 2^57 choices it
could not be represented at all. Item 1291 made the product charge its tables and step lists
directly as they grow, on both default and repair paths, so the upfront bound only
duplicated a charge that the product already made.

Item 1280 gives the repair and the rooted rules' premise lists one meter,
`dl_rules::Meter`. A read is one entry of the store's index, one literal of a clause, or one
step of a comparison of two premises. During saturation a meter checks the run's work bound,
memory budget, deadline and cancellation once every 1,024 reads. A step counts each read
before it makes it. A scan or copy of a premise, a side or a head reads one literal at a time
or in spans that end one read short of the next check, so that a step makes no more than
1,024 reads between two checks however wide one premise or clause is, and a check that stops
the meter comes before the rest of the scan. A count of many reads at once, such as the reads
that stand for one conclusion of the diagnostic, is split at each check with the rest carried
to the next. The library sorts of a list's sides by length, of an assembled clause's
literals and of the root fact offer's destinations are not split, and item 1319 follows
them. The count since the last check lives in the activation's
sink, which all meters of an activation share, so that many short scans in a row still meet
a check. The meter's checks are new, and its reads charge nothing to the work bound except
where a scan of the repair charged one conclusion for every 16 entries before. Those scans
keep that charge, so runs with the repair off and the repair's own counts of derived
conclusions are unchanged. A check that stops a meter ends the scan that made it at once.
The meter reads these steps.

- The premise lists of a returned clause are read twice, once to size them and once to build
  them, with a read for each body literal looked up, each index entry, each head literal of
  a premise, whose count sizes the premise's side, each literal that M-return-cf tests and
  each literal that the build pass copies, the fixed premise's and the base head's included.
  The buffer of the lists' sizes is admitted before it is made, and the lists, the product's
  slot and the pruning's scratch are admitted before the lists are built. The pruning of each
  list reads each side's literals for its signature and its test of order as it computes
  them, and a read for each comparison of two sides and for each literal that a comparison
  steps over. This holds on initial arrival, where the clause's index under its body literals
  reads through the same meter and the bounds are checked in full before the product is
  drained, and for every return that a late premise replays, with one meter across the
  replay, M-return-cf's test of the late premise's body and the search for its literal
  included, and a check of the bounds before each batch drains.
- The root's offer of a clause whose body is ground triggers reads through the meter for
  each body literal it tests, looks up in the index, adds to the index or compares to choose
  the holders it scans. A stop while the clause is indexed leaves the rest of its body
  unindexed in a store that the stop interrupts.
- On the default path, Hyper reads each body position of an ontology clause and each
  partner in the index, and its restriction, its test of a routable unit, its sizing and its
  copy read each literal of a partner and of the premise that they touch, in spans as they
  go. The build of a product's lists reads each body position again and copies the fixed
  premise in spans through the same meter. Hyper's test of root facts over a constant ends at the first refused read. Join reads each
  body literal of the given clause and each premise over a ground body literal, and its
  restriction, its count of the premise's head and its copy read in spans as they go. A stop
  inside Join's copy releases the conclusion's admission. The root's offer of a fact reads each literal of its head, each lookup of the
  registrations of a constant and each destination, and admits its list of destinations
  before it builds it.

The marker pass that the decline check and the diagnostic run after a rooted run saturates
is outside the saturation. Its redundancy test of each product reads the stored candidates,
the literals of their comparisons, the copies of the chosen premises and of the base's head,
each literal that the normalization of an assembled clause filters or compares as it removes
repeats, each literal that its test of a tautology compares, and each literal of the clause's
signature, through a meter of its own. The test of a tautology includes the search of the
head for each equality's inequality and each step of a walk over the sorted body and head.
The library sorts of the assembled clause and the lookup of its whole head among the root's
promoted facts read without the meter, and item 1319 follows them. That meter
checks only the deadline and cancellation through the diagnostic's callback, and it counts
its reads as separate deficit work, `dwork`, which neither the saturation's work bound nor
the admission of the deficit pass's scratch takes.

The upfront bound is gone. A return's premise lists are admitted exactly before they are
built, and the product charges its own tables as they grow. On a return of 64 × 64 premises
whose conclusions a stored unit subsumes, a counting allocator measures a heap that the
charged peak covers together with the return's retained record of the clause and its copy of
it, and the charge stays within four times the heap. A return over a wide body whose first
literal has no premise admits its buffer of the lists' sizes before it makes it, so that a
grant below the buffer refuses the return before the heap grows by it. A return of 2^57
choices now runs until the work bound or its tables' charge against the memory budget stops
it, where before it stopped at once on an unrepresentable bound. Under a budget of 4 MiB the
test fixture stops within the product's second step, before the product derives anything.

The tests of the meter cover these cases. They refuse each check of the bounds in turn
during a return's premise lists and their pruning, the index of a returned clause and of an
offered clause, an offered clause's holders, Hyper's lists and partners, the root's offer of
a fact, Join's premises and the search for a root fact. Each time they find no read asked
of the stopped meter and nothing held once the step returns. For a return's lists, Hyper,
its partners and Join, a wide premise of Join among them, they find the caller's guard
holding only what the step kept while the guard is still live, and for a return's lists
and Join no product or conclusion pushed. For the indexes and the offers they find the
store interrupted. A stop inside the index of an offered clause leaves the rest of its body
unlisted and sends nothing, and a stop in the root's offer of a fact sends nothing. A stop
while the 1,200 holders of an offered clause are scanned sends nothing, and a stop at a
check between the deliveries of their returns sends fewer returns than there are holders. A pruning of two wide sides reads every
literal of both, and a refusal of its first read ends it. A redundancy test of a product
whose head holds 4,096 distinct equalities stops at the first check inside its test of a
tautology, after exactly 1,024 reads since the check before, and asks no read of the
stopped meter. A redundancy test of a product whose body holds 4,096 distinct literals below
its one head literal stops at the first check inside the walk over the body in the same way.
A copy of a fixed premise of 3,072 literals in Hyper stops at each of its checks with the
caller's guard holding only what the step kept and no product pushed. A fresh meter that
charges and is refused at the first check of a count of 2,053 reads charges the 1,024 reads
up to that check and none of the rest, through a sink and through the diagnostic's
callback. A late replay whose preparation and flush are granted stops at the first
charge inside the drain, before it derives anything, and holds nothing. A return of one premise under a sink that
grants memory without the full check derives nothing after a stop that was already asked
for. A test reads through guarded views of one sink that keeps the activation's count, with
a check-only meter and a repair's meter that charges, and finds the check at the 1,024th
read of the two together and the repair charged one conclusion for every 16 of its own
reads. A late replay's batches keep M-return's rule across a flush. A test harness grows the
premises per literal, the length of a premise, the width of a single premise, the returned
body, the body that a late premise's literal is searched for in, the returns of a late
premise, the holders and body of an offered clause, the stored clauses of the diagnostic's
test, Hyper's premises, partners and fixed premise, the root fact offer's registrations and head, and
Join's premises and given body eight times over, as well as a premise, a partner, a side, a
returned clause's head and a late premise's body of up to 8,192 literals and a head of up to
2,048 equalities. It asserts each time that no more than 1,024 reads are counted between two
checks, that no more than 1,024 reads are made between two checks, as the scans and copies
count them where they make them, that every read the fixture needs is counted and that the
sink sees a check for every 1,024 reads. The fixtures are unit fixtures over one context's store, and
the harness does not run a whole saturation.

Item 1319 follows the scans that the meter does not read. These are Pred's lists in the
root, the pruning of the products of the other rules on the default path and of the unions
of one step of a product, the scans of the equality split and merge, which keep a counter of
their own, Join's head lookups, the universals over role atoms, the moves of universals to
and from constants, the registration of a context with the root, Hyper's restriction and
copy of its fixed premise alone for a conjunction or an existential restriction, the
materialization of the head atoms of a multi-head ontology clause in Hyper, the root fact
offer's list of constants and the sort, deduplication and delivery of its destinations, the
library sorts named above, the diagnostic's lookup of an assembled clause's whole head among
the root's promoted facts, and the copy of a returned clause that M-return takes before it
admits its lists. Outside the diagnostic's
counts and redundancy test, which check the deadline and cancellation, the marker pass that
the decline check and the diagnostic run after a rooted run saturates checks no bound, and
item 1260 follows it.

Item 728 delivers the clauses of [counting over definitions](#item-728s-clauses-for-counting-over-definitions)
and their record. With counting, an exact or minimum cardinality in a negative position
is the calculus's own at-most clause with the neighbours in its body, an exact
cardinality of one is an existential restriction in a negative position, and a product
derives each distinct conclusion once with its record charged to the memory budget. The
three files of the item still pass the bound of the counting run and classify again
without counting, so counting stays off by default, and the calculus change for the
equality reasoning over conditional successors that remains is item 894, which also takes
the design's third part, the witnesses' shared context.

Item 894 delivers [the order between number restrictions](#the-order-between-number-restrictions)
as the option `Limits::number_order`, which is on by default and applies to a run that
counts. Its changes to the rules of the
calculus, the cases of a union filler and the merge of successor terms, made no file
classify and are not kept, and its exploration round stayed on paper. The order changes
the clause set instead. A counting run without the root names each minimum, tells itself
the entailments between minimums and their contradictions with maximums, and makes no
witnesses for a minimum that a minimum of the context entails. With it ore_ont_6952 and
ore_ont_10140 classify with counting on and give HermiT's and Konclude's taxonomies, 6.3
and 6.5 times faster than Konclude 0.7.0 on four threads on `atlas` and 2.0 and 2.1 times
on one. ore_ont_6485 still passes the bound, and its classification with counting
requested, the failed attempt and the run without counting together, takes 1.28 times as
long with the order as without it, 157 ms against 123 ms on one thread. It took 3.1
times as long, 576 ms against 184 ms, before a context gave way to the contexts it woke.
The default accepts that cost, and item 896 follows that file and the default of
counting.

Item 897 gives each compound qualifier of a number restriction
[a name of its own](#compound-qualifiers-of-number-restrictions) in a counting run. Before
it, a maximum or one of item 728's clauses over an intersection or an existential
restriction that no other axiom named counted no neighbour, and the run reported an answer
that missed the restriction's consequences as complete. The reported cases and a generated
ontology that was reported consistent now have HermiT's answers, and the 205 files of the
ORE 2015 DL development set that give a taxonomy with counting give the same bytes as
before. The names alone changed no time in single runs, and the step in which a context
gives way made the failed attempt of ore_ont_6485 cheaper. Counting the neighbours that were missed costs work. The fixture `shiq-038`
derives 815,758 conclusions on one thread where it derived 246,957, so it passes the
default bound of 262,144 and falls back there, and it finishes with HermiT's answer at a
raised bound. A context of the disjunctive rules now
[gives way](#the-disjunctive-rules) to the contexts it woke after 256 clauses, which
keeps a generated ontology of this item within the bound on one thread. Counting stays
off by default until item 896.

Item 727 makes the disjunctive rules
[proceed in steps](#steps-for-a-bound-that-every-schedule-shares) in a run that has a
bound on its derived conclusions. `Limits::dl_steps` controls the steps and is on by
default. Such a run derives the same conclusions on one thread and on eight, so that it
keeps or passes its bound, and answers in full or falls back, in the same way on every
schedule. The 150 counting fixtures and the generated ontologies of items 894 and 897
show it, with the answers of a run without steps. On `atlas` on four threads the two ORE
2015 DL files that finish with counting take 4.0 and 4.9% longer in steps, and the three
whose attempt fails take 1.3 to 2.6 times less. With the steps ore_ont_6485's
classification with counting requested takes 213 ms with the order between number
restrictions against 188 ms without it on one thread. Item 950 follows the cost of the steps on
ontologies that classify in a few milliseconds. A rooted run and a run with the eager
strategy that counts take one thread with the option, which item 951 follows.

Item 962 composes each pair of links into the least properties of a chain's result. The
property hierarchy of the Horn rules is closed under inverses and every chain holds in its
mirror image, so a symmetric transitive property, which is its own inverse, also had the
transitivity of its inverse. The composition table then listed the property and its
inverse for every pair of links, each composed link was derived by both, and both were
composed again. A path of 60 links by such a property derives 548,060 conclusions on one
thread without the reduction and 72,284 with it, as many as the same path beside an
unrelated pair of inverse properties. A link by a property counts for each of its
superproperties in every rule, the table lists every pair of properties under a chain's
operands, and a property that clashes with itself is empty, so the least properties lose
no conclusion. Of equivalent properties the one with the smallest id stays, and the
reduction reads each result's superproperty list once and polls the run's stop as it goes.
Item 692 let the Horn rules take ore_ont_8737 and ore_ont_16744, whose inclusions under
the top property had made the rules refuse both files, and both have one such property. On
`atlas` on four threads ore_ont_8737 classifies with the Horn rules in 13.9 s with a peak
estimate of 3.0 GB, where it passed its budget of 7.05 GB and classified again by the EL
rules alone in 21.9 s, leaving its symmetric and inverse property axioms out.
ore_ont_16744 takes 3.9 s against 26.2 s, with 446 million conclusions against 2.23
billion by median and a peak resident set of 3.0 against 9.6 GiB. In three further rounds
of alternating order the item's build takes 13.4 s on ore_ont_8737 with the Horn rules
against 13.6 s with the EL rules alone, and 3.80 s on ore_ont_16744 against 3.71 s, so the
Horn rules now cost about what the EL rules cost on both files. The 210 files of the ORE
2015 DL development set that classify give the same taxonomies as before, and ore_ont_7192
passes the 13 GiB cap of the comparison on both builds. GO and ChEBI lite give the same
taxonomies, and in six rounds of rotating order on one and four threads the reviewed build
takes 0.981 to 1.019 times the base's time by median, within the spread of the rounds, and
item 968 follows ChEBI lite's 1.9% on four threads.

Item 710 lets the Horn rules pass over a successor that another dominates, as the section
[The Horn rules](#the-horn-rules) describes with its argument. A generated ontology of 15
axioms that derived 210 million conclusions in 5.06 s on one thread on `atlas` derives 2.0
million in 69 ms, and the 14,547 generated ontologies of the item's check derive 33 million
conclusions in all against 1.19 billion. Item 969 found why the failed disjunctive attempt
on ore_ont_9663 and ore_ont_14817 took from 10 s to more than 900 s. A product of the Pred
rule takes one premise from each of its lists, and on ore_ont_14817 thousands of products
had 100,000 to 1.4 million choices. A sample of 100 s of the attempt on four threads made
804 million choices in products of 100,000 or more, of which 6.6% gave a union of literals
not made before in the same product, and in the products small enough to check, 0.7% of
those unions contained no other. How many premises each list holds when the inference
runs depends on the order of the workers, so the attempt's length did too. A product that
is not ground now forms its unions list after list. Each step keeps each union once and,
up to 4,096 unions, only those that no other union of the step contains, and a last step
past 4,096 unions derives them as it makes them.

The argument for completeness is the store's subsumption. A union that contains another
union of its step gives, with every choice from the later lists, a union that contains the
other's union with the same choices. The conclusion it gives has every literal of the
other's in its body and its head, and `derive` changes a conclusion by literal, by dropping
body atoms that hold and head literals that cannot hold, and drops it when a literal holds.
The conclusion of the larger union is then subsumed by that of the smaller, or dropped
with it, and the store adds no clause that another subsumes and marks dead those that a new
one subsumes. Ground products of a rooted run keep the enumeration, since the restriction
on ground inferences filters their choices one by one. The steps' sets are charged to the
memory budget and each step releases what it drops. Past 4,096 unions of the steps before
the last, each further union counts toward a bound on derived conclusions, so a bounded
run still stops a wide product within its bound. The counting fixtures keep their answers
on every number of threads, and three more of them, `sriq-004` among them, finish within
the counting bound. Of the generated Horn ontologies with a union that meets nothing
beside them, 2,145 finish with counting on both builds, and seven more finish on this one,
whose answers equal those of the Horn rules. On `atlas` on four
threads three classifications of ore_ont_14817 take 19.9 to 38.9 s against 59.7 to 79.5 s,
and on one thread on the laptop the build before passed a timeout of 900 s where this one
finishes. The attempt still passed its budget before the Horn rules answered, until item
990 bounded it. On the Horn files that the sort of successors does not prune, such as
ore_ont_9855, ore_ont_8737 and GO, the build classifies in 1.01 to 1.06 times the base's
time on `atlas`, which item 991 follows. The sort adds its entries' memory at each check
of its bounds and at its end, and R6 its record's at the end of each rule. The run's sink
holds what they add in the activation and charges it once it reaches 16 KiB and when the
activation ends.

Item 990 bounds the conclusions that a run of the disjunctive rules without counting or
the rooted rules derives. `Limits::dl_attempt_work` takes 384 conclusions for each class
of the signature by default, and at least 2,097,152. Past the bound the run ends and the
Horn rules answer, as they do past the memory budget, with `Stats::dl_attempt_over` and
`Stats::dl_over_budget` set. The bound is a count and not a memory figure, so the number
of threads does not change it, and it grows with the classes, each of which has a context
in the EL saturation. Of the ORE 2015 DL development set, 12 files attempt the rules and
finish. Those with more than 5,461 classes, whose bound is above the floor, derive at
most 212 conclusions per class, ore_ont_16680 the most with 6.9 million. ore_ont_9663,
ore_ont_14817 and ore_ont_3215 pass their memory budget after 38, 49 and 791 million
conclusions on eight threads on the laptop, 660, 830 and 14,400 per class. A bound
relative to the conclusions of the EL saturation would not separate the two groups, since
ore_ont_16680 finishes after 2.8 conclusions per conclusion of the EL rules alone and
ore_ont_14817 passes its budget after 3.0. The largest attempt of the 2,166 generated
ontologies of item 969 that have a union derives 536,000 conclusions, and the dl603 and
dl897 fixtures derive at most 5,700, all within the floor. The bound keeps the driver
without steps, since on eight threads on the laptop the steps made the attempts of
ore_ont_5438, ore_ont_13103 and ore_ont_16680 1.7 to 1.9 times slower, which item 950
follows. Near the bound a schedule can therefore decide whether a run on several threads
answers in full. On `atlas` on four threads three classifications of each build took
7.5 to 8.8 s on ore_ont_9663 against 12.3 to 20.6 s, 7.9 to 11.3 s on ore_ont_14817 against
10.1 to 30.3 s and 4.4 to 4.6 s on ore_ont_3215 against 110 to 138 s, whose peak resident
set fell from about 7.7 GiB to about 2 GiB, with the same taxonomies. On one thread ore_ont_14817 took
57.7 s against 345 s and ore_ont_3215 13.6 s against 628 s, and ore_ont_9663 took 74.5 s
where the base passed the 900 s timeout. Of the 12 files whose attempt finishes,
ore_ont_10140, ore_ont_6952 and ore_ont_11460 classify 1.10 to 1.15 times slower than the
base on one thread and 1.06 to 1.10 on four, all of it in the saturation outside the
attempt's tests, and the others within 3% of the base apart from four files of under 10 ms
whose rounds vary. On `wally` the same build with the bound turned off was as slow, so the
check of the bound is not the cost, and item 1012 follows it. A sweep on the laptop classified GO, ChEBI lite, the 298 ORE 2015
EL files and the 207 DL development files once per build on one and eight threads. The
993 pairs that both builds classified gave the same taxonomy bytes, and no file changed
its tier. On one thread the base passed the 900 s timeout on ore_ont_9663, which this
build classifies in 70 s with the answer both builds give on eight threads. Neither build
answered on ore_ont_7192 within the sweep's limits, and the reader refuses ore_ont_9881. A
kept run counts the conclusions of the runs before it, as
[Incremental native DL classification](#incremental-native-dl-classification) describes,
and item 1010 follows the edits after which a kept and a fresh run fall on two sides of the
bound. Item 1011 follows the bound's form on other ontologies.

Item 1011 found what the attempts spend beyond their counted conclusions. On one thread
on the laptop the attempt took about 706 of the 728 G instructions of ore_ont_14817's
classification and 829 of 857 G on ore_ont_9663, and its cost per conclusion grew as its
clause stores grew. On the loaded laptop ore_ont_14817 took 5.1, 6.2, 7.9, 19.3 and 47.6 s
with bounds of 1, 2, 4, 8 and 16 million conclusions and 89 s with the default bound of
22.4 million. Half of the attempt's time went into the products of the rules, whose
choices below 4,096 unions a step count nothing toward the bound, and a fifth into pruning
their lists. The pruning of a product's lists now compares two sides by a signature of
their literals and then by a merge of the sorted literals, where it searched each literal,
and keeps the same sides in the same order. The Pred rule with a fixed premise moves its
lists into the last product instead of copying them. The 96 disjunctive files of the
development set derive the same conclusions with the same estimated bytes and taxonomies,
and the 15 that attempt the rules make the same products and clauses at every doubling
of the work. The classifications take 0.80 to 0.91 times the base's
instructions on the six files counted, with the limit below off. The times on the loaded
laptop are not measurements, but they show the classification of ore_ont_14817 taking 97
against 192 s on one thread in the sweeps with diagnostic counters. The
[laptop record](../../benchmarks/el-classification/runs/2026-11-20-disjunctive-products-laptop-53a51148/record.json)
holds the counts, the sweeps and the scripts.

On one thread an attempt also ends when one product of its rules has more choices than
`Limits::dl_attempt_width`, 1,048,576 by default. The 12 attempts of the development set
that finish make products of at most 256 choices on one thread, and the 2,166 generated
ontologies of item 969 with a union of at most 3,645. ore_ont_14817 makes a product of
120 million choices after 0.5 million conclusions, and ore_ont_9663 one of 4.2 million
after 4 million, where the bound is 22.4 million. Their classifications on one thread on
the laptop take 25.6 and 51.1 G instructions against 584 and 700 G without the limit, with the same
taxonomies, and every other file of the two sweeps keeps its answer and its statistics.
The width of a product depends on the order in which contexts receive their premises. On
four and eight threads the finishing attempts make products of up to 22,500 choices, and
in three runs on four threads ore_ont_9663 made products of at most 22,707 to 923,521
choices and ore_ont_14817 of 1.3 to 5.1 million, so the width does not tell an attempt
that finishes from one that cannot, and a run on more threads has no limit on it. Unlike
the bound on conclusions, the limit therefore depends on the number of threads. A run on
one thread may fall back to the Horn rules' answer, as any attempt that does not finish
does, on an ontology where a run on four threads answers in full with the disjunctive
rules, and the classifier accepts this. No file of the development set and none of the
generated ontologies does so, since their widest products on one thread stay 4,096 and
288 times under the limit. On one thread the order is fixed, and the
widest product of ore_ont_9663 falls from 999 million choices on one thread to 1.3 million
on two and 22,707 in one run on four, with 355, 71 and 24 million choices made by the products'
steps, which item 1225 follows. A kept run whose product passes the limit starts again in
full, as past the bound on conclusions. A run that leaves out an inference too wide to make,
for this limit or for an at-most restriction with too many candidates, now falls back even when it has
no bound on its conclusions, where it answered as complete without the inference before.

On one thread on forge the six files of the laptop counts classify in 0.79 to 0.91 times
the base's time with the pruning alone, and with the width limit ore_ont_14817 and
ore_ont_9663 take 2.3 and 4.9 s against 57.4 and 74.3 s for the base. GO, which has no
disjunctive attempt, takes 0.99 times the base's time. On four threads the rounds of one
binary spread by up to 24% on those two files, so the forge run decides nothing there,
and the four other files take 0.88 to 0.96 times the base's time. The
[benchmark report](../../benchmarks/el-classification/RESULTS.md#the-disjunctive-attempt-and-the-universals-on-the-benchmark-host)
gives every file.

Item 869 found where the Horn rules with individuals spend their time on four files of
the ORE 2015 DL development set with 7,856 to 114,957 property assertions,
`ore_ont_4510`, `ore_ont_12901`, `ore_ont_14375` and `ore_ont_10951`. A diagnostic build
counted no witness on them, so their worlds are not the cost. All but 44 to 116 of their
conjunction contexts held an individual and were real. R3 made the conjunction of `b`
and a filler for each assertion `r(a, b)` under a universal restriction or the range of a
transitive property, and that context derived `b`'s closure again and then exchanged
everything with `b`. R3 and R5 at a real context now send the individual the other
members of the conjunction and link to the individual's own context, as
[a successor that holds an individual](#the-horn-rules-with-individuals) argues. In four
rounds of alternating order on `atlas` the four files classify in 0.38 to 0.49 times the
base's time on one thread and 0.47 to 0.62 times on four, `ore_ont_4510` in 406 against
945 ms on one thread, with 116 conjunction contexts against 42,249 and 6.1 against 19.2
million conclusions. The Horn rules then take 2.0 to 2.2 times the EL rules' time on one
thread, against 4.2 to 5.6 times before, and item 1025 follows that remainder. The Horn
normalization also passes over an assertion about an anonymous individual before it
opens a scope for it, which takes 136 to 251 ms off the index stage of the four files
whose 243,972 to 457,662 such axioms the reader leaves out. Item 1281 replaced that
refusal, and those files now classify by the EL rules. GO, ChEBI lite, the 298 ORE
2015 EL files and the files of the development set keep their taxonomies byte for byte
on one and eight threads, as the
[benchmark report](../../benchmarks/el-classification/RESULTS.md#conjunctions-with-an-individual)
records with the files that pass its limits on both builds. The flush of R3 and R5 keeps
what it gives its own context, so that the context takes each conclusion once, and counts
what it sends an individual against the run's bounds. R5 could still give a context one
link for each pair of a role and a target before a bound counted them, as it did before,
which item 1026 ends.

Item 1025 measured what the Horn rules with individuals still derived beyond the EL rules
on the four files, and most of it was not the Horn rules' exchange. The files are
Phenoscape knowledge bases whose property `BFO_0000052` is functional and has a range,
in a run with the Horn rules also through the domain of its inverse. The index gave each
assertion over it the context of the individual and the range as its successor, 13,547
contexts on `ore_ont_4510`, and each derived its individual and repeated the individual's
closure, under the EL rules as well. The index now gives the restriction of an assertion
the individual's own context and tells the individual the range, as
[the EL classifier](#the-owl-2-el-classifier) argues for individuals and
[the Horn rules with individuals](#the-horn-rules-with-individuals) describe for the Horn
run. In four rounds of alternating order on `atlas` the four files classify in 0.60 to
0.63 times the base's time on one thread and 0.72 to 0.92 times on four, `ore_ont_4510`
in 240 against 401 ms on one thread with 2.48 against 6.08 million conclusions, and the
EL rules alone classify them in 0.65 to 0.71 times and 0.63 to 0.85 times. The Horn rules
then take 1.8 to 2.0 times the EL rules' time on one thread, against 2.0 to 2.1 times on
the base in the same rounds. On four threads they take 1.5 to 2.0 times, against 1.4 to
1.7 times, since the EL rules gain more there. Six further files of the development set
with such assertions classify in 0.45 to 0.84 times the time on one thread,
`ore_ont_9654` in 3.1 against 5.4 s. On `ore_ont_4510` the Horn rules still derive 2.48
million conclusions against 1.15 million for the EL rules alone on one thread. A
diagnostic build on the laptop counted the difference as five subsumers that every
context takes from `owl:Thing`, 300 thousand conclusions, the two conclusions that tell a
successor it is real, 548 thousand, the states of the automata, 103 thousand, 218
thousand further repeats of existential restrictions and 40 thousand successors. A
prototype in which a successor learns that it is real from one conclusion in place of two took 1.4 to 1.9% off the four files on one thread and was not kept, as
[the experiments page](../../PERFORMANCE-EXPERIMENTS.md#reasoning) records.

Item 1026 bounds what R5 gives a context before the activation applies it. A flush merges
the successors of one at-most restriction and leaves the other restrictions and R3
pending, as [the Horn rules](#the-horn-rules) describe, so the activation's own list
holds the links of one restriction. `ore_ont_9855` then makes 15,774 conjunction
contexts against 15,866 and derives 20.8 against 21.1 million conclusions on one thread.
A sweep on the laptop classified GO, ChEBI lite, the 298 ORE 2015 EL files and the 207
files of the development set with the base and with both items on one and eight threads,
and the 994 pairs that both builds classified gave the same taxonomy bytes, as the
[benchmark report](../../benchmarks/el-classification/RESULTS.md#assertions-under-a-range)
records with the files that pass its limits. GO, `ore_ont_1370`, `ore_ont_16744` and
ChEBI lite, which derive the same conclusions on both builds, are 2.3, 1.6, 1.5 and 0.9%
slower on one thread on `atlas` in every round, and item 1028 follows that. Item 1025
also timed `ore_ont_9663` against the build before item 869 now that item 990 bounds its
attempt. Over sixteen processes a side on four threads the build with item 869 takes a
median of 8.75 against 8.17 s and is slower in 12 rounds, and on one thread 76.3 against
75.8 s with the same conclusions in each of four rounds. Almost all of the difference is
outside the Horn rules' final saturation, in time that holds the attempt of the
disjunctive rules, which item 869 does not change. Item 1027 follows it.

Item 1025's guarded R7 search and covered link rules reduce classification time
on the four assertion-heavy ORE files. In six rounds on `atlas`, commit
`8b311a59` against `48344bca` takes 0.88 to 0.91 times the time on one thread
and 0.77 to 0.82 times on four by the median paired ratio. The Horn rules
take 1.63 to 1.68 times the EL-only time on one thread and 1.52 to 1.54 times
on four. On one thread `ore_ont_4510` takes 207.5 against 235.3 ms and
derives 2.21 against 2.36 million conclusions. Its 116 conjunction contexts
hold only 2,228 of 758,645 subsumers, so their duplication accounts for little
of the remaining work. On the rebased classifier the generated and hand-written
cases, including contexts whose witness verdict the covered successor changes,
agree with HermiT. The two paths with and without the witness clause select
identical sets of named classes for worlds when evaluated against the same
saturated state. This holds across 369 HermiT-checked cases and a set of 968
generated cases on one and eight threads. The clause changes a context's
witness verdict in 39 and 36 of the HermiT-checked cases, respectively.
Item 1028 follows the
one-thread costs on GO and EL-only `ore_ont_10951`. The
[benchmark report](../../benchmarks/el-classification/RESULTS.md#guarded-horn-rules-with-individuals)
gives the stages, controls and evidence.

Item 693 reduces the automata by which a run with the Horn rules unfolds its universal
and negative existential restrictions, as [the Horn rules](#the-horn-rules) describe. It
merges the states that accept the same words, drops a transition that a superproperty
covers and drops the transitions by properties that no link of the run can have.
`SPARKLES_HORN_AUTOMATA` selects the reductions for a comparison, and the default
applies all three. Six rounds of alternating order on the benchmark host compared the
build with the base and with itself without the reductions. Without their disjunctive
attempt the Horn rules classify ore_ont_14817 in 0.80 times the base's time on one
thread and 0.85 times on four, 2.0 against 2.5 s on one thread, and ore_ont_9663 in 0.75
times on both, 2.7 against 3.7 s, with peak resident sets of 713 against 808 MiB and 951
against 1,215 MiB. `ore_ont_9654` classifies in 0.25 times the base's time on one thread
and 0.41 times on four, with 9.6 against 45.9 million conclusions and 665 against 1,359
MiB, and `ore_ont_9855` in 0.64 and 0.67 times with 247 against 371 MiB. GO, ChEBI lite
and the files whose conclusions stay the same take 0.99 to 1.01 times the base's time on
one thread, and every taxonomy of the rounds is equal on the four sides. The default run
of the two Uberon files does not gain. Its bounded attempt of the disjunctive rules takes
nearly all of its 58 and 75 s on one thread and sets its peak resident set, and the run
is 2.5 and 1.8% slower on the build of the reductions in each of six rounds, although its
Horn saturation is 0.5 and 0.9 s shorter. A pinned process on the laptop executes the same
instructions in the attempt of ore_ont_14817 on the base and on the build with and
without the reductions, which suggests that the attempt does the same work there. On four threads
`ore_ont_8737` takes 1.03 to 1.04 times the base's time on the build with and without
the reductions, within the range of the base's own rounds. Item 1090 follows both, and
the [benchmark report](../../benchmarks/el-classification/RESULTS.md#reduced-automata)
holds every file. Item 712 applies the first two reductions in the unfolding of the
disjunctive rules as well. Item 693 keeps the remaining cost of the Horn rules on the two files, which
still derive 22.1 and 35.6 million conclusions against 16.5 and 10.8 million for the EL
rules alone.

Item 712 reduces the automata by which the disjunctive rules unfold their universal and
negative existential restrictions, with the first two reductions of the Horn rules, as
[reduced automata of the disjunctive rules](#reduced-automata-of-the-disjunctive-rules)
describes. On BioTop, ore_ont_12141 of the ORE 2015 DL sample, the 22 unfoldings of its 11
automata make 62 fresh classes and 77 transition axioms where they made 111 and 357, and
the attempt's `Stats::dl_work` on one thread falls from 136,117 to 66,200, with 42,242
stored clauses against 75,182. On `atlas`, twelve rounds in alternating order of one
binary with `SPARKLES_DL_AUTOMATA` at 0 and at 2 took BioTop's classification from 56.2 to
31.0 ms on one thread, a median paired ratio of 0.552 with every round between 0.550 and
0.556, and from 23.7 to 14.3 ms on four threads, 0.599 with rounds from 0.550 to 0.657.
The run's estimated bytes fall from 21.2 to 12.8 MB. Two other releases of BioTop among
the final files of the ORE sample, ore_ont_16847 and ore_ont_9024, halve their work in the
same way. Every taxonomy is equal at both levels on the 205 development files and the 183
final files that both classify and on the 875 fixtures of items 603, 897 and 712. Item 712 stays open for
its aim of three times Konclude's speed, which a comparison on one host decides.

The rule for a new universal no longer searches the context's universals for the same
role and filler. A context derives each universal once, since a role and a filler have
one node, and a repeat would only repeat work, because the successor rule deduplicates
the fillers it collects. A debug assertion keeps the premise, and it held in the test
suite and in debug classifications of GO, ore_ont_14817 and ore_ont_9663 on one and four
threads. A corpus sweep of the release builds with and without the search gave the same
results on the 1,010 pairs that both classified. On
one thread on forge ore_ont_14817 and ore_ont_9663 classify 2.1% and 2.4% faster, and GO
0.2% to 0.7% slower in each round, a median of 0.6 ms, which item 1234 follows.

Item 693 leaves out the universal restriction on `owl:Thing` of a range of a property that
chains reach when the told ranges of the links that end the property's paths already give
it, as [the Horn rules](#the-horn-rules) argue. The rule leaves out all 6 such universals
of ore_ont_14817 and 13 of the 16 of ore_ont_9663, which then derive 8.2% and 7.2% fewer
conclusions and take 7.4% and 6.5% fewer instructions per classification on the laptop.
On `atlas`, without their disjunctive attempt and against the same binary with every
universal kept, they classify in a median over five paired rounds of 0.942 and 0.955 times
the time on one thread and 0.927 and 0.960 times on four, with peak resident sets of 638 against 697 MiB and 910 against
959 MiB on one thread. GO has no such universal and takes the same time. The taxonomies
are equal on the 389 development and final-set files of the ORE 2015 DL sample that both
sides classify, and on 3,537 generated Horn ontologies, of which the 853 that leave a
universal out and that HermiT accepts give HermiT's taxonomy. `ore_ont_7646` now
classifies within 8 GiB on one thread with the attempt bounded at 1, with 219.2 million conclusions against 398.7
million. The [benchmark report](../../benchmarks/el-classification/RESULTS.md#range-universals-that-the-links-cover)
holds the rounds and the sweeps.

Item 868 asked for a cheaper criterion for the witnesses of a run of the Horn rules with
individuals, whose worlds can find nothing. A diagnostic build counted the witnesses and
timed the worlds on the 207 files of the ORE 2015 DL development set on four threads on
the laptop, with a limit of 120 s. The reader refuses one file and two pass the limit.
Of the other 204, 27 have witnesses, 1 to 160 of them with 1 to 470 classes
classified again in a world. The time from their first witness to the end of the run,
which holds the worlds and the build's own counting, is 89 ms of the 2,839 ms that the 27
files classify in, and at most 17.4 ms on one file, `ore_ont_9151`. The item closed
without a change, since a criterion with an argument of its own would save less than
that.

Item 753 makes a run after an edit of class axioms
[keep the contexts](#incremental-native-dl-classification) of a run with the Horn rules
without individuals and of a run of the disjunctive rules that does not count, with the
answer of a run that starts again. Horn runs with individuals stay with the item, and
runs that count, rooted runs and the run after a passed counting bound are item 953.

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
(Profiles §2.2.6). A restriction to an individual that an assertion tells is the
exception, which the paragraph on individuals below describes. A chain that breaks the
range restriction is left out. A context with a role loop takes the role's range, and the
index registers every range as a positive occurrence, so the existentials and data
restrictions of a range that only a loop reaches have their successors and told
subsumers (item 1262). The taxonomy groups
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
next three paragraphs describe. The profile checker validates XML literal namespace
conformance while retaining noncanonical spellings. Full canonical lexical acceptance
and canonical value comparison remain in item 663. A literal or a datatype that the classifier cannot
compare, which is an XML literal, an ill-typed literal or a datatype outside the OWL 2
datatype map, is an atom that only equals itself, and its
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
role under the bottom property derives `owl:Nothing`. A restriction `∃r.{b}` that is told
of an individual, which a property assertion `r(a, b)` is, puts `b` in the range of `r`,
since `a` has an instance in every model. The index tells `b` the range and gives the
restriction `b`'s own context as its successor, wherever the restriction occurs, where
the context of `{b}` with the range would derive `b` and repeat everything `b` holds
(item 1025). A restriction `∃r.{b}` that only classes hold keeps the context of `{b}`
with the range, since `b` is in the range only where one of those classes has an
instance. What a context that is not real
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
lattice. Integer, decimal and rational literals have exact canonical rational values
with arbitrary-precision numerators and denominators. Equivalent spellings and reduced
fractions compare and hash equally, including beyond 128 bits. Integer and decimal
datatype membership uses the canonical value. Other comparable values are a string,
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
other contexts drop their links from cleared contexts and keep what they take from the
index, which is rebuilt from the axioms on every run. A negative existential restriction
that an edit adds or removes has its filler among the changed parts, so every context
that takes it from the index is cleared, and a kept context renumbers the disjointness
sets it meets only when the run's sets differ from the last run's (item 1256). A change to the property
hierarchy, chains, reflexivity or ranges saturates everything again, and so does a
change to an axiom with an individual, a data property, a datatype, a key or a self
restriction. So does any change after a run that marked the real contexts, which
merges, worlds, keys and the facts of universal roles tie together in ways the
invalidation does not follow, and a
run after a run that stopped before its saturation ended. A run that stops later keeps
its saturated contexts. Property tests check that an incremental run after random
additions and removals gives the taxonomy of a full run.

A self restriction starts the run again because a context can have a loop by a role
without holding the self restriction whose subsumer the loop gives. A loop also comes
from a self restriction on a subrole, a reflexive role or a chain, so the invalidation
cannot find the contexts that a changed self restriction reaches by the expressions they
hold. Until item 1258, an added `ObjectHasSelf(s) ⊑ C` beside `r ⊑ s` and
`owl:Thing ⊑ ObjectHasSelf(r)` cleared no context and left out `owl:Thing ⊑ C`.

This reuse applies to eligible EL edits. Normalization with Horn or disjunctive axioms
currently clears the contexts and marks the run entangled, so a later edited source
rebuilds those runs, unless the run is one that
[keeps its contexts](#incremental-native-dl-classification). Reusing the completed
result at the same commit is separate from incremental maintenance.

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
charged for the growth of the peak, once, from the normalization on. A refused
estimate can raise the reported peak. Only successful grants count toward the
coverage that later phases reuse.
Numeric parsing and reduction poll cancellation and deadlines and admit construction
scratch before allocation. Large canonical values share immutable limbs whose live
storage remains counted until the last alias drops. Index copies, functional-data
groups and key scratch admit overlapping storage before growth. A numeric memory,
cancellation or deadline refusal stays a typed classification error through the Horn
and DL paths. It does not turn the value into an opaque atom or a completed fallback.
This accounting does not charge standalone XML lexical-validation scratch or complete
the hierarchical owner integration of item 675.
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
and removals, including edits involving individuals, nominals, keys, merges, universal
roles and conditional worlds, reuse unaffected contexts and worlds, as do edits of class
axioms after a Horn run without individuals and a disjunctive run that does not count,
while the other Horn and disjunctive runs rebuild after edits. Item 753 covers incremental
native DL maintenance under item 603, and item 664 delivered the EL individual, merge, key
and world invalidation tracking. One kept classification
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
`Classifier` and `Saturation`, in `tests/el_incremental_individuals.rs` targeted and
randomized differential runs over edits of assertions, nominals, merges, keys, universal
roles and conditional worlds, and in `tests/incremental_sweep.rs` incremental against
full runs on edits of class axioms beside fixed self restrictions, reflexive roles,
chains, ranges, individuals and the top and bottom properties, the same taxonomy on one, two and eight threads for an
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
incremental runs that follow merges, keys and worlds were item 664. Reads that
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
disjoint fillers, as the paper's Lemma 2 does. The rules run when one of them can fire,
and not at all under `Limits::el_only`, the ELK mode, which leaves every axiom outside
OWL 2 EL out as ELK does and gives ELK's taxonomy. An ontology with individuals,
functional data properties or top properties runs them beside the EL rules of
individuals, as the section
[The Horn rules with individuals](#the-horn-rules-with-individuals) describes (item
692). Axioms that are not Horn stay left out and reported, and so do property
hierarchies that are not regular, automata of more than 65,536 states and data restrictions outside
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
below removed that composition, and both now finish with the Horn rules. The waiting
messages count toward the budget, so a run near it depends on the schedule.
ore_ont_16744 of that sample has a budget of 7.57 GB, and before item 962 its Horn run on
four threads on `atlas` peaked between 5.6 and 7.4 GB in the 11 of 16 runs that finished,
over five versions of the classifier, and passed the budget in the other five, which then
classified by the EL rules alone and reported the file's two Horn axioms as left out.
All 16 taxonomies are the same bytes. Its symmetric transitive property composed each link
twice, which item 962 removed, as the Outcome describes. On the
benchmark host ore_ont_9663 takes 4.0 s and 1.0 GiB on one thread and 0.89 s and 1.4 GiB
on eight, and ore_ont_14817 takes 2.2 s and 0.7 GiB and 0.46 s and 1.0 GiB. The EL rules
alone take 0.77 and 0.68 s on one thread, and Konclude 0.7.0 took 8.9 to 9.7 s and 3.5
to 4.2 GB on four threads on the laptop. The two taxonomies miss 33 and 27 of Konclude's
subsumptions, which follow from axioms that are not Horn, and give none that Konclude
does not entail. On one thread the Horn rules still derive 52.0 million conclusions on
ore_ont_9663 and 29.4 million on ore_ont_14817, where the EL rules derive 18.5 and 20.8
million, which item 693 follows. Of the files of the development set below that the
rules accepted before item 692, none reaches the budget with item 693's rules, and no
case of the differential test does. ore_ont_8737 and ore_ont_16744, which the rules accept
since item 692, stay within it since item 962. The largest Horn run among the files accepted
before item 692, ore_ont_9855, classifies in 1.5 s on one thread on the benchmark host
against 4.4 s with the rules of phase 3, with a peak resident set of 359 MiB.

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
  classes. Merging pairs created a context for each subset on the way. A flush merges
  the successors of one restriction and leaves the other restrictions and R3 pending
  until the activation has applied what the merge gave the context. The activation's own
  list, which no bound counts, then holds at most a link for each successor that the
  restriction counts, the members of their cores and the loops, all of which the
  context's charged state bounds, where it held the links of every restriction at once
  (item 1026). When two restrictions count overlapping successors, a run can make other
  intermediate conjunctions than one that merged for both at once, so its statistics and
  a budget outcome near a threshold can differ, while a finished saturation gives the
  same answer.
- **Fillers no rule reads.** The rule for a successor passes over a filler that no rule
  takes as a premise and whose told subsumers every context holds. The rule for a
  universal restriction on an inverse still takes such a filler to the predecessors.

R3 and R5 add a successor to a context and keep the successors it extends or merges, so
a context with inverse properties, at-most restrictions and nested existential
restrictions can collect many successors whose roles and cores others contain. Counted
for every at-most restriction, each of them would take R6 to every predecessor, which
would receive the successor's core and a link each time, and a few axioms would give
millions of repeated subsumers and links. A successor by `r'` in `N'` dominates one by `r` in `N`
when `r'` is under `r` and the core of `N` is part of the core of `N'`. A run of the
Horn rules without individuals, universal properties or functional data properties
passes over dominated successors. A context sorts its successors into those that another
successor dominates and those that no other does, as they come when it has an at-most
restriction and at the end of each activation when it has only universals, and R6 sends a
predecessor the core of each successor once. Each core carries a signature of one bit for
each of its expressions, so that most pairs of successors are told apart before their
roles and cores are compared.

The argument for completeness rests on one property of such a run. A context's
subsumers, loops and at-most restrictions follow from its core and from what its
successors send back, since the rules that read a predecessor send their conclusions to
that predecessor. By induction on the derivations, a context whose core contains that of
another derives every subsumer the other derives, and each successor of the other is
dominated by one of its own. Domination is transitive and the successors of a context
only grow, so at the end of the saturation every dominated successor is dominated by one
that no other dominates. Each rule that passes over a dominated successor is then covered
by an instance on the dominating successor.

- **R3.** The universals on the superroles of `r` are on superroles of `r'` too, so the
  extension of `N'` contains that of `N`, and R3 extends `N'`.
- **R5.** The dominating successor holds every filler that `N` holds, so the at-most
  restriction counts it. The union of the counted cores is the same, and a role clash of
  `r` with another member is a clash of `r'`, since clashes hold for subroles. When `r'`
  and the other member's role are one role, that role clashes with itself and is empty,
  which already makes the context unsatisfiable.
- **R6.** The predecessor receives the core of `N'`, which contains that of `N`, and a
  link by the inverse of `r'`, which is under the inverse of `r`. A clash of the
  predecessor's role with the inverse of `r` is one with the inverse of `r'`.
- **The successor's own rules.** `N` keeps its link from the context and still runs its
  rules, whose conclusions for the context `N'` also sends.

A dominating successor can come into the counted members later than the one it dominates,
when its context derives the filler later. The counting then runs R5 and R6 on it as on
any new member. Runs with individuals keep every successor, since a context there can
take conclusions from its predecessors, such as realness and singularity. A differential
check compared the taxonomies of 14,547 generated ontologies of 8 to 18 axioms with the
build before on one and eight threads, and of 2,145 of them, with a union that meets
nothing beside them, with the disjunctive rules counting, and found no difference.

A run reduces each automaton before it unfolds a restriction by it (item 693). The
closure over the ε-transitions leaves states that accept the same words, transitions by
a subproperty beside the one by the property, and branches through properties that no
link of the ontology has. Each state becomes a class in every unfolding, and each
transition a universal restriction that R3 and R4 carry to every context on the way, so
a redundant state or transition is paid for at each of those contexts. The automaton of
the inverse of `part_of` in ore_ont_14817 has 4 states and 10 transitions after the
closure and 2 states and 2 transitions after the reductions. There are three, and
`SPARKLES_HORN_AUTOMATA` set to 0, 1 or 2 turns off all of them, the last two or the
last one for a comparison.

1. **States that accept the same words by the same transitions become one.** The run
   takes the coarsest partition of the states in which the states of a block are final
   alike and have transitions by the same properties into the same blocks. The states of
   a block accept the same words, so the quotient accepts from each block what its
   states accepted. The translation of the completeness argument above reads only the
   words each state accepts, and holds for the quotient unchanged.
2. **A transition that another covers by a superproperty goes.** When a state has
   transitions by `l` and by `l'` into the same state and `l` is under `l'`, the axiom
   `X ⊑ ∀l.Y` of the first follows from the axiom `X ⊑ ∀l'.Y` of the second under the
   property hierarchy. The translation without it has the same models, and the calculus
   is complete for it as for any translation. Of two transitions by equivalent
   properties the run keeps one.
3. **A transition by a property that no link can meet goes.** R3 applies a universal
   restriction on `l` to the links under `l`, and R4 applies it to the backward links
   and loops under the inverse of `l`. No other rule reads it. A property is live when a
   link or loop of the run can be under it or under its inverse, or when it is a
   universal property. The automaton loses the transitions by the other properties, then
   the states that no longer reach a final state, and the states the start no longer
   reaches. The universal of a lost transition was the premise of no rule instance, and
   a state that reaches no final state gives a context only universals over other such
   states, so the run derives the same subsumers among the expressions of the ontology.

The third reduction rests on the list of properties a link or loop can have. A context
links to the successor of each existential restriction it holds, which includes the
negative restrictions `∃r.F` that the run makes fillers of universals, since a context
that derives one links to its successor too. Composition adds links by the results of
the chains the run composes. R6 gives a predecessor a link by the inverse of a
successor's property. A loop has a reflexive property, the property of a self
restriction, the property of a link when R6 identifies a predecessor with a successor,
or a chain's result, and each loop has its inverse. The rules of individuals copy links
and loops between the contexts of one element and keep their properties. The live
properties are all of these with their inverses, which is more than the rules can
produce. A run that composes every chain prunes nothing. The index keeps the properties
whose transitions went, and a debug build checks at every link and backward link of a
Horn run that none of them applies to it.

The fresh classes of an unfolding are positional, and an incremental run gives the
state at each position the class the last run gave it. The third reduction makes the
automaton depend on which properties have links, so an added axiom such as `A ⊑ ∃v.B`
can bring a branch back and move the positions. This needs no rule of its own. A state's
class carries the told axioms of its transitions, and a kept context is saturated again
when the told axioms of an expression it holds differ from the last run's. A changed
property axiom starts the run again, as before.

A reduction takes at most sixteen million steps for an automaton. A step is a state or
a transition that a pass visits, a comparison of a sort or a comparison of a search in a
list of superproperties. Each pass charges an upper bound of its steps before it starts,
which it counts from the automaton's size and the length of the longest list of
superproperties alone, so that every run reduces an automaton the same way. A round of
the refinement charges the sort of each state's transitions, and a pass over the covered
transitions charges two lookups for each pair of transitions with one source and one
target. A pass that would exceed the bound is left out, and the automaton stays as the
passes before left it. The refinement of a chain of tens of thousands of operands, which
takes a round for each state, is such a case, and so is a state with thousands of
transitions into one target. Pruning is not counted. Its time is linear in the automaton
up to a logarithm, and it tests a property against a table of the properties that have a
live property under them, which the run fills once from the superproperty lists of the
live properties.

On one thread and without their disjunctive attempt, the Horn rules derive 22.1 million
conclusions on ore_ont_14817 where they derived 29.1 million, and 35.6 million on
ore_ont_9663 where they derived 50.9 million. Merging gives 27.9 and 46.0 million,
covered transitions 24.2 and 41.1 million, and dead properties the rest. The EL rules
alone derive 16.5 and 10.8 million. The contexts of conjunctions hold 5.47 million
subsumers on ore_ont_14817 where they held 8.50 million. On the benchmark host the Horn
rules alone then take 2.0 against 2.5 s and 2.7 against 3.7 s on one thread, with peak
resident sets of 713 against 808 MiB and 951 against 1,215 MiB, as the Outcome records.
The taxonomies are equal at every level on one and eight threads. The disjunctive rules apply the first two
reductions in their own unfolding and not the third, as
[reduced automata of the disjunctive rules](#reduced-automata-of-the-disjunctive-rules)
describes.

A differential check generated small Horn ontologies with chains, transitive properties,
inverses, ranges and property hierarchies over six properties, of which the class axioms
use mostly three, so that the automata have branches without links. Of 8,288 cases, 8,274
take the Horn rules, and none differs in its taxonomy between the unreduced automata and
each reduction on one thread, all three on eight threads, and a build that checks the
dead properties at every link. The reductions change the conclusion count of 2,730 cases,
and the third one that of 2,658. GO, ChEBI lite, the ORE 2015 EL files and the DL
development set, 497 files that the laptop classifies within 8 GiB, give the same
taxonomies with and without the reductions on one and eight threads, and 67 of them
change their conclusion count.

A range of a property that chains reach is the universal restriction `⊤ ⊑ ∀r.C`, which
every context holds and unfolds into the states of the automaton of `r`. The run leaves
it out when the told ranges already give `C` at the end of every path that implies `r`
(item 693). Every word that the automaton accepts ends with the label of a transition
into a final state. The universal is left out when the start state is not final and,
for every such label `l`, `C` is a told range of `l` or of a property above `l`, with
`r` itself among them. The told ranges include the domains, each a range of the inverse
property, and the universal property is never such a label. On ore_ont_14817 the rule
leaves out all 6 range universals and on ore_ont_9663 13 of 16. The property 207 of the
second file keeps its universal, since its paths can end with links by properties 206,
329, 355 and 356, which have no range `C`, and without that universal the taxonomy
loses subsumptions.

Leaving an entailed axiom out is sound. The argument for completeness changes the first
of the three steps above and leaves the other two.

1. **A told range holds on every link.** For each told range `C` of a property `p`, the
   index gives `C` to the successor of each existential restriction on a property under
   `p`, and to the source of each link by a property under `p⁻` through `∃p⁻.⊤ ⊑ C`. A
   loop takes the ranges of its property and of the property's inverse, and the range of
   a reflexive or universal property holds everywhere. R3 and the merges of R5 keep the
   cores that carry the ranges, R6 gives a predecessor the core of the successor it is
   identified with, and the rules of individuals copy links with their properties. This
   is how the run treats the ranges of simple properties, which never have a universal.
   The translation of the first step therefore keeps `⊤ ⊑ ∀p.C` on the links of the
   chain-free model for every told range, whether `p` is simple or not.
2. **The translation without the universal preserves named subsumption.** A model of the
   ontology is a model of the translation without the states of the left-out universal.
   Close a model of that translation under the chains. An edge that the closure adds by
   `r` runs along a path whose properties spell a word that the automaton of `r`
   accepts, by the construction of Horrocks, Kutz and Sattler. The last edge of the path
   is an edge of the model by a property under the word's last label `l`. That label has
   `C` as a told range of a property `l'` above it, and `⊤ ⊑ ∀l'.C` holds on the edges
   of the model, so the end of the path is in `C`. The closure then satisfies
   `⊤ ⊑ ∀r.C`, and every other axiom as the first step shows.
3. **The rest is unchanged.** The calculus without composition is complete for this
   translation, and chain selection derives what that calculus derives, as before.

The argument holds for several universals left out at once, since each one's case reads
only the told ranges on links and not another universal. Its first point needs every
property that a link can have to count for the rule `∃p⁻.⊤ ⊑ C`. A context that derives
a negative existential restriction made positive by its unfolding links to its
successor, so the index gives those properties the same rule (item 693). Those rules
add conclusions on 27 of 3,537 generated ontologies, on none of the 389 files of the ORE
2015 sweep and on no file of the benchmark, and change no taxonomy. The
automaton's reductions and pruning do not affect the test, which reads the labels into
the final states of the closure over the ε-transitions before them. It finds them from
the transitions between a state that the start reaches and one whose ε-transitions reach
the end, without building the closure, in time linear in the automaton apart from the sort
of the labels and with polls of the run's deadline and cancellation, and it refuses
scratch whose upper bound passes the run's memory limit, as the property closure does. When the left-out universals were a run's only
universal restrictions and no other Horn rule can fire, the run takes the EL rules, which
compose every chain, as for any such run. A change to the axioms after a run with the
Horn rules normalizes them again, so a removed range that covered a universal brings the
universal back.
Tier D makes its own range universals and keeps them all. `SPARKLES_HORN_RANGES=keep`
keeps every range universal for a comparison.

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

Items 720, 721 and 722 measured these follow-ups on `atlas`, which has the benchmark
host's hardware. A process's first helper threads take much longer to start than later
ones, whose stacks it keeps. A warm run on eight threads that starts no helper takes 0.21
ms on `ore_ont_2775` against 0.29 ms when it starts them at once, and a cold run takes
0.64 against 1.55 ms. The saturation therefore records how long its first worker took to
start the helpers, and a later saturation of fewer than 4,096 roots starts them once the
first worker has worked that long, between 20 and 500 us. The first saturation of a
process still waits 0.5 ms. The first worker looks at the clock after each activation
until then, since an activation of the Horn or disjunctive rules can take tens of
microseconds. Warm runs of the five files of item 720 on eight threads take 2.1 to 3.8
ms, 0.04 to 0.21 ms longer than with the build before item 709 against 0.4 to 0.6 ms
before, and item 819 follows the rest. Cold runs keep item 709's gain. A run on one thread sends through its outbox
once it is congested, so that its deliveries drop the messages their contexts hold as
those of several threads do. A run that is not congested reads the batching flag at
each message it sends to another context, as before, and adds a comparison of its
waiting messages with its stored conclusions to each check of the bounds. On one thread `ore_ont_8737` peaks at 5.9 GiB against 9.3
GiB and takes 90.7 against 95.8 s, while its run on four threads keeps its 28 s and 6.8
to 7.9 GiB, which item 818 follows against Konclude's 11 s. The one-thread times of GO,
ChEBI lite, `ore_ont_3836`, `ore_ont_13482` and `ore_ont_7914` moved by up to 2% between
builds that differ only in code that one thread never runs. The build of item 720 is 0.6%
faster to 0.9% slower than the base on them, and item 722 stays open for the layout.
Two more variants that measured no better are in
[PERFORMANCE-EXPERIMENTS.md](../../PERFORMANCE-EXPERIMENTS.md), together with a
deduplication of congested deliveries that slowed the one-thread run of `ore_ont_8737`
to 119 s.

Item 818 measured where the time of `ore_ont_8737` on several threads goes before it
changed anything. The ontology is 2.5 million axioms `C ⊑ ∃r.D` over one transitive
symmetric role with no class hierarchy, so its saturation composes a transitive closure
through backward and forward links, and each link of a component of k classes is derived
about k times. The run sends 3.0 billion messages, 1.49 billion backward links and 1.49
billion forward links, for about 32 million stored conclusions, and 0.7% of them go to a
context another worker holds, so the wait that the item's row supposed is not the cause. A
profile on four threads put 26% of the cycles in the outbox's radix sort of its messages by
destination, 18% in the congested delivery's filter, 12% in the sends and about 30% in the
rules, and the filter of item 695 never looked at forward links, which it let through as
repeats. Two changes follow. A congested delivery drops every kind of conclusion that
`State::holds` answers, forward links and loops included, since every rule returns at once
from a held conclusion. And the rules of `Back`, `Fwd` and the existential subsumers store
their link and leave its products, the subsumer each propagation gives each source and the
links the chains compose from each pair, to the activation, which sends them once its to-do
list is empty, or when idle workers wait, pairing what is new since its last flush with
everything and what is old with what is new, so that each pair is sent once as before. The
products for one source or target leave together, the outbox records these runs and sorts
the run headers rather than the messages, and a delivery then checks a long run against one
context's state. The ownership argument is unchanged, since a flush reads only the state of
the context its worker holds under the state's lock and sends through the same outbox and
to-do lists, and a run that answers in part flushes before it gives its conclusions back.
On `atlas` the saturation of `ore_ont_8737` takes 13.5 s on four threads against 25.2 s,
9.0 s on twelve against 18.5 s and 45.0 s on one against 83.0 s, applies 1.26 billion
conclusions on four threads against 2.12 billion, and peaks at 5.1 GiB of resident memory
on four threads against 6.9 GiB, with the same taxonomy on every thread count. In an earlier
slot the base took 28.6 s on four threads and the filter alone 22.0 s, and the runs take the
rest. Konclude 0.7.0 classifies the ontology on `atlas` in 9.5 to 9.7 s with one, four or
twelve workers, at 6.7 GiB, so the run on twelve threads is now faster than Konclude and the
run on four threads takes 1.4 times its time in three quarters of its memory. The 298 ORE 2015 EL
files, GO and ChEBI lite give the same taxonomies on one and eight threads. GO and
`ore_ont_3836` classify within 1% of the base on one, four and twelve threads, ChEBI lite
1.9% slower on one thread, within its spread on four and slower in three of six rounds on
twelve, and `ore_ont_13482` 3.4% slower on one thread and 3.1% on four, which item 890
follows for both files. The
remaining cycles of a four-thread run are the sends, 22%, the filter's probes of the
destination's tables, 29%, and the to-do list copies, 8%, which item 889 follows.

Item 889 takes up that cost in three changes. A flush writes each chunk of a group's link
products into the outbox once, and each destination's run header points at that copy, so
that the outbox holds one copy of the chunk for all its destinations and a delivery writes
each message once more, into the to-do list of each destination that does not hold it. A
run without an outbox takes each to-do list's lock once for the chunk, and a run in steps
keeps one message for each destination in its own list, in the order each destination
received them before. A congested delivery checks a run against the context's tables in
one inlined loop, which looks up the sources of a backward link's role once for each
change of role. An activation applies the batch it takes from its to-do list from the
list's buffer instead of copying it reversed onto its own conclusions, and a run that
answers in part gives back the rest of the batch in the order it did. Every destination
receives the same messages in the same order as before, so the derivations do not change.
On `atlas` the saturation of `ore_ont_8737` by the EL rules takes 8.9 s on four threads
against 13.7 s, 24.6 s on one against 45.0 s and 7.4 s on twelve against 9.3 s, the
medians of three rounds, with the same taxonomy. On this build its Horn attempt passes its
memory budget before that saturation, which item 962 has since removed, so these are the times of
the saturation and the classification times include the attempt. ChEBI lite classifies 8
to 14% faster, GO 1 to 3% faster, `ore_ont_13482` 1 to 2% faster and `ore_ont_3836` within
its spread, 0.3 and 0.4% slower on four and twelve threads, which item 890 rechecks, and
the 298 ORE 2015 EL files, GO, ChEBI lite and `ore_ont_8737` give the base's taxonomies on
one and eight threads. An open-addressing table for the sets of more than 16 members
measured no faster on `ore_ont_8737` and slower on ChEBI lite and `ore_ont_13482`, and was
not kept. In a profile on the laptop the filter's probes take 45% of a four-thread run's
cycles and the rules' inserts 13%, since the run still sends three billion messages for 32
million conclusions, and item 956 follows them.

Item 956 takes up the repeated links. A transitive role's closure was derived once for
each intermediate context, since a middle context composed its backward links with every
forward link it held, those that chains had composed included. A role is now linear when
every pair of roles equivalent to it composes into the roles it composes into with
itself, and every role that composes with one of its links on the right does so alike
with its equivalents, into roles that compose again with a further link by it into the
same roles. A link of a linear role that a chain composes from two links by its
equivalents then keeps no forward link at its source, and a middle context pairs its
backward links only with the forward links that the axioms and the other chains give.
Every backward link that the saturation stored before it still stores, by induction on
the composition of the links that keep no forward link, so the taxonomies do not change.
`SPARKLES_LINEAR_CHAINS=0` composes every pair as before, for measurements. On the
laptop a four-thread run of `ore_ont_8737` with an early build of the composition sends
296 million messages instead of the 2.98 billion it sends with the switch off, and stores
15.8 instead of 20.9 million conclusions. On forge, against the build
before the item, the file's saturation with the Horn rules takes 0.94 s on four threads
against 8.22 s, 2.99 s on one against 25.41 s and 0.70 s on twelve against 7.05 s, its
classification 1.39 s against 9.07 s on four threads, and its peak resident set 2.3
instead of 6.3 GiB on four threads and 2.1 instead of 3.8 GiB on one. ChEBI lite
classifies in 0.81 times the time on one thread, 0.69 on four and 0.48 on twelve, and
GO and `ore_ont_3836` within the noise. `ore_ont_13482` classifies 2.5% slower on four
threads and `ore_ont_9855` 1.0% slower on one, with the same conclusions in those
cells, which item 1224 follows. The six timed files give the same taxonomies on
both builds. On the laptop so do the 298 ORE 2015 EL files, GO and ChEBI lite on one and
four threads with commit 3f20f7db, and 205 of the 207 files of the ORE 2015 DL
development set on four with commit 05838a64, which has the item's engine code, where
both builds refuse one file at the reader and stop one at its memory budget. The
[EL classification results](../../benchmarks/el-classification/RESULTS.md#left-linear-chains-of-transitive-roles)
give the rounds.

Item 1269 removes allocations from the index stage. The profile check of each axiom ran
the profile checker, whose datatype pass copies each class and property IRI into a
signature. It now walks the axiom without allocating and falls back to the full check
only for an axiom that may break a rule, so the axioms left out keep their rules,
messages and order. Normalization looks up an object property's role by its IRI in a
table of role ids. On forge `ore_ont_13482` classifies on one thread in 898 ms against
999 ms, with an index stage of 216 against 345 ms, and in 0.918 of the time on four
threads. ChEBI lite classifies in 0.964 and 0.956 of the time. GO's index stage is
faster, but its saturation moved with the code's layout, and its classification takes
1.0% longer on one thread, which item 1288 holds. On GO, ChEBI lite, the 298 ORE 2015 EL
files and the 207 DL development files on one and four threads, the 1,010 pairs that both
builds classify give the base's taxonomies and the base's axioms left out, with their
rules and messages, and both builds fail on the same two DL files. The
[EL classification results](../../benchmarks/el-classification/RESULTS.md#allocations-of-the-index-stage)
give the rounds.

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

### The Horn rules with individuals

Item 692 runs the Horn rules on ontologies with individuals, functional data properties
and top properties, as this section describes. The design keeps two promises. The answers equal HermiT's on the Horn files
with individuals of the ORE 2015 DL sample, and an ontology without individuals,
functional data properties or top properties is unaffected, so that its taxonomy is byte
for byte what it was. The second promise is about the taxonomy and the code the run
chooses, not about its time, which code layout can move.

A Horn ontology with individuals classifies by default with the Horn rules, in the same
run as the EL rules of individuals. Nothing changes for an ontology without individuals,
functional data properties or top properties, whose run chooses the code it chose before,
and nothing changes under `Limits::el_only`. The disjunctive rules keep a condition of
their own, which is the Horn rules' condition today and refuses an ontology with
individuals, data properties or top properties, so that lifting the Horn rules' condition
changes nothing for them. `Limits::rooted` lifts the refusal of individuals for the
disjunctive rules under the further conditions of its section, as it does today. When the
disjunctive rules decline an ontology, the Horn rules take what they can of it, with its
individuals, and leave out and report the axioms that need case analysis and the axioms
they left out before, which are at-least restrictions above 64, data restrictions that
neither OWL 2 EL nor the datatype checker accepts, and axioms over property hierarchies
that are not regular or whose automata pass the bound. Chains through the top property
stay partial, as item 663 records.

**One saturation.** Kazakov's rules read a context as the set of instances of its
conjunction, and every conclusion of a context holds of each of its instances. The EL
rules of individuals read a context the same way, so the two rule sets run in one
saturation. The saturation has four instantiations of its rules today, with and without
the rules of individuals, with the Horn rules and with the disjunctive rules, and gets a
fifth, with the rules of individuals, universal roles and functional data roles beside
the Horn rules, which it chooses when the index has the Horn rules and is not plain. The
four stay as they are.

The Horn normalization takes a singleton `ObjectOneOf` and an `ObjectHasValue` wherever
it takes a class, as the EL index's nominal and the existential restriction to it, and an
enumeration of several individuals in a negative position as the union of their
singletons, which becomes a fresh class above each of them, so that an at-most
restriction over such a filler counts the individuals together. An enumeration of several
individuals in a positive position is not Horn and stays with the disjunctive rules. The
normalization takes a class assertion `C(a)` as `{a} ⊑ C`, so that a Horn class
expression in an assertion reaches the rules, and an object property assertion over an
inverse property as the assertion over the property the other way round. The other
assertions are EL axioms already. A run with individuals takes a positive at-least
restriction above one, up to `MAX_AT_LEAST` over a simple role, by unfolding `≥n r.C`
into `n` existential restrictions `∃r.B_1 ⊓ … ⊓ ∃r.B_n` with fresh, pairwise disjoint
subclasses `B_i ⊑ C`, as Kazakov's Lemma 2 does without individuals.

Every model of the unfolding is a model of the original ontology, because pairwise
disjointness forces the `n` successors to be distinct. For the converse in the Horn
fragment with individuals, take any model `I` of the original ontology and set
`κ = max(ℵ_0, |Δ^I|)`. When `|Δ^I| = 1`, no element satisfies `≥n r.C` for `n ≥ 2`, so
interpreting every `B_i` as empty suffices. When `|Δ^I| ≥ 2`, form the direct power `I^κ`
with domain `(Δ^I)^κ`, coordinatewise atomic classes and object roles, and each named
individual interpreted as its constant tuple `(a^I)_{α < κ}`, lifting a fixed satisfying
valuation of the existentially quantified anonymous individuals in `I` to constant tuples
as well. By structural induction after root-level dualization, every negative-position
Horn expression `G` satisfies `G^{I^κ} ⊆ (G^I)^κ` and every positive-position Horn
expression `H` satisfies `(H^I)^κ ⊆ H^{I^κ}`, because positive nominals are singletons,
positive at-most restrictions have bound at most one, and no positive disjunctions occur.
Role inclusions, chains, inverses, `ObjectHasSelf`, reflexivity, irreflexivity,
asymmetry, disjointness, universal object roles, assertions, individual equalities and
inequalities, and keys over named individuals all hold in `I^κ`.

For OWL 2 EL data properties, resolve all EL `DatatypeDefinition` axioms first so that
every EL data range reduces to empty, a singleton `{v}`, a proper EL datatype `T`, or
`rdfs:Literal`. The OWL 2 EL datatypes form disjoint chains of finite height beneath
`rdfs:Literal`, and every proper EL datatype `T` has an infinite top atomic cell
`T^∘ = T \ ⋃_{T' ⊊ T} T'`. Distinguish the source data domain `Δ_D` from the target data
domain `Δ_D'` of `I^κ`. Under the full OWL 2 datatype map, `rdfs:Literal` already
contains values outside every proper EL chain, such as `xsd:boolean` values that do not
occur in EL data axioms, so `Δ_D' = Δ_D`. Over the strict EL datatype map alone, where
`Δ_D` has no value outside the proper EL chains, set `Δ_D' = Δ_D ∪ {u_⊤}` by adding one
fresh value `u_⊤` to `rdfs:Literal` outside every proper EL datatype. Pick one fresh
representative `u_T ∈ T^∘ ⊆ Δ_D'` avoiding all finitely many literal values in the
ontology for each EL datatype `T`, using `u_⊤` for `rdfs:Literal` when `Δ_D'` is
extended. Define `φ : (Δ_D)^κ → Δ_D'` by `φ(v⃗) = v` when `v⃗ = (v)_{α < κ}` is constant
and `φ(v⃗) = u_{T(v⃗)}` otherwise, where `T(v⃗)` is the unique smallest EL datatype or
`rdfs:Literal` containing `{v_α : α < κ}`. For every EL data property `d`, including
`owl:topDataProperty` and any role forced universal by inclusions, set
`d^{I^κ}(x) = Δ_D'` when `d^I(c) = Δ_D` and `d^{I^κ}(x) = d^I(c)` otherwise on each
constant tuple `x = (c)_{α < κ}`, and set
`d^{I^κ}(x) = {φ(v⃗) : v⃗ ∈ ∏_{α < κ} d^I(x_α)}` on each non-constant tuple `x`. On
`owl:topDataProperty` and any universal superrole, every constant tuple receives `Δ_D'`,
and every non-constant tuple's full product `(Δ_D)^κ` produces all values of `Δ_D` via
constant vectors `(v)_{α < κ}` together with `u_⊤`, when `Δ_D'` is extended, via any
non-constant vector combining values from two disjoint proper EL chains such as an
`xsd:integer` and an `xsd:string`. Applying the same map `φ` to every data property
preserves `SubDataPropertyOf` and preserves `FunctionalDataProperty` because
`|d^I(x_α)| ≤ 1 < |Δ_D|` gives `|∏ d^I(x_α)| ≤ 1` and never makes a fiber equal to
`Δ_D`. It preserves `DataPropertyRange`, `DataHasValue` in both polarities because no
`u_T` equals an ontology literal, and `DataSomeValuesFrom` in both polarities because
`u_{T(v⃗)} ∈ T^∘ ∩ T` forces `T(v⃗) ⊆ T` on the tree of disjoint EL chains. On constant
tuples, `DataPropertyAssertion` and `NegativeDataPropertyAssertion` hold because `u_⊤` is
distinct from every literal in `Δ_D`, and `HasKey` on named individuals holds without
spurious collisions because dummy values never enter non-full diagonal fibers and any two
diagonal fibers that share `u_⊤` were both equal to `Δ_D` in `I`, so those named
individuals already shared an existing value of `Δ_D` in `I`.

For each occurrence of `≥n r.C` processed bottom-up with already-unfolded filler `C'`,
every tuple `x` in `((≥n r.C)^I)^κ` has at least `n^κ ≥ 2^κ` distinct `r`-successors in
`(C^I)^κ ⊆ (C')^{I^κ}`, while `|Δ^{I^κ}| = |Δ^I|^κ = 2^κ`, so transfinite recursion
assigns `n` distinct unassigned `r`-successors of each `x` to `B_1, …, B_n ⊆ (C')^{I^κ}`
without reusing any element. Unfolding therefore preserves satisfiability and all
named-class subsumptions `A ⊑ B`, including named-class unsatisfiability, because any
counterexample `a ∈ A^I \ B^I` lifts to the constant tuple `(a)_{α < κ} ∈ A^{I^κ} \ B^{I^κ}`
with named extensions left uncoloured. Arbitrary non-Horn subsumptions over the original
signature need not be preserved, as `∀owl:topObjectProperty.{a, b, c} ⊑ owl:Nothing`
beside `DifferentIndividuals(:a :b :c)`, `IrreflexiveObjectProperty(:r)` and
`owl:Thing ⊑ ≥2 r.owl:Thing` illustrates.

When an ontology also contains a non-Horn finite data range handled by the datatype
checker, such as `DataOneOf` over three values alongside `≤1 r.(d = v)` restrictions, the
unfolded ontology can be semantically stronger under full case analysis over data values.
The Horn saturation still stays sound on such ontologies because the datatype checker
emits only valid Horn implications `⋀ Γ ⊑ ψ`: `owl:Nothing` for incompatible local
premises, a single entailed body atom, or a shared told positive consequence of a cover.
Non-told uses such as at-most qualifiers cannot trigger a grouped told consequence, and
any unresolved cover sets a diagnostic and is reported as used in part. Replacing each
checker atom with a fresh atomic class interpreted coordinatewise as a unary predicate
from `I` rather than from concrete OWL data values, and replacing the checker axioms with
the finite set of emitted Horn implications, yields a pure Horn ontology satisfied by `I`
whose unfolding `I^κ` models, so every Horn derivation stays sound.

Three mechanisms of the EL rules of individuals have to reach the links and contexts of
the Horn rules.

- A real context's successors are real. The rule for an existential restriction sends
  `Real` to the EL successor, and the Horn rules send it to the successors they create
  when the context that links to them is real, which are the conjunction that R3 extends
  a successor to, the conjunction that R5 merges successors into and the link that R6
  gives a predecessor. A context that becomes real tells its Horn successors as well as
  its EL ones.
- A context that derives an individual stands for the individual wherever it has an
  instance, and the two exchange their subsumers today, from the individual to the
  context always and from the context to the individual while the context is real. In a
  run with the Horn rules the context also gives the individual its backward links, with
  what it knows of each predecessor, real or not, since a link from `P` by `r` to a
  context that stands for `a` is a link from `P` to `a` whether or not the context has an
  instance. While real it gives the individual its loops and its Horn successors too,
  since its one instance is `a`, and takes the individual's subsumers, Horn successors
  and loops, as it takes the subsumers today. A Horn successor is a link that R6 gives a
  predecessor without an existential restriction among its subsumers, so a partner that
  takes only subsumers would miss it, and it travels as the `Succ` conclusion it is.
  What a context knows of a predecessor is whether the predecessor holds each at-most
  filler, which `PredIn` carries, and whether it is real, which the conclusion below
  carries, and both go with the link and as they come later.
- A universal restriction `∀t.A` on a universal role `t` held by a real context is a
  fact that holds everywhere, as a negative `∃t.F ⊑ x` whose `F` is real is today. The
  index lists the restriction among the facts of universal roles, the rounds between
  saturations and the worlds apply it as they apply the negative restrictions, and the
  Horn rules pass over it, since R3 and R4 would only spread `A` along links. An at-most
  or self restriction on a universal role, which OWL 2 DL forbids because the property
  is not simple, is left out and reported. The universal role is its own inverse in the
  role box. The top data property needs nothing new, since its rules are the EL index's,
  and so do the functional data properties, whose rule is local to a context and runs in
  the conjunction contexts as in any other.

**Singular contexts.** A context is singular when it has at most one instance. A context
that derives an individual is singular. A context `M` that links by `r` to a singular
context `N` holding `≤1 S.B`, with `r⁻` under `S`, and that holds `B` is singular too,
since every instance of `M` is an `S`-neighbour in `B` of `N`'s one element, and `M` is
empty when `N` is. A context keeps the flag and tells each predecessor that it is
singular, when it becomes singular and when a predecessor comes later, and a predecessor
that one of its at-most restrictions counts takes the flag from it. Singularity says
nothing about having an instance, which `Real` says.

**R7, the predecessors of a singular context.** Kazakov's rules have no rule over two
predecessors of one context, because without nominals the contexts of a Horn ontology
unravel into trees in which every element has one predecessor, and R5 and R6 cover the
successors and the predecessor of each element. A nominal is one element with any number
of predecessors, and so is every context that singularity reaches from it. This is the
interaction that HermiT's NI rule and the Nom rule of the rooted calculus handle, and the
earlier design work on this item recorded its simplest case, `A ⊑ ∃r.{a}`, `B ⊑ ∃r.{a}`,
an inverse functional `r` and `B(b)`, which entail `A ⊑ {b}` and which no rule above
derives. R7 closes it. At a singular context `N` holding `≤1 S.B`, two real
predecessors `M_1` and `M_2` that the restriction counts, linked by roles whose inverses
are under `S` and holding `B`, have one instance each and the two instances are one,
since both are the `S`-neighbour in `B` of `N`'s element. The two contexts exchange their
subsumers, Horn successors, backward links with what they know of each predecessor, and
loops, now and as they come, and the two roles are checked against the disjoint and asymmetric roles, as R5
checks the roles of merged successors. A loop of `N` by a role under `S` counts `N`
itself among the neighbours when `N` holds `B`, and then `N` and a real counted
predecessor exchange the same way, where the loop case of R6 today gives the predecessor
`N`'s core and loops only. R7 creates no context and no fresh individual. A context learns
that a predecessor is real from the predecessor, through a conclusion beside `Real` that a
real context sends to each successor it links to and that a context sends to its
successors when it becomes real. Only the fifth instantiation sends it.

**R8, role clashes at a singular context.** Disjoint and asymmetric roles clash on a pair
of elements, and the Horn rules check them today where R5, R6 or a loop puts two roles
between one pair. A nominal puts two roles between one pair without any of these, which
the design review of this section found. With `r` and `s` disjoint, `A ⊑ ∃r.{a}` and
`A ⊑ ∃s.{a}` make `A` unsatisfiable, and with `r` asymmetric, `{a} ⊑ ∃r.A` and
`A ⊑ ∃r.{a}` make the ontology inconsistent, while no at-most restriction fires and no
context derives a nominal beside `a`'s own. R8 has two cases, both at a singular context
`N` and both unconditional, since each concludes that a context is empty. Two backward
links into `N` from one source `M` by roles that clash make `M` unsatisfiable, since
every instance of `M` would have both roles to `N`'s one element. A forward link from `N`
to a context `X` by `r_1`, among the successors of its existential restrictions or its
Horn successors, and a backward link from `X` by `r_2`, where `r_1` and `r_2⁻` clash,
make `N` unsatisfiable, since `N`'s element would have an `r_1`-successor in `X` and
every instance of `X` has `r_2` to `N`'s element. The second case covers the asymmetric
example at `a`, whose forward link goes to `A` and whose backward link comes from `A`,
without a chain in the ontology. The first covers the disjoint example at `a`, whose two
backward links come from `A`. It also covers two sources that are one element, since the
exchange gives either the other's subsumers and Horn successors, so that the target sees
both links from one source, and two targets that stand for one individual, since each
gives the individual its backward links. R8 runs on a singular context's own state and
sends only `owl:Nothing`.

The soundness of the additions follows the reading of a context. A conclusion moves
between two contexts only when every instance of each is the same element, which the
exchange between a real context and its individual and the exchange R7 makes both
establish, and a backward link moves to a context without that condition only when the
link holds of the context's individual, which `P ⊑ ∃r.M` and `M ⊑ {a}` give.
Singularity propagates along a link into a singular context through an at-most
restriction, and the argument above is the whole of it. R7 needs both predecessors real
because `M_1 ⊑ X` for a subsumer `X` of `M_2` holds only when `M_2` has the instance that
every instance of `M_1` equals. R8 concludes emptiness, which needs no instance.

**A successor that holds an individual.** R3 and R5 link a context to the conjunction of a
successor with the fillers of its universals, or of the successors an at-most restriction
counts, and make a context for the conjunction. When the context is real and the
conjunction holds an individual `a`, they send `a` the other members of the conjunction as
subsumers and link the context to `a`'s own context, and make no context. The step is
sound, since the context has an instance, the instance's successor is in the conjunction,
and the only element of the conjunction is `a`, so `a` is in every member and the instance
has `a` as the successor. It loses nothing, by the list of what reads the conjunction's
context. That context would be real, as the successor of a real context, and would derive
`a`, so the exchange above would give `a` its subsumers, its loops, its Horn successors
and its backward links with what they know of each predecessor, and would give it `a`'s.
The rules that read a successor are the EL rules over a backward link, R2 and R4, the
`SuccIn` conclusion that R5 and R6 count, the `LinkTo` conclusion, the singular flag that
a counted predecessor takes, and R7 and R8 at the successor, and each of them reads the
successor's subsumers, loops, flags and backward links, which `a`'s context holds as the
conjunction's would, with the context's link, its fillers and its realness among them.
The context's own state differs only in the id of the successor it names, which four
readers observe. R3 and R5 read the core of the successor with that id, which for `a`'s
context is `a` and for the conjunction's is a set that holds `a`, so a later extension or
merge of either holds `a` and takes the same step. R6 sends a predecessor the core of
the successor that is the same element, which is `a` where it was the conjunction, and a
context that derives `a` takes `a`'s subsumers, the other members of the conjunction
among them. R8 compares the ids of a singular
context's successors with the ids of its predecessors, to find an element that two
clashing roles join to the context. The conjunction's context and `a`'s have the same
Horn successors after their exchange, so one of them is a predecessor of the context by
a role exactly when the other is. The comparison therefore finds `a` on both sides where
it found the conjunction on both, and a contradiction that it sends a predecessor reaches
`a` in either form. The step itself compares the id of the successor it extends with `a`
and adds no link when they are equal, since the context already names `a` by that role.
The set of a context's successors, the `SuccIn` conclusions that name their sender and
the record of the cores R6 has sent are keyed by the id and hold `a` where they held the
conjunction. The order of dominated successors compares ids as well, and a run with
individuals does not use it. When `a` is the context itself, the members go to the
context's own list, each once in a flush, and the activation applies them before it
ends. A context that is not real keeps the
conjunction, since what the conjunction holds is then true of `a` only where the context
has an instance, which a world decides. To let the step apply from the first saturation,
a run with the Horn rules
over an ontology with individuals makes `owl:Thing` and each individual real when it
initializes their contexts, where the rounds between the saturations did so before, and
the rounds keep the facts of universal roles and the keys. A context that becomes real
after R3 or R5 made a conjunction for one of its successors keeps the conjunction, which
is then real and exchanges with its individual as before, so the conjunctions a run makes
depend on the order of delivery while its answer does not. On the four assertion-heavy
files of the development set, whose property assertions between individuals lie under
universal restrictions and ranges of transitive properties, every conjunction but 44 to
116 of 3,098 to 42,249 held an individual and was real, and each repeated its
individual's closure.

**An assertion under a range.** A domain of `r⁻` is a range of `r` in a run with the Horn
rules, so an assertion `r(a, b)` over a role with either had the context of `{b}` with
the range as its successor. That context derived `b` and exchanged with `b` as every
context that holds an individual does. It also took `b`'s Horn successors, so where `b`
is a predecessor that an at-most restriction of a singular context counts, the context
was a second such predecessor and R7 identified it with `b`. Every subsumer of `b` then
went three ways. The index links the restriction of an assertion to `b`'s own context
and tells `b` the range, as [the EL classifier](#the-owl-2-el-classifier) argues for
individuals, so the run makes none of these contexts (item 1025).

**Guarded Horn rules.** When a new predecessor triggers R7, the rule looks for
pairs only if the predecessor is real and the current restriction counts it.
The rule visits the same predecessor pairs in the same order when the run
continues. It checks the run's bounds periodically during the scans and after
a restriction that makes a pair or sends a message. The scan of the roles that
clash is charged before the lookup. Successor counting and singular propagation
read the existing at-most restrictions one at a time.

A Horn link to a successor is covered when the context already holds a link
by a different subrole to that same successor. When the new role has an inverse,
the covering inverse must exist and be a subrole of that inverse. This preserves
what R6, R7 and R8 can read at the target. The retained link matches the
superroles needed by the forward rules, the two operands of a composition,
and clashes inherited under subroles. The link retains the same source and
target for filler and realness propagation. The rule applies the coverage
check to Horn links in runs with individuals while the source already holds
at most sixteen successors. If the broader link arrives first, it stays.
R6 also omits a repeated core at a predecessor that is itself the successor,
whose core was scheduled when that context was initialized.

`SPARKLES_HORN_RULES` selects the comparison level. Level 0 uses the earlier
rule bodies. Level 1 adds guarded R7 and reads restrictions without copying
the list. Level 2 also uses covered links and omits the repeated core. Level
2 is the default.

**Witnesses and worlds.** The reachability condition of the EL rules of individuals stays
the mechanism that makes a conclusion that depends on a class having an instance hold
only where it does. The rules that read whether a context is real are the exchange from
a context to its individual, R7 and the facts of universal roles, and the keys between
the rounds, and every other rule, R8 included, runs the same in the global saturation and
in a world. A world for a class makes the class and what it reaches real, so a class
whose global subsumers are incomplete reaches a context that one of these rules would
read as real. Each such context is a witness. A context that is not real, derives an
individual and has a subsumer, a Horn successor or a loop that the individual lacks is
one, which extends the EL witness by the Horn successors and loops. At level 2
the individual already has a Horn successor when its own retained link covers
it by the rule above. The witness check applies this criterion at any successor
count. The individual may have obtained its subrole link without ever receiving
the broader link. A context that is not real and that an at-most restriction of a
singular context counts as a predecessor is one, since R7 identifies it with another predecessor only where it has an
instance. A context that is not real and holds a fact's restriction on a universal role
is one, as today. The keys read the links of a real context to named individuals and its
data values, which come from its subsumers and its Horn successors, so a context that
derives an individual and is not real adds a key record only when it has a subsumer or a
Horn successor the individual lacks, which makes it a witness of the first kind. The
world's check that no real context is unsatisfiable reads realness too, and an
unsatisfiable context that a class reaches makes the class unsatisfiable by the EL rule
for `owl:Nothing` already. The simplest case of R7 shows the
second kind, since `A` is not real in the global saturation, derives no individual and
reaches no context that does, so without it `A` would get no world and `A ⊑ {b}` would be
missed. The kinds are sufficient, by the list of rules that read realness, and not
necessary, since `A ⊑ ∃r.{a}` with an inverse functional `r` alone makes `A` a witness
whose world finds nothing. A cheaper criterion would need an argument of its own and is
not built, since the worlds cost little on the files measured, as the
[Outcome](#outcome) records for item 868. A run with the Horn rules marks the real contexts whenever the ontology has
individuals, from the first saturation on, where a run of the EL rules does so only after
a merge, a key or a universal role, because R7 reads the real predecessors of the global
state and R3 and R5 read whether their own context is real.

Each named class whose context reaches a witness is classified again in a world, as
before, and the worlds extend to the conjunction contexts. The global table of
conjunctions outlives the saturation, which it does today for the statistics, the views
over the contexts include its contexts, and the witnesses and the classes reaching them
are found over them too. A world has a table of its own for the conjunctions that its
rules create, whose ids lie above the global table's. The world's sink looks a
conjunction up in the global table first, which no world changes, and then in its own,
and reads a conjunction's core from whichever table holds it. A core is a sorted set of
the index's expressions and never names a conjunction context, so a world's table is
bounded as the global one is. The world's copies, its table and the states of its own
contexts count against the world budget beside the saturated state, as the copies do
today, and the Horn budget and its fallback to the EL rules cover the world's bytes and
the global table as they cover the first saturation. A world makes a context real from
the global state without a copy today, and then sends `Real` to its EL successors and its
subsumers to its individuals. In a run with the Horn rules it also sends `Real` to the
Horn successors and the predecessor's realness to every successor, and a context that
the world copies does so through the rules. The world runs the fifth instantiation of the
rules on one thread, keeps only the conclusions the global state lacks, and flushes the
pending work of R3 and R5 at the end of each activation as the saturation does. Its
answer, the class's subsumers and whether it is unsatisfiable, is the same in any order of
delivery, since every rule derives a consequence of the ontology and the fixpoint is the
least one over the conclusions, while the set of conjunction contexts a run makes on the
way can depend on the order, as it does in the global saturation today. The facts of
universal roles and the merges of keys apply in a world between its saturations as
before, and in a run with the Horn rules a key merge exchanges the two individuals'
backward links and loops with their subsumers, through the exchange above, since each
individual's context derives the other and is real.

**Completeness.** The argument is Kazakov's with the identifications below, and this
section states it as an argument and not as a proof. For a named class `D`, take the
world of `D`, in which `D` is real, whether or not the run executes it. When `D` reaches
no witness, the list of rules that read realness says that the world's subsumers of `D`
are the global saturation's, which is what the run answers, so the model below serves
for both. Build a model from the real contexts of `D`'s world as the paper's Theorem 4
does, with the paper's choice of the successor that holds the most and its reuse of the
predecessor where it serves, and with three identifications. `D` has an element in it,
which is what a subsumption the world does not derive needs. Each individual is one
element, which every real context that derives the individual stands for, and these
contexts agree on their subsumers, backward links and loops by the exchange, with their
ids and bookkeeping apart. Each real singular context that derives no individual has one
element, which the contexts R7 identified it with share, and they agree the same way.
Every other real context unravels into trees from these elements. The told axioms hold
at each element by the paper's argument for the axioms without nominals. A nominal `{a}`
holds at `a`'s element alone, and every context that derives it maps there. An at-most
restriction `≤1 S.B` at an element counts its successors, which R5 merged into one, its
predecessor in a tree, which R6 identified with the successors, its loops, and, at a
singular element, every predecessor, all of which are real and which R7 identified
pairwise, so they are one element. Disjoint and asymmetric roles hold on each pair by the
checks of R5, R6, the loops and R8, which together cover every pair that two roles can
share, since a pair shares two roles only through a merge, a loop or a singular element.
A universal restriction holds through R3, R4 and the automata as before, and one on a
universal role holds everywhere through its fact. The data restrictions hold by the EL
rules' argument, and the keys by the rounds. The gate below is evidence for the argument
on the ORE 2015 DL sample and on generated ontologies, and not a proof.

**Termination and cost.** The normalized vocabulary is finite, so a run has at most
exponentially many conjunction cores, each with finitely many subsumers, links, loops,
flags and partners, and the rules add conclusions only, so a fair saturation ends, in a
world as in the global saturation. R7 and the exchange add no symbol, and the conclusions
they move can make R3 and R5 create contexts, which the same bound covers. The number of
worlds is at most the number of named classes, and each can approach a Horn saturation of
its own, so the Horn budget counts them, and the scans of R7 and R8 over a singular
context's predecessors, which are quadratic in their number, poll the run's stop as the
long rules do. `Stats` gains no count of the witnesses and the worlds, since the API
compatibility check guards the struct. An ontology without
individuals, functional data properties or top properties runs the code it ran before,
which the EL identity gate checks byte for byte, and the Horn files without individuals of
the development set are checked the same way. The Horn files with individuals were
measured against the build that left their Horn axioms out, in interleaved rounds on
`atlas`, which the gate paragraph below and the
[benchmark report](../../benchmarks/el-classification/RESULTS.md#horn-individuals) give.

**Gate.** The gate is the Horn stratum of the ORE 2015 DL development set, 73 files, of
which HermiT 1.4.5.519 classifies 68 within 1200 s, and among them the 47 files with
individuals, functional data properties or top properties, whose Horn axioms the
comparison before this item left out, 44 of which have a reference. On four threads the
classifier's taxonomy equals
HermiT's on 34 files and is sound on 33, with no difference against HermiT on any file,
where the build before the item was equal on 23 and sound on 44. Every axiom the 33 files
still leave out is outside the Horn rules' scope: anonymous individuals in 29 files, and
datatypes outside OWL 2 EL or data cardinalities in 4. The reader refuses `ore_ont_9881`
for an invalid IRI in both builds. The 25 Horn files without individuals and the 298 EL
files of the EL identity gate keep their taxonomies byte for byte, on one and eight
threads for the EL files, and the disjunctive rules keep every refusal of individuals,
data properties and top properties. Hand cases cover each rule above, on one and eight
threads, each with HermiT's answer: the simplest case of R7, a cascade of three singular
contexts, a loop that an at-most restriction counts at a nominal, the two examples of R8
and their forms through a context that derives the individual and through two contexts
R7 identified, R5 and R6 with a nominal on either side and with a nominal acquired by a
merge, a universal restriction on the top property and on its inverse held by a real
context and by one that is not, a functional data property beside a universal
restriction, a key beside an inverse functional property, a Horn class assertion, a
conjunction context that a world creates, an at-least restriction of two with a nominal
filler, and a world whose first new conclusion is `Real`. Three unit tests deliver the
conclusions of one context in each order a saturation can produce them, for a Horn
successor made after its source became real, a member registering after its individual
has loops and Horn successors, and the filler of a counted loop arriving last. A
differential run of 300 generated Horn ontologies with nominals, inverse roles,
functionality, universals, self restrictions and disjoint and asymmetric roles, written by
the dl603 fixture generator with a family of its own and seed 692, equals HermiT's
taxonomy on every file on one and eight threads, where the build before the item left
axioms out on 290 of them. The `dl_*` suites and the `sparkles-owl` library tests pass.
The [record](../../benchmarks/el-classification/runs/2026-11-10-horn-individuals-atlas-0f944b5c/record.json)
holds the judgements, the exclusions, the generator and the timings, which the
[benchmark report](../../benchmarks/el-classification/RESULTS.md#horn-individuals)
discusses.

**Out of scope.** Enumerations of several individuals in a positive position need case
analysis and stay with the rooted run. Chains through the top property stay in item 663. A cheaper
witness criterion is not built. Item 1281 later took anonymous individuals, as
[Anonymous individuals and top-property inclusions](#anonymous-individuals-and-top-property-inclusions)
describes. Data cardinalities
and keys over data properties beside the disjunctive rules stay with item 768, counting
with item 728 and incremental runs after a change with item 753.

### The disjunctive rules

Item 603 classifies ontologies whose axioms outside OWL 2 EL need case analysis with the
consequence-based calculus of Bate, Motik, Cuenca Grau, Tena Cucala, Simančík and
Horrocks (*Consequence-Based Reasoning for Description Logics with Disjunctions and Number
Restrictions*, JAIR 2018), written from the paper over the EL classifier's contexts. It
covers ALCHI, which adds unions, complements and universal restrictions in every position
to the EL classifier's language, with inverse properties and property hierarchies.
Transitive properties and chains are taken through the automata of Horrocks, Kutz and
Sattler, which unfold the universal restrictions, negative existential restrictions and
ranges on the properties they reach, after the reductions that
[reduced automata of the disjunctive rules](#reduced-automata-of-the-disjunctive-rules)
describes. A run that does not count reads an at-least
restriction in a positive position and an at-most restriction in a negative one as the
existential restriction they imply, which keeps ALCHI's models. The rules run when an
axiom needs case analysis and the ontology has no individuals, data properties, self
restrictions, reflexive properties or top and bottom properties. Otherwise the Horn rules
take what they can, and the axioms that need case analysis stay left out and reported, as
do nominals and property hierarchies that are not regular. Data properties wait on a
datatype checker with data cardinalities, item 768. `Stats::tier` is `Disjunctive` for such a run, and
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

`Limits::partial` makes a classification that its deadline, its cancellation or its memory
budget stops after saturation begins answer with what it proved instead of failing. The
flag was introduced for the disjunctive rules and is now shared across the EL, Horn and
disjunctive tiers (item 714; see the Outcome subsection below and
[the reasoning guide](../../reasoning/README.md) for the EL and Horn phase-by-phase
recording and memory-admission details). The answer's `Classification::complete` is false,
`incomplete` lists the classes whose subsumers it may miss, and `consistency` is
`Consistent`, `Inconsistent` or `Unknown`. A stopped worker gives back the conclusions it
did not apply and delivers the messages it holds, and an inference the stop cut off marks
its context, so that every context with work left shows as open. In the disjunctive rules,
a class is complete when every context it reaches is closed, none of its candidates is left
untested and the ontology is consistent; consistency is decided when `owl:Thing`'s context
is unsatisfiable, or when it and every context it reaches are closed, and when it is unknown
no class is complete. The taxonomy of a partial answer is built from its proved subsumptions
closed transitively, after non-taxonomy saturation state is freed. A partial answer that
still exceeds the memory budget fails with the memory error without downgrading to a weaker
tier, and the next run starts again. A class whose context the stop came before is open too,
and so is the consistency while `owl:Thing` has no context.

Phase 7's surfaces give the partial answer to every caller of a classification.
`ClassifyRequest::partial` and `ClassifyRequest::expansion` carry the flag and the
strategy, and the JSON operation, `POST /$/ontology/{ds}/classify`, the Python, Node.js
and JVM bindings and `sparkles ontology classify --partial --expansion` take them. Every
answer gives `tier` and `consistency`, and a partial one has `partial` true, `complete`
false and the open classes under `incomplete` with `incompleteCount`. The `complete` of
the JSON and of `Classified::complete` stays true only for a run that finished and reported
no axiom left out. A rooted run can miss entailments with the flag true, which item 1231
records, and a run that counts rests on item 1229's repair, which is not proved. The dataset keeps no partial answer. A request that writes its taxonomy,
asks for the functional-style answer or runs a ROBOT-compatible command refuses or ignores
the flag, since none of them can say that a taxonomy is partial. The command line takes
`--timeout`, notes a partial answer on stderr and exits with status 2 for one that is
otherwise clean. Each surface's tests classify a generated ALCHI ontology under deadlines
from a fiftieth of a full run's time upwards, and check that a partial answer is never
complete, that it lists its open classes and that its consistency agrees with the full run
or is unknown, which an inconsistent ontology gives when stopped early. Item 719 extends
the Workbench with deadline, partial-answer and expansion controls and displays partial
hierarchies, open classes and unknown consistency, as [Outcome](#outcome) records.

A run of the disjunctive rules has the Horn rules' memory budget, 64 times the estimated
bytes of its index and axioms, at least 1 GiB, and no more than the caller's. The clause
stores and the table of clauses that contexts send each other count against it, and the
case analysis of one inference derives its choices one at a time and checks the bounds as
it goes. A run without the partial flag that passes the budget classifies again with the
Horn rules, leaves the axioms that need case analysis out and reports them, and sets
`Stats::dl_over_budget`. A run that neither counts nor is rooted also has a bound on the
conclusions it derives, `Limits::dl_attempt_work`, of 384 for each class of the signature
and at least 2,097,152, past which it falls back in the same way and sets
`Stats::dl_attempt_over` too (item 990). On one thread such a run also falls back when one
product of its rules has more than `Limits::dl_attempt_width` choices (item 1011). ore_ont_3215 of the ORE 2015 DL sample, with
18,323 unions over a transitive property, needs this, item 713.

Counting is part of the same rules and is off by default. `Limits::counting` turns it on,
and `el_classify --counting` does the same. A run that does not count leaves the axioms
that need counting out and reports them, and takes a minimum as the existential
restriction it implies, so it costs what the classifier cost before counting. The reason
is the cost on real ontologies. Five files of the ORE 2015 DL development set attempt
counting. Without the order between number restrictions none of them finishes, and with
it the two versions of the Protein Ontology finish, as [its subsection](#the-order-between-number-restrictions)
records. The other three spend their attempt before they give up where the run without
counting finishes. Item 728 replaced the clauses of every context and the hypothesis about
the predecessor, as [its subsection](#item-728s-clauses-for-counting-over-definitions)
records, item 894 added the order, item 897 gave compound qualifiers their names, and
turning counting on is the work of item 896 for the cost that remains. A run that counts when asked takes a number restriction that the Horn rules
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
as before. Any other run that neither counts nor is rooted has the bound of item 990,
`Limits::dl_attempt_work`, past which the Horn rules answer without a repeat on one thread
or a run without counting, as [Outcome](#outcome) describes. Of the 150 counting fixtures, all but four derive at most 21,747 conclusions on
one thread. `sriq-004` derives 369,367, `shiq-038` 815,758 and `shiq-046` 1.1 million, and
these need a raised `Limits::dl_work`. `shiq-028` passes any bound, which item 729
follows. `shq-041` derives 17,844 conclusions with the order between number restrictions
and 71,345 without it.

A context that has processed 256 of its clauses in one activation gives way to the
contexts that its worker woke. The count includes the minimums whose witnesses the
context released. It goes behind them in the worker's queue with its other
clauses left in its store, and its next activation takes them up. Before it gives way it
sends the products of the links and propagations that the activation stored, which a
context otherwise sends once its to-do list is empty, since its next activation pairs
only what that activation stores. A generated ontology in which the clause that gives a
context a class is the last of its share shows the need. Its context stored the
propagation of `owl:Nothing` over a role and gave way, and 375 of 511 unsatisfiable
classes were satisfiable on one thread. Without this step a
context on one thread derives everything it can before any other context runs, and what
the others conclude can make most of that work unnecessary. The generated ontology
`qz-0048` of item 897 shows it. Its class `C1` has six successors under a maximum of
three, and the successors have no model. The context of `C1` equated its successors in
2,122,545 conclusions on one thread before the successors' contexts ran, where three
runs on four threads derived between 2,152 and 26,775 because the successors'
contradiction arrived early.
With the step the run on one thread derives 29,625. Of the 750 ontologies of the counting
fixtures and the two fuzz sets, 25 derive a different number of conclusions on one thread
with the step, and 18 of them derive fewer. Seven come within the default bound, among
them `qz-0048`, and none leaves it. `shiq-013` derives 15,468 conclusions where it derived
102,896. The fixture `shq-041` without the order between number restrictions passed a
bound of 8 million conclusions on one thread, and it finishes in 71,345 with HermiT's
answer. One ontology of item 894's fuzz that derived 5.4 million conclusions passes a
bound of 8 million with the step, and it passes the default bound either way. Shares of
64 and of 256 clauses measure alike, a share of 1,024 loses the seven, and shares of 16
and of one clause move ontologies across the bound in both directions. The step does not
make the bound deterministic, which the steps of item 727 do.

A run without those steps, which `dl_steps: false` asks for, has a limit that this
section states plainly. Near the bound, the thread schedule can decide whether the run
answers in full or falls back, since the clauses stored at the fixpoint vary with the
schedule, for example 3,229 to 6,762 on `sriq-010` at four threads against 3,253 on one.
A fallback is always reported through the statistics and the axioms left out and is
never a different complete answer, so a caller learns from `dl_counting_over` whether
counting finished. `Limits::dl_steps`, which is on by default, removes the dependence. A
run with the bound then
[proceeds in steps](#steps-for-a-bound-that-every-schedule-shares) and derives the same
conclusions on every number of threads.

The generated `shq`, `shiq` and `sriq` fixtures equal HermiT 1.4.5.519 on every case that
Tier D takes whole with counting on, on one and four threads and under both strategies. Of 50 cases each,
Tier D takes whole 49 of `shq`, 48 of `shiq` and 16 of `sriq`, where the others name
individuals or pass the bound. Random tests of counting ontologies check that minimums of
two answer as the existential restrictions they imply and that a functional property
answers as a maximum of one on every class. No development file of the ORE 2015 DL
sample is taken whole with counting. Its counting files fall back when counting is on,
ore_ont_6485 after 205 ms on one thread against 7 ms without counting, and ore_ont_6952
and ore_ont_10140 after 1.4 and 1.7 s against 0.6 and 0.5 s. Bounds of 1, 4 and 16
million conclusions do not finish ore_ont_6485 either, and the last stops after 63 s. Its
20 clauses `⊤ → y ⊔ ≤(n-1) r.F ⊔ ≥(n+1) r.F`, one for each exact cardinality in a
definition, are copied into all 156 of its contexts, which hold 173,476 clauses at the bound of 262,144
conclusions. The copies are 3,120 of those clauses, and 12 contexts hold 6,190 to 7,199
clauses each, so sharing the copies would not end the run. With the definitions absorbed,
as in the experiment that PERFORMANCE-EXPERIMENTS.md records, the run stores 25,818
clauses at the stop, and the hypothetical successor of `∃s.HeteroAtom` holds 10,295 of
them. Their bodies combine the markers that each predecessor sends, such as
`∃r1.H` and `∃r1.Alkyl`, and their heads are disjunctions of `Y` literals for the cases of
the HeteroAtom union together with the qualified-role and successor literals of the at-most
clauses. The symmetric bond properties make those clauses count the predecessor too, item
731. The cost is not counting alone. A copy of ore_ont_6485 that replaces every exact and
minimum cardinality `=n r.F` and `≥n r.F` by `∃r.F`, with `owl:Thing` for an unqualified
filler, does not finish in 120 s on one thread on the build before counting, and takes
105 s with the premise index and the Pred check below. A copy that replaces only the exact
cardinalities classifies in 44 to 50 ms but leaves its four minimums of two out. Item 728
chose among the remedies, as its subsection below records, and item 894 holds the
calculus change that remains.
The normalization of a clause with a universal or a maximum
beside a class or an existential restriction keeps the class in the clause's body, which
classifies BioTop in 56 ms on one thread and 27 ms on four against 940 and 350 ms before.
Each clause store then indexes its processed clauses by their maximal literal over a
successor term, so that the premises of one such literal are found without scanning the
others of its term, and Pred checks that a new premise fits a place of a predecessor clause
before it gathers the clause's other premises. The inferences are the same, and in
interleaved runs on the laptop BioTop classifies in 0.59 of the time of the build before
on one thread and 0.44 on four, 44 and 23 ms, about Konclude's 23 ms, item 712. The
index holds one more id for each processed clause whose maximal literal is over a successor
term, which raises the peak heap of a one-thread run by 9.6% on BioTop, from 17.7 to 19.4
MiB, and by 3.3 to 3.5% on ore_ont_5438, ore_ont_13103 and ore_ont_16680. The speed costs
that memory, and the estimate counts it.

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
classifies in Konclude's time on four threads, item 712. The taxonomies of the 298 ORE 2015 EL
ontologies, GO, ChEBI lite and every development file that the disjunctive rules do not
take are byte for byte those of the Horn tier on one and eight threads. The saturation's
arms for the disjunctive rules compile away in other runs. They first made the one worker's
activation too large to inline, and GO and ChEBI lite classified 0.9% and 1.4% slower on
one thread on the benchmark host. Each rule set now has a worker of its own that inlines
its activation, and with `sparkles-owl` in one codegen unit GO classifies on one thread in
0.99 of the time of the build before and ChEBI lite in 0.98, with the same taxonomies, as
the [code layout results](../../benchmarks/el-classification/RESULTS.md#code-layout) give.

#### Item 728's clauses for counting over definitions

This subsection is the design of item 728, written before its code. It decides how a run
that counts, without the root of a rooted run, takes an exact or minimum cardinality in a
negative position, which is the form the right-to-left direction of a definition such as
`AcylGroup ≡ CarbonAtom ⊓ =1 hasDoubleBondWith.OxygenAtom ⊓ =2 hasSingleBondWith.OrganicGroup`
gives, and where the witnesses of a minimum saturate, so that ore_ont_6485, ore_ont_6952
and ore_ont_10140 of the ORE 2015 DL sample classify with counting on. The three files are
in the `dl-q` stratum. The first has 15 definitions of this shape over the symmetric bond
properties, with the union fillers `HeteroAtom`, `HalogenAtom` and `CarbonGroup` and
pairwise disjoint atom classes. The other two are versions of the Protein Ontology with
1,014 and 1,020 cardinalities in 30,363 definitions such as
`PR_000024883 ≡ =3 has_component.MOD_00041 ⊓ PR_000024871` over 83,036 classes, six
transitive properties, 26 unions and no universal restriction. Rooted runs keep their
order, their rules and their witnesses as the section on nominals describes, and nothing
here changes them.

The normalization before this item follows the paper's Table 1 after the structural
transformation. `=n r.F ⊑ y` becomes the clause of every context `⊤ → y ⊔ M ⊔ H`, where
`M` is a fresh class that holds `≤(n-1) r.F` through the at-most clause DL4,
`M(x) ∧ r_B(x, z_1) ∧ … ∧ r_B(x, z_n) → ⋁ z_i ≈ z_j`, with `r_B` the qualified role of
`r` and the negative form `B` of `F`, and `H` is the conjunction of `n+1` existential
restrictions with fresh pairwise disjoint fillers below `F`, which is the paper's DL2 with
the inequalities read as disjointness. For `n = 1` the maximum is the universal
restriction `∀r.N` in place of `M`, where `N` is the fresh complement class of `F`,
disjoint with `B`. A minimum `≥n r.F` in a positive position makes `n` witnesses in the
same way, `∃r.b_i` with fresh `b_i ⊑ F` pairwise disjoint, and each witness has the
context of its own filler `b_i` as its successor, since the index gives an existential
restriction its filler's context. The paper names this translation as the source of its
own reasoner's difficulty with number restrictions, where each application of an at-most
clause to the clauses that define the neighbours gives a complex clause that gives rise
to numerous others, and it writes that the problem seems inherent in transforming number
restrictions as Table 1 shows.

The traces of the counting attempts, at the bound of 262,144 conclusions, show two
different costs. On ore_ont_6485 the attempt derives 281,019 conclusions in 137 contexts,
90,522 of the universal rule over successors, 38,264 of the at-most rule, 32,118 of the
unfolding, 30,341 of the qualified-role clauses, 26,229 of the disjointness certificates,
21,808 of the universal rule over the predecessor and 17,089 paramodulations, and every
context begins with the 20 body-less clauses. The form costs in three ways there. The
clause has an empty body, so Hyper derives it in every context. For `n = 1` the disjunct
`∀r.N` sends the marker `N` to every `r`-successor of the context and, since the bond
properties are symmetric, derives `N` of the predecessor. `H` is an expression, which the
order of atoms ranks above every fresh and named class, so it is the maximal literal of
the derived clause and Succ makes its hypothetical successors in every context. Those
successors share one context per fresh existential restriction and gather the markers of
every predecessor, each counts its predecessor over the symmetric role, and the equalities
of the at-most clauses rewrite the literals of a known successor onto a hypothetical one,
which sends them down the chain as markers. Absorbing the definition's named conjuncts
into the body of the clause, the experiment that PERFORMANCE-EXPERIMENTS.md records, cut
the stored clauses at the bound from 285,856 to 56,635 and still passed the bound, because
the named guard holds in every context of a carbon atom and the clause still spawned `H`
and sent `N` from each. On ore_ont_6952 the attempt derives 280,093 conclusions in 4,870
contexts against 214 without counting, 156,882 of the unfolding of the transitive
properties and 107,536 of Pred, and only two body-less clauses fire in a context. The
extra contexts are the witnesses, 1,575 of the minimums in a positive position and 2,589 of
the `H` disjuncts, and each repeats the unfolding of its filler's transitive properties in
a context of its own and sends the clauses back through Pred.

The decision has three parts, the clause, the rank of `H` and the context of a witness.
All three are choices the paper's calculus leaves open, and none departs from it.

The first part writes the at-most disjunct of such a clause as the paper's at-most clause
itself, with the neighbours in the body and no holder. With counting, an exact
cardinality `=n r.F` in a negative position with `n ≥ 1` becomes the DL-clause

`r_B(x, z_1) ∧ … ∧ r_B(x, z_n) → y ⊔ H ⊔ ⋁_{i<j} z_i ≈ z_j`

and a minimum `≥n r.F` in a negative position that stands alone, with `n ≥ 2`, becomes

`r_B(x, z_1) ∧ … ∧ r_B(x, z_n) → y ⊔ ⋁_{i<j} z_i ≈ z_j`,

where `B`, `r_B` and `H` are as above and the `z_i` range over distinct neighbour terms,
the successor terms and the predecessor. Distinct variables do not assert distinct
elements, since the equalities in the head cover two variables that name one neighbour,
and an instance that repeats a term is a tautology, so the rule enumerates the subsets of
distinct terms. Both clauses are DL-clauses of the paper's Section 2.4, whose bodies may
hold atoms `S(x, z_j)` over neighbour variables and whose heads may hold any DL-literal,
with each `z_j` in the body, and the paper states that DL-clauses outside Table 1 are
handled correctly by its algorithm. For `n ≥ 2` the clause is the resolvent on `M` of the
two clauses it replaces, and `M` occurs in no other clause, so the two sets have the same
models over the ontology's signature. A model of the new clause satisfies the old pair
when `M` is read as `¬y ⊓ ¬H`, since where `M` holds with `n` qualified neighbours the new
clause gives `y`, `H` or an equality, and the first two are false. For `n = 1` there is
no holder to resolve on, and the clause is `r_B(x, z) → y ⊔ H`, which replaces
`⊤ → y ⊔ ∀r.N ⊔ H` together with `N ⊓ B ⊑ ⊥`. Where `y` and `H` are false the old pair
puts every `r`-neighbour in `N` and so outside `B`, and the new clause forbids every
`r`-neighbour in `B`, which is the same constraint, and where one of them holds both are
satisfied, so the two forms agree on every model of the normalized signature once `N` is
read as `¬B`, with `B` and whatever constraints it has kept as they are. Each form relates
to the original axiom through the structural transformation as the old one did, whose
models extend to the fresh names and restrict back to the original signature. The change
for `n = 1` is therefore a change of proof shape and not of meaning. The old form derives `N` of each successor and of the predecessor as markers,
and the new form waits for a neighbour's own context to derive that it is in `B`, which
the qualified-role clause of DL4 turns into `r_B` and Pred carries back. Soundness is the
paper's Theorem 8 and completeness its Theorem 9, which hold for any set of DL-clauses,
any expansion strategy and any context term order in the sense of its Definition 3, for
a query whose conditions C2 and C3 hold. The runs of this spec test subsumption through
refutation, in a context whose core holds the class and the candidate's complement and
whose query has an empty head, so C2 is vacuous and C3 is the Core rule. The rules that
apply the clause exist, since the at-most clauses of the holders already take their
candidates from the qualified-role literals over the successor terms and over the
predecessor, make one product per subset of `n` candidates under the bounds on the widest
clause, the subsets and the payload, and cut an equality that a disjointness certificate
contradicts. The new clause is such a product with an empty holder and with `y` and `H`
added to its head. It is derived whenever a new premise of a qualified-role literal over a
successor term arrives, with that premise fixed in the product, as the holders' clauses
are now, so that a term first counted under a conditional premise is counted again under
a later unconditional one, and for `n = 1` also once when a context starts, for the
predecessor alone.

The measurement revised the `n = 1` case. The clause `r_B(x, y) → y ⊔ H` with the
predecessor as its one neighbour is a hypothesis about the predecessor's type, and the
engine's hypotheses are the paper's markers `A → A`, which Pred discharges in the
predecessor, so every context that carries the inverse of the role speculates on it, and
with the symmetric bond properties of ore_ont_6485 that is every atom context for every
one of the fifteen definitions. Under the present rank `H` is maximal there and Succ
makes its witnesses everywhere, which a functional holder then equates with the
predecessor and with each other, and under the low rank `y` is resolved into the
definition's class wherever its guard holds, whose own holder then counts every
hypothetical witness in the context. Both attempts passed the bound, and the traces are
in the lane record. The `n = 1` case therefore takes the form `∃r.F ⊑ y ⊔ H`, which is
`=1 r.F ⊑ y` written as `⊤ ⊑ y ⊔ ∀r.¬F ⊔ H` with the universal restriction moved to the
left, and an existential restriction in a negative position as any other of the
normalization. A context derives `y ⊔ H` once it knows a neighbour in `F`, a successor
through the successor's own context and Pred, or the predecessor through the universal
rule over the predecessor, which derives `∃r.B(y)` in the neighbour's context from `B`
and the edge and sends it back, and never from a hypothesis. The two regressions of the
acceptance cover the predecessor as the only neighbour and beside a successor. The lazy
clause with the neighbours in the body remains the form for `n ≥ 2`, where it fires only
with `n` qualified neighbours, the predecessor among them.

The predecessor is a candidate neighbour whenever the context carries the inverse of the
qualified role, as a premise `r_B(x, y)` in the body of the product and not as a fact, so
the new clause for `n = 1` is derived with the body `r_B(x, y)` in every context that
carries the inverse, which with symmetric roles is every context the role reaches. That
clause does not go to the predecessor, since a clause goes there only when it has no
maximal literal of its own, every head literal being over the predecessor, and this head
is over `x`, and it has a body, so it never takes part in the canonical model. What it can
do is give its maximal literal to the rules, and under the present rank that literal is
`H`, so Succ would make `H`'s hypothetical successors from a conditional body in every such
context, and the first part alone would not stop the expansion of `H` on the symmetric
bond properties. The second part of the decision therefore ranks `H` below every fresh
class and every named class, as the lowest rank of the atoms over `x` in the order of a
run without the root, for the at-least conjunction of every clause that a cardinality in
a negative position makes, the exact cardinality's, the minimum's and the maximum's
`≥(n+1) r.F`. The paper's Definition 3 leaves the order of the atoms over `x` free, and
its Theorem 9 holds for every order the definition admits. The rooted order keeps the
named classes minimal and incomparable for its own completeness argument, and the second
part does not touch it. Under the new rank the derived clause `Γ → y ⊔ H ⊔ E` with `E` the
equalities first resolves its equalities, which rank above every atom over `x`, through
Eq, Ineq and Factor with the certificates, and then has `y` as its maximal literal. Hyper
takes `y` into the definition's clause `G ∧ y ∧ … → A`, whose conclusion has the named
class `A` and then `A`'s told subsumers as its maximal literals, and `H` becomes maximal
only once every literal above it is resolved away. The rank delays the expansion of `H`
and does not confine it. A class context resolves the literals above `H` away when it
holds a complement, as a context of `X ⊑ ¬A` does for `→ A ⊔ H` through the disjointness
of `A` and its complement, or a universal restriction or a negative existential
restriction that makes a case false, and then expands `H` itself, and a clause whose head
still holds a literal over a successor term or an equality above `y` keeps that literal as
its maximum. Where nothing resolves them, the clause `→ y ⊔ H` with an empty body enters
the canonical model of the context, which is built from such clauses in the order of the
literals, and makes `y` true when no other atom of its head already holds, so the
definition's class becomes true when its other conjuncts hold, and a candidate of the
context. The test context of that candidate holds its complement, resolves the class
away, and is left with `Γ → H ⊔ …` where `H` is maximal, so Succ makes `H`'s successors
there, the holder's at-most clause equates them with the known neighbours, the
inequalities of DL2 refute the equalities, and the test derives `⊥` when the count is
exact and stays satisfiable when it is not. This is the order in which the hypertableau
explores the disjunction, where the at-least restriction is expanded only when the other
disjuncts fail. The expectation is that most expansions of `H` move from the contexts that
have the neighbours to the tests, and the measurements record how many conditional
predecessor clauses each rank derives and how many hypothetical successors they make. A
candidate adds a test and never a subsumption, so the rank carries no soundness risk, and
the completeness obligation is the one the runs already meet, that candidates come from a
model of the context and that no required test is missed. The present rank is measured
beside the new one as the control.

The measurement of the rank. The low rank stays behind `SPARKLES_DL_LOW_H=1`, off by
default. On the fixture sriq-010 it diverges where the present rank finishes, because the
ontology's own clause `⊤ → ∃r1⁻.¬(C2 ⊔ C5) ⊔ ∀r0.¬C0 ⊔ ≥2 r1.C1` shares a clause with `H`
after Hyper, and once `H` ranks lowest the `≥2 r1.C1` witnesses are the maximal literal
in its place, Succ makes two more successor terms, and the functional role equates them
with the other neighbours, which multiplies the at-most products and the paramodulations
by about ten in one context. The rank moves the exploration onto the other disjuncts of
such a clause rather than delaying it. On the three files of this item both ranks pass
the bound, with the clauses of the first part and with the `n = 1` revision above, and
the products of one shared hypothetical successor context are the cost that remains.
That context, the hypothetical successor of `∃hasSingleBondWith.HeteroAtom`, is
registered by fourteen predecessors, holds the markers of all of them, and resolves the
union cases of `HeteroAtom` and `HalogenAtom` against every disjoint marker and the
predecessor-direction existential restrictions into tens of thousands of distinct clauses
over the predecessor, whose bodies combine the role atoms of different predecessors. Their
Pred products in one class context, with the at-most products and paramodulations they
feed, pass three million conclusions. A product now derives each distinct conclusion once,
since two choices whose premises share their literals merged to one conclusion and each
counted toward the bound, and that alone removed two thirds of the Pred conclusions of
that context. The product's record of the conclusions it has derived lives until the
product ends and is charged to the run's memory budget as it grows, so a wide product
whose conclusions are all redundant still stops the run at its budget. The remedy this leaves is the one named below, a hypothetical successor
keyed by the predecessor's restriction, so that no context combines the markers of
different predecessors.

The measurement of the keyed successor. The keyed successor was built and measured and
is not kept, and PERFORMANCE-EXPERIMENTS.md records it. A run that counts, without the
root, sent a conditional existential restriction to a context made during the run and
keyed by the predecessor's own existential restriction, when the predecessor is a
hypothetical successor, and by the predecessor's context otherwise, against the shared
context as the control. On ore_ont_6952 and ore_ont_10140 the two choices take the same
time and both pass the bound, since their cost is the witnesses' unfolding and not the
shared markers. On ore_ont_6485 both choices pass the default bound and the bound of four
million conclusions. With the keyed successor the
cost moves out of the hypothetical successors into one EL successor context, the
successor of a witness `∃r.b_i` whose fresh filler lies below a union, which derives 1.9
million paramodulations and 1.7 million lazy products of the four million. Its clauses
hold the union's cases, each case's existential restrictions as conditional successor
terms, eighteen of them, the lazy clauses equate the conditional qualified successors
under the cases, and each equality, maximal in its clause since the terms rank above the
atoms over x, rewrites every stored clause whose maximal literal is over the larger term,
and the rewritten qualified-role literals feed the products again. Without the keyed
successor the same four million spread over three class contexts. The keyed successor
removes the combination of predecessors' markers and leaves this cost, which is the
equality reasoning over conditional successors that the paper's Section 7 names as
inherent in its translation of number restrictions, so it is not a remedy for this file.

The third part gives the witnesses of one filler one context. A minimum `≥n r.F` in a
positive position keeps its `n` witnesses `∃r.b_i` with fresh fillers `b_i ⊑ F` pairwise
disjoint, since the terms must differ and the disjointness is what certifies that they
do, but each witness takes the context of `∃r.F`'s successor, the filler `F` with the
role's range, as its successor in place of a context of its own. The facts of a witness's
term and the core of its successor context are two different things, and the design
keeps them apart. The facts of the term `f_i(x)` are `b_i`, `F` and the role's range, and
the predecessor holds them in both paths that make a successor. For a unit witness it
holds them as unit clauses `⊤ → b_i(f_i(x))`, `⊤ → F(f_i(x))` and `⊤ → range(f_i(x))`,
which are DL2's `B_1 → B_2(f_i(x))` with `b_i ⊑ F` and the range unfolded, and for a
witness of `H` it holds them under the clause's body and beside its other head literals,
as `Γ → Δ ⊔ b_i(f_i(x))` and so on, since the unfolding of a conditional existential
restriction must give the term's own facts and not only the shared successor's. The
disjointness certificates read `b_i(f_i(x))` and `b_j(f_j(x))` from those clauses, unit
or conditional, so a clause `Γ → H` whose two witnesses of a maximum of one are equated
still derives `Γ → ⊥`. In both paths `b_i` goes to the shared context as a marker, which
the unit path must send as well, since making an EL successor sends a backward link and
no marker today. The selection is the paper's Definition 7 applied as its cautious
strategy applies it to DL2, whose witnesses all have `B_2(f_i(x))` as their one certain
atom and all take the context `v_{B_2}`, and the fresh filler changes only the name of the
term. The definition's condition is on the set `K_1` of the Succ rule, the successor
triggers that the predecessor holds as unit clauses over `f_i(x)`, and on the chosen
context's core being equal to the chosen subset of `K_1`. For a unit witness `K_1` holds
`b_i`, `F`, the range and the components of the flattened conjunction of `F` and the
range, since the predecessor holds each as a unit clause, and the chosen context's core
is exactly `F` with the range, the core the index gives `∃r.F`'s successor, so the
condition holds by the predecessor's premises and not by entailment. For a witness of `H`
the chosen context has the empty core, which is a subset of whatever `K_1` holds, and a
witness that later becomes a unit, through unit premises of its own, takes the unit
contract above from then on. The
marker `b_i` has no rule of its own beyond `b_i ⊑ F`, which the core satisfies, and its
disjointness with its siblings, which gives the clause `b_i ∧ b_j → ⊥` in the shared
context and nothing else. The witnesses of `H` are conditional, so nothing is certain of them and
their core is empty, and the hypothetical successors of all the witnesses over one role
and one filler share one context in place of one per fresh filler, which the definition
allows since an empty core is a subset of anything. Sharing is measured against one
context per witness, and the measured one is kept. The sharing follows what the trace of
ore_ont_6952 shows, that the witnesses repeat their filler's unfolding 4,164 times, and it
leaves each predecessor its copy of the shared context's clauses over the predecessor for
each witness term, which Pred makes once per term as DL2 requires.

Counting meets a union filler through the qualified roles, and only through clauses that
Pred can carry. The atom `r_B(x, z)` for a neighbour `z` is derived in the neighbour's
context from DL4's clause `r(y, x) ∧ B(x) → r_B(y, x)` and reaches `x` through Pred, so a
neighbour counts toward `=n r.F` only when its own context has derived that it is in `B`,
and the qualified-role clauses apply for every qualified role of a filler the neighbour
derives, whatever role the edge from the predecessor has. When the neighbour's filler is
a union, as `HeteroAtom → HalogenAtom ⊔ NitrogenAtom ⊔ OxygenAtom ⊔ SulfurAtom`, Hyper
resolves each case that has a qualified role in use into that role's atom over the
predecessor, and a conclusion goes to the predecessor once it has no literal of its own
left, as a disjunction of qualified-role atoms such as `r_B(y, x) ⊔ r_C(y, x)` or of
qualified-role atoms with other atoms over the predecessor. A case that is still an atom
over the neighbour's own element blocks that clause, and the case is resolved by the
neighbour's further inferences, Hyper, a disjointness, a universal restriction or a
negative existential restriction, which can remove it or turn it into an atom over the
predecessor, or stays and keeps the clause in the neighbour's context, where its canonical
model settles it. In ore_ont_6485 the qualified roles are those of the fillers of the
at-most restrictions, `OxygenAtom` over the double bond and `BromineAtom`,
`HalogenAtom`, `OrganicGroup`, `HydrogenAtom`, `Alkyl`, `CarbonAtom` and `CarbonGroup`
over the single bond, so a hetero atom gives a qualified-role atom under the halogen case
and under the oxygen case over the double bond, and the nitrogen and sulfur cases have
none, so a clause that holds them stays until something resolves them. The case analysis
therefore runs in the neighbour's context and, when a clause crosses, in the predecessor's
products, and not in every context of the ontology. One application of the at-most rule
is bounded in the number of its subsets, the width of its clause and the literals of its
products, and each subset's conclusions are the product of the premise lists of its
candidates, so a neighbour with several conditional premises multiplies them, which the
bound on derived conclusions covers as it does now. A disjointness certificate cuts an
equality only when the certificate's body and head fit the conclusion, so two cases known
to be disjoint cut the equality of two neighbours only once each neighbour's case is in the
clause. In ore_ont_6485 every pair of atom classes is disjoint, so fillers of different
cases are cut once their cases are derived.

The paths of the normalization that the decision leaves as they are. A minimum
`≥n r.F` beside named conjuncts in a negative position moves to the head as the holder of
`≤(n-1) r.F` with the named conjuncts as the body, as `dual` does now, so the four nested
minimums of two in ore_ont_6485, as in
`SulfurAtom ⊓ ∃d.OxygenAtom ⊓ ∃s.HeteroAtom ⊓ ≥2 d.OxygenAtom ⊑ y'`, give
`SulfurAtom ∧ ∃d.OxygenAtom ∧ ∃s.HeteroAtom → M ⊔ y'`, whose holder fires its at-most
clause only with two qualified neighbours and so is already lazy. A maximum `≤n r.F` in a
negative position keeps `⊤ → y ⊔ H` with `H` for `≥(n+1) r.F`, which the second part
ranks lowest as well, so that the clause makes `y` true in the model and expands `H`
where `y` is resolved away. Without counting a minimum in a negative position still reads
as the existential restriction. The eager strategy is not the remedy, since the
hypothetical successors of `H` have no certain core beyond the filler and the range, so
the strategy chooses the same shared context, which its measurement past the bound
showed.

The bounds and the fallback are unchanged. A run that passes the bound on derived
conclusions, the widest at-most clause, its subsets or its payload starts again as the
section above says, on one thread and then without counting, and reports it through
`Stats::dl_counting_over`. Every inference of the design is an inference of the paper's
calculus on DL-clauses, so a run that finishes is complete, and a run that does not finish
falls back and is sound. What the decision removes from the trace of ore_ont_6485 is the
body-less clauses, their holder premises in the products, and the universal rule's
propagation of `N`, and from the trace of ore_ont_6952 the repeated unfolding of the
witnesses' fillers, and what it leaves is the qualified-role clauses, the resolution of
the unions, the at-most products, the equality reasoning, Pred's copies per witness term
and the expansion of `H` where a test or a context with a maximal `H` asks for it. The
share of the universal rules in the first trace is not a predicted saving, since those
rules also serve the universal restrictions the ontology states. If the measurements show
the hypothetical successors of `H` still dominate, the next remedy is a hypothetical
successor keyed by the pair of the predecessor's own existential restriction, or its
class, and the successor's existential restriction, in place of one shared by every
predecessor of that restriction, which the paper's Definition 7 allows since a strategy
may return a fresh context for each application of Succ, and whose contexts are bounded
by the finite set of such pairs, with its context count and memory measured. A filter that
would derive the new clause only in a context whose clauses mention an at-most holder over
a role above `r` is not a remedy this design admits, since the argument that a model with
`n` successors and no such holder extends to one with `n+1` by a copy of a successor's
subtree fails with inverse roles, where a filler with an at-most restriction over an
inverse role forces every such successor to be a neighbour over another role that the
context does bound. Items 727, 729 and 731 keep their scope.

The acceptance is the gate of the roadmap row. The three files classify with counting on,
with `Stats::dl_counting_over` and `Stats::dl_over_budget` false and every axiom that
needs counting taken, so that neither the fallback of the bound nor the fallback of the
budget passes as a success, and no development file of the ORE 2015 DL sample costs more
than twice its run without counting, so that counting can be on by default. The
development files, the generated fixtures and the `shoi` and nominal fixtures give the
taxonomies of HermiT with no new timeout, on one and four threads, under both ranks, both
strategies and both choices of the witnesses' contexts. Two regressions cover the
predecessor, one where the predecessor is the only qualifying neighbour and one where it
counts beside a successor, and two cover the shared witness context, one where a maximum
of one equates two conditional witnesses of `H` and the certificates must derive `⊥`
under the clause's body, and one where a unit witness's marker must reach the shared
context. The two large files are measured apart from the first, with
the time of their normalization, their saturation, their candidate tests and the
completion of their counting, and the counts of the conditional predecessor clauses and
of the hypothetical successors under each rank are recorded.

The item ends with its gate not met, and it ships its clauses and its record. The first
part is kept, with the `n = 1` revision above, so an exact or minimum cardinality of two
or more in a negative position is the at-most clause with the neighbours in its body, an
exact cardinality of one is `∃r.F ⊑ y ⊔ H`, and rooted runs keep their former translation.
A lazy clause over a non-simple role fails the simple-role check as an at-most clause does,
and the run then leaves every disjunctive axiom out and reports it, as it did before the
item. The `n = 1` form needs no such check, since it counts no neighbours. The second part,
the low rank of `H`, stays behind `SPARKLES_DL_LOW_H`, off, since it diverges on `sriq-010`
and finishes none of the three files. The third part, the witnesses' shared context, is
not built, since the keyed successor's measurement showed that the cost of ore_ont_6485
lies in the equality reasoning over conditional successors, which no choice of the
witnesses' contexts touches, and it goes to item 894 with the two versions of the Protein
Ontology, whose cost it addresses. The product dedup and its memory charge are kept. On
`atlas` on one thread, release, with the binary of the keyed experiment set to the shared
contexts and the control rank, which is the path the revert keeps, the whole classification
of ore_ont_6485 takes 11 ms without counting and 190 ms with counting requested, where the
attempt passes its bound and the run classifies again without it, ore_ont_6952 takes 907
and 1,105 ms, and ore_ont_10140 927 and 1,128 ms, each with `Stats::dl_counting_over` set
and the answer of the run without counting. The generated fixtures with the 150 counting
cases and the `shoi` and nominal fixtures keep their results against HermiT on one and
four classifier workers, with the five `shoi` fixtures of item 822 falling back as before,
and the Tier D suite holds the regressions of the predecessor, of the simple-role check,
of the rooted translation and of the product record's charge. The development files of
the ORE 2015 DL sample were measured for time and not compared again against HermiT in
this item, since counting stays off and the run without counting is unchanged. The two
regressions of the shared witness context go to item 894 with the third part. Item 894 is
the calculus change for the equality reasoning that remains, from published calculi only,
with the hypertableau's merging of equated terms and Kazakov and Klinov's exploration as
the candidates named above, and its design precedes its code.

#### Item 894's cases and merging for counting

This subsection is the design of item 894, written before its code. It decides how a run
that counts, without the root of a rooted run, takes the case analysis that a union filler
puts into a successor, and it records what a replacement of the equality reasoning between
two successor terms must satisfy before it can be a decision, so that ore_ont_6485,
ore_ont_6952 and ore_ont_10140 classify with counting on. Rooted runs keep their rules,
their order and their terms, and nothing here changes them. The design cites the calculus
of Bate and others in JAIR 2018, the hypertableau calculus of Motik, Shearer and Horrocks
in JAIR 2009, the nondeterministic consequence-based procedure of Kazakov and Klinov at DL
2014 and the Horn-SHIQ calculus of Kazakov at IJCAI 2009, which [the Horn rules](#the-horn-rules)
of this spec implement.

The trace shows the cost that remains after item 728. The counting attempt on
ore_ont_6485 at the bound of 262,144 conclusions, with item 728's clauses, derives 274,640
conclusions and processes 73,403 clause records, where a record is a clause the store
queued and the rules then took as a premise, and its clause ids show at least 76,781
entries made across the contexts. The hypothetical successor of
`∃hasSingleBondWith.HeteroAtom`, which fourteen predecessors share, derives 140,889 of the
conclusions and processes 19,921 records. Of its derivations 109,560 resolve a case of the
`HeteroAtom` union against a disjointness certificate of another case, in clauses whose
bodies hold one to eight marker atoms after duplicates are removed, among them the role
atoms of three bond properties and the hypothesis `HeteroAtom` in two spellings, as the
class and as the conjunction that unfolds to it, and whose heads hold four to twelve
literals, the union's remaining cases beside the literals over the predecessor. At the
bound of four million conclusions, with the hypothetical successor keyed by its
predecessor so that no context combines predecessors' markers, one EL successor context,
the successor of a witness `∃r.b_i` whose fresh filler lies below the union, derives 1.9
million paramodulations and 1.7 million at-most products over the conditional successor
terms of the union's cases. The two costs have one shape. A context clause's body names
the hypotheses under which its head holds, so a clause is the conclusion of one branch of
a case analysis, and its head is the disjunction of the branches not yet taken. The
deterministic calculus derives every branch's conclusion as a clause of its own, two
spellings of one hypothesis give two clauses with different bodies that Definition 4's
syntactic subset does not relate, and two orders of the same premises give the same clause
twice as two attempts the store merges. An equality between two successor terms takes part
in the Eq rule when it is the maximal literal of its clause, and the rule then rewrites each clause whose maximal literal is over the larger
term into the equality's clause, each with the union of the two bodies and heads, and the
rewritten clauses are premises of the at-most products and of the next equality. The
hypertableau takes the same equality in one branch as the ≈-rule, which merges `s` into
`t` by replacing `s` with `t` in every assertion after pruning `s`'s descendants, a rewrite
of a few unit assertions. Kazakov and Klinov explore the branches of one context one at a
time, with units on each branch and the common conclusions of all open branches as the
entailed ones, and their Theorem 2 shows that a split of a disjunction into branches is
sound for each element separately. Kazakov's Horn-SHIQ calculus takes two successors that
an at-most restriction identifies as one successor whose filler is the conjunction of
theirs and whose roles are both, and never writes an equality.

The decision has two parts that are built, the cases and the witnesses' context, and a
third that is specified and not decided, the merge.

The first part gives each case of a union filler a successor of its own. With counting, an
existential restriction `∃r.F` in a positive position whose filler `F` is a union
`C_1 ⊔ … ⊔ C_k` in the ontology, or a fresh filler `b ⊑ F` of a witness of `H` or of a
positive minimum below such a union, is normalized as the disjunction of one existential
restriction per case, `∃r.(F ⊓ C_1) ⊔ … ⊔ ∃r.(F ⊓ C_k)` and
`∃r.(b ⊓ C_1) ⊔ … ⊔ ∃r.(b ⊓ C_k)`, each with a fresh conjunction class and a function
symbol of its own. The two forms are equivalent in every interpretation, since
`∃r.(A ⊓ (C_1 ⊔ … ⊔ C_k))` is `∃r.(A ⊓ C_1) ⊔ … ⊔ ∃r.(A ⊓ C_k)` and
`b ⊑ F ⊑ C_1 ⊔ … ⊔ C_k` makes `b` equal to `b ⊓ (C_1 ⊔ … ⊔ C_k)`, so the normalized
ontology has the same models over the original signature once the fresh names are read as
the conjunctions, and Theorems 8 and 9 give soundness and completeness as they do for the
Table 1 translation, since only the set of DL-clauses changes and the rules do not. The
case restrictions are conditional in the predecessor, since the clause that holds them is
`k - 1` literals wider than the clause it replaces, so each lies in `K_2` of the Succ rule,
and in `K_1` only where a context supports it as a unit, and its successor is what the
engine's `target` makes today for the restriction. Under the cautious strategy a
conditional case restriction gets a hypothetical successor context with an empty core and
the filler, the case and the union as markers, a case restriction that a context supports
as a unit gets its EL successor, and the eager strategy may give the context a core of the
atoms that are certain, with the markers of `K_2` outside the core sent as they are today.
What changes is which contexts exist and what each is sent. Each conditional case
restriction has a hypothetical successor of its own, and the distributed clause itself sends it the filler,
its own case and the union and never another case's existential restrictions. The Succ
rule still sends every marker of `K_2` that holds of the term, from every predecessor that
reaches the context and from the universal restrictions they carry, so a case's context
can receive another case's marker from a predecessor whose universal restriction puts the
successor in that case, or an exclusion marker that is disjoint with one of the cases, and
the union clause `F → C_1 ⊔ … ⊔ C_k`, conditional on `F`'s marker, can then resolve more
than once against disjointness certificates, each resolvent with the exclusion markers in
its body and not redundant against the marker clause of the case. The saving of the part
is therefore a measured hypothesis and not a guarantee, that the case contexts of the
three files hold fewer cases' restrictions and resolve the union fewer times than the
shared successor does, and the regressions include a restriction with two predecessors of
which one sends another case through a universal restriction and one sends a marker
disjoint with a case. The predecessor's clause carries the `k` alternatives, and the
Succ rule makes up to `k` successor terms for the restriction, one per case, whose
qualified-role literals are candidates of the at-most products. A product over two terms
of two cases of one restriction is redundant under this lemma. If the two qualified-role
premises both descend from one occurrence of the `k`-way clause `Γ_0 → Δ_0 ⊔ E_1 ⊔ … ⊔ E_k`,
if each keeps in its head the alternatives of the clause other than its own case, and if
each keeps `Γ_0` in its body, then the product's head holds `Δ_0` and every `E_i` and its
body holds `Γ_0`, so the product contains the `k`-way clause up to redundancy by
Definition 4 and the Elim rule drops it. A candidate of the at-most rule is a term with a
list of supporting premises, and the products enumerate every combination of the
candidates' supports, so the lemma certifies one combination and not a subset. The rule
drops a subset before the bounds on the widest clause, the subsets and the payload count
it only when two of its candidates have every pair of their supports covered by the lemma,
with an occurrence of the `k`-way clause in the context's store as the witness of the
redundancy, since then every product of the subset is redundant on arrival. A subset with
a support outside the lemma, such as an independent conditional support `A → r_B(x, f_1)`
beside the source-derived one, stays with its accounting, and only the individual
conclusions the lemma covers are skipped. Every other product keeps its equality
reasoning. In particular a case restriction that a context supports independently, as a
unit or under a premise that has dropped the alternatives, is not covered, and neither are
two cases of two different restrictions whose classes overlap, so an equality between
terms of different cases can still be derived and the Eq, Ineq and Factor rules take it as
before, and a regression holds source-derived and independent conditional supports of
one candidate together. The part applies in runs that count and without the root, since those are the runs
whose witnesses and hypothetical successors carry union fillers, and it applies to a union
of pairwise disjoint classes and to any other union alike, since the identity does not
need the disjointness. The witnesses of one restriction keep their pairwise disjoint
fillers `b_i`, and the certificates of the inequalities are unchanged, since the
inequality of two witnesses is between their fillers `b_i` and `b_j` and not between the
cases. A union of more than sixteen cases keeps the former translation. That number is a
heuristic that bounds the terms one restriction adds to a context, and it does not by
itself keep a context within the at-most rule's bounds on the widest clause, the subsets
and the payload, which stay as they are and are the measured quantities of this part,
since sixteen candidates and a maximum of five give 3,003 subsets for one fixed candidate
and pass the payload bound before any product is derived. The measurement records, for
each of the three files, the terms per context, the subsets the lemma skips, the products
derived and the work at the bound, under the former and the new translation.

The second part is item 728's third part, the witnesses' shared context, as that design
states it, applied after the first part, so that the `n` witnesses `∃r.b_i` of one case
share one successor context per role, filler and case, with `b_i` kept in the term's core
for the certificates and sent as a marker, and the unit path sends the marker as that
design says. It targets ore_ont_6952 and ore_ont_10140, whose cost is the witnesses'
repeated unfolding of the transitive properties, 156,882 of the 280,093 conclusions of the
attempt, and not an equality. The first part is measured on the two files before this one
is built, and this one is built when either file still falls back or still misses the
speed gate after the first.

The third part, the merge of two equated successor terms in place of the Eq rule, is
specified here and not decided, since the review of this design found that the simple
form of Kazakov's at-most rule over context clauses is not complete, and the item builds
the first two parts and measures them before it returns to this part. The simple form
derives from `Γ → Δ ⊔ f_1(x) ≈ f_2(x)`, where the terms are the successors of `∃r_1.K_1`
and `∃r_2.K_2`, the clauses `Γ → Δ ⊔ ∃r_1.(K_1 ⊓ K_2)(x)` and `Γ → Δ ⊔ ∃r_2.(K_1 ⊓ K_2)(x)`.
Each is entailed by the premises, so the form is sound, and it is not a replacement for
the equality, because the two conclusions do not say that one element carries both role
edges and both terms' accumulated facts. The ontology with `r_1 ⊑ r`, `r_2 ⊑ r`, `r_1` and
`r_2` disjoint, `A ⊑ ∃r_1.K`, `A ⊑ ∃r_2.K` and `A ⊑ ≤1 r.K` has `A` unsatisfiable, which
the Eq rule finds by rewriting one role literal onto the other term, and the simple form
derives only `∃r_1.(K ⊓ K)` and `∃r_2.(K ⊓ K)`, which add nothing, while a shared context
that takes both role markers derives the clash under both markers in its body, which the
Pred rule cannot discharge for either term, since neither term carries both edges. This
example is a regression of the item whatever form the part takes. A replacement must
therefore carry the identity of the element. Its merged restriction is `∃(r_1 ⊓ r_2).(K_1 ⊓ K_2)`
over the conjunction of the roles and of the fillers, as the Horn rules' successors carry
a set of roles and a conjunction of classes, and its context must take every positive and
negative fact of both terms, the certificates of their inequalities and the universal
restrictions their predecessor sends, so that the model construction of Theorem 9 can map
the term `f_1(t)` to the merged context in place of the rewrite `f_1(x) ⇒ f_2(x)` and find
every clause over either term true there. Its fillers are canonical conjunctions over the
finite set of the ontology's atoms, so that a merge of `K_1 ⊓ K_2` with `K_1` returns the
restriction it has and a repeated merge makes no new name, and the number of merged
restrictions is bounded by the subsets of those atoms and the role sets, which is the
eager strategy's bound and exponential in the ontology, not by the pairs of the initial
restrictions, since a merged restriction can merge again. Its function symbols take a
global well-founded order compatible with that reuse, since a symbol that is below its
inputs and reused for a merge with one of them cannot be below itself, and Definition 3
needs the order on function symbols to be well founded. Its schema must say which equality
of an at-most conclusion over a maximum above one it replaces and how the other equalities
of that head and their dependencies survive, how it takes an equality that arrives through
the Pred rule or through the Eq rule rather than through the Hyper rule directly, under
what certificates the qualification of each term by the counting qualifier `B` holds when
the term's filler does not imply `B` and the qualification comes from a conditional fact or
a universal restriction, and how the Ineq and Factor rules keep their contribution to
completeness once the equalities they consume no longer exist. Until a round of this
design meets those requirements and its completeness argument is reviewed, the Eq rule
stays for every equality, between two successor terms and between a successor and the
predecessor alike, and the first two parts are the item's implementation. If the first
two parts leave ore_ont_6485 past the bound or the speed gate, the item returns to this
part or to the nondeterministic exploration of Kazakov and Klinov as the remaining paths,
and the decision between them is a further round of this design.

The acceptance is the gate of the roadmap row. The three files classify with counting on,
with `Stats::dl_counting_over` and `Stats::dl_over_budget` false and every axiom that needs
counting taken, within the speed target, which for ore_ont_6485 is a run of at most the 18
ms of Konclude 0.7.0 on four threads and a goal of three to five times faster, and no
development file of the ORE 2015 DL sample costs more than twice its run without counting.
Each part is measured apart, behind a switch of the lane that the landing removes, on the
three files and on the counting fixtures, with the conclusions derived, the clause records
processed, the entries made and the contexts made under each setting, on one thread on
`atlas` beside the run without counting. The development files, the generated fixtures
with the 150 counting cases and the `shoi` and nominal fixtures keep their results against
HermiT on one and four classifier workers under both strategies, which means the full
taxonomy for every run that is taken whole and finishes, soundness and a reported fallback
for the cases the suite already records as falling back, `shq-041` and `shiq-028` of item
729 among them, and `shiq-046`'s raised work bound as it is, with no new timeout and no
new fallback. Regressions cover a union filler whose cases are not disjoint, where the
distribution is still the identity and a successor may be in two cases, a witness
`≥2 r.(C ⊔ D)` with `C` and `D` disjoint and a maximum of one for each case, whose two
witnesses must be free to take different cases, two case restrictions one context supports
independently so that the lemma does not apply and the equality is derived, two cases of
two different restrictions whose classes overlap, a conditional inequality certificate
after the distribution, a qualification by DL4 that reaches the predecessor by Pred, the
unfolding of a transitive property under a case, the rooted run's unchanged
normalization, the disjoint-role example of the third part, and the two regressions of
the shared witness context from item 728's design. The measured effect of each part is
recorded here when the item ships, and a part that measures no faster is not kept and
goes to PERFORMANCE-EXPERIMENTS.md.

#### Item 894's decision between merging and exploration

This subsection is the further round that the design above calls for when its first two
parts leave ore_ont_6485 past the bound. It records what the first part measured and why
the second was not built, shows where the three files' attempts spend their bound, and
decides between merging two equated successor terms and the nondeterministic exploration
of Kazakov and Klinov. The measurements are debug builds on one thread with the derivation
trace's tallies by rule and by context, which count derivations and not time, at the
default bound of 262,144 and at a work bound of 4,194,304.

The first part, built behind a switch of the lane, does what it was meant to do in the
contexts of the union's cases, where the derivations of the disj rule on ore_ont_6485 fall
from 110,668 to 28,407, and it makes no file classify. ore_ont_6485's attempt spends the
freed bound in the hypothetical successor of `∃hasSingleBondWith.CarbonAtom`, which 48
predecessors share and which derives 100,421 Pred conclusions at the default bound, and
spends the larger bound on lazy products, 245,820 of 594,991 derivations. The attempt on
ore_ont_10140 is the same with and without the part and the attempt on ore_ont_6952 nearly
so, since neither file has a union filler in the axioms that Tier D normalizes. On the
generated fixtures the part adds work wherever it applies, because each case of a
restriction is a successor term of its own and so a candidate of the at-most products.
`shiq-013`, `shiq-045` and `sriq-010`, three of the six fixtures that classify within a
raised bound, pass that bound and fall back, `shq-038` grows past 9 GiB, and fourteen more
fixtures derive two to a hundred times as much. The decision below does not use the part,
so it is not kept.

The second part is not built. Its target was the witnesses' repeated unfolding of the
transitive properties, 156,882 of 280,093 conclusions in a trace of ore_ont_6952 taken
before item 728's clauses landed. With those clauses the attempts on ore_ont_6952 and
ore_ont_10140 derive 3,350 and 3,698 unfoldings at the default bound, about one percent of
their work, so the part cannot change their fallback.

Every attempt stops at the work bound, which counts the derived conclusions and, at one
per sixteen, the certificate clauses that the equality simplification reads. On
ore_ont_6952 and ore_ont_10140 the largest rule at the default bound is again, with 46,709
and 72,613 derivations. The rule derives once more every stored clause with an equality or
an inequality of a successor term each time a new certificate of that term is processed,
so that a certificate that comes after the clause still simplifies it. In the hottest
context of ore_ont_6952, a protein complex with three successors of exact cardinalities
over `has_component` beside the successors of its definition, the rule derives 46,709
conclusions from 356 processed clauses at the default bound and 240,338 from 775 processed
clauses at the larger one. A diagnostic build that reports each rederivation that changes
the store found none among those 46,709, of which 46,617 are ones that the filter below
skips. The count is between 0.37 and 0.40 times the square of the processed clauses, since
each processed certificate derives every live equality clause over its term again and both
sets grow as the context saturates, and each rederivation reads the certificates of both
of its terms again, so the work grows faster than the square of the clauses. This is why
the rule's cost grows with the bound and why a larger bound only stops the attempt at a
context that has processed twice as many clauses. A filter that skips the rederivations
that the new certificate cannot change, kept as an experiment and not adopted, removes all
but 118 of them on ore_ont_6952 and all of them on ore_ont_10140. The attempts then spend
the larger bound on paramodulation, 247,395 and 223,556 derivations, in contexts that have
processed 1,681 and 2,276 clauses and are still deriving. ore_ont_6485's attempt without
the first part derives 1,522,033 conclusions at the larger bound, 575,550 of them
paramodulations and 127,945 rederivations, and 1,065,930 of them in one context. The three
files therefore spend their bound on equality reasoning between successor terms, through
the rederivation of the equality clauses that paramodulation makes and through
paramodulation itself once the rederivation is filtered.

The Eq rule makes this cost in two ways. It rewrites into the equality's clause every
clause whose maximal literal is over the larger term, so that each equality clause
multiplies the clauses over its terms, and the rewritten clauses, with the unions of the
premises' bodies and heads, are premises of the next products and equalities. And since an
equality is a literal of a stored clause, the certificates that refute it reach it only
through the rederivation above. Merging, as Kazakov's Horn-SHIQ calculus merges two
successors that an at-most restriction identifies, writes no equality between two
successor terms. Without such equalities the again rule has no clause to derive again, no
equality is rewritten into another, and the Factor and Ineq rules have nothing to take,
which the merge gives by construction. The transfer rule below still combines each clause
of a merged atom with each clause of a fact of a term it merges, so a merged term with `k`
clauses of its atom and `l` clauses of facts of its terms can take `k × l` transfers, and
a fact moves to every merged term whose atom holds and whose set contains its restriction.
That is the shape of the paramodulations of one equality. Whether the transfers and the
merged terms' contexts cost less than the paramodulation they replace is therefore a
hypothesis of this design, which the traces above do not show and the measurement below
decides.

The exploration of Kazakov and Klinov instead keeps no disjunction and takes one branch of
a context at a time. Its published calculus is for ALCHI, with no number restrictions, no
equality, no transitive properties and no nominals, so it says nothing of an at-most
restriction or of the predecessor's part in a count. Taking counting into it would mean
merging two terms within a branch, as the hypertableau's ≈-rule does, which would be an
extension of ours with a completeness argument of its own. It also replaces the store of
context clauses that the Pred rule, the hypothetical successors, item 728's lazy clauses
and item 804's rooted run are built on, so it is a new Tier D and not a rule of the
present one. Both paths change the inference system behind Theorem 9 and need an argument
of completeness. The merge's argument is given below, and it keeps every rule of the
calculus except the equalities of the at-most conclusions.

The decision is the merge. It changes the rule whose cost the traces measure and keeps the
calculus, its contexts and its other rules. The exploration stays the remaining path if
the merge's measurement fails, and the questions above about counting and completeness are
the design work it would then need. The merge applies in runs that count without the root.
Those runs have no nominals, no self restrictions and no reflexive roles, so no clause
holds a loop `S(x, x)` or an equality of `x`, and every equality is `f(x) ≈ y` or `f(x) ≈
g(x)` for successor terms `f(x)` and `g(x)`. Rooted runs keep the Eq rule for every
equality.

A merged term is named by the set `S` of the existential restrictions of the normalized
ontology whose successor terms it merges, with the function symbol `m_S` and the merged
atom `M_S`. A restriction `e` of the ontology has the set `{e}` and its own symbol `f_e`.
The merge of two terms with the sets `S` and `T` is the term of `S ∪ T`, interned during
the run in a table beside the Horn rules' table of conjunctions, so a merge with a term
whose set contains the other's returns that term and a repeated merge makes no new symbol.
The name is the set of restrictions and not a pair of unions of roles and fillers, since
two different pairs can have one union, as `∃r.A` with `∃r.B` and `∃r.A` with `∃r.(A ⊓ B)`
do, and the transfer below may move facts only from the terms of the restrictions in the
set. A merged atom is a name of its own and never an existential restriction of the
ontology, so the rules do not unfold it, and a merged term has the facts that the transfer
and the calculus's other rules derive for it and no others. The merged symbols are bounded
by the subsets of the ontology's existential restrictions, an exponential bound like that
of the calculus's own contexts, so the run terminates. The function symbols are ordered by
the size of their set, then by its sorted members. Each set names one symbol, so the order
is total on finitely many symbols and well founded as Definition 3 requires, and a symbol
is above every symbol whose set is a proper subset of its own. A merge that returns one of
its inputs adds no symbol and no pair to the order. The context order places the merged
atoms over `x` above the named classes, as it places every atom that the normalization
makes, and no merged atom is a predecessor trigger, so condition 5 of Definition 3 holds
as before.

The at-most rule and item 728's lazy clauses are the only rules of these runs that would
write an equality between two successor terms. No clause of the ontology holds one, since
a minimum is normalized to witnesses with pairwise disjoint fillers. The Eq rule rewrites
a successor term only into the predecessor once such equalities are gone, and the Pred
rule of these runs sends only atoms and role atoms over the predecessor, so neither rule
makes one. Where the at-most rule or a lazy clause would write `s(x) ≈ t(x)` for two
successor terms with the sets `S` and `T`, it writes the merged atom `M_{S ∪ T}(x)`, which
is the atom of the larger term when one set contains the other. Each equality of an
at-most conclusion over a maximum above one becomes a merged atom of its own, and the
conclusion's equalities with the predecessor, its other literals and its body stay as they
are, so `Γ → Δ ∨ ⋁ s_i(x) ≈ s_j(x)` becomes `Γ → Δ ∨ ⋁ M_{S_i ∪ S_j}(x)` with `Δ`
unchanged. A merged atom leaves the head when it is made if two of its terms have
certificates of classes in one disjointness set under the clause's body and the rest of
its head, as the equality leaves it today. The disjoint-role rule derives the clash of two
disjoint roles over the merged term once both role literals are transferred, which decides
the disjoint-role example above. Neither needs a later rederivation, since a certificate
that arrives after the merged atom reaches the merged term through the transfer, and the
clash in the merged term's context returns to the atom's clause through the Pred rule.

The merged symbols are sound under definitions that add no consequence over the ontology's
signature. Read `M_S(x)` as the statement that the terms `f_e(x)` of the restrictions `e`
in `S` are one element, and `m_S(x)` as that element where `M_S(x)` holds. Where it does
not, read `m_S(x)` as the element that corresponds to `f_e(x)`, for the least `e` in `S`,
in another copy of the model. The disjoint union of a model with a copy of itself is again
a model of the ontology, since these runs have no nominals, and no role relates an element
to the other copy. The disjoint union of any model with its copy therefore has an
expansion that satisfies the definitions, and it satisfies the same clauses over the
ontology's signature as the model, so the definitions add no consequence there. A rule's
conclusion is a consequence of the ontology with the definitions. A role literal of
`m_S(x)` makes `M_S(x)` true, so an at-most conclusion's merged atom holds where its
equality does, whether its terms are terms of the ontology's restrictions or merged terms,
since the equality of `m_S(x)` with `f_e(x)` or with `m_T(x)` makes every term of `S ∪
{e}` or of `S ∪ T` one element. A transfer holds since `M_T(x)` makes `f_e(x)`, for `e` in
`T`, and `m_T(x)` one element, and the simplification by certificates holds since two
terms in disjoint classes are two elements while `M_{S ∪ T}(x)` makes `m_S(x)` and
`m_T(x)` one.

The transfer rule gives the merged term the facts of the terms in its set, which the
requirements above call the identity of the element. From a clause `Γ_1 → Δ_1 ∨ M_T(x)`
whose maximal literal is the merged atom and a clause `Γ_2 → Δ_2 ∨ L(f_e(x))` with `e` in
`T`, whose maximal literal `L` is a class atom over `f_e(x)` or a role literal between `x`
and `f_e(x)` in either direction, it derives `Γ_1 ∧ Γ_2 → Δ_1 ∨ Δ_2 ∨ L(m_T(x))`. A unit's
literals over `f_e(x)` take part in place, as they do in the Eq rule. These are the
literals over a successor term that a clause of these runs holds apart from the equality
`f_e(x) ≈ y`, which the transfer does not move and need not, as the argument below shows.
A merged term's own facts do not move to a larger merged term, since the model below maps
only the terms of the ontology's restrictions, so every transfer starts at a term
`f_e(x)`. The context of a merged term thus takes every fact of its terms, their
certificates among them, and the universal restrictions of the predecessor reach it
through its transferred role literals. The Succ rule makes the merged term's successor
context as it does for any successor term and sends it the transferred facts as markers.
The merged terms are candidates of the at-most rule like any successor, through their
transferred role literals, so a count that needs three terms in one element merges a
merged term again.

The completeness argument extends the proof of Theorem 9 in Appendix C. Appendix C.2
builds the model fragment `R_t` from the ground clauses `N_t` of the context of a term
`t`, and Appendix C.3 unfolds the context structure into the rewrite system `R`. Both
constructions and Lemmas 13 to 28 rest on the forms of context clauses, on the order of
Definition 3 and on the saturation under the Core, Eq, Ineq, Factor, Pred and Succ rules.
The merged atoms are unary atoms over `x` and the merged symbols are function symbols of
sort a, the order extends as above, and those rules are kept, so `R` is built as there and
the lemmas hold, with the merged symbols in its Herbrand universe. The proof uses this
productive interpretation and not an arbitrary model of the clauses, since in `R` every
atom that is true in `R_t*` and whose terms `R` does not reduce is the maximal literal of
a generative clause of `N_t` whose other literals Lemma 16 makes false, and such a clause
is an instance of a context clause that can be a premise of the rules. Lemma 29 shows from
the saturation under the Hyper rule that `R*` satisfies each clause of the ontology, and
its proof holds unchanged for every clause except the at-most clause of DL4 and item 728's
lazy clauses, which have its form with atoms over `x` added to the head and whose Hyper
conclusions are changed. For these clauses the same proof gives a weaker property. If the
body atoms over `t` and `S_B(t, u_i)` hold in `R*` for `n + 1` different neighbours `u_i`
of `t` that `R` does not reduce, then Lemma 27 makes each of these atoms true in `R_t*`,
the rule applies to the context clauses behind their generative clauses, and by Lemmas 18
and 22 its conclusion is in `N_t` up to redundancy and true in `R_t*` while its premises'
other literals are false. One of its head literals is therefore true. It is an atom over
`t`, an equality of a `u_i` with the predecessor, which would reduce `u_i`, or a merged
atom `M_{U_i ∪ U_j}(t)` of two of the successor terms, where `U_i` is the set of `u_i`.

The qualification of a term by the counting qualifier `B` is the first clause of DL4 at
that term, which Lemma 29 covers at a merged term as at any other term, whether its `B`
comes from a conditional fact, from a universal restriction of the predecessor applied to
a transferred role literal, or from the term's own context. So `S_B(t, m_T(t))` holds in
`R*` when `S(t, m_T(t))` and `B(m_T(t))` do, and Lemma 27 makes it true in `R_t*`, where
the weaker property reads it.

The model is then the part of `R*` that the merged terms leave uncovered. A successor term
`u` of `t` that `R` does not reduce, with the set `U`, is covered when `R*` makes `M_T(t)`
true for a set `T` that strictly contains `U`, so the atom of a merged term never covers
that term itself. For a covered term choose such a `T` that is maximal by inclusion. Then
`m_T(t)` is not covered, since a true atom of a strictly larger set would contradict the
choice, and since the sets are finite every covered term has such a representative. The
interpretation `I` has as elements the terms that `R` does not reduce and that are reached
from the constant `c` through successors that are not covered. Its atoms and role atoms
are those of `R*` over these elements, and the value of a function symbol `f_e` at an
element `t` is the normal form under `R` of `f_e(t)` when that term is not covered and of
`m_T(t)` for the chosen `T` when it is. The representative occurs in `R_t`, since the
transfer gives it the atoms of `f_e(t)`, so its normal form is an element of `I`.

`I` satisfies the clauses of the ontology. A clause with no function symbol and no
equality in its head, such as DL1, DL3, the first clause of DL4 and DL7 to DL9, holds in
`I` since it holds in `R*` and `I` keeps the atoms of `R*` over a subset of the terms that
`R` does not reduce, closed under predecessors, with the role atoms between neighbours as
there. The at-least restrictions of these runs are witnesses with disjoint fillers, so the
clauses of DL2 are `B_1(x) → S(x, f_e(x))` and `B_1(x) → B_2(f_e(x))`, and no clause of
the ontology holds an inequality. At an element `t` with `B_1(t)` the proof of Lemma 29
makes both atoms over `f_e(t)` true in `R_t*`. If `f_e(t)` is neither reduced nor covered,
it is an element and both atoms hold of it. If `R` reduces `f_e(t)` to the predecessor,
both hold of the predecessor as in Theorem 9. If `f_e(t)` is covered, each atom and the
atom `M_T(t)` have generative clauses in `N_t`, the transfer of the context clauses behind
them is in `N_t` up to redundancy and true in `R_t*`, and its other literals are false, so
the atom holds of `m_T(t)` and of its normal form. For the at-most clause and a lazy
clause, suppose that an element `t` with the body atoms and none of the head atoms over
`t` had `n + 1` elements `u_i` with `S_B(t, u_i)` in `I`. Each is the predecessor of `t`
or a successor that is not covered, and the weaker property makes `M_{U_i ∪ U_j}(t)` true
for two of the successors. If `U_i ∪ U_j` strictly contains `U_i`, that atom covers `u_i`,
and otherwise `U_j` is a proper subset of `U_i` and `M_{U_i}(t)` covers `u_j`. Either
contradicts the choice of elements, so every count holds in `I`. `I` agrees with `R*` over
`c`, so it does not satisfy the query clause, and the ontology does not entail it.

The equalities and inequalities that remain need nothing more. An equality `f(x) ≈ y` of a
stored clause needs no transfer. When `R` makes it true at `t`, `R` reduces `f(t)` to the
predecessor, so the term is not covered and its value is the predecessor as in Theorem 9.
A merged term that the at-most rule equates with the predecessor is reduced in the same
way, and the terms it covers take the predecessor as their value, where the transferred
atoms hold. When `R` makes the equality false, its clause holds in `R_t*` through another
literal, and `I` need not keep the equality or its negation for a covered term, since `I`
is checked against the clauses of the ontology and not against the context clauses, only
the at-most and lazy clauses have equalities in their heads, and no clause of the ontology
holds an inequality. The Ineq and Factor rules keep their part in Lemma 21 without work.
Every equality of these runs is `f(x) ≈ y`, so two equalities of one term have one right
side and Factor has no premise. No clause of the ontology holds an inequality, Factor is
the only rule that makes one from equalities and the Eq rule makes one only from another,
so no clause holds an inequality and Ineq has no premise. The Pred rule's trigger `x ≈ y`
never fires, since no clause of these runs holds an equality of `x` and their Pred rule
sends only clauses whose heads are atoms and role atoms over the predecessor.

The merge is built behind a switch of the lane and measured on the three files at both
bounds, with the tallies by rule and context above, on the generated fixtures and the
`shoi` and nominal fixtures, before any timing on `atlas`. Beside the rule tallies the
trace counts the transfer attempts, the transfers the store keeps, the transfers it
already holds up to redundancy, the merged terms and their contexts, which test the
hypothesis that the transfers cost less than the paramodulation they replace. The again
rule then has no equality clause between successor terms to derive again, so its
experimental filter stays unadopted. The acceptance is the row's, as the design above
states it. The regressions add a count that needs three terms in one element, a merged
atom refuted by certificates when it is made and one refuted by a certificate that arrives
after it, made by the at-most rule and by a lazy clause, two true merged atoms whose sets
overlap without a true atom of their union, a merged term equated with the predecessor, a
merged term that takes a conditional fact of each term, and a qualification that reaches a
merged term only through a transferred role literal and a universal restriction, beside
the regressions above. If the merge leaves a file past the bound, the item returns to this
round with the exploration.

The merge was built as above behind a switch of the lane and measured, and it is not kept.
It makes no file classify. The attempts of ore_ont_6485, ore_ont_6952 and ore_ont_10140 at
the default bound derive 281,066, 186,129 and 242,027 conclusions and stop at the work
bound, and at the larger bound they derive 562,845, 1,189,769 and 506,166, where
ore_ont_6485 and ore_ont_10140 pass the at-most rule's width limit and ore_ont_6952 the
work bound. The hypothesis holds for the part it names. The transfers are few, 21,086, 516
and 270 at the default bound, of which the store keeps 8,696, 310 and 180, and the again
rule and the paramodulation between successor terms are gone. The cost moves into the
at-most rule and item 728's lazy clauses, which make 219,789, 168,148 and 219,295
conclusions at the default bound and 499,544, 1,170,975 and 480,842 at the larger one.
Each merged term is a candidate of the at-most products beside the terms in its set, and
every at-most conclusion is a disjunction of merged atoms, four to six in the hottest
context of ore_ont_6952, which the transfer carries into the role literals of the merged
terms. In that context the 15 terms of the restrictions have one or two role literals each
as premises and the 15 merged terms 2 to 20, so the products cross long lists of
conditional premises, and one processed role literal of a merged term gives up to 12,768
conclusions. The Eq rule rewrote an equated term into the other and so kept the candidates
few, and the merge keeps every term and every merged term as a candidate. On ore_ont_6485
the unions of these candidates make 15,284 merged terms at the default bound, 490 of
them with a successor context. On the generated fixtures the merge gives no taxonomy that
HermiT does not, and 42 of the 640 fixtures change, none for the better. Seven pass their
bound and fall back, `shiq-006`, `shiq-013`, `shiq-036`, `shiq-045`, `shq-017`, `shq-047` and
`sriq-010`, and the others derive 2 to 300 times as much, `shq-023` 145,915 conclusions in
place of 684. The merge's code is reverted with an entry in PERFORMANCE-EXPERIMENTS.md, and
its seven regressions stay in the Tier D suite, where the Eq rule passes them. The item
returns to this round with the exploration, which the next subsection takes up.

#### Item 894's exploration round

This round takes up the nondeterministic exploration of Kazakov and Klinov after the merge
left the three files past the bound, and weighs it against a narrower repair of the merge.
It counts from the derivation traces what the contexts that spend the bound hold, states
what counting inside a branch needs beyond the published calculus, and recommends how item
894 ends. The traces come from debug builds of the reverted engine on one thread with
counting, at the default bound, and for ore_ont_6952 and ore_ont_10140 also at a work bound
of 4,194,304 with the again rule's experimental filter, so that those attempts reach the
contexts behind the first one. The partition counts below read the processed clauses and
the at-most and lazy conclusions of one context, as the trace holds them when the attempt
stops, as ground clauses. An equality holds when its two terms are in one block of a
partition of the successor terms, an atom over a successor term is read once per block, a
successor term takes part only where its existential restriction holds, and the fillers of
one minimum's witnesses are pairwise disjoint, as the normalization makes them. A partition
counts when some values of the atoms satisfy every clause. These are exact counts for the
clauses of each snapshot. They are not the branches of a finished exploration, since the
ontology's other clauses and the neighbouring contexts can remove partitions or add choices.

On both versions of the Protein Ontology the hottest context at the default bound is that
of `PR_000024883`, a complex that the ontology defines as `PR_000024871` with exactly three
components of `MOD_00041`. The context derives 55,040 of the attempt's 55,560 conclusions
on ore_ont_6952 and 88,320 of 90,218 on ore_ont_10140. Its successor terms are the three
witnesses of the minimum, a successor of `∃has_component.MOD_00041`, and the witnesses of
three alternatives that the definitions of other classes put in its heads. The definition
of `PR_000024901`, which allows at most two such components, gives the clauses that either
the context holds that class or three more witnesses with pairwise disjoint fillers exist.
Two other definitions give alternatives with four such witnesses, and the trace holds the
at-most conclusion over each group of four with no literal besides the alternative's
guard, so no partition admits either four-witness alternative. With `PR_000024901` true,
3 partitions of ore_ont_6952's terms admit the context's clauses, in which the successor
of the existential restriction joins one of the three witnesses, although the class's own
maximum of two refutes them once its definition is applied. Without it, 18 partitions
admit them, the 3! ways of merging the alternative's three witnesses with the minimum's
three, times the 3 places of that successor. ore_ont_10140 also states the exact
cardinality in a told `SubClassOf` axiom beside the definition, which gives the context a
second group of three witnesses, so 18 partitions admit its clauses with `PR_000024901` and
108 without it, 3! × 3! × 3. Every such partition has exactly three blocks. A search that
merges a pair of the first four blocks while more than three remain, as the hypertableau's
≈-rule follows the at-most clause, visits 49 nodes on ore_ont_6952 and 319 on ore_ont_10140
and ends in the 18 and 108 partitions when it skips the pairs known to be different, and
meets 138 and 948 more clashes when it tries them.

The larger bound with the filter shows the contexts that follow. The attempt on
ore_ont_6952 derives 161,338 conclusions in the context of `PR_000024923`, which has the
same shape and the same counts of 3 and 18 partitions, 98,516 in that of `PR_000024883` and
57,088 in that of `PR_000026735`. The attempt on ore_ont_10140 derives 231,936 of its
233,068 conclusions in the context of `PR_000024883`. `PR_000026735` has exactly six
components of `PR_P08034` and inherits exactly six components of `PR_000007997` from
`PR_000026734`, and `PR_P08034` is a subclass of `PR_000007997`, so the six witnesses of
each group must be the six of the other. 720 partitions admit the context's clauses, the
perfect matchings of the two groups, and none with fewer merges, while its 55,466
paramodulations had not saturated it when the attempt stopped. A census over the told
axioms takes, for each class, the groups of its stated and inherited exact cardinalities
over `has_component`, and for each pair of groups whose qualifiers are equal or related by
told subsumption it counts the ways of mapping the smaller group one to one into the
larger, then multiplies these counts. It finds 72 classes with such a pair in ore_ont_6952
and 81 in ore_ont_10140. The largest products are 331,776 for `PR_000027411` and
`PR_000027412`, which each have four pairs of groups of four, 720 for `PR_000026735` and
`PR_000026737`, and 120 for `PR_000027403`. The products are counts of mappings and not
measured searches, and they leave out the candidates' alternatives, which add groups as
they do for `PR_000024883`.

The two files therefore spend their bound on contexts whose clauses admit a few dozen to a
few hundred partitions. The Eq rule and paramodulation derive clauses that hold in all of
them together, through each combination of the terms' facts and equalities, and the merge
derived them through the combinations of conditional premises of its merged terms. A
branch of the exploration takes one partition at a time, with every fact of the branch
unconditional. It stores no equality clause, so nothing is derived again when a
certificate arrives, and an at-most step over `n + 1` present neighbours offers at most
`n(n + 1)/2` merges of facts, with none of the products of conditional premise lists that
gave the merge 12,768 conclusions from one role literal. The cost moves into the number of
branches. A test that needs one open branch, such as a candidate that does not hold, can
end in the first partition that survives. A candidate that holds in every branch, or the
collection of the subsumers common to all branches that the paper's branch exploration
performs, visits every branch unless the search avoids some. As an illustration that
assumes the four pairs of groups of `PR_000027412` are independent matchings, a search that
skips known-different pairs takes 64 merges to enumerate the 24 matchings of one pair, and
enumerating all four while keeping the shared prefixes takes 64 × (1 + 24 + 24² + 24³) =
923,200 merges. Dependency-directed backtracking, which HermiT adds to the hypertableau,
skips choices that a clash does not depend on, and the paper reduces classification work
through the taxonomy, but neither is shown here to remove that enumeration, and the round
does not establish how an exploration would charge such work against the present bound.

ore_ont_6485's attempt at the default bound derives 258,378 conclusions, 140,857 of them in
the hypothetical successor of `∃r.HeteroAtom` over one of the bond roles, which 21
predecessors share, and 29,424 in the hypothetical successor of a restriction over the same
role whose filler is a name of the normalization, which 4 predecessors share. The first
context spends its work on case analysis. The second's largest rule is paramodulation, with
12,240 of its 29,424 derivations, over equalities of a successor term with the predecessor
such as `y ≈ f(x)`, which the merge keeps. In the first, 109,560 derivations are the disj
rule's, which takes the cases of unions such as `HeteroAtom`, a union of four classes one
of which is the union of five halogens. It processes 19,901 clauses whose bodies hold one
to five of 21 markers, the atoms over `x` that a predecessor can send, in 267 distinct
sets. The second processes 4,843 clauses, 148 with one marker, 4,485 with two and 210 with
three, out of 12 markers in 36 distinct sets. The present calculus thus keeps one context
and conditions its clauses on the sets of markers. The exploration instead makes a
successor a conjunction of its filler and the facts that one branch of one predecessor
sends, so each hypothetical successor becomes one conjunction for each set of markers that
a predecessor's branch sends, each with branches of its own. In the processed clauses of
the predecessors' contexts when the attempt stops, every marker is sent under a condition,
5 to 13 markers for each predecessor of the first successor and 10 for each predecessor of
the second, so that snapshot allows up to 14,208 and 4,096 sets. A finished saturation
could send more, and the conditions, inclusions and overlap between predecessors could make
far fewer occur. The bond roles are symmetric, so a successor also counts its predecessor
in its own exact cardinalities, and a conjunction must state whether the predecessor has
each qualifier that counts it. The ontology's exact cardinalities over the bond roles have
the seven qualifiers `OxygenAtom`, `BromineAtom`, `HalogenAtom`, `HydrogenAtom`,
`CarbonAtom`, `Alkyl` and `OrganicGroup`. Under its told disjointness and inclusions, where
`BromineAtom` is a halogen and `Alkyl` is a carbon atom and an organic group, they allow 13
combinations for a predecessor, and the cardinalities of the classes of oxygen and
nitrogen, with the qualifiers `HydrogenAtom`, `CarbonAtom` and `Alkyl`, allow 4. For this
snapshot the conjunctions of the two successors are therefore at most 14,208 × 4 = 56,832
and 4,096 × 13 = 53,248, against the 267 and 36 sets of markers that the calculus
conditions on. The number that the predecessors' branches send in practice is known only
from an implementation, so these figures show no pass and no failure.

Counting inside a branch needs more than the published calculus, whose rules are for ALCHI
without number restrictions or equality and whose implementation takes transitive roles
through an encoding. First, the branch needs an at-most step over present neighbours with
the hypertableau's merge, where two successors merged in a branch become the successor of
the conjunction of both labels and keep their distinctness from the other witnesses and
the roles that reach them. Second, the step needs the qualification of each counted
neighbour. A neighbour whose label the branch does not decide needs a guess between the
qualifier and its negation, in the way that the paper's R←∃ rule guesses between `H ⊑ ∃S.C`
and `H ⊑ ∃R.(K ⊓ ¬C)`, so each counted qualifier can double the successor conjunctions.
Third, a count over an inverse or symmetric role needs the predecessor's part. The
successor conjunction must carry a guess of the predecessor's qualification, and the
predecessor must refute the guesses that it does not satisfy, as the R←∀ rule does for
universal restrictions. A merge of a successor's own successor with the predecessor sends
facts back to the predecessor, which the guesses must also cover, and the backtracking of
one local branch must stay independent of the others after such a merge. Fourth, the
counts must follow the role hierarchy, counting a qualified neighbour once across the
subroles that reach it, keep the simple-role restriction, and keep the transitive roles'
encoding apart from the counted roles. Fifth, the extension needs a finite vocabulary of
conjunctions and neighbour configurations for termination, and a proof that a set closed
under the rules gives a model, extending the paper's Theorem 1 as Appendix C of Bate and
others proves Theorem 9 for the disjunctive calculus with number restrictions. None of the
sources that this round reads gives that proof. The exploration also replaces the store of
context clauses that the Pred rule, the hypothetical successors, item 728's lazy clauses
and item 804's rooted run are built on, so it is a new Tier D beside the present one.

The narrower repair keeps the merge and changes what its at-most rule counts. In the
merge's traces, a merged term is a candidate beside the terms that its set covers, and the
session that measured it proposed that a product never take two candidates one of whose
sets strictly contains the other's. The member sets of the merged terms follow from the
filler facts that the transfer moves into them, since each witness has its own filler. In
the merge's hottest context of ore_ont_6952, 108,839 of its 168,146 at-most and lazy
conclusions at the default bound take such a pair, which leaves 59,307, and in that of
ore_ont_10140, 69,929 of its 211,275 take one, which leaves 141,346. The products that
remain are those of merged terms whose sets are incomparable, such as the merged terms of
one witness of each group, whose number grows with the matchings above, and the merge's
attempt on ore_ont_6952 at the larger bound derived 1,170,975 at-most and lazy conclusions
without finishing. The repair has no demonstrated pass on ore_ont_6485. In the reverted
engine, that file's hottest context spends its work on case analysis, which the merge does
not change. Under the merge, that file's hottest context, the one of `AcylBromideGroup`,
makes 219,761 at-most conclusions over maximums of one, and the attempt interns 15,284
merged terms. A maximum of one merges any two incomparable candidates, so the repair does
not stop those unions, while the share of comparable products that it removes there is not
measured. Its completeness argument looks provable. Fix an element `t` of the model. A
merged successor term of `t` that `R` does not reduce and that is reached through a true
role literal has that literal from the transfer, which derives it only from a clause whose
merged atom is then true, so the term covers every successor of `t` with a strictly smaller
set. The successors of `t` that are elements, neither reduced nor covered, are thus
pairwise incomparable, and the weaker property of Lemma 29 needs to hold only for `n + 1`
such neighbours, which the restricted products still derive. A merged term that `R` reduces
to the predecessor counts as the predecessor, as before, and a lazy clause keeps the atoms
of its head, so the same contradiction applies when they are false. The argument would
state this local restriction for each parent.

The options rank as follows, with their expected cost. Closing item 894 at its measured
outcome, with counting off by default and the axioms that need it left out and reported as
they are now, costs a change to F24, the roadmap row and a follow-up item, and it leaves
item 894's acceptance unmet, which is a scope decision for the maintainer. Item 603 already
names saturation with a tableau as the path where the calculus cannot finish, and the
partitions and conjunction counts above are what a follow-up item for counting by search
would start from. The merge with the restricted products comes next. It costs one to two
sessions, since the merge's code is one revert away and the change to the proof is small,
and it is a bounded experiment whose snapshot leaves 35 and 67 percent of the at-most and
lazy conclusions in the hottest contexts. It has no demonstrated pass on ore_ont_6485,
where the reverted engine's hottest context spends its work on case analysis, so it is
unlikely by itself to let counting be on by default. A split that builds the exploration
for the two versions of the Protein Ontology costs a new Tier D with the counting extension
above and its proof, and since ore_ont_6485 would still fall back, counting would stay off
by default after it. Exploration for all three files costs the same with no demonstrated
path for ore_ont_6485, whose snapshot alone allows tens of thousands of conjunctions for
two successors. The round recommends closing the item, and treats the restricted merge as
an optional experiment for the record rather than a path to the item's acceptance.

#### The order between number restrictions

This subsection records what item 894 delivers. The rounds above changed rules of the
calculus and left the three files past the bound. This one changes the clause set that the
normalization gives the calculus in a run that counts without the root, and no inference
rule. `Limits::number_order` controls it. It is on by default, and false turns it off. It
changes nothing without `Limits::counting`, in a rooted run, or in a run whose axioms need
no counting, so a run with the default limits, which does not count, is unchanged. With
it, ore_ont_6952 and ore_ont_10140 classify with counting on and give HermiT's and
Konclude's taxonomies, and ore_ont_6485 still passes the bound, as the measurements below
state.

The traces of the earlier rounds show where the two versions of the Protein Ontology spend
the bound. The normalization relates no two number restrictions. `PR_000026735` holds
`=6 has_component.PR_P08034` and inherits `=6 has_component.PR_000007997`, with
`PR_P08034 ⊑ PR_000007997` told, so its context makes twelve witnesses and the at-most rule
searches their 720 matchings. `PR_000024883` holds `=3 has_component.MOD_00041`, and the
definition of another class by `=2` over the same role and filler puts the alternative
`≥3` with three witnesses of its own into the context's heads, which the context's maximum
of three then matches with its own three. A minimum that the context already holds
entails the second minimum in both cases, and the second group of witnesses adds nothing
to any model.

With the option, a minimum `≥n r.F` with `n ≥ 2` in a positive position becomes a fresh
name `G` with the told inclusion `G ⊑ W`, where `W` is the conjunction of the `n`
existential restrictions over fresh pairwise disjoint fillers below `F` that the
normalization made before. The alternative `H` of item 728's clauses is such a minimum and
gets a name in the same way. No rule builds `G` from the witnesses, so `G` holds only
where an axiom or a clause states the minimum, and `G` ranks with `W` in the order of
atoms. After the axioms are normalized, `dl_norm::number_order` closes the told role
inclusions under reflexivity and transitivity, and closes the told class inclusions in the
same way from each filler, with the operands of an intersection above it and `owl:Thing`
and what the ontology tells of it above every filler. Each closure is a preorder of
consequences of the ontology. A minimum `≥m r.F` then entails `≥n s.F'` when `m ≥ n`,
`r ⊑ s` and `F ⊑ F'` are in the closures, and it contradicts a maximum `≤k s.B` when
`m > k`, `r ⊑ s` and `F ⊑ B`. The normalization adds three kinds of relation from this.

- The minimums of one bound, role and filler have a representative, the least name. Each
  of the others is told equivalent to it and has it as its only blocker.
- Between representatives, the name of a minimum is told to be below the name of each
  minimum that it entails, and it is a blocker of that minimum. Two representatives that
  entail each other, through equivalent roles or fillers, are told equivalent, and the
  smaller name blocks the larger.
- The name of a minimum and the holder `M` of a maximum that it contradicts are a
  disjointness set, for the representative and for the names it stands for.

A name that is told below one of its blockers keeps no inclusion into witnesses of its
own. That is every name beside its representative and the larger name of two
representatives that entail each other. Wherever such a name holds, as a unit or under a
clause, the told inclusion gives the blocker under the same clause, and the blocker's
witnesses are the name's.

The blockers lie within a strict order, strict entailment and then the names, so they have
no cycle. The normalization checks this, and a run would block nothing if a cycle showed.
The relation is not transitive as stored, since a name that is not a representative has
only its representative as a blocker. The relations are found through buckets of the
minimums and the maximums by role and filler, so the cost is the size of the closures and
not the square of the number of minimums. They number at most 1,048,576. Past that the
rest are left out, which is sound and complete, since each is a consequence that the run
can do without and a minimum without a blocker makes its witnesses. A name gets both
inclusions to its representative and the blocker together or none of them. The closures
poll the run's stop. The tables are charged to the estimates of `DlAx::bytes`,
`DlIx::bytes` and `DlSt::bytes`. This memory buys the run's speed on ontologies with many
related cardinalities, and on the two Protein Ontology files the peak resident memory with
the order is 319 to 403 MiB against 267 to 343 MiB without counting.

Two places in the engine read the tables. The first is blocking. A unit `G_2` that has
blockers and witnesses of its own does not derive `W_2` with its other told subsumers.
Its name waits in the context, in `DlSt::pending`, until the context has taken its units,
which is when `dl_rules::flush` runs, and it then derives `W_2` unless the context holds
as a unit a blocker of `G_2`. A clause `Γ → Δ ∨ G_2(x)` whose maximal literal is `G_2` skips the told
step to `W_2` when the context holds such a unit, and takes it otherwise. A context with a
waiting name is not idle, so a run that a deadline, a cancellation or the memory budget
stops in that window reports the context's class incomplete. The second place is a cut.
`dl_rules::derive` removes from the head of a derived clause an atom over `x` that is in a
disjointness set with a unit of the context. The cut applies only in a run with the order,
so a run without it derives what it derived before.

The argument has a part for the clause set and a part for the two rules. Let `T` be the
ontology after the structural transformation, with each positive minimum read as a
minimum, and `O` the clause set of the run without the order, in which each such minimum
is its conjunction of witnesses. `T` and `O` have the same consequences between the
ontology's classes by Kazakov's Lemma 2, as before. Its argument does not extend an
arbitrary model of `T`. It takes a model that refutes a subsumption, unravels it into a
tree, and colours the neighbours of each element with the fresh fillers from the root
down. An element's neighbours over a simple role are its predecessor and its successors,
the predecessor carries at most one filler of a group when the element is reached, and
the element is free in the fillers of its successors, so the colouring exists. Let `O⁺`
be the clause set with the names, the inclusions `G ⊑ W` and the relations. In one
direction every model of `O⁺` satisfies `T`, which the next paragraph shows for a weaker
clause set. In the other direction, the coloured tree model becomes a model of `O⁺` when
each `G` is read as the set of elements with `n` neighbours over `r` in `F`. Every such
element has its witnesses by the colouring, the told inclusions between names hold because
the closures hold in the model, and a name and a maximum's holder that it contradicts
share no element. So `O⁺` has the consequences of `T` between the ontology's classes and
no others.

For blocking, let `O'` be `O⁺` with each clause `G_2(x) → W_2(x)` replaced by
`G_2(x) → W_2(x) ∨ ⋁ G_1(x)` over the blockers `G_1` of `G_2`. `O⁺` entails `O'`, so `O'`
is sound. `O'` also entails every consequence of `T` between the ontology's classes. In a
model of `O'`, by induction on the blockers, which is well founded since they have no
cycle and are finitely many, an element of `G_2` has `n` neighbours over `s` in `F'`. It
either satisfies `W_2`, or it is in a blocker `G_1` for a minimum `≥m r.F`, has `m`
neighbours over `r` in `F` by the induction, and these are neighbours over `s` in `F'`
because the told inclusions are clauses of `O'`. So every model of `O'` satisfies each
axiom of `T` that states a minimum, and the other clauses of `T` are in `O'`. A context of
the blocked run is saturated for `O'` up to redundancy. Where a blocker holds as a unit,
the Hyper conclusion `Γ → Δ ∨ W_2 ∨ ⋁ G_1` contains that unit's atom and is redundant.
Where none holds when the step is taken, the run derives `Γ → Δ ∨ W_2`, which subsumes the
conclusion. A blocker that becomes a unit after `W_2` was derived leaves sound clauses
behind, and a unit is never withdrawn. For a name `G_2` that is told below a blocker
`G_1`, the clause `G_2(x) → W_2(x) ∨ ⋁ G_1(x)` of `O'` is subsumed by the told clause
`G_2(x) → G_1(x)`, so `O'` without it is the same clause set up to redundancy, and the run
leaves the name's witnesses out everywhere. Without this, a cardinality that two axioms
state made two groups of witnesses in a context that holds both only under a clause, such
as a hypothetical successor, where no minimum is a unit and nothing was blocked. The
paper's Theorem 9 holds for any set of DL-clauses and therefore for `O'`, with the order
of atoms unchanged apart from the rank of the names. The cut is the Hyper conclusion of
the clause, the unit and the disjointness set. It is a consequence of the context's
clauses and it subsumes the clause it replaces.
In a hypothetical successor and in a test context the units it reads are conclusions of
that context, relative to its core, and never the markers of a hypothesis.

An earlier form of the design shared the first `n` witnesses of a larger minimum with a
smaller one over the same role and filler. That is unsound with inverse roles. An element
whose predecessor is the third witness of the predecessor's minimum cannot count it as the
first or second witness of its own `≥2`, since the fillers are disjoint, so `≥2 r.F ⊓ ≤2 r.F`
at that element would be unsatisfiable when the predecessor is one of its two neighbours.
The order shares no filler. A blocked minimum makes no witnesses at all, and the model
reads its neighbours from the blocker's. A regression holds this case.

The measurements are of commit 35fb0f31 on `atlas`, with one process at a time in a 13 GiB
scope, as medians of five classifications in one process, and for Konclude 0.7.0 of three
processes. The whole run is the wall time of one process, with reading the file. The
taxonomies with the order equal HermiT 1.4.5.519's and Konclude's on both versions of the
Protein Ontology, where HermiT takes 392 and 228 s on the laptop. Without counting each differs from
them by four missing and four extra edges.

| File | Threads | Without counting | Counting without the order | Counting with the order | Whole run with the order | Konclude, classification | Konclude, whole run |
|---|---|---|---|---|---|---|---|
| ore_ont_6952 | 1 | 875 ms | 1,039 ms, falls back | 1,154 ms | 1,313 ms | 2,336 ms | 3,169 ms |
| ore_ont_6952 | 4 | 288 ms | 756 ms, falls back | 365 ms | 538 ms | 2,310 ms | 3,132 ms |
| ore_ont_10140 | 1 | 913 ms | 1,107 ms, falls back | 1,178 ms | 1,404 ms | 2,510 ms | 3,694 ms |
| ore_ont_10140 | 4 | 301 ms | 950 ms, falls back | 373 ms | 609 ms | 2,422 ms | 3,590 ms |
| ore_ont_6485 | 1 | 7.0 ms | 184 ms, falls back | 576 ms, falls back | 609 ms | 40 ms | 55 ms |
| ore_ont_6485 | 4 | 6.5 ms | 245 ms, falls back | 688 ms, falls back | 719 ms | 37 ms | 60 ms |

On four threads the two Protein Ontology files classify 6.3 and 6.5 times faster than
Konclude and their whole runs are 5.8 and 5.9 times faster. On one thread the
classification is 2.0 and 2.1 times faster. The run with the order derives 23,282 and
27,078 conclusions of the bound's 262,144 on one thread, and it costs 1.24 to 1.32 times
the run without counting. On four threads it is also faster than the counting run
without the order, which spends its attempt before it classifies again.

The 207 development files of the ORE 2015 DL sample were classified with counting on
four threads on `atlas`, without the order and with it, with 300 s and 13 GiB for each
run. The reader refuses ore_ont_9881, and ore_ont_7192 passes the memory limit in both
runs. The other 205 give a taxonomy in both runs, and the two taxonomies are the same
bytes on 203 of them. The other two are ore_ont_6952 and ore_ont_10140, which fall back
without the order and finish with it. Five files attempt counting, and ore_ont_6485,
ore_ont_14817 and ore_ont_9663 pass the bound with the order as without it, in 646
against 257 ms, 6.7 against 6.5 s and 3.8 against 4.1 s for the whole run. No run with
the order passes its time where the run without it finishes. The runs are judged against
HermiT's taxonomy for 67 files and against Konclude's for 135. Konclude cannot parse
ore_ont_15516 and ore_ont_15993 and passes 300 s on ore_ont_7192, and the comparison
passes its memory on ore_ont_10621, so those have no judgment. With the order 85 runs
equal the reference and 117 leave axioms out and answer only entailed subsumptions,
against 83 and 119 without it, and no run answers a subsumption that the reference does
not entail.

Both sweeps were repeated after a context [gives way](#the-disjunctive-rules) to the
contexts it woke. Each of the 205 taxonomies is the same bytes as in the sweep before
it, with the order and without it, so the same two files finish only with the order and
the judgments stand. The three files that pass the bound take 232 against 208 ms, 6.9
against 6.5 s and 3.8 against 3.7 s for the whole run with the order and without it.

The order is on by default for a run that counts because of these measurements. Two more
files of the sample finish with it and none that finishes without it stops finishing. Of
the 296 generated ontologies below, 270 finish with the order and 255 without it on one
thread. Sixteen finish only with it, and one, `fz-0081`, finishes only without it, in
218,013 conclusions against 1,671,639 with the order. Of the 206 fixtures with a minimum
or exact cardinality of two or more, 202 stay within the bound on one thread with the
order and the same 202 without it. No answer differs from a finished run without the
order. Its measured costs are ore_ont_6485 and `fz-0081`.

When the order became the default, a context did not yet
[give way](#the-disjunctive-rules) to the contexts it woke. Two fixtures and 26 generated
ontologies then finished only with the order, and none finished only without it.

ore_ont_6485 remains a measured limitation. Its counting attempt still passes the bound
with the order, and the failed attempt is dearer. The classification with counting
requested, which is the attempt and the run without counting together, takes 157 against
123 ms on one thread on `atlas`, 1.28 times as long, and 205 against 170 ms on four, 1.21
times, as medians of three interleaved rounds of five classifications. Before a context
gave way to the contexts it woke, it took 576 against 184 ms on one thread, 3.1 times as
long, and 688 against 245 ms on four, and the counts of derivations in the rest of this
paragraph are of that run. With a context that gives way, the attempt on one thread
spends its 262,144 conclusions differently. The Pred rule and the lazy witnesses make
60,038 and 45,432 of them, which are cheap, and the universal rule and the qualified
roles make 50,841 and 9,688. A
caller for whom that attempt matters sets `Limits::number_order` to false. The order removes the cheap
derivations of the disjointness rule, which fall from 110,800 to 1,078, and the bound is
then spent on the universal rule and the qualified roles, which rise from 30,563 and
10,036 derivations to 118,258 and 68,900 and cost more each. What remains is the
alternative `H` of a definition `=n r.F ⊓ … ⊑ C`. A context with `n` qualified neighbours
derives `y ∨ H` with `H` maximal and makes `n + 1` hypothetical witnesses. The file has
nine such restrictions over the symmetric bond roles, and a witness has a known neighbour,
its predecessor, so it derives the same alternatives. Three hypothetical successors
derive 69,109, 63,521 and 58,201 of the attempt's 281,121 conclusions. Two ideas are
untried. A minimum could be blocked by a strictly stronger one that holds under the same
clause and not only as a unit. A positive `∃r.F` whose negative name the context already
holds could stay unexpanded, which needs a proof that the name is witnessed in the model.
Both, and the default of counting, are item 896. Two other changes to the clause
set were measured with the order and are not kept, one name per compound expression and
a name of its own for a negative existential restriction, as
[PERFORMANCE-EXPERIMENTS.md](../../PERFORMANCE-EXPERIMENTS.md) records.

This item also found that a counting run missed a maximum over a compound qualifier
that no other axiom names, with the order or without it, which
[item 897 repairs](#compound-qualifiers-of-number-restrictions).

The regressions are in `tests/dl_number_order.rs` and in the unit tests of `dl_norm.rs`
and `dl_rules.rs`. Four hand cases pass the bound without the order and finish with it,
the inherited cardinality over a wider filler, the cardinality stated twice, equivalent
and wider roles, and recursive cardinalities over a symmetric role and an explicit
inverse. The others cover the told contradiction with a maximum, fillers equivalent
through `owl:Thing`, intersection and union qualifiers, unqualified minimums, unsatisfiable
fillers, minimums under clauses, a refused axiom, every bound on the relations and the
predecessor case above. One case runs with the default limits and counting, where it
finishes, with the field set to false, where it passes the bound, and without counting,
where the field changes nothing. The 206 fixtures of item 603 with a minimum or exact cardinality
of two or more run with the order on one thread and on four with the eager strategy. Each
answer is among HermiT's subsumptions, each case taken whole has exactly HermiT's, no case
that stays in the bound without the order leaves it with the order. When the order was
added two more stayed in it on one thread, `shq-041` among them. Both now stay in it
without the order too, since a context gives way to the contexts it woke, so of item
729's two fixtures `shiq-028` remains. A fuzz of 300 generated ontologies with related cardinalities, role
hierarchies, an equivalent, an inverse and a symmetric role, judged against HermiT on the
296 that HermiT finishes in 60 s, gives 264 equal and 31 sound answers with the order on
one thread, 265 and 30 on four, and 238 equal and 57 sound without it. Every ontology
that finished without the order in these runs finished with it. The one that differs, with the order
and without it, is the ontology of the qualifier gap above, which item 897 repairs. The fuzz also found the
cardinality stated twice under a clause, which passed the bound with the order before a
name beside its representative lost its own witnesses, and that ontology is a regression.

#### Compound qualifiers of number restrictions

This subsection records what item 897 delivers. The qualifier of a maximum `≤n r.F`, and
of item 728's clause for a minimum or an exact cardinality in a negative position, is in
a negative position. A neighbour is counted once it derives the qualifier's negative form
`B` with `F ⊑ B`, through the qualified role `r_B`. For a class, a union, a complement and
the other forms the normalization makes `B` an atom. For an intersection and an
existential restriction it makes an expression, which the rules build only where the left
side of an inclusion or the body of a clause names it. A qualifier is neither. So a
neighbour that held `P` and `Q` never derived `P ⊓ Q`, and the maximum counted no
neighbour unless another axiom happened to name the same expression. `A ⊑ ≥2 r.C`,
`C ⊑ P`, `C ⊑ Q` and `A ⊑ ≤1 r.(P ⊓ Q)` left `A` satisfiable in a run that reported
itself complete, and so did a maximum over `∃s.C`.

The normalization of a counting run now gives a compound qualifier a name, for a maximum
of one or more and for item 728's clause of a minimum or an exact cardinality of two or
more. A maximum of zero is a universal restriction and an exact cardinality of one in a
negative position is an existential restriction, and both keep their forms.
`dl_norm::Norm::qualifier` replaces an intersection by a fresh class `N` with the told
inclusion `P ⊓ Q ⊑ N`. It replaces an existential restriction `∃s.F`, as a qualifier or
within one, by a fresh class `N` with `F ⊑ ∀s⁻.N` in a run without the root. That is
`∃s.F ⊑ N` in the form the calculus gives an existential restriction in a negative
position, and the unfolding of universal restrictions carries it over a role that is not
simple. The expression `∃s.F` itself is not used, because it is also the positive
restriction. A context makes a successor term wherever it derives that expression, so a
neighbour that derived it from a successor of its own would make a copy of that successor
for the at-most rule to count. Registering the expression was tried first, and the
fixture `shq-015` then passed its bound on one thread and not on four. With the name it
derives 16,003 conclusions on one thread. A rooted run has no rule for a universal
restriction over an inverse role beside a nominal and a maximum, so there the name is told
above the expression, `∃s.F ⊑ N`, as an axiom of the ontology would state it.

The names keep the answers of the ontology. Every model of the ontology extends to one of
the normalized axioms by reading each name as the expression it stands for, the inner
names first, and in every model of the
normalized axioms the qualifier is below its name, so the maximum over the name gives the
maximum over the qualifier and item 728's clause over the name gives the clause over the
qualifier. This is the structural transformation of a negative position, which the
normalization applies to every other expression.

Counting the neighbours that were missed costs work on ontologies that have such
qualifiers. Of the 150 counting fixtures, 26 derive a different number of conclusions on
one thread at a bound of 8 million. `shiq-038` derives 815,758 where it derived 246,957,
through a minimum of three over `∃r1.C3` in a negative position, so it now passes the
default bound of 262,144 and classifies again without counting. It finishes with HermiT's
answer at the raised bound of its test. `shiq-013` derives 15,468 against 62,548 and
`shq-015` 16,003 against 12,188. No other fixture moves across the default bound, and
item 729 follows `shiq-038`. Its trace shows the cost in the at-most rule, which makes
493,787 of the conclusions, with 182,774 from paramodulation, spread over six contexts
that each derive more than 50,000. The same work appears on four threads, so the order
in which the contexts run does not cause it. On the ORE 2015 DL development set no
taxonomy changes. Its 205 files that give a taxonomy with counting on four threads on
`atlas` give the same bytes as before the names, with the names alone and with a context
that gives way, and ore_ont_6485, ore_ont_14817 and ore_ont_9663 pass the bound as
before. With the names alone the two versions of the Protein Ontology classified in 375
and 382 ms against 380 and 371 ms in one run each, which is not a benchmark. With a
context that gives way they classify in 363 and 373 ms against 379 and 383 ms with the
names alone, and ore_ont_6485's classification with its failed attempt takes 210 against
715 ms, as medians of three interleaved rounds of five classifications on four threads.

The regressions are in `tests/dl_tier_d.rs` and `tests/dl_nominals.rs`. They cover a
maximum over an intersection, over an existential restriction on a simple, an inverse and
a transitive role, over one within the other, item 728's clauses over an intersection, a
predecessor that is in the qualifier, a qualifier that is also a positive restriction,
the generated ontology that was reported consistent, and a rooted run over individuals.
Each fails without the names. The fuzz of item 894 gives 270 equal and 26 sound answers on
one thread and on four, with no answer that differs from HermiT's. With the names and
without the step in which a context gives way it gave 265 and 31 on one thread and 266
and 30 on four, and the 68 of its ontologies whose work the names change all stayed on
the same side of the bound.

A second fuzz generates 300 ontologies whose maximums, exact cardinalities and
cardinalities in negative positions have compound qualifiers, over simple, inverse and
transitive roles, and judges them against HermiT on the 295 that HermiT finishes in 60 s.
Without the names 25 answers differ from HermiT's and are reported complete, 267 are equal
and 3 are sound. With the names none differs, and 292 are equal and 3 sound on one thread
and on four. `tests/fixtures/dl897` holds the 25 with HermiT's taxonomies, and
`tests/dl_fixtures.rs` requires HermiT's answer on each within the default bound on one,
four and eight threads.

Two ontologies that finished without the names now pass the default bound and classify
again without counting, with a sound answer, and the same test holds them to that on
every schedule. `qz-0202` derives 2,413,688 conclusions on one thread where it derived
217, and `qz-0082` passes a bound of 8 million where it derived 68. Their traces show the
cost in the equality rules of single contexts. In `qz-0202` the context of one class
processes 1,562 clauses and derives 323,051 of the run's 332,529 traced conclusions,
170,078 of them by the rule that derives the stored clauses with an equality of a
successor term again when a new clause puts that term in a class of a disjointness set,
and 129,934 by paramodulation. The work bound also counts the clauses these rules read.
In `qz-0082` four contexts derive about 460,000 traced conclusions each before the bound,
nearly all by the first of the two rules. Both do the same work on
four threads. No qualifier's name arrives late in these runs. The contexts count more
neighbours than before, and the equalities between that many successor terms are the
cost, which item 729 follows with `shiq-038`.

A third ontology, `qz-0048`, whose answer was wrong before, derived 2,122,545 conclusions
on one thread and 4,811 on four with the names alone, so it fell back on one thread and
finished on four. There the order of the contexts was the cause, and a context that
[gives way](#the-disjunctive-rules) to the contexts it woke removes it. The run on one
thread now derives 29,625 conclusions and has HermiT's answer. The difference that
remains between schedules is the subject of item 727.

#### Steps for a bound that every schedule shares

This subsection records what item 727 delivers. `Limits::dl_steps`, which is on by
default, makes every saturation of the disjunctive rules proceed in steps in a run that
has one of the three bounds below on its derived conclusions. With it such a run derives the same conclusions,
passes or keeps its bound on them, and so answers in full or falls back, in the same way
on every number of threads and every schedule. The memory budget, the deadline and a
cancellation still stop a run when their checks see them, and the Horn rules' fallback on
their memory budget is unchanged.

Three runs have such a bound. They are a run that counts, the run that starts again without
counting after it passed the bound, and a rooted run. A run that counts is one that
`Limits::counting` allows on an ontology with an axiom that needs counting. Any other run
of the disjunctive rules has the bound of item 990, past which the Horn rules answer. That
bound keeps the driver without steps, since the steps made the attempts that finish up to
1.9 times slower on eight threads on the laptop, so near it a schedule can decide whether a run on several threads
answers in full, as it can for the memory budget. A run of the disjunctive rules without
any bound finishes with the same answer on every schedule, so
the steps would change only its cost, and it keeps the driver without steps whatever the
option says. Counting and the root are off by default, so a classification under the
default limits runs as it did before this item. `dl_steps: false`, and `--no-dl-steps`
of `el_classify`, give a run with the bound the driver without steps. Its workers
deliver their messages during the saturation, with no step between them, a run near its
bound can then answer in full on one schedule and fall back on another, and an attempt
on several threads that passed the bound is repeated on one thread before the run falls
back.

A step works on a frontier of contexts. The frontier is a queue that the seeds start and
the roots join while it holds fewer than 4,096 contexts, and a step takes its first
4,096. In the first phase the workers activate those contexts. An activation reads only
its context's state and to-do list, and its messages for other contexts go to a list of
its worker, so that no to-do list changes during the phase. In the second phase the
workers deliver the messages. Each worker takes the destinations of its share and reads
every activation's messages in the order of the frontier, so a to-do list receives its
messages by the place of their sender in the frontier and, for one sender, in the order
it sent them, whatever the number of workers is. Between steps one thread sorts the
contexts that the deliveries woke and appends them to the queue, and compares the count
of derived conclusions with the bound. The count is then a sum over whole activations,
which no schedule changes. A run in steps compares that total with the bound nowhere
else.

An activation of a run in steps has a share of 256 clauses of its store, with the
minimums it released, and a share of 4,096 derived conclusions. The activation reads both
between two clauses, so they are thresholds and not limits. One inference can derive more
than 4,096 conclusions, and one release of minimums can pass 256. A context that used a
share and has clauses left gives way whoever else waits. It goes on in a later step, at
once when it has new messages and otherwise when no other context is active, since what
the others conclude can close it. A context sends the products of its links and
propagations when its to-do list is empty and before it gives way. The driver without
steps also sends them from a long activation when a worker is idle, which a run in steps
does not do, so the order of an activation's messages is the same on every schedule.

An activation also compares its own count of derived conclusions with the run's bound. It
does so between clauses and at each look at the bounds within an inference of many
conclusions, which comes after every 256 choices, whether the inference reserves memory
for its record of them or not. Once that count alone is more than the bound, the
activation ends its inference and marks the run past its bound. It decides this from its
own count, so no schedule changes it. The total is compared between steps only, so a run
can pass its bound by the work of one step before that comparison, which is the same
amount on every schedule.

The steps do not change which workers do the work. A step with two contexts or more is
shared among the workers when it has 128 contexts or when the activations of the last
step took 150 µs, its messages are delivered by all workers from 4,096 messages on, and
otherwise the calling thread does the step alone. The helper threads start with the first
shared step and wait between phases until the calling thread opens the next, so a small
ontology starts none. A worker that panics, in a caller's callback for example, stops the
run and leaves its phase as it unwinds, and the calling thread releases the helpers when
it unwinds, so the panic reaches the caller. The workers of the driver without steps stop
the run in the same way. Until this item the other workers of either driver waited
without end for the one that had panicked.

The argument that an activation is a function of its context's state and its to-do list
rests on three observations about a run without the root that uses the cautious strategy.
The rules read the index, their own context's state, the cores of contexts, which do not
change during a saturation, and clauses of the shared table by id. The ids of that table
follow the order in which the workers push clauses, and a receiver uses an id only to
read the clause, as a member of the set of clauses it has seen from a successor, and in
that successor's list, which is in the order of arrival. Nothing sorts by a table id or
iterates a table keyed by one. The subsumption tests between saturations make their
contexts on one thread in a fixed order. Two kinds of ids are not canonical. A rooted run
interns its literals in the order the workers meet them and compares them, and the eager
strategy interns contexts during the run. With the option a rooted run, and a run with
the eager strategy that has the bound, take one worker of the driver without steps, whose
order is fixed. Their worlds and their taxonomy keep the caller's threads. Item 951 gives
those ids a canonical order, so that these runs share their steps among the workers.
`run_with` does not repeat on one thread an attempt that passed its bound, since the
repeat would derive the same.

The review of the design found a defect of the driver without steps as well. An inference
too wide to make passed the bound by adding a quarter of the count's range, so four of
them between two checks wrapped the count to a small number, and the run ended as
complete without those inferences. The mark is now a flag that stays set.

The tests are in `tests/dl_steps.rs` and in the classifier's unit tests. The 150 counting
fixtures and the 27 generated ontologies of item 897 derive the same conclusions, store
the same clauses and give the same answer on one, two, four and eight threads, with
HermiT's answer where the run finishes. One generated ontology run 36 times on one, four
and eight threads gives one outcome at the default bound, at a bound equal to the
conclusions it derives and at one conclusion less, where every run falls back. A unit
test runs an ontology of more than 8,192 contexts with every step and every delivery
shared among the workers, on one, three and eight threads, and cancels runs at six
points, after which the same classifier gives the outcome of a new one. Eager and rooted
runs give one outcome on one, four and eight threads. Runs without counting on the `alc`
and `shi` fixtures, which have no bound on their conclusions, give HermiT's answer on
one, four and eight threads with the option and without it. A unit test reads from the
poll of a cancellation which driver calls it. A run that counts is in steps under the
default limits and without them with `dl_steps: false`, with one answer, and a run
without counting is without steps either way. A rooted run that counts falls back twice
past a bound of one conclusion with the option as without it. One inference that derives
3,600 new conclusions ends within 1,000 at a bound of 300, where its reservations of
memory did not look at the count. A callback that panics in a helper or in the calling
thread ends the run with the panic, with the step driver and with the driver without
steps.

On the 750 ontologies of the counting fixtures and the two fuzz sets, at a bound of 8
million, a run in steps gives the same work, stored clauses and taxonomy on one, three
and eight threads. On one thread it derives 16.16 million conclusions in all on the 722
ontologies that both drivers finish, against 16.24 million for the driver without steps,
and the same 714 ontologies stay within the default bound. One ontology that the driver
without steps does not finish within 8 million conclusions finishes in steps in 1.66
million. The differential runs against HermiT give 270 equal and 26 sound answers of 296
and 292 equal and 3 sound of 295, on one thread and on four, as the driver without steps
does. Of the fixtures that the section on compound qualifiers names, `qz-0048` derives
10,448 conclusions in steps and `shiq-038` 821,347, on every number of threads, where a
run without steps derives 29,625 and 815,758 on one thread and 13,319 and 914,090 in one
run on four.

The cost against the driver without steps was measured on `atlas`, one process at a time
in a 13 GiB scope, while other work kept the load average of the host between 2 and 6.
The times are medians of five classifications in one process, and for the five ORE 2015
DL files that attempt counting the median of three such processes for each driver, run in
turn. They compare the two drivers on one host and are not a published benchmark. The
table is of the classifier of commit 335fb9c7, which has the outbox of item 818, except
the row of ore_ont_14817 on one thread, which is of commit b351050c.

| File | Threads | Without steps | In steps |
|---|---|---|---|
| ore_ont_6952 | 4 | 359.3 ms | 373.8 ms |
| ore_ont_10140 | 4 | 364.2 ms | 382.0 ms |
| ore_ont_6485 | 4 | 203.9 ms, falls back | 77.6 ms, falls back |
| ore_ont_14817 | 4 | 6,090 ms, falls back | 3,401 ms, falls back |
| ore_ont_9663 | 4 | 3,349 ms, falls back | 2,551 ms, falls back |
| ore_ont_6952 | 1 | 1,134.0 ms | 1,134.3 ms |
| ore_ont_10140 | 1 | 1,163.1 ms | 1,165.0 ms |
| ore_ont_6485 | 1 | 157.7 ms, falls back | 213.1 ms, falls back |
| ore_ont_14817 | 1 | 132 s, falls back | 10.1 s, falls back |
| ore_ont_9663 | 1 | 6,910 ms, falls back | 7,118 ms, falls back |

On four threads the two files that finish take 4.0 and 4.9% longer in steps. The three
that fall back take 1.3 to 2.6 times less, since a run in steps does not repeat its failed
attempt on one thread. On one thread the two that finish take the same time within 0.2%,
the fallback of ore_ont_6485 takes 35% longer and that of ore_ont_9663 3% longer. The time
without steps for ore_ont_14817 on one thread is one classification, since five passed
600 s.

The cost on the two finishing files varies with the build by more than its size. The
classifier of commit b351050c measured 3.4 and 3.5% on four threads and 1.1 and 1.3% less
on one, in the same run as the table. A build of commit 451a6b76, whose saturation is
that of b351050c, measured 12 and 13% on four threads and 12% on one. Item 950 times the
files on the benchmark host.

With the steps the classification of ore_ont_6485 with counting requested, the failed
attempt and the run without counting together, takes 213 ms with the order between number
restrictions and 188 ms without it on one thread, 1.14 times as long, and 78 against 68
ms on four.

With the classifier of commit b351050c, the 149 counting fixtures that both drivers
finish at a bound of 8 million take 2,180 ms in steps against 2,046 ms on one thread in
sum, 6.5% longer, and 1,146 against 1,360 ms
on four, 16% less, where `sriq-004` derives 387,617 conclusions in steps against
2,171,241. `shiq-028`, which passes the bound, takes 4,370 against 3,505 ms on one thread
and 1,301 against 3,991 ms on four.

The steps cost most on the smallest inputs. The fixtures that take less than 10 ms
without steps, which are 144 of the 149 on one thread and 143 on four, take 9% longer in
steps on one thread in sum and 4% longer on four. Single fixtures go both ways, up to
0.79 ms against 0.17 ms on one thread and 8.7 ms against 3.6 ms on four. Item 950 follows
this cost.

A sweep of the 207 development files of the ORE 2015 DL sample with counting on four
threads gives the same bytes under the default limits as without steps on each of the
205 files that give a taxonomy. An earlier sweep, of commit b351050c with every
saturation of the disjunctive rules in steps, gave those bytes too, and its times follow. The disjunctive rules answer
92 of them within their bounds, in 22.2 s in steps against 22.7 s in single
classifications. No cardinality restriction on an object property and no functional,
inverse functional, disjoint or asymmetric object property occurs in 23 of the 92, so
they have no bound and keep the driver without steps. The file with the largest ratio of
the sweep is one of them, at 2.3 ms against 0.7 ms. At most the other 69 have the bound.
They take 16.4 s in steps against 17.8 s, and the largest ratio among them is 0.86 ms
against 0.33 ms.

The steps are the default for the determinism, at the cost this subsection states. On
four threads a run that finishes with counting takes 4 to 5% longer in the measured
build, and a run that falls back takes less, since it makes its attempt once. On one
thread the fallback of ore_ont_6485 takes 55 ms longer, and that of ore_ont_14817 takes
10 s against 132 s without steps. The times above are from one host under load, and
none is from the benchmark host.

### The datatype checker

Items 768 and 972 add a datatype checker that every tier of the classifier shares. This
subsection is their design, and its outcome follows as the phases land. The checker
decides, for one context, the data restrictions, data ranges, data cardinalities,
functional data properties and data-property keys that apply there, over the whole OWL 2
datatype map with its facets. The EL and Horn tiers call it for the meets and inclusions
of their data lattice and for the covers that would need a case analysis, Tier D and the
rooted run call it through a theory rule over the data atoms of each context, and the
worlds of the Horn rules with individuals call it for the values of keys and
functional properties. It is written from the OWL 2 Structural Specification §4, §5.7,
§9.3.6, §9.4 and §11.2, the Direct Semantics §2.2 and §2.3, and XML Schema 1.1 Part 2,
and the Hyper rule of the clause calculus. No other reasoner's source is read.

**Why it is needed.** On the ORE 2015 DL development files that the census of item 1272
found leaving axioms out for data, the datatypes in use are `xsd:int`, `xsd:string`,
`xsd:anyURI`, `xsd:double`, `xsd:integer`, `xsd:float`, `xsd:boolean`, `xsd:dateTime`,
`xsd:nonNegativeInteger`, `xsd:decimal`, `owl:real` and `xsd:positiveInteger`, in that
order of use. Only two files restrict a datatype with facets, and both use the four
bounds. No file uses `pattern` or a length facet, data unions or complements, keys or
`DisjointDataProperties`. Functional data properties appear in 21 files, exact data
cardinalities in 21 and maximum data cardinalities in 9. Item 972 is mostly `xsd:int`,
which is outside OWL 2 EL because its value space is finite, and the floating-point and
Boolean datatypes. The checker covers the whole map so that the remaining gaps are named
and small, and its first phases favour these datatypes.

**Values and identity.** A data value is compared by identity, as §9.3.6 requires of
cardinality restrictions and functional data properties, and a facet is compared by the
datatype's equality and order. The two differ in three places. The values `+0` and `-0`
of `xsd:float` or `xsd:double` are equal and not identical, so a functional property
with both values is inconsistent, while `minInclusive 0` admits both. NaN is identical to
itself and is excluded by every bound facet, since it is incomparable with every value.
Two date-times that denote the same instant with different offsets are equal and not
identical (§4.7). The checker therefore computes an exact value of each literal as its
identity key, with rationals of any size and date-times with years of any length and
seconds of any precision, and keeps the order that the facets use beside it.

**Families and value spaces.** The checker reads the map as disjoint families. Within a
family the datatypes nest or are intervals, and between families the value spaces are
disjoint. §4.2 states that `xsd:double`, `xsd:float` and `owl:real` are pairwise
disjoint, §4.5 and §4.6 state it of the two binary datatypes and of `xsd:anyURI` and
`xsd:string`, and the families of `xsd:boolean`, `xsd:dateTime` and `rdf:XMLLiteral` are
disjoint from the others, the first two as primitive datatypes of XML Schema. The
disjointness of `rdf:XMLLiteral` rests on its definition in RDF and is verified in phase 1. The families are these.

| Family | Datatypes | Representation |
|---|---|---|
| Real | `owl:real`, `owl:rational`, `xsd:decimal`, `xsd:integer` and its twelve subtypes | Four strata, the integers, the decimals that are not integers, the rationals that are not decimals and the reals that are not rationals. Each stratum holds a finite union of intervals with exact rational ends, each end open or closed. |
| Double, Float | `xsd:double`, `xsd:float` | A finite union of intervals over the values in their total order, with `-0` below `+0` and NaN as a separate point. |
| Plain | `rdf:PlainLiteral`, `xsd:string`, `normalizedString`, `token`, `NMTOKEN`, `Name`, `NCName`, `language` | Untagged strings by a set of lengths for each cell of the string chain. Tagged strings by a set of lengths for each tag, given by the longest basic language range of a finite map that matches the tag or by a default, so that a range minus a narrower range is represented. `rdf:langString` is the tagged strings alone. |
| URI | `xsd:anyURI` | Strings with a set of lengths and finite included and excluded values. |
| Hex, Base64 | `xsd:hexBinary`, `xsd:base64Binary` | Octet sequences with a set of lengths and finite included and excluded values. |
| Boolean | `xsd:boolean` | A subset of the two values. |
| Time | `xsd:dateTime`, `xsd:dateTimeStamp` | Values with an offset by intervals of instants, values without one by intervals of local times, and finite included and excluded identities. |
| XML | `rdf:XMLLiteral` | Opaque values, as today. |

The integer subtypes are intervals of the integer stratum with the bounds of XML Schema
1.1 §3.4, so `xsd:int` is the integers from -2147483648 to 2147483647. The string
datatypes form one chain, `xsd:string ⊃ normalizedString ⊃ token ⊃ NMTOKEN ⊃ Name ⊃
NCName ⊃ language`, since every language tag starts with a letter and holds only
letters, digits and hyphens. A cell of the chain is the strings at one level and not at
the next. A range in normal form is a union over the families of these pieces, with finite sets of
values included beyond them and excluded from them, which is closed under intersection,
union and complement. `rdfs:Literal` is the whole data domain, which the Direct
Semantics §2.2 requires only to contain the value spaces of the map. The normal form
therefore has one more region, the values outside every datatype of the map, which no
literal denotes and which is infinite for the checker, since an interpretation can
always add values there. A complement is taken within the data domain, as Table 3 of the
Direct Semantics defines `DataComplementOf`, so `DataComplementOf(xsd:int)` holds every
string, and the complement of the union of every datatype of the map is not empty.
A datatype definition is replaced by its range, which §11.2 keeps acyclic. Normal forms
are interned, and the meet, inclusion and emptiness of two ids are computed once and
cached. The values that a family's base holds as points are never held among the
included or excluded values. These are the numbers, the floating-point and Boolean
values, the date-times without an offset and the members of the classes of at most
three values, which are the empty string, the space, `:`, the tab, line feed and carriage
return, and the empty values of `xsd:anyURI`, `xsd:hexBinary` and `xsd:base64Binary`.
The tests check that equal ranges of these families have one id. A larger finite class,
such as `xsd:hexBinary` of length 1, can be held whole in the base by one form and value
by value by another, so equal ranges can have two ids. Ids are therefore cache keys, and
whether a range lies within another is decided through sizes, never by comparing ids.

**Facets.** The bound facets of the real, floating-point and time families cut their
intervals. A bound of the time family on a value without an offset follows the partial order of
§4.7, under which a local time and an instant are never equal, a local time is below an
instant only when it is below it with the offset `-14:00`, the latest instant it can
denote, and above it only when it is above it with `+14:00`, the earliest. A bound with
an offset therefore narrows the local times by fourteen hours on that side and becomes
strict there, and a bound without an offset does the same to the instants. Under the
§4.7 example, `minInclusive "1956-01-01T04:00:00-05:00"` holds `1956-01-01T10:00:00Z`
and not `1956-01-01T10:00:00`. The length facets
of the string, URI and binary families cut their sets of lengths, counted in characters
for strings and in octets for binary data, and `rdf:langRange` keeps the tags that a
basic language range matches. The tags are the well-formed language tags of BCP 47, which
RDF requires, so the range `a` matches no tag and keeps nothing, while `en-a` matches
`en-a-bb`. The `pattern` facet needs an automaton for an XML Schema
regular expression, which the first phases do not build. Until a later item builds it, a
range with a pattern, like an unknown datatype, is not passed to the checker, and its
axiom is left out and reported as used in part.

**Sizes.** Every range has a size, the number of its values saturating at 2^63, given as
a lower and an upper bound. The model's cardinalities are 32-bit numbers, so their sums
stay far below the saturation, and the checker declines a problem whose counts reach it.
A size also says whether the range is infinite, which only an unbounded source sets, so a
finite count that saturates stays a lower bound that removing values lowers. A
bounded interval of the integer stratum, every range of the floating-point and Boolean
families and every finite set of values have finite sizes. An interval of a dense stratum of the reals is empty when it holds no value of
the stratum, one value when it is a point of the stratum, such as `xsd:decimal[≥0.5,
≤0.5]`, and infinite otherwise, so `owl:real[≥1/3, ≤1/3]` without `owl:rational` is
empty. The seconds of a date-time are a decimal of unbounded precision, so an interval
of instants is infinite unless it is one instant, which has one value for each of the
1,681 offsets from `-14:00` to `+14:00`. A length of `xsd:anyURI` holds at least the strings of ASCII letters of that
length, which are relative IRIs, and at most every string of it, since XML Schema 1.1
makes every string a value and OWL 2 §4.6 calls the value space the set of IRIs. The cells of the string chain are small at short lengths. The empty string is
the only string of length 0, `normalizedString` without `token` holds only the space at
length 1, `string` without `normalizedString` holds three strings of length 1, and
`language` holds 52 of length 1 and caps each subtag at 8 characters. The checker counts
the cells of bounded length exactly up to a small length and reports an axiom as used in
part when it would compare a cardinality with a string or binary cell it has not
counted. The tagged strings are counted by tag. Each irregular grandfathered tag, such
as `i-klingon`, is a whole tag that nothing extends, so `rdf:PlainLiteral[rdf:langRange
"i-klingon", length 0]` has one value. A range that matches any other tag matches
infinitely many. The other strata, unbounded lengths and the reals are infinite. An
underestimate of a size would be unsound and an overestimate incomplete, so each
family's size function has its own tests at the boundaries.

**Literals.** A literal of a datatype in the map has the value of its lexical form,
which `datatypes::lexical` checks. Date-times are checked by the checker's own parser,
since `datatypes::lexical` refuses fractions of a second past eighteen digits, which
XML Schema 1.1 allows, and item 1289 corrects it for the other readers. The bound facets
of the time family are checked by the same parser. A language-tagged literal has a value
when its string is of XML characters and its tag is well-formed under BCP 47, in either
of its forms, and item 1290 brings the other readers to the same rule. A number or
date-time whose lexical form is longer than 4,096 characters is not passed to the
checker, which bounds the cost of its exact arithmetic, and its axioms are left out and
reported as used in part. A literal whose lexical form is outside the
lexical space of its datatype violates §5.7, so the ontology is not in OWL 2 DL and the
Direct Semantics gives the literal no value. Reading it as the empty range would draw
entailments from an input the semantics does not define, since its complement would hold
every value and a negative assertion with it would say nothing. Such a literal is
therefore never passed to the checker, nor is an XML literal or a literal of a
datatype outside the map, since the checker cannot tell which of them denote the same
value. Their axioms are left out and reported as used in part.

**Data restrictions are atoms.** Every data restriction of the ontology,
`DataSomeValuesFrom`, `DataAllValuesFrom`, `DataHasValue`, `DataMinCardinality` and
`DataMaxCardinality`, is an atom over x in every tier, and `DataExactCardinality` is the
conjunction of its minimum and maximum, since its complement is no atom. A minimum of
zero holds of every element and is replaced by `owl:Thing` before the atoms are formed,
and its complement by `owl:Nothing`, as the object cardinalities already are. A functional
data property is the restriction `≤1 d` on `owl:Thing`. A data value has no successors,
and the data property hierarchy, disjointness, ranges and functionality all apply to one
element, so the data successors of an element exist only inside the checker. The top
data property, which OWL 2 DL allows only as a superproperty, relates every element to
every value and adds no constraint between elements. A restriction over it is never an
atom, since `∃U.R` with R not empty holds of every element and would need a valid clause
with an empty body, so its axiom is left out and reported. A negative data assertion
`NegativeDataPropertyAssertion(d a v)` in a component of the checker is the atom
`∀d.¬{v}` on the constant a, a positive atom. With it, a constant whose values of d are
all ruled out is a conflict and needs no cover, and the negative assertions add no body
atoms. A component that stays with the EL lattice keeps the EL index's form `{a} ⊓
∃d.{v} ⊑ ⊥`. The complement of a data atom is a
data atom, `∀d.¬R` for `∃d.R` and `≤(n-1) d.R` for `≥n d.R`. The normalization keeps
one invariant that the theory rule needs. Every data atom in the body of a clause is of
the at-least kind, an existential, a value or a minimum, since a universal or a maximum
in a negative position becomes its dual in the head, as `∀d.R ⊑ A` becomes `⊤ ⊑ A ⊔
∃d.¬R`. No disjunction of at-least atoms is valid on its own, because an element without
data values satisfies none of them, so no valid clause with an empty body is needed.

**The local problem.** The checker decides whether a set of data atoms can hold of one
element together with the negations of another set. It counts values by type. A type is
a cell of the ranges involved, a nonempty intersection of each range or its complement,
together with an upward-closed set of the data properties involved, which are the
properties that relate the element to a value of the type. The unknowns are one
nonnegative integer for each type, the number of distinct values of that type. A minimum
`≥n d.R` says that the types whose cell is within `R` and whose property set holds `d`
sum to at least n, an existential is a minimum of one, and a maximum is the same sum
bounded above. A universal `∀d.R` and a range of `d` set to zero every type with `d`
whose cell is outside `R`, and `DisjointDataProperties` sets to zero every type whose
property set holds two of its properties. The types of one cell sum to at most the
cell's size. Each cell records, for every range involved, whether it lies within the
range or its complement, which its construction decides, so no test of inclusion rests on
comparing ids. The answer is whether this small integer program has a solution, which
the checker finds exactly by a bounded search over the types that some bound mentions.
The search bounds each count by what the other counts leave to each minimum and maximum,
and prunes a minimum whose counts all lie under a maximum with a smaller bound.
The counts make no assumption about which witnesses are the same value, so two
existentials under a functional property are satisfiable with one value in the meet of
their ranges, and large minimums need no enumeration of witnesses. A search past its
bound answers that the bound was passed, which the callers treat as their work bounds.

**The theory rule.** A context's clauses hold data atoms like any other atoms over x, and
the context's units held in the EL index count as clauses with an empty body. The rule
fires when a processed clause's maximal literal is a data atom P and when the context
gains a data atom P as a unit, whichever rule derived it. It takes P with the data atoms
that are maximal in the context's other clauses or are its units, and with the body
atoms. The body atoms are the data atoms with a negative occurrence anywhere in the
index. Such an occurrence is in the body of an ontology clause, on the left of a told
subsumer such as the `∃d.rdfs:Literal` of a domain, an operand of a conjunction, the
filler of a negative existential restriction, the qualifier of an at-most restriction, a
member of a disjointness axiom or a key clause. The index registers them where it
registers every negative occurrence, so that no position is missed. The atoms it takes are those
of P's component, the data properties connected by the hierarchy and by disjointness. It
asks the checker for the pairs of a set of maximal atoms containing P and a set of body
atoms whose negations make them unsatisfiable, minimal in both sets at once, so that a
pair with a smaller head set and a larger body set is kept beside one with a smaller
body set and a larger head set. For each pair and each choice of one clause for each
maximal atom, it derives the union of those clauses' other literals and the pair's body
atoms under the union of their bodies. A pair without body atoms is a conflict, such
as a minimum above the size of its range, one body atom is an entailment such as
`≥2 d.xsd:boolean ⊑ ∃d.{true}`, and two or more are a cover, such as `∃d.xsd:boolean
⊑ ∃d.{true} ⊔ ∃d.{false}`. Every body atom of a minimal cover is consistent with the
pair's set of maximal atoms and not entailed by it, so for a set of maximal atoms the
rule asks for covers only when two or more body atoms of the component pass both tests.
The tests are made against that set and not against every atom of the context. With
`Γ1 → ∃d.xsd:boolean` and `Γ2 → ∀d.{true}`, the atom `∃d.{false}` is inconsistent with
the context's atoms taken together, and the cover `Γ1 → ∃d.{true} ⊔ ∃d.{false}` is still
needed, since the universal holds only under Γ2. No filter compares the ranges of the
atoms, since a cover can hold an atom whose range contains the others, as `∃d.xsd:int[1,
2] ⊓ ∃d.xsd:int[2, 3] ⊑ ≥2 d.xsd:integer ⊔ ∃d.{2}` does. In a rooted run, after an
equality rewrites a term, the rule reads the atoms of the term that remains.

**Completeness.** The rule is Hyper over the clauses `P_1(x) ∧ … ∧ P_k(x) → Q_1(x) ∨ … ∨
Q_m(x)` that are valid in the datatype map, with the `P_i` among the ontology's finitely
many data atoms and the `Q_j` among its body atoms. Added to the ontology they give it
the same models, since an element whose data atoms are true cannot be given values with
its other body atoms false only when one of these clauses is violated, and a data atom
that occurs in no body triggers no ontology clause. The calculus with them is complete
by its own theorem, since they are ordinary ontology clauses over x, and the invariant
above, with the replacement of zero minimums and no atoms over the top data property,
makes the clauses with an empty body unnecessary. The theorem needs the run with the rule
to be the calculus on the larger ontology in its other rules too. The valid clauses are
over x alone, so they add no atom over y, no role atom and no qualifier of an at-most
restriction, which are what Pred and the at-most rule read from the ontology, and Succ
sends a successor the marker of every atom over its term whatever the ontology's bodies
hold. A data atom is an atom of the index like a named class, in the cores of
successors, in markers, in the conjunctions of the Horn rules, under the equality rules
and as a qualifier. A data atom over a successor or predecessor term therefore reaches
the context whose x it is, and the rule fires there. The tests take `A ⊑ ∃r.B`, `A ⊑
∀r.∃d.{1}` and `B ⊑ ∃d.{2}` with d functional, which entail `A ⊑ ⊥`, and two successors
of a functional property, one with `∃d.xsd:int[1, 2]` and the other with
`∀d.xsd:int[2, 3]`, whose merged successor has the value 2. The rule computes them lazily
and fires on exactly the premises that Hyper would take with them, since a valid clause
whose body atoms are not all maximal in the context cannot fire there. This makes the
unrooted run complete in the sense of the existing tiers. The rooted run takes the same
rule, and its completeness rests on the rooted calculus, which item 1231 records as not
proved. A search or a set of minimal pairs past its bound passes the work bound as the
case analysis of an inference does. An earlier design kept the data successors as terms
of the clauses, with data function symbols, equalities between data terms and a data
at-most rule over every set of n + 1 neighbours. It needs new rules and an argument that
theory literals entailed by later ones do not break the model construction, and it buys
nothing that the atoms do not, since no data term is shared between contexts.

**The EL and Horn tiers.** The data properties fall into components connected by the
hierarchy and by disjointness. A component whose ranges are all in the OWL 2 EL map and
that has no universal restriction, cardinality, enumeration of several values, facet or
`DisjointDataProperties` axiom stays with the EL lattice of `classify/data.rs`, which is convex
there, since every value space of the profile is infinite and no union of its ranges
covers another, and the lattice already meets the ranges under a functional data
property. An ontology whose data is within OWL 2 EL therefore runs the code it runs
today. Every other component is all data atoms. A lattice over the whole map
would not be convex, since `∃d.xsd:boolean` entails a class that both `∃d.{true}` and
`∃d.{false}` imply, and `∃d.xsd:integer` one that `∃d.xsd:integer[≤0]` and
`∃d.xsd:positiveInteger` imply. The Horn tier applies the theory rule to the data atoms
of its contexts. A context of the Horn rules holds only units, so its data atoms in a
component are one set S and the rule needs no minimal pairs. A conflict of S gives
`owl:Nothing`, and each body atom of the component that S entails is given as a unit,
which the Horn rules can take. A remaining body atom, one that the context does not
hold, is free when its only negative occurrences are told subsumptions whose subsumers
the context holds, since its truth then changes no conclusion. A class C is given when S
with the negations of the remaining atoms whose only negative occurrences are told
subsumptions by C or by units of the context is unsatisfiable, as for a Boolean property
whose two values both give C, and those atoms become free. A cover remains when S with
the negations of the remaining atoms that are not free is unsatisfiable. It needs a case
analysis, so the Horn tier reports its axioms as used in part and a run that Tier D takes
decides them there. With these rules the Horn tier is complete when it reports nothing.
Kazakov's canonical model has an element for each context, whose atoms are the
context's units. When no cover remains, each element can be given data values that
satisfy S and falsify every remaining atom that is not free. A data atom in a negative
position of an axiom is then true of the element only when the context holds it or the
subsumers it gives are units already, so the model satisfies the data axioms and has
the subsumers that the rules derived. Several successors and pigeonholes come out as
entailments of one body atom. A data restriction in a negative position whose negation is not Horn, a universal
restriction or a maximum on the left, stays with Tier D. On the 67 ORE 2015 DL files
that the census found leaving axioms out for 768 or 972, all 27 files with data
restrictions outside OWL 2 EL have them only in positive positions, so no negative
universal or cardinality keeps them from the Horn tier. Covers can still arise from
values on the left, such as a Boolean property whose two values define different
classes, and the Horn tier reports such a cover as used in part. A context that the run
finds unsatisfiable reports no cover, since it has every subsumer. A functional object
property joins the atoms of its successors in the context of their conjunction, so a
conflict, an entailment or a cover between atoms that different classes give is found
there. The worlds of the Horn rules with individuals apply the same rule to the copies of
the contexts they change. A world reads the last evaluation of each copy once it reaches
its fixpoint, and it skips unsatisfiable copies and every copy when its class has no
instance, so that what it reports does not depend on the order of its derivations. A
`DisjointDataProperties` axiom over properties of a checker component is a pair of
disjoint properties of the local problem. In a run with a checker component, a
`DatatypeDefinition` outside OWL 2 EL is taken when the checker converts its range,
since every property its datatype is used on is then in a checker component and reads the
definition through its atoms. `Stats::tier` still counts a data union, an enumeration of
several values, a complement on the left or a higher data cardinality as disjunctive, so
it reads `Disjunctive` for a run whose only such axioms are data restrictions, while Tier
D, which refuses every data restriction, takes no axiom of the run and the Horn tier
decides it.

**Tier D.** Tier D's eligibility drops its refusal of data roles and keeps its refusal of
the top data property. Data atoms are atoms of the index like named classes, ordered
above them by id, and the theory rule runs beside Hyper in every context that holds a
data atom.

**Keys.** A key constrains only named individuals (Direct Semantics Table 9), so it adds
nothing to a run without individuals, and Tier D without individuals ignores keys. In a
rooted run, a key `HasKey(K (p…) (d…))` relates two constants u and v of K with one
clause for the pair, `K(u) ∧ K(v) → u ≈ v ∨ D`, where D holds, for each key property,
the literal that u and v share no value of it. For an object property that literal is
the difference of their named fillers. For a data property the first phases take only
keys whose properties, and the subproperties of those, have no positive restriction
other than a single value, so that a shared value is the same value on both sides, as
the worlds take today. A hash of the constants by their key values finds the pairs.
Every other key is reported as used in part, since a range such as `xsd:int[5, 6]` on
one side could need the cover `∃d.{5} ⊔ ∃d.{6}` to decide the pair. The worlds of
the Horn rules with individuals report every key over a property of a checker component
as used in part, and the singleton case waits for the rooted run of item 768.

**The rooted run.** A data assertion `DataPropertyAssertion(d a v)` is the atom
`∃d.{v}` on the constant a, and its negative form is the atom `∀d.¬{v}` on a, as the
atoms paragraph gives it. Hyper binds x to a constant, and the theory rule does the same, so each context
decides the ground data atoms of each constant as it decides those over x.

**Representation and incremental checking.** A run builds the data tables only when its
index has a data atom. A context then keeps its processed clauses with a maximal data
atom by the component of the atom's property, with the cached answers of the checker for
the sets it has asked about, and the run keeps the cells of each component's ranges. A
new maximal data atom is checked only with the atoms of its component, and the answers
for the same sets of atoms are shared by every context through a run-level cache. The
checks and the minimal pairs count toward the run's work bound and memory budget as the
case analysis of an inference does.

**Cost without data.** An ontology without data atoms builds no data tables, interns no
ranges and adds no rule. The run chooses its clause loop once, before saturation, so a
run without data atoms executes no added test per processed clause. Phase 3 compares instruction counts of the
disjunctive fixtures with and without the change, and its timing A/B runs on a quiet
Intel host.

**Cost with data.** Item 768's experiment took the EL data restrictions as atoms in Tier
D, which is this design without the checker, and found that sending these files to Tier
D costs far more than leaving their data axioms out, 442 ms against 6 ms on
ore_ont_3231. It also found that ore_ont_8273 did not finish in 120 s and that
ore_ont_13306 left more axioms out, because Tier D leaves out Horn axioms that the Horn
rules took. Where that cost goes is not measured, so phase 3 first profiles `2f8080e5`
on these files with `dl_contexts`, `dl_tests` and `dl_testing`. Phase 2's Horn tier
takes the files whose only case analysis would be in their data. For the rest, phase 3
weighs a hybrid in which the Horn tier records the contexts where a reported cover or
other left-out axiom fired. A class whose Horn saturation reaches no such context would
be complete, and Tier D would test only the classes that reach one, with the union of
the two tiers' subsumptions as the answer, since both are sound. The proof that a class
reaching no such context is complete is that phase's obligation before it is kept.

**What stays out.** The `pattern` facet until its automaton is built, ill-typed literals,
XML literals and datatypes outside the map stay out of the checker and are reported as used in part,
and so do cardinalities compared with string cells the checker has not counted. The top data
property stays with the EL index and declines Tier D, as the top object property does.
Keys outside the forced singleton case are reported as used in part.

**Phases and tests.** Phase 1 builds the checker as a module with unit tests from the
W3C OWL 2 test cases that involve datatypes, among them `Datatype-Float-Discrete-001`,
`Plus and Minus Zero Integer`, `Minus Infinity is not in owl:real`,
`Qualified-cardinality-boolean`, `inconsistent-integer-filler`,
`Contradicting-dateTime-restrictions`, `New-Feature-DataQCR-001`, the
`New-Feature-DisjointDataProperties` and `New-Feature-Keys` cases and
`string-integer-clash`, with tests of the zeros, such as `minExclusive "-0"` excluding
`+0`. A differential test compares the checker's answers on random small sets of atoms
with a brute-force model finder over small finite stand-ins for the value spaces, and a
second one does the same for the theory rule's clauses. Phase 2 is item 972 with the
Horn tier's data atoms, checked against HermiT on the thirteen OML probes and compared
before and after on the 222 non-held-out census files whose default run left out data
axioms, with HermiT checking the consistency changes, as the Outcome above records. Phase 3 is item 768, data atoms in Tier D and the rooted run, with the census
tool run on the 61 files before and after.

#### Reduced automata of the disjunctive rules

The disjunctive rules unfold a universal restriction `∀r.C` on a property that chains or
transitivity reach by the automaton of `r` without its ε-transitions, and a negative
existential restriction `∃r.F ⊑ G` as `F ⊑ ∀r⁻.G`. Each state `s` becomes a fresh class
`X_s`, the holder of the restriction gets `X_start`, each transition by `l` from `p` to `q`
gives `X_p ⊑ ∀l.X_q`, and each final state gives `X_s ⊑ C`. The restriction itself stays,
which is sound on the direct links. Item 712 reduces each automaton before the unfolding
with the first two reductions of [the Horn rules](#the-horn-rules), and
`Limits::dl_automata` or `SPARKLES_DL_AUTOMATA` set to 0 or 1 turns off both or the second
for a comparison. The default is 2, which applies both.

`X_s` stands for the universal restriction over every word that `s` accepts, in which a
link meets a letter `l` when its property is under `l`. The rules read universals in that
way. A universal on `l` reaches each successor whose property is under `l`, through the
existential restrictions and the stored clauses whose maximal literal is a role under `l`,
each predecessor whose link is under the inverse of `l`, through the propagations over the
backward links, and in a rooted run each constant that a link under `l` reaches. Every one
of these tests reads the index's property hierarchy, which closes the same property
inclusions as the automata, closed under inverses, and in which every property that an
automaton names has its inverse. The tables that let a role reach a successor or a
predecessor at all hold every superproperty of a role they hold. A test that holds under
`l` therefore holds under every superproperty of `l`.

1. **States that accept the same words become one.** The coarsest partition of the states
   whose blocks are final alike and have transitions by the same properties into the same
   blocks puts states with the same language together. Each block accepts what each of its
   states accepted, so every class of the quotient stands for the universal that the class
   of any of its states stood for, and the quotient's axioms have the consequences of the
   axioms they replace. A merge that ignored which states are final would make the start
   final and put the holder under the filler.
2. **A transition that another covers by a superproperty goes.** When a state has
   transitions by `l` and by `l'` into the same state and `l` is under `l'`, the transition
   by `l` goes, and of two transitions by equivalent properties the one with the lower id
   stays. The state accepts fewer words, but each word it no longer accepts differs from
   one it accepts only in letters that are subproperties of the accepted word's letters,
   so the universal it stands for is the same. `X_p ⊑ ∀l.X_q` follows from
   `X_p ⊑ ∀l'.X_q` and `l ⊑ l'`, and the rules derive each of its instances from the kept
   axiom, since they apply the kept universal over every link under `l'` in both
   directions. Dropping the transition by `l'` instead would lose the paths whose link is
   only under `l'`.
3. **Transitions by properties that no link can have stay.** The Horn rules' third
   reduction rests on the list of properties that their links and loops can have, argued
   for their rules R3 and R4. The disjunctive rules make links from the heads of clauses
   and from hypothetical successors as well, and the list is not argued for them, so their
   automata keep these transitions.

The two reductions alternate until neither applies, within the Horn rules' bound of steps.
They read only the property axioms, so the positions of an unfolding's fresh classes, which
a kept run reuses, depend on the property axioms and the level alone, and the level is one
of the options that a kept run must share with the last one.

`tests/dl_automata.rs` checks the reductions. Eight hand-written cases combine transitive
properties under transitive properties, inverses, negative existential restrictions, a
chain, equivalent properties, a nominal and a qualified maximum with unions. In the
counting case two links of a simple subproperty merge under the maximum, and the merged
successor needs the universal that the reduced automaton carries over them. Each is taken whole by the
disjunctive rules at every level on one and four threads with the answer of the reference
reasoners, HermiT 1.4.5 and Konclude 0.7.0, whose union is the reference because each
misses subsumptions that the other finds. The reduced automata derive fewer conclusions on
each case. A merge that ignores which states are final and a covered transition dropped
by the superproperty each fail all eight, the backward cases through a holder that only a
union gives, so that the EL rules cannot answer them. 200 generated cases with transitive
properties, chains and inclusions between properties and between their inverses are taken
whole at levels 0 and 2 on one thread with the reference answer, and the reduction changes
the conclusions of 107 of them. The wrong merge fails 172 of them and the wrong drop none,
so the hand-written cases alone guard the second reduction. Levels 0 and 2 give one answer on every fixture of items
603, 897 and 712 on which neither run falls back, and none of the fixtures of items 603
and 897 has an automaton that the reduction changes. A classifier kept across runs answers
as a fresh one at every level while a universal is added and removed, reuses its contexts
between runs at one level and starts afresh when the level changes, since the fresh
classes of an unfolding are numbered by its automaton's states.

### Nominals in the disjunctive rules

This section is the design of item 603's sixth phase, which extends the disjunctive rules
to nominals. The rules it describes are built, apart from keys and the universal property,
which item 805 builds. They run when `Limits::rooted` asks for them, and it is off by
default, because the completeness of the calculus as item 804 departed from the journal's
rules is not proved and two of the 50 `shoi` fixtures of the phase's gate pass the work
bound of a rooted run, which items 1061 and 1075 follow. Without the option the disjunctive rules
refuse ontologies with individuals and nominals as the section above says, and every run
takes the paths it took before these rules existed. The option changes nothing for an
ontology without individuals, nominals or the universal object property. The measurements of the journal's rules are at the end of
this section, and the departures of item 804 that follow them are the design the run
follows where this section says so.

A run of the disjunctive rules with nominals uses the general rooted calculus of Tena
Cucala, Cuenca Grau and Horrocks in the journal version, *Pay-As-You-Go Consequence-Based
Reasoning for the Description Logic SROIQ* of Artificial Intelligence in 2021. Its Tables 2
and 3 and its Algorithm 1 are the reference, and the conference version of IJCAI 2018
explains the design but is not mixed into the rules. The journal's Horn variant, Algorithm
2, is not used. Its soundness holds for one query body under the eager strategy, so it
cannot answer the conditional clauses that a classification run shares between its
contexts. Algorithm 1 is exponential on ELHO in the worst case, which is why OWL 2 EL runs
with nominals keep the worlds of the EL classifier and Horn runs with individuals stay with
item 692. A rooted run never enters a world, so no run mixes the two mechanisms. The EL
rules still serve as the calculus's inferences on units in every context other than the
root, as they do in the rules above.

The rooted run takes `ObjectOneOf` and `ObjectHasValue` in class axioms and the object
assertions of the ABox, which are class assertions, object property assertions, negative
object property assertions and same and different individuals. A class assertion `C(a)` is
read as `{a} ⊑ C`, `r(a, b)` as `{a} ⊑ ∃r.{b}`, `¬r(a, b)` as `{a} ⊑ ∀r.¬{b}`, and same
and different individuals as equalities and inequalities of constants. The ABox adds little
to the calculus once the root exists, since a nominal in a class axiom already gives the
root ground clauses, such as `⊤ → B(o)` for `{o} ⊑ B`. The run also takes keys over object
properties and the universal property, as below. Data properties wait on item 768, and self
restrictions and reflexive properties stay refused. An irreflexive property is left out and
reported, and the run takes the other axioms, as it does with a minimum above one when
counting is off. The property is left out because a nominal or an equality of constants can
close a loop, as `r(a, a)` does, and the rules above check irreflexivity only on simple
properties without loops. The Nom rule with its generated nominals is not built. Nom fires only when a nominal,
an inverse property and an at-most restriction meet, so the run refuses that interaction. A
symmetric or inverse functional property counts as an inverse for this test, and so does
the symmetric property that replaces the universal property.

The terms of a rooted run are `x`, `y`, `f(x)`, the constants `u` of the named individuals,
and `f(u)` in the root. Besides the literals over `x`, `y` and successor terms of the rules
above, the run needs the ground atoms `B(u)` and `S(u, v)`, the role atoms `S(x, u)`,
`S(u, x)`, `S(y, u)` and `S(u, y)`, the atoms over `f(u)` in the root, and the equalities
and inequalities `x ≈ u`, `y ≈ u`, `u ≈ v` and `f(x) ≈ u`. Clause bodies may hold ground
atoms and equalities of constants. The packed literal has one free kind, which marks an
interned literal whose record holds the predicate, the terms and the polarity, so that the
seven present kinds keep their encoding and a literal stays eight bytes. Every test of a
literal's kind treats the new kind by its record.

One root context has the empty core. Its clauses hold no `x`, may hold `y`, and are the
place where facts about named individuals meet. A singleton class `{u}` is a proxy class
`N_u` with the clauses `N_u(x) → x ≈ u` and `⊤ → N_u(u)`, so every named individual exists,
including one that only a class axiom names. A clause `⊤ → ⊥` in the root makes the
ontology inconsistent.

Every context of a rooted run uses an order that is admissible in the sense of the
journal's Definition 7, built as its Appendix A builds it from one total order on function
symbols and constants, with one strengthening that item 804's departures below describe.
Two ground literals are compared by one order that every context shares, and otherwise
a function-free atom that mentions `y` or a constant is above only what Definition 7's
fifth condition allows. Named classes over `x` are incomparable with one another and with
every ground atom. Each context of a named class is then a query
context that meets condition C2 of the journal's Theorem 2, and under the journal's rules
its unit subsumers are complete without the candidate tests of the rules above, whose
model construction ignores ground atoms. With item 804's departures below that
completeness is the open obligation they record. A clause can have several incomparable maximal literals, so the clause store
indexes every one of them. A clause keeps a canonical literal order for hashing, apart from
the order that selects its premises.

The rules are those of the journal's Tables 2 and 3, written in Tier D's literals. A
premise is a clause whose selected literal is maximal, which means that no other literal of
its head is greater.

| Rule | In the rooted run |
|---|---|
| Core | A context's core atoms are units, as now. The root's core is empty, and a hypothetical successor's id is not a core atom. |
| Hyper | In every context the ontology clause's `x` is `x` or a constant, so the rule matches ground atoms wherever they are. The journal binds `x` to a constant in the root only, and the departure below says why every context does. In the root `x` is always a constant. |
| Eq, Ineq and Factor | They run whenever the run is rooted, with counting on or off, and rewrite constants as well as successor terms. Eq keeps the journal's conditions on the rewriting equality, on a rewritten `x` and on rewriting every occurrence of a constant in the root. |
| Elim | Subsumption and tautology elimination work as now. A marker `A → A` stays. |
| Join | It resolves a maximal ground atom or equality of constants in one clause's head with the same literal in another clause's body, in any context. It is indexed by ground body literal so that either premise can arrive first. |
| Succ | It creates or reuses a successor for a maximal literal over `f(x)`, or over `f(u)` in any context, where the edge also records the constant. One edge must cover every successor trigger of the journal's Definition 4, the ground atoms and constant equalities included. The ground triggers are every ground atom and equality of constants, a superset of Definition 4's, which only adds markers that no inference asks for. |
| Pred | A successor's clause carries its ground body atoms and its ground head literals to the predecessor unchanged and resolves its other body atoms there. Along an edge that records a constant the predecessor reads `y` as the constant and `x` as `f(u)`, and also resolves the successor's core atoms against its own maximal premises, since such an edge holds only for its constant. |
| r-Succ | Not used. A context reasons about its own ground literals, as the departure below says, so the root holds no markers. |
| r-Pred | Only for the root's processed clauses with an empty body, which reach the contexts registered for one of their constants, as below. A root clause with a body stays in the root, where Join resolves its ground body literals. |

The journal's r-Pred sends a root clause without a body to every context. The rooted run
sends it only to the contexts that hold one of its constants in a maximal literal or in a
body, and a context registers with the root for the constants of every maximal literal of
its processed clauses and of every ground literal in their bodies. A context that resolves
such a clause with one of its own clauses has a premise that mentions one of the clause's
constants, so it already holds the clause. Some inferences
are still left out, such as the marker that Succ would pass on to a successor of a context
that never mentions the constant. Their conclusions matter only to a context that mentions
the constant, and that context takes the clause itself once it does. The journal's system
Sequoia holds back asserted ground facts from a context in a similar way.

A minimum restriction above one in a positive position is not weakened to an existential
restriction in a rooted run with counting off, since `A ⊑ ≥2 r.{o}` makes `A`
unsatisfiable. Such an axiom is reported as left out instead. With counting on, a minimum
restriction gets inequalities between its successor terms, `M(x) → f_i(x) ≉ f_j(x)`, in
place of the globally disjoint fresh fillers of the rules above, which a nominal can make
unsound. Three individuals with `M ⊑ ≥2 r.⊤` and `∀r` over two of three shared constants
each show it, since fillers disjoint everywhere would ask for two colours on a triangle.
`ObjectHasValue(r, u)` is `∃r.{u}`, and an enumeration of several individuals in a
positive position is a disjunction of their proxies.

A key `HasKey(C (r_1 … r_m) ())` over object properties holds only of named individuals, as
the OWL 2 Direct Semantics says. The run reduces it to class axioms in the calculus's
normal forms, so that the journal's theorems cover it. For each named individual b other
than the first and each key property `r_j`, the axiom `{b} ⊓ C ⊑ ∀r_j.V_jb` marks the
values of b when b is a C. For each pair of distinct named individuals a and b, each key
property `r_j` and each named individual c, the axiom `{a} ⊓ ∃r_j.({c} ⊓ V_jb) ⊑ M_jab`
marks that a has a named value of b, and `{a} ⊓ C ⊓ M_1ab ⊓ … ⊓ M_mab ⊑ {b}` makes them
equal. A key whose class is not a name gets a fresh class equivalent to it, so that the
axioms hold the name and not copies of the expression. The fresh classes make this a
conservative extension. A model of the ontology
extends to them, with `V_jb` the `r_j`-values of b when b is a C and empty otherwise, and
`M_jab` the set `{a}` when a has a named `r_j`-value in `V_jb`. A model of the rewritten
axioms satisfies the key, since two named instances of C that share a named value for each
property make every marker hold. The reduction has about m·n³/2 axioms for n named
individuals. When the keys' reductions pass the smaller of 131,072 axioms and one axiom
per 2 KiB of the memory budget, the ontology is not rewritten at all and takes its default
answer, which is about 60 individuals for a key with one property. Keys over data
properties wait on item 768.

The first reduction gave each named individual b a fresh property `W_b` with `⊤ ⊑
∃W_b.{b}`, so that any element could see b, and marked shared values through
`∃W_b.({b} ⊓ ∃r_j.{c})`. It is a conservative extension too, but it links every context
to every named individual. On 96 generated ontologies with four individuals and keys or
the universal property, the rooted run passed its bound on 53 with `W_b` and on 21 with
the markers. The five of them with keys and no universal property passed it with `W_b`
and finished with the markers. The markers keep the facts on the
values, which the root shares, and need no inverse property, which would meet the Nom
rule's refusal beside an at-most restriction.

An occurrence of `owl:topObjectProperty` becomes a fresh property U′ that is symmetric and
transitive, with `⊤ ⊑ ∃U′.{h}` for a fresh internal individual h. Every element then
reaches h and back, so U′ relates every pair of elements. An inclusion or chain whose
superproperty is the universal property is a tautology and is dropped before the
replacement, since OWL 2 exempts such axioms from the regularity conditions and U′ would
otherwise make the hierarchy irregular. The replaced hierarchy is then checked for
regularity, and an ontology that fails the check takes its default answer. The transitive
property goes through the automata of the rules above, and h is not a named individual for
keys or for the taxonomy. A domain or range of the universal property becomes `⊤ ⊑ C`, a
minimum of none over it is `⊤` and a minimum of one is an existential restriction over U′,
and a property equivalent to it or its inverse gets U′ as a subproperty. A property above
U′ is not simple, which OWL 2 DL forbids in a number restriction, a functionality, a
disjointness or an irreflexivity, and the run either leaves such an axiom out and reports
it or gives the default answer. A larger minimum, a maximum, an exact cardinality, a
self restriction, functionality, asymmetry, irreflexivity or disjointness of the universal
property itself has no replacement, and the ontology is then not rewritten. Neither is an
ontology that names an entity with the prefix `urn:sparkles:internal:` of the fresh names. A
rewritten run that turns the ontology Horn or OWL 2 EL and takes every rewritten axiom keeps
its lower-tier answer with the original axioms' statistics and fresh-name hiding, while a
rewritten run that does not take the disjunctive rules and either remains disjunctive or
leaves out any rewritten axiom starts again on the original axioms. Both declined cases give the default
answer. The classes and individuals of an axiom that
the rewriting drops stay in the signature, and the statistics count the original axioms
outside OWL 2 EL. The universal property links every context
to h, and on the generated ontologies above every run that passed the bound had the
universal property, which item 1061 follows with the cost of ground reasoning in each
context.

The run relies on the journal's Theorem 1 for soundness, Theorem 2 with condition C2 for
the completeness of each named class's context under the journal's rules, which item
804's departures below leave as an open obligation, and Theorem 5 for termination with an
admissible expansion strategy. The hypothetical successors of the cautious strategy are
admissible under Definitions 5 and 9 once one edge covers every ground trigger as above. A
run that stops at its bound is not saturated. The dependency record of the rules above
follows edges from `owl:Thing` and cannot see work in the root, in the contexts that the
root reaches through its successors, on a propagation to or from the root or on a key.
A stopped rooted run therefore always reports its consistency as unknown and leaves every
class open. Constants that the run creates for its own encoding, such as h, never
reach the answer.

The phase's gate is the `dl-o` stratum of the ORE 2015 DL development set, the nominal
fixtures that need no counting and the hand cases. Of the six development files in `dl-o`,
ore_ont_15803 and ore_ont_14924 have no data properties and both name the top object
property. The other four use data properties and join the gate with item 768. The `dl-oiq`
stratum needs counting and the Nom rule and joins the gate after item 896. The fixtures in
the gate are the 50 `shoi` cases, `sroiq-048` and the hand cases `nominal-disjunction`,
`nominal-ripple`, `nominal-ripple-conditional`, `key-disjunction`, `top-forall` and
`top-forall-conditional`, each on the rules that take it. Those hand cases that are Horn
or EL also get a copy with one disjunctive axiom added, so that the rooted run takes them.
The hand cases `nominal-inverse-max`, `nominal-bounded-count` and
`nominal-predecessor-cases` need the Nom rule. Every case in the gate runs with counting
off, on one and four threads, under both strategies, and must equal HermiT's taxonomy and
consistency. Regressions with one disjunctive axiom added check that an irreflexive
property beside a nominal is left out while the run takes the rest, and that a stopped rooted run reports its consistency
as unknown.

With the journal's rules as first built, the rooted run finishes 132 of the 200 runs of the 50 `shoi` fixtures,
on one and four threads under both strategies, within 5 s each in a debug build, and each
taxonomy it finishes equals HermiT's. It times out at least once on 19 fixtures. The
smallest case, three axioms reduced from `shoi-000`, gives no answer within 200 s. Two
causes are measured. The first is the fifth condition of Definition 7, which leaves ground
atoms of different predicates incomparable, so the root's resolution over ground
disjunctions is unordered and r-Pred copies those disjunctions into the contexts. The
experiments on the order ran at commit 846dd24e with Eq blocked on `x ≈ u`, as the journal's
Section 7.2.3 describes, in every context but the root. With that blocking, an order of
every ground atom by predicate, which Definition 7 does not admit, classifies the small
case in 0.4 s, and 39 fixtures then finish in every configuration with HermiT's
taxonomies. Ordering only the unary atoms over one constant, which is what the nominal
contexts of the journal's system Sequoia allow, classifies it in 5.2 s. The second cause
remains with the order of every ground atom and that blocking, with one query atom
selected per clause as its Section 7.3.1 describes and without the ground
markers to successors. The root then spends its work on clauses whose bodies join up to
five r-Succ markers, which is the calculus's own conditional reasoning. A read-only
consult found that the growth follows Tables 2 and 3 rather than a wrong inference, and
recommended Sequoia's nominal contexts in place of the single root. Item 804 departs from
the journal's rules instead as the next subsection records, with the rationale, the
argument for soundness and completeness and the measured effect of each departure.

#### Item 804's departures from the journal's rules

The journal's root reasons about markers. A context that derives `Γ → Δ ∨ B(u)` tells the
root only that `B(u)` may hold, as the marker `B(u) → B(u)`, and the root derives what
follows at `u` from every combination of the markers it holds, under those markers as
conditions, so that r-Pred can hand each conditional clause to every context that holds
its conditions as maximal literals. The root therefore computes the consequences of the
ontology at `u` under every subset of the ground atoms that any context has ever found
maximal, which is the second measured cause, and the fifth condition of Definition 7 keeps
every ground atom maximal so that each one can become a marker, which the first cause
needs. The four departures below remove the markers, order the ground literals, block
the equalities with `x` outside query contexts and select the ground literals of bodies
outside the root. Each is an inference restriction or an
extension by sound instances of the ontology's clauses, so the run stays sound by the
argument of the journal's Theorem 1, and the soundness of each is argued in its place.
The journal's completeness proof does not carry over to the departures as it stands. Each
departure therefore records the argument for its completeness, the obligations of the
journal's proof that the argument leaves open, and its measured effect, and the run keeps
a departure only when the gate needs it. The gate, exact taxonomies and consistency on
every fixture, is regression evidence for completeness and not a proof of it, and a
further check classifies freshly generated ontologies of the gate's families against
HermiT.

**A context reasons about its own ground literals.** Hyper binds the ontology clause's `x`
to a constant in every context, not only in the root, with the other variables bound to
`x`, `y`, a successor term or a constant as the context's literals allow, and it binds the
clause's `x` to the context's `x` with a role atom's other end bound to a constant, so
that a holder of `∀t.C` over `x` and `t(x, u)` give `C(u)` whichever comes first. A maximal
literal over `f(u)` takes Succ in any context. The edge records the predecessor, the
existential restriction, the constant and the successor, and is distinct from the edge
for `f(x)` of the same restriction even when both reach the same successor. Pred along
such an edge reads `y` as `u` and `x` as `f(u)`, resolves the successor's core atoms as
it does for the root and admits the head forms that Table 3 admits for the root, among
them a role atom between the successor's `x` and a constant, and it replays for an edge
or a premise that comes later. The run uses neither r-Succ nor the r-Pred of a root
clause with a body, and the root holds no markers. The root is the context of the
unconditional ground facts, which the assertions, the singleton classes and the clauses
with the body `owl:Thing` give it, saturated with Hyper at the constants, Succ over
`f(u)` and the equality rules. Its processed clauses with an empty body reach the
contexts registered for their constants as above, whether the registration or the
clause comes first, and a context registers for the constants of every maximal literal
of its processed clauses, inside `f(u)` included, for the constants of `x ≈ u` and for
those of the ground literals in its bodies. The empty clause in the root has no constant
to route it by, so the root's bottom is the run's inconsistency and reaches the answer
without registration. A context's own ground literals are conditional on its core and
on its bodies, so the root cannot take them, and the context derives their consequences
at `u` itself from the actual disjunctions it holds, where the root would derive them
under every combination of markers.

The literals of a context other than the root are those over `x`, `y` and `f(x)` of the
rules above, the ground literals, which are `B(u)`, `S(u, v)`, `u ≈ v` and `u ≉ v`, the
role atoms `S(x, u)` and `S(u, x)`, the equalities `x ≈ u` and `f(x) ≈ u`, and the
literals over `f(u)` that the root holds, which are `B(f(u))`, a role atom between `f(u)`
and any constant in either direction, `f(u) ≈ v`, and the inequalities `f_i(u) ≉ f_j(u)`
of a minimum restriction with counting. The set is closed under the rules. Hyper at a
constant `u` makes literals over `u` and `f(u)` only. Pred along an edge for `f(u)` reads
`x` as `f(u)`, `y` as `u` and keeps the constants, so a successor's head over `x`, `y` and
constants becomes one of the forms listed, among them `S(v, f(u))` from `S(v, x)`, which
is a Hyper premise for the universal restrictions of `v`'s holders and no successor
trigger, since Definition 4's `S(y, x)` reads as `S(u, f(u))` along that edge. Eq rewrites
a constant at every occurrence in the selected literal, in every context, as Table 2 has
the root do, so that `S(u, f(u))` rewritten by `u ≈ v` is `S(v, f(v))` and `f(u)` never
appears beside a rewritten form of `u`, and Factor makes inequalities of the a-terms
already present. The order groups every literal over `f(u)` with the literals over
successor terms, which are totally ordered as above by the greatest successor term, the
role, the predicate and the terms.

Successors still take markers. When Succ fires, the successor gets `A → A` for every
ground literal `A` of Definition 4's successor triggers that is maximal in a processed
clause of the predecessor, as the journal's rule K2 says, and a context sends a later
trigger to the successors it already has. A hypothetical successor with the empty core
takes its predecessors' hypotheses as markers and nothing as a core unit, and keeps every
guard when several predecessors share it. The triggers are every ground atom and
equality of constants, a superset of Definition 4's, since a told subsumption puts its
subclass in a body and so nearly every predicate qualifies, and a marker that no
inference asks for costs one clause in the successor's store.

The run also restricts the ground inferences. A ground inference is a Hyper whose body
atoms are all bound to constants, an Eq or Factor whose equality and rewritten literal
hold neither `x` nor `y`, or a Join of two clauses that hold neither `x` nor `y`, and
its conclusion has a body only when a premise carries a hypothesis. Such a conclusion, a
clause that holds neither `x` nor `y` and has a body, is not made in any context. The
restriction keeps a successor from computing the ground closure of the ontology under
the markers of all its predecessors, which would be the root's explosion again, one
successor at a time, and it keeps a hypothesis from creating successors of `u` in a
context that holds it only as a marker. What a marker contributes is what follows for
the successor's own element from the hypothesis, through Hyper with a body atom bound to
`x` or `y`, which may conclude a ground clause with a body, such as `A(u) → B(u)` from
`D(x) ∧ S(x, u)` and a universal restriction, and through the ground inferences whose
conclusions mention `x` or `y`. The ground consequences of a hypothesis alone are left to
the context that holds the marked literal in an actual clause.

The argument does not hold for Join against facts that only the context's core gives, which
no ancestor holds, so a Join whose premise on the side of the head has no ground body
literal is exempt from the restriction, as the Outcome records for item 1220.

The argument for completeness is the one for ordered resolution. A root clause `A_1 ∧ … ∧
A_n → Δ` that r-Pred would hand to a context `v`, where `v` holds each `A_iσ` as a
maximal literal in `Γ_i → Δ_i ∨ A_iσ`, is derived in the root by Hyper at constants, Eq,
Join, Succ over `f(u)` and Pred from the markers and the ontology clauses, and each of
those inferences has a counterpart in `v` with the actual premises in place of the
markers, since `v` has the same rules at constants and the same successor for `f(u)`.
The counterpart's conclusion carries the residual disjunctions `Δ_i`, which can leave an
intermediate ground literal below a greater one, so that the next inference of the chain
is not made in `v` while the root made it. Ordered resolution leaves such an inference
out and stays complete by a model construction, in which a clause whose greatest literal
the construction makes true needs no inference on a smaller one, and whether that
construction carries over to the contexts of the departed run is the open obligation
below, so this is a motivation and not an argument. The restriction leaves
out a ground conclusion under hypotheses, and the argument for it is that the context
which holds a marked literal in an actual clause can derive the ground conclusion from
its actual clause and the other premises, which reach it as root facts by registration or
as ground heads that Pred carries, and sends each literal of the conclusion as a marker
once it is maximal there. The argument is not a proof. The journal's Appendix D builds
its model one term at a time with a ground order chosen from the literals it has already
made false, under its properties O1 to O3, and lifts the ground clauses to context
clauses with its Lemmas 7 and 8, which exclude `f(u)` from a context other than the root.
The departure leaves open the lifting with `f(u)` in every context and the discharge of
hypotheses across a successor that several predecessors share and across chains of
successors, where no one context holds every actual premise of a left-out inference. The
restriction is therefore kept only if the gate needs it, and its effect is measured
with it on and off. Termination holds as in the journal's Theorem 5. The terms of a
context are `x`, `y`, `f(x)`, the constants and `f(u)`, and a successor's `x` stands for
`f(x)` or `f(u)` of its predecessor, so no context holds a nested term and the set of
literals of each context is finite.

**One order of ground literals.** Every context compares two ground literals by one
order that every context shares. The order is on terms first. The constants are ordered
by the one total order on function symbols and constants that Definition 7 starts from,
and the ground atoms, which are terms of the sort of the predicates, by the lexicographic
path order of a precedence that puts every predicate above every constant, the roles
above the classes and then the predicates by their ids. Two ground atoms thus compare by
predicate first and then by their constants from the left. The equalities and
inequalities of constants compare as Definition 7 has them, by the multiset extension of
the order on constants, and a ground atom is above every equality and inequality of
constants, since a predicate is above every constant. Everything else follows Definition
7 and the Sequoia construction as above. A ground atom is above `x`, `y`, the constants
and the equalities `x ≈ u` and `y ≈ u`, and it stays incomparable with every atom over
`x` or `y`, so a clause that holds both a ground atom and an atom over `x` still
resolves on either.

The fifth condition of Definition 7 leaves ground atoms of different predicates
incomparable. The journal explains the condition by the literals that Pred carries to a
predecessor, which must not block a literal that only the context itself can resolve, and
that part of the condition the departure keeps, since a ground atom never blocks an atom
over `x` or `y`. Two ground atoms of one clause both go to the predecessor whatever their
order, and Succ's markers follow the order, so a ground trigger that is not yet maximal
is not yet a hypothesis of its successors, which the departure conjectures to be
dispensable and does not show. A context then resolves its ground literals one at a time
by the order, as ordered resolution does, which is the motivation for the departure and
not an argument. The journal's proof needs more than that. Its Appendix D chooses the ground order of each
model fragment from the literals already made false, under its properties O1 to O3, and
the fifth condition is what lets every such order extend the context order. A context
order that fixes the order of two ground atoms can contradict the order a fragment would
choose, so the proof's construction does not carry over, and whether the departed run is
complete under the shared order is an open obligation. The departure is kept only if the
gate needs it, and its effect is measured against the admissible order with the other
departures in place.

**Equalities with `x` in query contexts.** The rewriting equality `x ≈ u` rewrites `x` to
`u` only in a unit clause whose atom is a core atom of a query context, which is the
context of a named class or, since item 1070, of `owl:Thing`, and rewrites `u` to `x` only
in a named-class atom over `u` and only in a query context. Elsewhere Eq does not rewrite
with `x ≈ u`. Every other rule stays. Factor takes `x ≈ u` and `x ≈ v` in one head as
Table 2 says, and an equality of constants `u ≈ v` rewrites the constant in `x ≈ u` to
give `x ≈ v`, since it is the rewriting equality and `x ≈ u` only the rewritten literal,
and `f(x) ≈ u` rewrites as usual. The journal's Section 7.2.3 reports that its
completeness proof needs `x ≈ u` as the rewriting equality only in query contexts and only
on query atoms, and its Lemmas 21 and 22 use exactly the two forms kept. Lemma 21 rewrites
a unit `Γ_0 → A(x)` with `A(x)` in the query body, which for classification is the
context's core atom, so that the constant's facts include `A(u)` and the consequences at
`u` follow from the ontology's clauses there, and Lemma 22 rewrites `x ≈ u` by `u ≈ v` and
then needs a clause with `x ≈ u` maximal beside named atoms, which the second form
rewrites back to `x`. A successor context that is not the context of a named class has no
query and needs neither form. Its element's equality to a constant reaches the predecessor
as `f(x) ≈ u`, where Eq rewrites `f(x)` to `u` as usual and the predecessor reasons at
`u`. A successor that reuses the context of a named class has that class's query, and
there Eq and Factor in the form of a query leave out a selected premise. That no
entailment is lost there is the open case of item 1220. Sequoia blocks the same
inferences. The journal's observation is about its own calculus, and that it carries over
to the run without the root's markers is part of the open obligation above. Without the
blocking the run rewrites every clause over `x` of a context that holds `x ≈ u`, and in
the earlier experiments the blocking was on in every configuration that finished.

**Selection of the ground literals in bodies.** In a context other than the root, a clause
whose body holds a literal with neither `x` nor `y` is selected, a marker `A(u) → A(u)`
included. A selected clause is a premise only of an inference local to the context, of
Join on its ground body literal as the side with the body, of Pred and of Succ, and it
still sends the hypotheses and markers of its maximal literals. An inference is local when
it is on a literal that holds a successor term of the context or on a role atom between
`x` or `y` and a constant or a successor term of a constant, since no predecessor sees
such a literal and no other context can make the inference. Hyper over `x` and `y`, Hyper
at a constant and Eq or Factor of ground or `x` literals on a selected clause are left to
the predecessor, which receives the clause by Pred, discharges the ground body by Join
against its actual clauses and sends the hypotheses and markers that let the context go
on. One exception stays in the context. A selected clause whose head holds an atom over
`x` cannot go to the predecessor, since Pred carries no such head, so it takes the Hyper
over `x` itself. Hyper over `x`, and Pred from a successor over a successor term of a
constant, take selected clauses as partners, and the other partner lookups leave out
selected clauses the same way that the rules leave out a selected premise, so an inference
between two clauses fires or not whatever their order of processing. The rationale is the
measured cause of the remaining work after the first three departures. In shoi-005's
dominant context 13,731 of 17,384 distinct clauses held a ground literal in the body and
9,731 two or more, which is the root's marker explosion moved into one context, since the
context combined the markers of all its predecessors in bodies. A restriction of
inferences is sound. The argument for completeness is that of ordered resolution with
selection, where a clause with a selected literal resolves only on it, and the predecessor
that holds the marked literal in an actual clause makes the inference the context leaves
out. What the argument leaves open is the return path. The predecessor's clause after Join
holds the context's literals over `f(x)` and sends them as hypotheses, and whether every
inference the context leaves out comes back this way across a shared successor or a chain
of successors is not shown. A consult proposed the ontology `Q ⊑ ∃s.D`, `Q ⊑ ∃t.{a}`, `Q ⊑
∀t.∀r.A`, `D ⊑ ∃r⁻.{a}`, `A ⊑ ⊥` as a case where the exception is needed, and it is a
regression test, which passes. The selection is measured with it on and off, and the run
keeps it only if the gate needs it.

The argument does not hold when both premises of an inference over `x` are selected, since
no predecessor then holds either premise. That is why those two lookups take selected
partners, as the Outcome records for item 1220, whose completeness is not proved.

**The facts of a hypothetical successor go to the root.** A hypothetical successor has the
empty core and no denied class, and its hypotheses and markers are in the bodies of its
clauses, so a clause of its with an empty body holds of every element under no assumption.
Such a clause whose literals hold neither `x` nor `y`, which are over constants and their
successor terms, holds in every model of the ontology, and it goes to the root as a fact,
once per distinct head, instead of staying in the context. The root derives it as its own
clause, with its redundancy, processes it and offers it to the contexts registered for its
constants as it does its other facts, this context included when it is registered. The
root's own facts stay in the context that takes them. The empty clause with an empty body
takes no such path, since it has no constant, and makes the context unsatisfiable, which
reaches its predecessors and the root by Pred as before. The rule is sound by the argument
above. For completeness, the inferences on a clause over constants and their successor
terms are Hyper at the constants, Join, Eq, Factor and Succ over `f(u)`, which the root
makes on the same clause with the same rules, and the context's inferences that need the
clause as a partner, Join on a ground body literal and Hyper or Eq with a mixed clause,
need a clause of the context that mentions one of the fact's constants in a maximal
literal or a body, which registers the context for that constant, so the fact comes back
before the inference is due. The rationale is measured. A hypothetical successor derived
2,956 pure ground clauses with an empty body in one second of shoi-005 after the
selection, two thirds of them over a successor term of a constant, where the root derived
197 distinct clauses in the same second, and the context ran Hyper at the constants, Succ
and Pred over them again. With the rule, shoi-015 answers in 0.5 s where it timed out
before.

**Measured effect.** The measurements are on a debug build on the laptop under load, with
one thread unless noted, and the counts of conclusions come from the derivation trace
(`DLTRACE`), which labels each conclusion by context and rule before normalization and
each processed clause. With the journal's rules, the smallest case gives
no answer in 200 s and 19 of 50 `shoi` fixtures time out. With the first three departures,
the smallest case answers in 2.9 ms, and 44 of 50 `shoi` fixtures give HermiT's taxonomy
in every one of the four configurations, one and four threads under both strategies, in 5
s, with 0 wrong anywhere, and six time out, shoi-005, 015, 022, 027, 032 and 042. With
the selection and the promotion of facts, 45 of 50 pass in every configuration, 180 of
200 runs, with 0 wrong, shoi-015 answers in 0.5 s, and shoi-005, 022, 027, 032 and 042
time out in every configuration at 5 s and at 10 s. The remaining work is attributed. In
one second of shoi-005 the dominant context, a hypothetical successor shared by several
predecessors with the hypotheses `N_a0(x)`, `∃r0⁻.C3(x)` and `∃r3.{a0}(x)`, derives
24,286 distinct conclusions, 21,665 of them mixed, with literals over `x` beside ground
literals in the head, and 18,524 of the mixed conclusions are Eq inferences whose
rewriting equality is `f(x) ≈ u`, for the successor term of `∃r3.{a0}` and the constants
that `{a2} ⊔ {a0}` and `{a3} ⊔ {a2}` make it equal to. 433 processed clauses with such an
equality maximal, each with its own residue of ground literals, hypotheses and markers,
each rewrite about 180 clauses that hold `f(x)` in a maximal literal, each with its own
residue, and the rewritten clauses hold ground atoms over the constant beside the
residue's literals over `x`, so the context then reasons at the constant once per residue.
Every other rule contributes under a thousand conclusions. The product is ordered
paramodulation on `f(x) ≈ u` as Table 2 has it, and the journal's calculus makes the same
inferences in the same context, since the successor's `x ≈ u` reaches the predecessor as
`f(x) ≈ u` there too. Sequoia's nominal contexts of Section 7.2.2 fragment the root by
constant and keep the journal's markers, so they do not touch this context's work. A
restriction that leaves these inferences out needs an argument that the knowledge about
`f(x)` reaches the constant another way, and the run has none yet, so the five fixtures
go to item 822 with this attribution and the candidate restrictions, which are to treat
`f(x) ≈ u` like `x ≈ u` under the third departure with another route for the successor's
knowledge to the constant, to order the literals over `f(x)` below the ground literals of
a mixed clause so that it processes on its ground part first, which changes the lifting of
Pred and Succ, and demodulation where `f(x) ≈ u` is a unit under the hypotheses, which the
disjunctive equalities here do not give.

**The landed run.** Item 804 lands the rooted run as the option `Limits::rooted`, off by
default, under the policy that it becomes the default only after 0 taxonomy differences on
the ORE 2015 DL sample, several thousand generated fixtures against HermiT and the `shoi`
and nominal fixtures, which item 823 holds. A rooted run without counting has a bound of
524,288 derived conclusions, which `Limits::dl_work` changes, since the 45 fixtures that
finish derive under 190,000 on one and on four threads and the five that do not derive
millions. A run that passes it classifies again without the rooted rules, which leaves the
axioms outside OWL 2 EL out and reports them as a run without the option does, and sets
`Stats::dl_rooted_over` and `dl_over_budget`, so an ontology that the run cannot finish
gets its default answer after the bound's work. A rooted run that counts keeps the
counting bound and its retry without counting, which then has the rooted bound. Over the
50 `shoi` fixtures with the option on, 48 give HermiT's taxonomy in every configuration
with item 822's departure and the other two fall back to their default answer, with 0
wrong, which the `dl_fixtures`
suite checks on one thread under the cautious strategy and four threads under the eager
one. The share of the work that the duplication of ground reasoning across contexts
costs will be measured on ore_ont_15803 with its two top-property axioms removed, since
the file itself waits on item 805.

#### Item 822's departure: the constant of a restriction into a singleton

An existential restriction `∃r.B` whose class B is below a singleton `{v}` through told
subsumers and conjunctions alone gives the role atom `r(t, v)` and the atom `B(v)` in place
of the successor term `f(t)` with `r(t, f(t))` and `B(f(t))`, at x and at a constant t, in
the clauses of a context and in its units. The restriction makes no successor and no edge,
its units take no part in the universal restrictions over successor terms, and a unit's
EL successor sends it no clause by Pred and gets none of its ground markers. The run then reasons about v with the root's
facts and the context's own ground literals, by Hyper at the constant and the universal
restrictions over a role atom to a constant that item 804's first departure describes. A
counting run keeps the successor terms, since an at-most restriction counts its
neighbours over them, and `SPARKLES_ROOTED_NOMINAL_SUCC=0` turns the departure off for
measurement.

The rewriting is an equivalence, since `B ⊑ {v}` makes `∃r.B(t)` hold exactly when
`r(t, v)` and `B(v)` hold, so the run stays sound. For completeness, the departure is
demodulation by a unit. B's told subsumers give `N_v(f(t))` and so `f(t) ≈ v` under the
same body and residue as the restriction itself, and with that equality every clause that
holds `f(t)` rewrites to the clause over v, which is what the journal's Eq makes in the
same context, as the measured effect above describes. The departure replaces the clauses
over `f(t)` with their rewritten forms. A successor's own knowledge about its element,
which Pred would carry back over `f(t)`, is the knowledge about v under the successor's
markers, and the context derives it at v from the same premises, since the markers of
such a successor are the context's own literals. What the argument leaves open is the
same as for the first departure, the lifting of the ground clauses that the context then
holds over v, and it is not a proof. The rationale is the attribution below. In the first
second of shoi-005 with the departure off, 80,589 of 240,993 conclusions are Eq inferences
whose rewriting equality is `f(u) ≈ v`, a successor term of a constant equal to a
constant, and the successor terms are those of restrictions into singletons. With the departure, shoi-005 finishes with 21,485 conclusions
where it passed the bound and finished with 6.4 million under a larger one, and shoi-027
and 032 finish too. The derivation trace now names an Eq conclusion by the kinds of terms
of its rewriting equality, such as `eq:fx>c` for `f(x) ↦ u`.

The two fixtures that still pass the bound have other causes. In shoi-022 most of the work
is Eq on equalities `f(u) ≈ v` of successor terms of constants, with no x in the
conclusion, in 15 contexts besides the root, since `¬{a3, a0} ⊑ {a3}` makes every element
`a0` or `a3`. The contexts derive 143,756 distinct ground conclusions between them in the
first 1.5 million lines of the trace, of which 14,077 are distinct across contexts, which
is the duplication of ground reasoning across contexts that item 1061 follows. In shoi-042 most of the work is Eq on disjunctive equalities of constants into
clauses that hold x, about 380,000 of 866,000 conclusions in four seconds. With a bound of
200 million it finishes after 4.7 million conclusions in 4.4 s on four threads of a build
without link-time optimization. Item 1075 follows these equalities.

#### Item 1075: binary splitting over disjunctive equalities of constants

When a rooted non-counting classification enables `Limits::rooted_eq_split` or sets
`SPARKLES_ROOTED_EQ_SPLIT=1`, the classifier resolves explosive root disjunctive
equalities between named constants by binary semantic case splitting outside the
consequence-based calculus. The switch is off by default, and the alternative block-order
restriction on the rooted literal order remains deferred because its global transitivity
and completeness transfer across mixed contexts are unproved.

The soundness and completeness argument rests on two properties. First, for any ontology
`O` and any two distinct named individuals `u` and `v` from the original input signature,
every model of `O` satisfies either `u = v` or `u ≠ v`. Consequently `O` is consistent if
and only if at least one of the two branches `O ∪ {u = v}` and `O ∪ {u ≠ v}` is
consistent, and a class subsumption or individual class assertion holds in `O` if and only
if it holds in every consistent leaf of the binary split tree. Second, before projecting
any consistent leaf into the split accumulator, the classifier closes subsumptions over
all met classes including internal reduction classes and verifies that no real nominal in
the leaf, including anonymous witnesses and any active universal-property hub, reaches an
unsatisfiable class. It then projects subsumptions and public instances onto the original
visible signature, treating unconstrained declared-only individuals as instances of every
class subsuming `owl:Thing`, and intersects the projected relations across all consistent
leaves. If every leaf is inconsistent, the ontology is inconsistent and every visible
class is unsatisfiable.

Candidate discovery inspects empty-body root clauses that contain at least one positive
equality between two distinct original named individuals not already decided in the
current branch partition or disequality set. Pure equality disjunctions take priority over
mixed disjunctions, with deterministic tie-breaking by clause head length and original
individual rank. At depth zero, the root populates the best candidate while saturation
runs, and a branch forks once total rooted work crosses the probe threshold of 16,384
conclusions, or immediately on a pure equality disjunction at depth one and below. The
equality child merges `u` and `v` in its branch partition, remaps all inherited branch
disequalities and closes immediately as inconsistent on a self-disequality collision, and
the disequality child adds `DifferentIndividuals(u, v)`. All prefix work before forks and
restarts is charged in `Stats::dl_rooted_spent` disjointly from terminal leaf work in
`Stats::dl_work`, so the combined total stays within one frozen `attempt_bound` across
every branch, bounded by default depth and leaf caps of 8 and 32. If no fork occurs, the
run preserves its ordinary context index and incremental state. If a fork occurs, the
classifier retains a compact `SplitAnswer` holding the intersected taxonomy and public
instances so `Classifier::describe` and `Saturation::describe` answer without keeping a
branch index, and clears that answer on any edit or strict `Error::Incomplete` refusal.
Every branch partition, disequality list, clause and dynamic conjunction insertion,
leaf closure, taxonomy construction and retained answer is pre-admitted against the
single memory ledger and caller pool before allocation.

On the `shoi-042` fixture, enabling `Limits::rooted_eq_split` with `Limits::rooted_merge`
completes across one and four threads under both cautious and eager expansion and both
stepped and unstepped drivers, with two splits, three consistent leaves, two merge
restarts, 18,103 prefix conclusions and 21,144 leaf conclusions for a total of 39,247
derived conclusions against the 524,288 bound. With `Limits::rooted_merge` off it
completes across all configurations with 18,032 prefix conclusions and 30,595 leaf
conclusions for a total of 48,627 conclusions. Every configuration matches the checked-in
HermiT taxonomy oracle and the exact individual instances of `shoi-042` with zero excluded
axioms and no fallback. Across all 50 `shoi` fixtures with both switches on, all 50
complete within the 524,288 bound with zero taxonomy differences against the checked-in
HermiT oracles. Benchmark-host timings remain for default-on qualification under item 823.

### Anonymous individuals and top-property inclusions

Items 1281 and 1282 close the two cheapest gaps of item 1272's census of what
classification leaves out. On the ORE 2015 DL development set, 55 files left out
2,827,410 axioms with anonymous individuals, most of them `oboInOwl` reifications such as
`ClassAssertion(oboInOwl:DbXref _:x)`, and for 31 files nothing else. In six files an inclusion
`SubObjectPropertyOf(r owl:topObjectProperty)` was one of the things that kept the
disjunctive rules closed.

**Anonymous individuals.** OWL 2's Direct Semantics satisfies an ontology when some
mapping of its anonymous individuals to elements satisfies every axiom, so each anonymous
individual stands for an element that the whole ontology quantifies existentially. A
fresh constant in its place keeps the ontology's consistency and every entailment that
does not name it, which is how standard reasoners classify such ontologies. The
classifier now interns each anonymous individual as a nominal named `_:label`, which no
IRI can be since the scheme of an IRI starts with a letter. The EL, Horn and disjunctive
normalizations take it in class and property assertions, negative assertions,
`SameIndividual`, `DifferentIndividuals`, `ObjectOneOf` and `ObjectHasValue`, where they
left out the whole axiom before and dropped anonymous members of equalities without a
report. A label names one individual wherever it occurs among a run's axioms, as the
axiom model makes equal axioms one. A key binds named individuals alone, as
`HasKey`'s semantics requires, so the EL rules' key matching skips anonymous nominals as
members and as values, and item 805's rewriting of keys already enumerated named
individuals only. A description never lists an anonymous individual among the
instances of a class, and a change to an axiom with one runs in full. The detection of
individuals that makes a disjunctive run rooted, and that selects the Horn rules with
individuals, counts anonymous ones too.

An anonymous individual is an individual for the choice of tier. The disjunctive rules
without the rooted run take no individuals, so an ontology whose only individuals are
anonymous, and which has axioms that need case analysis, now leaves those axioms out
instead of its assertions. No file of the census is such an ontology, and the rooted run
takes it, which item 823 follows.

A class assertion of a named class whose anonymous individual occurs in no other
logical axiom, and only once in it, says that the class has an instance and no more. Two
of them over one class are interchangeable, so the one whose individual has the least
label stands for the others, which the run does not read. A class assertion of a named
class is an OWL 2 EL axiom that every run takes in full, so the run reports nothing for
the assertions it does not read. Before the run, a scan counts the labels of these
assertions in every logical axiom. Its scratch of 96 bytes for each assertion and a
pointer for each axiom, which holds the list the run reads, is admitted against
`Limits::max_memory` beside the caller's axioms and against the pool before it is
allocated. A refusal leaves every assertion its own individual. The run draws first on
what the pool granted for the scan and counts it in its peak, every pass of the scan
polls the deadline and cancellation, and a stopped scan leaves the next run to start
again in full, as a stopped run does. Every merged assertion in the census is of a named class.
On `ore_ont_5857`, with 723,805 such assertions, the merge keeps the rooted run within
its bound, which a constant for each assertion passed.

**Top-property inclusions.** `SubObjectPropertyOf(X owl:topObjectProperty)`, for a
property, an inverse or a chain `X`, and `SubDataPropertyOf(d owl:topDataProperty)` hold
of every ontology, and the role table already puts every role below a top property that
another axiom names. The classifier takes them as OWL 2 EL axioms whose normalization
adds nothing and interns no top property, so they no longer close
`Saturation::disj_allowed` or the rooted run's gate. A chain among them meets no range
restriction of OWL 2 EL, and item 805's rewriting neither starts for them alone nor
lets a data property among them keep it from an ontology. The run reports them neither
as left out nor as taken.

**Gate.** The census tool of item 1272, built on the base 0779d666 and on the lane tip,
reran the 59 files that hold anonymous individuals or top-property inclusions on two
threads, in the default and the rooted run. Their left-out axioms fall from 5,659,868 to
5,048 across both runs. Thirty files leave nothing out in the default run and 31 in the
rooted run, where all 59 left something out before, and no file leaves out an axiom
without an anonymous individual that the base took. The anonymous-only files move from
the Horn rules to the EL rules or stay with the Horn rules, and every file keeps its
consistency. The six files with top-property inclusions leave out what they left out
before, since their data properties, datatypes and, in the default run, individuals keep
the disjunctive rules closed too, which items 768, 972 and 823 follow. Item 1272's
ablation without those axioms showed that the inclusions then no longer close the gate
and the files reach the rooted run's bound, which item 1284 follows. The peak
resident memory of a census process, which reads the file and runs both, falls from 752
to 196 MiB on `ore_ont_10073` and `ore_ont_5481`, from 1,674 to 501 MiB on
`ore_ont_5857` and by a sixth to two thirds on the other files above 400 MiB. It rises
by 6 to 7% on `ore_ont_4510` and `ore_ont_9654`, whose anonymous individuals occur in
other axioms and become individuals of the run, and by 4 to 5 MiB on six files of about
22 MiB. On `atlas`
with four threads, in three alternating rounds of three classifications a side of
3c7d0eb0 against 0779d666, 34 of the 118 file and run cells are faster in every
classification and 7 slower. The tip classifies `ore_ont_10073` and `ore_ont_5481` in 105
to 107 ms against 761 to 775 ms, `ore_ont_5857` in 312 against 1,388 ms by default and in
2,045 against 3,398 ms rooted, and the other files above 100 ms in 0.17 to 0.80 times the
base's time. `ore_ont_4510`, `ore_ont_12901`, `ore_ont_14375` and `ore_ont_9654` take 1.21
to 1.27 times as long while they take the 479 to 3,981 axioms that the base left out,
since their anonymous individuals occur in several axioms, which item 1293 follows. The
same rounds at 781da4c8, before the review fixes, classified `ore_ont_10073` and
`ore_ont_5481` in 88 to 90 ms and `ore_ont_5857` in 242 ms by default, so the review fixes
cost 16 to 18 ms and 70 ms there, and item 1293 also follows that cost. HermiT
1.4.5.519 references exist for 29 of the 31 files that anonymous individuals alone kept
from a full answer, and the tip's taxonomy equals HermiT's on all 29 in both runs on four
threads, with the same consistency. The hand cases of `tests/anonymous_individuals.rs`
cover each position of an anonymous individual in each tier, keys with anonymous members
and values, the hiding in descriptions, edits, the shared individual and its budget, and
the reports and counts of a run that starts again. `dl_tier_d` and `dl_nominals` cover
the inclusions in each form, a chain beside a range of the top property and item 805's
rewriting beside a data inclusion. The census and the HermiT comparison above are those
of 781da4c8, and a rerun with the build of 3c7d0eb0, after the review fixes, gives the
same left-out axioms, tiers and consistency in all 118 cells and HermiT's taxonomy in all
58 runs, as its
[record](../../benchmarks/el-classification/runs/2026-11-21-anonymous-individuals-laptop-3c7d0eb0/record.json)
shows. The
[record of 3c7d0eb0's timings](../../benchmarks/el-classification/runs/2026-11-21-anonymous-individuals-atlas-3c7d0eb0/record.json)
and the
[record of 781da4c8](../../benchmarks/el-classification/runs/2026-11-21-anonymous-individuals-atlas-781da4c8/record.json),
which holds the earlier timings, the census and the judgements, are discussed in the
[benchmark report](../../benchmarks/el-classification/RESULTS.md#anonymous-individuals-and-top-property-inclusions).

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
threads, deadline and budgets, and writes functional-style syntax, OWL/XML, Manchester
OWL syntax, OBO 1.4, OBO Graphs JSON, RDF/XML, Turtle and N-Triples, gzipped when the
name ends in `.gz`.

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

**Output formats.** `sparkles_owl::owx`, `sparkles_owl::manchester`, `sparkles_owl::obo`
and `sparkles_owl::obographs` write OWL/XML (`.owx`), Manchester OWL syntax (`.omn`),
OBO 1.4 (`.obo`) and OBO Graphs JSON (`.json`) documents with proper escaping, W3C
`PN_PREFIX` prefix validation and bounded nesting (item 686). OBO 1.4 and OBO Graphs JSON
sort axioms canonically for order-independent output, while OWL/XML streams in input order
and Manchester groups axioms into sorted entity frames while preserving input order within
frame sections and miscellaneous frames (repeatable for a given input order). OWL/XML
covers all 37 OWL 2 axiom kinds losslessly, encodes non-`NCName` blank-node labels
injectively and duplicates single-operand equivalences to satisfy XSD `minOccurs="2"`. The
Manchester writer retains standard `rdf`, `rdfs`, `xsd` and `owl` prefix bindings, emits
W3C-standard `HasKey:`, parenthesized nested complements aligned with the parser's depth
budget, standard quoted and typed boolean literals, and `BLANK_NODE_LABEL`-validated
anonymous individuals, synthesizes `DataProperty:`/`Datatype:` and colliding entity frames
for referenced undeclared entities, and refuses or reports ambiguous punned object/data
property constructs. OBO 1.4 follows OBO 1.4 URL-as-ID (`http://` and `https://`) and
`idspace` rules, enforces unique stanza IDs across `[Term]`, `[Typedef]` and `[Instance]`
and single-cardinality stanza tags including `[Typedef]` `domain:` and `range:` (routing
eligible extra or boundary-whitespace annotations to `property_value:` and skipping extra
`instance_of:`, `domain:`, `range:` or unrepresentable values), emits `is_asymmetric: true`
and `subsetdef:`/`synonymtypedef:` headers, preserves reserved header metadata without
conflation, and skips or refuses `DataProperty` declarations, `DataPropertyAssertion`
axioms and non-string qualifiers. OBO Graphs JSON emits and reads `propertyType`
(`OBJECT`, `DATA`, `ANNOTATION`), rejects cross-axiom and same-axiom entity-kind punning
collisions, restricts generic `basicPropertyValues` to plain `xsd:string` values, and
reserves `owl:imports` in graph header `basicPropertyValues` for imports. The Manchester,
OBO 1.4 and OBO Graphs JSON writers accept `WriteOptions` so callers can either refuse the
first unrepresentable axiom or header annotation in strict mode or skip it and receive a
bounded `WriteReport` capping each sample list (`skipped` and `skipped_header`) at
`MAX_REPORTED_SKIPPED` entries. File outputs stage beside the resolved destination
(following symlinks, preserving existing file permissions and applying standard umask mode
to new files) and replace it atomically only after serialization and gzip completion
succeed. `sparkles ontology reason`, `reduce`, `relax`, `materialize` and chains select
these writers from the output file extension, including `.gz` variants, and log a concise
warning to stderr whenever a lossy format skips axioms or header annotations. `sparkles
ontology axioms` and `sparkles ontology classify` also accept `--format owx`, `omn`, `obo`
and `obographs`.

**Limits.** The commands read no dataset on a server (item 685) and refuse
`--prevent-invalid-references true` (item 687). `reason --reasoner emr` asserts
named superclasses only, as ELK does, not the class expressions that ROBOT's expression
materializing reasoner adds, which item 691 follows.

### OML and openCAESAR models

Item 967 surveyed what openCAESAR's analysis of Ontological Modeling Language models
needs from Sparkles. The OML v2 specification gives the mapping to OWL 2 and SWRL as a
table of correspondences, so the axiom-level mapping was read from the public source of
openCAESAR's `oml2owl` adapter and its `owl-reason`, `owl-load`, `owl-query` and
`owl-shacl` tools, which are Apache-2.0.

In that mapping, each OML ontology becomes an OWL ontology with `owl:imports` for its
imports. Concepts, aspects and relation entities become classes, with `SubClassOf`,
`EquivalentClasses`, `HasKey` and an `ObjectOneOf` equivalence for an enumeration, and
property restrictions become universal, existential, value, self and qualified
cardinality restrictions as superclasses. A relation entity becomes a class with a
forward object property and an optional inverse, with domains, ranges and the seven
property characteristics, and a DL-safe SWRL rule derives the forward property from each
relation instance's `oml:hasSource` and `oml:hasTarget`. OML rules become SWRL rules with
class, data range, property, same-individual, different-individuals and `swrlb` builtin
atoms. A vocabulary bundle adds a closure over the concepts and relation entities in its
import scope. Their taxonomy is rooted at `owl:Thing` and made a tree by replacing the
parents of every class with several parents with differences, written as
`ObjectIntersectionOf(A ObjectComplementOf(B))`, and every set of siblings becomes a
`DisjointClasses` axiom, or a `DisjointUnion` when asked. Description bundles add no
axioms. `owl-reason` then runs Openllet on a description bundle, checks consistency and
the satisfiability of every class outside the OML backbone, checks that every instance of
a class with a told minimum, exact or existential restriction has enough entailed
values, and writes the chosen entailments as ontologies. `owl-load` puts each ontology
into the named graph of its IRI with Graph Store `PUT`, into a Fuseki dataset whose
default graph is the union of its named graphs, and `owl-query` and `owl-shacl` post
queries and shapes to it.

The survey translated the Kepler16b sample and its metrology library by a
re-implementation of this mapping, since the sample's Gradle build was not run, and
wrote the Jena rules file in the adapter's form the same way. It compared Sparkles with
HermiT 1.4.5.519 running `owl-reason`'s checks in place of Openllet, with Jena 6.2.0's
rule engine on that rules file, and with Jena's `arq` on the sample's five queries over
the asserted data and HermiT's entailments. The closure is 17 ontologies and 2,998
triples, with 644 logical axioms, 42 SWRL rules, two keys and seven `DisjointClasses`
axioms. The classifier's taxonomy equals HermiT's 102 subclass pairs with or without
counting and the rooted option, and both find the sample consistent with no unsatisfiable
class. `ontology reason` gives HermiT's 307 named types of named individuals and 21 more,
the `swrl:Variable` types of the rule variables that the RDF reader takes for
individuals. OWL RL materialization gives the same 307 types, an `rdfs:Resource` type for
each of the 84 individuals, and 125 of HermiT's 159 object property values, missing only
those that follow from SWRL rules. Four of the five queries
equal the reference under OWL RL, materialized or on demand, and the `connections` query
needs the rules. Sparkles' Jena rule engine derives exactly Jena's 337 facts from the
rules file. `sparkles serve --fuseki-config` starts from the sample's own `.fuseki.ttl`.
With the requests that `owl-load`, `owl-query` and `owl-shacl` send, made with `curl`, it
stores the ontologies and HermiT's entailment graphs, answers the five queries as the
reference does over them and answers the SHACL request.

Thirteen probes each added one violation to the sample. After item 972's datatype checker
in the Horn tier, the classifier agrees with HermiT on eleven of the thirteen probes,
covering disjointness from the closure, keys, asymmetry, universal restrictions, the
closure's differences, functional object properties, a functional string property, two
values of a functional `xsd:double` property and a string in an `xsd:boolean` range, as
well as the two closed-world probes that HermiT finds consistent. It misses a reflexive
fact that a SWRL rule derives and an individual of an enumeration that differs from its
members, and it has no closed-world cardinality check. The rooted run creates no contexts
on the sample, because it refuses keys and functional data properties, and the sample's
base vocabulary and the metrology library declare both. Item 768 takes the keys and
functional data properties, and items 970, 971 and 973 to 977 record the other gaps, the
SWRL reader, the lenient read that drops an `AllDisjointClasses` axiom with an undeclared
member, the `all-disjoint-classes` check that passes its row limit on the sample,
owl-reason's entailment outputs and an OML importer.

A later run checked these stand-ins against openCAESAR's own pipeline. The sample's
Gradle build ran with oml2owl 2.13.1, owl-reason 2.15.0 with Openllet 2.6.5 and
metrology-vocabularies 7.4.0. Its OWL is the same theory as the survey's translation. In
each of the 17 ontologies of the closure the two differ only in the declarations of
imported entities, a `swrl:AtomList` type on each atom list of a rule, some annotations,
the lexical forms of `xsd:double` values and a declaration of `owl:sameAs` as an object
property, which the adapter writes for the rules whose heads are `sameAs` atoms. The
translation also wrote seven `rdfs:subPropertyOf` statements between annotation
properties of ISO 1087-1 that the adapter leaves out. The adapter's closure has 3,197
distinct triples without its `owl:imports`. HermiT gives the same results on it as on
the translation, and Openllet's entailments from the sample's `owlReason` task equal
HermiT's. Both find the sample consistent with no unsatisfiable class, and Openllet's
output holds the same 102 subclass pairs beside 85 reflexive ones, the same 307 named
types and the same 268 property values, with no `sameAs` statement beyond the reflexive
ones. These entailment ontologies also declare each entity, give each individual an
`owl:Thing` type and keep the 56 reflexive subproperty statements and the relation
instances' `oml:hasSource` and `oml:hasTarget` values. They leave out the superclasses
that are `owl:Thing`. `owl-reason` finds all thirteen probes inconsistent. It fails
eleven of them as HermiT does, and it fails the two that HermiT finds consistent by its
closed-world cardinality check, which passes them when it is turned off.

On the adapter's closure Sparkles gives the survey's results. The classifier's taxonomy
equals Openllet's with or without counting and the rooted option. `ontology reason`
gives Openllet's 307 types and the 21 `swrl:Variable` types, OWL RL materialization
gives the same types and misses the same 34 object property values, and the five queries
give the same answers. On the probes the classifier finds the nine inconsistencies that
need no rule or enumeration, including the two OWL 2 DL datatype probes after item 972.
It agrees with HermiT, and with Openllet's consistency check alone, that the two
closed-world probes are consistent, and it misses only the rule and enumeration probes.
The `all-disjoint-classes` check still
passes its row limit. The rules file that the adapter writes with `generateRules` holds
the adapter's four default rules and the six bundle rules of the survey's file, but it
gives its prefixes as `@prefix oml: http://opencaesar.io/oml#` without angle brackets
and writes prefixed names. Jena's parser reads it and derives 321 facts. The 16 more
that it derived from the survey's file came from the translation's extra sub-property
statements. Sparkles' rule parser refuses the adapter's file at its first line, and with
the brackets added it derives exactly Jena's 321 facts. Item 1020 takes the bare
namespace IRIs.

### Completeness for the whole ontology

Item 1271 aligns `complete` with complete OWL reasoners. HermiT, Pellet, FaCT++ and
Konclude either answer for the whole ontology or refuse it, and the OWL API's reasoner
interface has no completeness flag, so its callers read every answer as complete. A
profile reasoner such as ELK ignores the axioms outside its fragment and warns. Before
this item `complete` was ELK's weaker statement, true while `outside` listed axioms the run
left out. It now means that every subsumption between named classes and every
unsatisfiable class that the whole input ontology entails is in the taxonomy. It is false
when the run left out or used only in part an axiom, which `excluded` lists, and for the
partial answer of a stopped run. `completeForTaken` in the wire answers and
`Classification::complete_for_taken` in Rust keep the old meaning, false only for a
partial answer. `partial` is its negation, and the dataset retains a run by it.

A class outside `incomplete` keeps its exact answer when axioms are left out. The
classifier extracts the syntactic bottom-locality module of the class, after Cuenca Grau,
Horrocks, Kazakov and Sattler, and the class is complete when the module holds no axiom
the run left out. Let `O` be the ontology, `L` the axioms left out and `T = O \ L`. A model
of the module `M_C` of `{C}` in `O` extends to a model of `O` that agrees with it on the
module's signature and makes every other class empty, so the named superclasses of `C`
that `O` entails are those that `M_C` entails. When `L` misses `M_C`, `M_C` is inside `T`,
and the run, which is complete for `T`, finds them all. Equivalent classes and
unsatisfiability follow. A satisfiable class equivalent to such a class is in its module's
signature, so its own module is inside `M_C`. Every unsatisfiable class shares the node of
`owl:Nothing`, which a missed unsatisfiable class leaves short, so when any class is
incomplete every unsatisfiable class is listed too. The answer is exact for the superclasses,
equivalents and satisfiability of a class outside `incomplete`, and not for its
subclasses. A class can be complete while a subclass of it is missed, since the subclass's
module can hold a left-out axiom. On item 1271's `irl1`, which a plain run classifies
without `K ⊑ Ans`, `Ans` is complete and its children are not.

The consistency verdict needs its own condition, since an inconsistent ontology makes
every class unsatisfiable. A verdict of inconsistent is sound for `O`. When the run finds
`T` consistent, `O` is consistent if `L` misses the bottom module of the empty signature in
`O` or its top module, whose models extend to models of `O` with every class outside the
signature empty or the whole domain. Otherwise `consistency` is `unknown` and every class is
incomplete. A third layer decides more classes. Without nominals or `ObjectHasValue`
outside the assertions and keys, and without the universal object property, a model of
the terminology's module joins a model of `O` in a disjoint union, so the module of a class
in `O` without its assertions and keys is enough. The module documentation of
`transform/module.rs` gives the three arguments in full, and a rule unsure of an axiom's
locality calls it non-local, which only makes a module larger. The test is syntactic and
over-approximates, so it can list a class whose answer is exact, such as `S` in `irl1`, and
never omits a class whose answer can be missed. `dl_fixtures` checks every complete class
against HermiT on the 640 cases of item 603 with and without the EL-only rules. In 1009
runs that left axioms out, the consistency stayed known in 379 and 1486 of 5914 classes
were complete, with no mismatch.

A classification of the terminology alone, which is the default of `ontology classify`,
HTTP and the bindings without `assertions`, does not read the file's assertions, which are
part of its ontology. Any assertion with logical content among them counts as left out,
whether or not it could change an answer. The run reads the assertions up to the first
such one and reports it in `excluded` under the rule `assertion-not-read`, with one entry
for all of them. The answer is then not complete, every class of the taxonomy is
incomplete, and a consistency the run found becomes `unknown`, while an inconsistency
stays, since it holds for the whole ontology too. A class that only an unread assertion
names is in no taxonomy, so `incomplete` cannot list it, and it shows through `complete`
and the `assertion-not-read` entry, and as a class that is not found. An earlier design
let assertions that the module layers proved harmless leave the run complete. Three
review rounds each found a case it missed, all of the kind of a class that only an
assertion names, so it was removed, and roadmap item 1301 records a sound replacement.
`assertions` true gives the complete answer, and roadmap item 1279 makes it the default.

A triple that maps to no axiom, such as an assertion that a strict read cannot map or
the domain of a property that is not declared, may carry any logical
content. Every classification counts such triples, of the terminology and of the
assertions, in `Stats::unmapped`, and with any of them the answer is not complete, every
class is incomplete and a consistency found is `unknown`. No `excluded` entry names them,
since an entry names an axiom, so the wire answers report the count as `stats.unmapped`
and list the first 100 triples in `unmappedTriples`, in the order the input gave them, as
N-Triples terms, with `unmappedTruncated` when there are more. The CLI lists the first
ten, ROBOT's warning the first five and a strict run's refusal the first three. The OWL
API calls such triples unparsed and reports them through its RDF parser's metadata.
Sparkles calls them unmapped, since a read of the store maps stored triples and parses
nothing. A run of the terminology alone reads the assertions only up to the first that
has logical content or maps to no axiom, so it counts and lists at most one unmapped
assertion triple. A file in functional-style syntax
whose entities are all declared reads without the store and has no unmapped triples. One
with an undeclared entity takes the store's reading, in which a domain or range of an
undeclared property is such a triple. A class assertion whose class is a blank node that
no triple of the terminology describes, such as `:x a _:c` alone, is one too. A
classification later in a ROBOT chain, such as `relax` followed by `classify`, counts the
unmapped triples of the input that the chain read, while a `check` there checks the
mapped document, which has lost them, as roadmap item 1311 records. A classification with
unmapped triples still runs the modules' pass for its left-out axioms, whose answer the
unmapped triples then override, and roadmap item 1309 records skipping it. A typing as `rdf:Property` alone, as RDFS
vocabularies type their properties, carries no logical content, so a classification does
not count it, while the read and the profile checks still report it as unmapped. The GO and ChEBI lite releases in RDF/XML, and stores of
OWL2Bench and of LUBM with their assertions, count no unmapped triple. The triples of a SWRL rule are not unmapped, since
the RDF reader maps a rule and its atoms to assertions about anonymous individuals, so a
classification with `assertions` reads them as assertions and can answer complete while
it ignores the rule. Roadmap item 1302 records this.

The pass costs nothing when nothing is left out and nothing is dropped. A classification of
the terminology alone reads the assertions only up to the first with logical content, and
takes no extraction for them. The pass's extractions share one bound on the
axioms they examine, 1024 for each axiom of the input and at least about a million, past
which the classes not yet decided are incomplete, so an answer depends on the input
alone. `Limits::coverage_work` sets the bound and `Stats::coverage_over` reports it. A
deadline or cancellation during the pass leaves every class incomplete and the
consistency unknown with `coverageStopped` set, rather than discard the saturation, and the
dataset retains the saturation but publishes no such answer. On a loaded laptop with four
threads the pass took about 0.4 s on GO and 0.8 s and 107 MiB on ChEBI lite with the
EL-only rules, which leave axioms of both out. These are single approximate runs. `Limits::coverage` false skips it, which the ROBOT-compatible commands
do since they read only the taxonomy.

`strict` on the request, `--strict` on `ontology classify` and `--strict true` on ROBOT's
`reason`, `reduce` and `materialize` refuse a run that is not complete for the whole
ontology. The refusal is the component error `classification-incomplete`, HTTP 422, which
names the axioms and the rule each breaks, `UnsupportedError` in Python and Node.js, an
unsupported error in the JVM and the FFI, and ROBOT's `INCOMPLETE REASONING ERROR`. The
default stays non-strict, except that ROBOT's commands are strict when they name HermiT
or JFact as the reasoner, since a caller who asks for those expects a complete answer or
an error. `--strict false` turns that off, and ELK and the other names keep the warning.
`reduce` classifies ROBOT's subset of the subclass axioms and the property
characteristics, so its refusal concerns the axioms of that subset. The JVM binding has
no OWL API reasoner adapter and its `ClassifyQuery` names no reasoner, so its `strict`
stays off by default as the opt-in. The content fingerprint of a retained classification is
version 2, since it now covers both flags. The server computes it in memory and stores it
nowhere, so only a client that kept an older fingerprint sees a new value.

### Roadmap follow-ups

#### Incremental native DL classification

Item 753 delivers item 603's affected-module maintenance for axiom insertions and
deletions in two kinds of run. The first is a run with the Horn rules whose index has no
individuals, universal properties, keys or data. It includes a run whose Horn
normalization gives the index no rule that can fire, such as a pair of inverse
properties beside OWL 2 EL axioms in ChEBI, which the EL rules then saturate. The second
is a run of the disjunctive rules with the cautious strategy and without counting or a root.
Both keep their contexts, the contexts of conjunctions or of tests, and for the
disjunctive rules the table of the clauses that contexts sent one another. The other
runs start again after every edit: Horn runs with individuals, which wait on items 590
and 664, and runs whose bound on derived conclusions decides their rules, which a
counting run, a rooted run and the run after a passed bound have and which item 953
follows, since that bound counts derivations in an order that a kept context would
change. The bound of a run of the disjunctive rules that neither counts nor is rooted,
past which the Horn rules answer, is counted across kept runs instead, as described
below.

A run with changes holds the last index until the new one is built and compares the two
by expression. An expression is changed when its successor, its told superexpressions,
its conjunctions, its negative existential restrictions or its disjointness sets differ,
when a Horn flag on it or on a universal's filler differs, and for the disjunctive rules
when a clause with the expression in its body exists in only one index or its
hypothetical successor, its successor's core or its rank in the order of atoms differs.
A context is cleared when it holds a changed expression. For the Horn rules its
subsumers are what it holds. For the disjunctive rules they are its units, its markers
and every literal of every clause its store took, including the clauses a later one
subsumed, since each premise of an inference is one of these and the rules read the
index by the atoms and successor terms of their premises. The run also clears every
source of a backward link into a cleared context and every predecessor that registered
with one, since a predecessor holds what its successors sent back, and the class of a
cleared test that had held. Kept contexts drop the cleared ones from their predecessors,
which register again and are answered again. The tables that no expression keys, the
property hierarchy, chains, ranges and inverses, must be equal on the properties of the
last index, or the run starts again.

The fresh classes of a normalization are named by scope, so that a later run over the
same interner gives the same axiom, unfolded universal or hypothetical successor the
same classes. The interner keeps a copy of each axiom outside OWL 2 EL as the key of its
scope, which the retained bytes count. The scope of an unfolding is the universal or the
negative existential restriction it unfolds, since two restrictions over different
properties can share a filler. The Horn rules unfold every universal of the interner
over a property that chains reach, and the unfolding makes universals over its fresh
classes that a run does not unfold again. The interner marks those, so that a later run
over it does not unfold them either, and a normalization that names one removes the
mark. Without the mark each run gave them new classes and cleared every context that
held one, which was about half the contexts of the Gene Ontology after any edit.

A kept run of the Horn rules leaves each kept context as a run that starts again would.
A kept run of the disjunctive rules does not. Its fresh classes have other ids and so
another order, and a kept successor holds the markers that cleared predecessors sent,
with the clauses they gave. Its answer is still that of a run that starts again. Each
kept clause follows from the edited ontology with its context's core, by the argument
above, and a marker is a tautology, so the context structure is sound for the edited
ontology in the sense of Definition 6 of Bate and others. The saturation then closes
every context under the rules. Theorem 9 of that paper gives completeness for every
context structure to which no rule applies, and the paper notes that the claim does not
depend on the expansion strategy that built the structure. The tests of candidate
subsumers are refutations in contexts of their own, and each run computes the
candidates of every class again, so a kept test answers at once.

A run starts again with a new interner in these cases, which is the safe full rebuild.
The change is to an axiom other than a class axiom or a domain over classes,
intersections, unions, complements and object restrictions, while axioms outside OWL 2
EL are in play. The rules that the edited ontology needs differ from the last run's, or
the options that select rules do. The tables that no expression keys differ. The run
keeps more than twice the expressions, the conjunctions or tests, or the clauses of the
table that the last full run had, plus 4,096. A kept run that stops on its memory budget
also starts again in full before a lower tier takes over, since the last index and the
contexts that nothing reaches count against the budget of a kept run alone. A run kept
by the OWL 2 EL rules does the same, with or without `Limits::el_only`, and the run in
full draws first on what the pool granted the kept attempt (item 1263). Before the
comparison the run admits both indexes, the kept contexts, the clause table and the
marks and queue of the invalidation against that budget. A run that
its deadline or a cancellation stops leaves a classifier whose next run starts again.

A run of the disjunctive rules that neither counts nor is rooted has a bound on the
conclusions it derives, past which it ends and the Horn rules answer (item 990). A kept
run counts the conclusions of every run since the last one that started again together
with its own, and the conclusions of contexts that removed axioms left behind still
count. A kept run that passes the bound starts again in full, as it does past its memory
budget, and the caller gets the answer of that run, whose count starts from its own
conclusions. The sum stands for what a run that starts again over the edited axioms
would derive, but it does not bound it, since that run derives its redundant conclusions
in another order. On one thread with a bound of 6, a run over five axioms with two unions
derives 4 conclusions and the kept run after three more axioms 2, so it answers in full,
while a run that starts again over the eight axioms derives 13 and the Horn rules answer.
A kept and a fresh run therefore agree on whether the Horn rules answer only when their
counts fall on the same side of the bound, which item 1010 follows. The attempts measured
on real ontologies either finish far within the bound or run far past it, and a schedule
already decides the outcome of a fresh run on several threads near the bound. A run past
the bound costs one run that starts again, so a long sequence of edits that each derive
much starts again once in a while.

`tests/dl_incremental.rs` compares each run of a kept classifier with a classifier that
starts again, in subsumptions, taxonomy, unsatisfiable classes, consistency,
completeness and the axioms left out, on 1, 4 and 8 threads. It covers fixed edits that
take away the last support of a subsumption, of a test that held and of a clause that a
successor sent back, random and seeded sequences over generated Horn and disjunctive
ontologies, removals and returns of axioms over the 640 fixtures of item 603 and the
generated ontologies of item 897, the ORE files that `SPARKLES_ORE_DIR` names, a memory
budget that only a kept run passes, with and without a large data value, cancellation and a deadline during a kept run, and
the bound on kept state. `tests/dl_attempt.rs` checks on one thread, whose order fixes
the counts, that a kept run past the bound of item 990 starts again in full and answers
in full when a run that starts again keeps the bound, and that a kept and a fresh run
over the ontology of the test give the same answer, with or without the Horn
rules' fallback, at every bound from one conclusion to past what both derive. `examples/incremental_classify.rs` times kept runs after
single-axiom edits against a classifier that starts again and checks their taxonomies.

A kept run still normalizes every axiom and builds its index and its taxonomy again, so
an edit saves the saturation of the kept contexts and not those steps, which item 955
follows. The memory tradeoff is the last index beside the new one during the
comparison, the copies of the axioms outside OWL 2 EL, and the expressions, tests and
clauses that nothing reaches until the run starts again.

The figures below are development measurements on a laptop that ran other work, with
`examples/incremental_classify.rs`, and are not benchmark-host results. Each file takes
20 removals of one class axiom, each followed by its return, and the time of a kept run
after a removal is compared with a classifier that starts again over the same axioms,
both as medians. The taxonomies are equal after every run.

| Ontology | Rules | Four threads, kept against full | One thread, kept against full |
|---|---|---|---|
| `ore_ont_12141`, 440 contexts | Disjunctive | 6.4 against 42.8 ms | 3.9 against 91.8 ms |
| `ore_ont_6485`, 215 contexts | Disjunctive | 0.67 against 3.5 ms | 0.64 against 8.5 ms |
| Five other ORE 2015 DL files | Disjunctive | 1.2 to 2.4 times faster | 1.1 to 2.0 times faster |
| Eleven ORE 2015 DL files | Horn | 1.1 to 2.0 times faster | 1.1 to 2.3 times faster |
| Gene Ontology, 54,692 contexts | Horn | 143 against 219 ms | 147 against 540 ms |
| ChEBI lite, 238,151 contexts | Horn, no rule fires | 530 against 981 ms | 743 against 1,921 ms |

The median edit clears one to three contexts on the two large files and on most of the
ORE files. On the Gene Ontology a kept run saturates in 0.4 ms where a full run takes
120 ms on four threads, and spends 93 ms on the normalization, the index and the
comparison against 52 ms in a full run, and 30 ms on the taxonomy in both. An edit that
clears most contexts therefore costs more than a run that starts again: the removal
that clears 331 of the 440 contexts of `ore_ont_12141` takes 94 ms on one thread, where
the full runs take 80 to 95 ms. Item 954 follows the cost of the comparison. A counting
run of `ore_ont_6485` keeps nothing and takes the time of a full run.

The retained state after the 20 edits is within 3% of the state after the first run,
151 against 147 MB for the Gene Ontology and 653 against 636 MB for ChEBI lite. The
peak of a kept run on four threads is 1.3 to 1.45 times that of the first run, 212
against 147 MB for the Gene Ontology and 863 against 651 MB for ChEBI lite, since the
run holds the last index and the chunks of the parallel normalization at once.

Item 590 owns assertion retention, and item 664 delivered EL individual, merge, key and
world invalidation. Coordinate their dependency records before making those changes eligible
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

### Partial answers from every tier (item 714)

`Limits::partial` and `ClassifyRequest::partial` now apply to EL (`Plain` and `Full`)
and Horn (`Horn` and `HornFull`) classifications alongside Tier D across the library,
HTTP, CLI and language bindings, while `Saturation::saturate`, DL queries and ROBOT
commands remain non-partial. When a deadline, cancellation or memory budget stops an EL
or Horn run after saturation begins, the classifier records interrupted activations on
`State::interrupted` inside `Run::process`'s existing partial epilogue without adding
branches to the non-partial hot loops, bounds `Run::flush_links` uniformly and stops
`horn_rules::flush` promptly on `sends.stop`. A unified two-pass flat-CSR and compact
bitset dependency recorder (`classify::partial::record`) handles EL, Horn and Tier D
stops with strict pre-allocation admission against `max_memory` and `charge`, a
zero-allocation inconsistency pass over `owl:Thing`, individuals and real contexts, exact
Phase 1 and Phase 2 backward reachability, Phase 3 partial world retention and a
zero-scratch conservative fallback when admission fails. Before admitting retained state
and building a partial EL or Horn taxonomy, the classifier frees conjunction contexts,
pending to-do buffers and non-subsumer tables and sets `partial_over` on memory refusal
so a Horn run preserves its tier rather than silently downgrading to EL.
