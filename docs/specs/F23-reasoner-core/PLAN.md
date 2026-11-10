# F23 implementation plan

This plan orders the remaining phases of the [reasoner core](SPEC.md) and fixes the
decisions that the [candidate phases](SPEC.md#candidate-phases) left open. It names the
files and types each phase changes, the maintenance algorithms with their correctness
arguments, the memory bounds and the gates. Roadmap [item 456](../../ROADMAP.md) holds the
workstream, and the numbered items below are its phases.

## What exists

The inventory below is the state the plan starts from. File paths are under
`crates/sparkles-reasoner/src` unless another crate is named.

The rules come from `parser.rs`, which reads Jena rules into `Rule`, `Clause` and `Node`,
and `engine.rs` compiles them to `CRule` with `Atom` over three `Slot`s and `CBuiltin`.
Every atom is a pattern over one ternary relation of triples, and n-ary relations exist
only through the tuple nodes that C24 §6.4 makes with `makeSkolem`. `engine::plan_with`
orders a body greedily by `estimate`, which reads per-predicate counts and distinct
subjects and objects. `engine::run_observed` runs the semi-naive iteration, splits work
into chunks of 2,048 derived or 4,096 base rows, and stops at a candidate limit derived
from `max_inferred`.

The facts live in two tiers. `facts.rs` is the base tier, which reads the snapshot's
permutations and never copies the input. `graph.rs` is the derived tier. A derived fact is
a `[u64; 3]` row in `triples`, numbered by a `u32` row id from the base tier's size on.
The row is found by triple through 64 sharded `HashTable<u32>`s and by predicate, by
predicate and subject and by predicate and object through `FxHashMap`s of `SmallVec<[u32;
4]>` row lists, with optional per-subject and per-object lists built on demand. The row,
its hash slot and the three list entries come to about 135 bytes when most `(p, s)` and
`(p, o)` keys are distinct, which is the figure the docs cite. Deletion marks a row dead
in a bitmap, and `incremental::Closure` adds one bit per row that marks a derived row as
explicit. The base tier takes row ids too, so an input of more than `u32::MAX / 2` triples
is refused.

The closure operators in `closure.rs` detect transitive rules, the guarded `prp-trp` and a
symmetric rule paired with a transitive one, and evaluate them after each merge with
Tarjan's condensation, reachability sets and union-find. `lib.rs:431` enables them only in
a full run. Incremental updates, query sessions, witness builds and analysis evaluate the
rules as joins. The operators keep no provenance.

Maintenance in `incremental.rs` is backward/forward maintenance after Motik, Nenov, Piro
and Horrocks. A deleted explicit fact is a candidate, `Search::prove` looks for another
derivation depth first through every rule instance that derives it, and only facts without
a proof are deleted. No fact carries a derivation record, so a candidate's proof search
starts from nothing. On a transitive clique that search visits many rule instances per
checked fact, so deleting 462 triples from OWL2Bench(1) grew past 14 GiB where a full run
needs less than 800 MiB, and roadmap item 798 bounds the search by the cost of a full run.
Insertion appends the new explicit facts as flagged delta rows and runs the semi-naive
engine from them. Any rule with `noValue`, `now`, `makeTemp`, `makeSkolem`, a head action,
a blank node in its head or an uncovered `listForAll` makes every update a full run, as do
a changed or derived list when the rules read lists, changed rules, changed input graphs
and, with at least 10,000 explicit facts, a deletion of more than one in twenty of them.
The kept closure, `maintain::Cache`, holds the derived tier in memory and rebuilds it from
the inferred graph after a restart.

Query-time reasoning builds its programs in `demand.rs`, magic-set programs with sideways
information passing in up to three stages, with a cap of 50,000 rules, and `query.rs` runs
them on a `Graph` over the snapshot with a per-view table cache. A compact session
evaluates the original rules to closure when the view is small and the rule constants fit
in 128 terms.

`witness.rs` computes bounded minimal supports per derived fact after the
closure, from the rule instances that the backward plans find, with F13's `prov` types.
`explain.rs` and `whynot.rs` do not read them and derive their answers again.

The incremental core, `crates/sparkles-core/src/incr`, maintains views as Z-set
circuits whose operators probe the store's permutations at the old and new snapshots. Its
fixed-point operator, `incr/fixpoint.rs`, is not a timestamped recursive circuit. It is a
worklist over a hash set of facts driven by a `Program` trait with `axioms`,
`consequences`, `prove`, `broken` and `added`, and it keeps for every fact one record, the
instance that first derived it. Records are well founded, since an instance is recorded
only over facts that held before it. A pass deletes by backward and forward rederivation
from the broken records, so a fact whose record is intact is never checked. Two programs
implement the trait, the property-path closure of `incr/ops/path.rs` and the reasoner's
`fixpoint.rs::RuleProgram`, which takes forward triple-pattern rules over at most four
variables and is used by nothing but its tests. Footprints exist as `incr::footprint::
Footprint` for circuits and `recompute` nodes, as `InferredFootprint` for views over
RDFS on read, and as the reasoner's watched graphs and schema-read analyses, with no shared
type. A view over a dataset with inferences is one `recompute` node, so no circuit reads
the derived tier incrementally, which roadmap item 221 covers.

The reference is item 791's Nemo program. `scripts/rl-reference/owl2rl.rls` writes the OWL
2 RL rules, and aligned with `sparkles-extensions.rls` and without the reflexive
equivalence cases it derives the same closure as `owl-rl` triple for triple on LUBM(1),
OWL2Bench(1) and 66 W3C OWL 2 RL cases, while the W3C rules alone derive a subset that
`owl-rl` contains. `mise run test:rl-reference` and `bench:rl-reference` check and time
it. On the development laptop Sparkles materialized LUBM(1) in 0.25 s against Nemo's 1.83
s and OWL2Bench(1) in 1.44 s against 14.88 s, both on one thread, but its peak resident
set on OWL2Bench(1) was 462 MiB against 129 MiB, about 346 bytes per derived fact against
about 97. Nemo runs on one thread and has no incremental maintenance. Speed is therefore
not where the reasoner loses to its reference, and memory per derived fact is.

## Decisions

### The derived tier is columnar and sorted

The tier becomes, per predicate, two sorted orders of the facts, by subject then object
and by object then subject. Each order is a list of immutable sorted runs plus one mutable
run for the iteration in progress. A run holds its columns as separate arrays, `s`, `o`
and the fact id, sorted by its key. Each derived fact gets a dense `u32` fact id in the
order of first derivation, as rows are numbered today, and the tier keeps a position
table from fact id to its place in the subject order, a dead bitmap, an explicit bitmap
and the per-predicate counts. A probe with a bound predicate and subject gallops in the
subject order, one with a bound object gallops in the object order, a bound triple is a
lookup in the subject order, and a step whose bindings arrive sorted is a merge join. The
base tier's PSO and POS permutations are sorted by the same ids, so a step reads both
tiers as one merge. A run is immutable until a compaction merges the runs of an order,
which happens when an order has more than eight runs or when a quarter of its facts are
dead, and the compaction rewrites the position table in one pass. Rows with a variable
predicate and a bound subject or object read sorted orders over the whole tier, built on
demand as the per-subject and per-object lists are today.

The semi-naive ranges of `engine::run_observed` keep their meaning, since the runs of one
iteration are the delta and the fact id bounds give the visible end that `noValue` and the
list builtins read. Each order costs 20 bytes per fact for its two ids and the fact id,
and the position table 8, which gives 48 bytes before provenance, with two bits of bitmaps
and the run headers on top, within the 40 to 60 bytes that RDFox reports and about a third
of today's figure. A compaction holds the old and new runs of one order at once, so the
peak adds that order's size while it runs, and an order over the whole tier that a
variable-predicate rule builds on demand adds 20 bytes per fact while it exists. The base
tier no longer takes row ids, so the `u32::MAX / 2` cap on the input goes and the `u32`
fact id caps only the derived facts. A second layout, a dense fact table by fact id with
the two orders holding fact ids alone, costs 32 bytes but pays an indirection per
comparison, and the phase measures both before it keeps one.

The columnar tier reverses a dependency that the spec stated. F16 §5's derived layer, item
9 Phase 2, writes sorted permutations of derived triples to disk, and the tier's subject
order sorted by `(p, s, o)` is that input. The plan proposes that the tier come first and
the layer serialize it, since nothing in the tier waits for item 9, and the maintainer
decides the order below.

### Provenance is a derivation record per fact, with counts only where measured

Every derived fact keeps a record, the rule and the variable bindings of the instance that
first derived it, as `RuleProgram::Record` holds them today. The bindings name the body
facts by instantiating the rule's atoms, so a premise in the base tier, which has no fact
id, and one in the derived tier are addressed the same way. The records are a column keyed
by fact id that holds a `u16` rule index and a `u32` offset into a pool of `u64` bindings,
so a record costs 6 bytes plus 8 per rule variable, from 6 bytes for an axiom to 94 for
`prp-spo2-3`, whose body binds eleven variables, with 30 bytes for a three-variable rule
and 38 for the four-variable domain, range and inverse rules. A deletion finds the broken
facts without a
reverse index, as the operator does. The instances that read a removed fact are enumerated
over the old closure, and a head is broken when its record is that instance.
The record is the same object as the fixed-point operator's `Record`, and it is well
founded for the same reason, since a merge records an instance only over facts that held
before the iteration. A fact of a closure relation records its predecessor on the path, as
`incr/ops/path.rs` does, so the operators keep provenance without changing their
algorithm. The record gives maintenance a reason to skip a fact, gives `Why` one real
witness without a search, and gives F18's reviewed-only visibility a cheap test. The
bounded antichain of minimal supports that item 416 computes stays an option on top, stored
in the same columnar form under item 556. Derivation counts are not part of the design.
F13 §4.3 rules them out for recursive strata, and in a non-recursive stratum a fact whose
record is intact already survives a deletion without a search. The provenance phase
measures counts on LUBM deletions and keeps them only if they are faster than the record
check, recording the result in `PERFORMANCE-EXPERIMENTS.md` either way. Records are
charged to the run's budget, and a run that keeps no closure may drop them at its end.

### One maintenance core is the record-based backward/forward maintainer

The spec asked for a recursive Z-set circuit with iteration timestamps. The operator that
item 417 built is not one, and the reasoner's maintenance is of the same family as the
operator's, so the shared core is the record-based backward/forward maintainer of
`incr/fixpoint.rs`, and the reasoner becomes its largest `Program`. The reasoner's rule
engine over the columnar tier implements the trait, with `consequences` as the
semi-naive evaluation of the instances that read a fact, `prove` as `Search::prove` under
item 798's bound, and `broken` as the facts whose record reads a removed fact. The trait
gains batched forms of `consequences` and `prove`, with the per-fact forms as defaults,
because a rule engine over sorted runs evaluates a batch of facts in one merge, and it
gains a program-owned fact store, so the reasoner's tier replaces the operator's hash map
of facts for that program while the path closure keeps its own. The toy `RuleProgram`
retires once the measurement of item 547 is recorded. Property paths in views and the
reasoner's rules then share one operator, one `Record` type and one `Footprint` type, and
the reasoner's pass emits the derived tier's change as a Z-set that a view's circuit reads
beside the commit's own changes, which is what item 221 needs.

The transitive and equivalence relations are maintained by their operators inside this
core. A relation's facts record the predecessor on their path, a deletion breaks the facts
whose chains pass a removed edge, and the operator proves the broken facts again by
reachability from the affected components over the remaining edges, which costs at most
the operator's part of a full run. Insertions are the operator's late-edge mode, which
reads only the nodes that reach the new edges. The demand engine gets the same operators
seeded by its magic facts, so a query over a transitive relation walks from its seeds
instead of joining within the reachable component.

### Correctness argument

The result of every maintained run must equal a fresh full run, and the argument has four
parts. First, records are well founded, so following any fact's record ends at base facts
of the snapshot the closure was built against. Second, deletion is the backward/forward
algorithm of Motik and others. The candidates are the facts whose record reads a removed
base fact and, through records, everything that depends on them. A fact that is not a
candidate has a record chain of facts that still hold, so it is in the new closure and is
rightly kept. A candidate is kept when a backward search finds an instance over
non-candidates at the new snapshot or when forward chaining from the kept and added facts
derives it again, and deleted otherwise, which the paper proves gives exactly the new least
fixpoint. Third, insertion is semi-naive evaluation from the added facts over the kept
closure, which reaches the least fixpoint that contains them. Fourth, a closure operator
computes the least fixpoint of its rules over its edge relation, so replacing the rules by
the operator inside the iteration is chaotic iteration of monotone operators, which
converges to the program's least fixpoint by Knaster and Tarski, and recomputing the
operator over the remaining edges after a deletion gives the facts a full run would derive
for that relation. Non-monotonic rules are outside this argument. The full run evaluates
`noValue` against the facts visible when the rule fires, as Jena's forward engine does, so
a negated rule can fire before another rule derives its blocker, and `tests/demand.rs`
records a program where the stratified answer differs. Maintenance stratum by stratum
gives the stratified answer, so it can equal a full run only once the full run evaluates
stratifiable programs by strata too, which changes results and is a decision for the
maintainer below. Until then every program with `noValue` runs in full. `makeSkolem` is a
function of its inputs within one run, but its node is written as a fresh blank node, so a
maintained run would keep nodes a fresh run does not name, and rules with `makeSkolem`,
`now`, `makeTemp` or blank nodes in heads keep the full run until the identity decision
below. The gates below check the argument on random histories against
a full run, on the clique and chain cases, and against Nemo's closure of the changed data.

### Memory bounds

A full run's memory is the tier, the records, the term table, the iteration's candidate
buffers and the operators' workspace. The tier and records are bounded above by the bytes
per fact stated in the two decisions above, with one order's size added while a compaction
runs, the candidate buffers by the candidate limit
that `max_inferred` derives, and the operators charge their reachability sets before they
grow, as today. A deletion adds the candidates and the proof searches, which item 798's
visit bound keeps below a multiple of the full run's cost, and the maintainer falls back to
a full run past it. Every structure charges `Budget` before it grows and `max_memory` fails
the run, as F16 §3.4 specifies. The tier's peak at OWL2Bench(5) and (10) is measured in
the first phase, which folds in item 522's profile, and the gate is a halving of the peak
heap per derived fact rather than a figure chosen before the profile.

### A native n-ary core is a generalization of the tier, not a second engine

A run's columns are fixed-width `u64` tuples with a sort key prefix, so a relation of
arity n is an order over n columns and the triple relation is the arity-2 case partitioned
by predicate. The n-ary core adds a rule IR with named relations and arities, a Jena
frontend that maps triple patterns onto the partitioned relation, a planner over several
relations, a restricted chase with weak acyclicity and later MFA for existential heads,
stratified negation for every relation, and aggregates in two steps, stratified first and
monotone in recursion second. Property chains and keys of any length, item 517, are its
first consumers, through auxiliary relations over the list prefixes, so one rule covers
every length and the demand programs lose the rules per length that the 50,000 cap
counts. The Datalog surface syntax stays item 72 and is a frontend of the same IR.

### Rule frontends translate to the IR

`sh:TripleRule` compiles to a forward rule in the engine's current form, and to the IR once
Phase 6 defines it, whose body is the `sh:condition` conformance, as a builtin over the
SHACL validator, and whose head is the triple of its node expressions.
`sh:SPARQLRule` runs its CONSTRUCT query through the query engine over the base and the
derived tier, deduplicated as a set, and is maintained natively when its body is a basic
graph pattern with filters and otherwise as a recompute stratum of the shared core, which
Phase 4 supplies. Rules
run in `sh:order`, and the rules of one order iterate to a fixed point before the next
order starts, with a one-pass option, which is an open decision below. SPIN rules translate
to `sh:SPARQLRule` textually. N3 rules take the subset of `{ } => { }` without nested
formulas and with the `log:` and `math:` builtins that have Jena counterparts. SWRL imports
the DL-safe subset from F21's axiom model once item 71 lands. None of them gets an engine
of its own.

## Phases

The phases are listed in dependency order. Each names the roadmap item that holds it, its
files, what is routine and what needs judgment, and its gates. A gate common to every phase
is that `tests/owl_rl_rules.rs`, `tests/closure.rs`, `tests/incremental.rs`,
`tests/witnesses.rs`, `tests/demand.rs` and `mise run test:rl-reference` pass, and that
`mise run bench:rl-reference lubm 1` and `owl2bench 1` report equal closures before and
after every insertion and deletion. The reasoner's speed target against its reference is
three to five times faster than Nemo on full runs at every scale and thread count that
both complete, with a tie as the floor, and an incremental run must beat Nemo's full run
of the changed data by the same factor. Published numbers come from forge, lanes measure
on atlas, and effects under 5% need atlas.

### Phase 0, measurements that the plan reads

Item 547 measures `RuleProgram` on `incr/fixpoint.rs` against `incremental::update` on
LUBM(1) and LUBM(10) with the insertions and deletions of `examples/incremental.rs`, on
atlas. The plan reads two numbers from it, the cost of a record-based deletion against a
record-free search and the plug-in's about 144 bytes per fact, its key, record, slot and
three index entries, against the tier's. It is routine, and it runs beside the first
phase.

Item 791's published numbers come from `bench:rl-reference lubm 100` and
`owl2bench 1` to `5` on atlas and then forge, published in the reasoning results, so
every later phase has a baseline per scale. It is routine.

Item 798's bounded deletion lands first, so every maintained run has a cost bound
and a full-run fallback before the tier changes under it.

### Phase 1, the columnar derived tier, item 835

A new module `tier/` holds `run.rs` for the sorted columns, `order.rs` for the runs of one
order with galloping and merge probes, `compact.rs` for the merge of runs and the
position table, and `mod.rs` for the `Tier` with its per-predicate relations, bitmaps and
counts. `graph.rs` becomes a thin front over it in the first step and goes in the last.
`engine.rs` changes `Eval::rows` and `base_facts` to probe the tier, `plan_iteration` to
address runs, `Cands` to carry run slices, and `estimate` to read the tier's counts.
`incremental::Closure` moves its explicit flags to the tier's bitmap, `budget.rs` charges
runs and the position table, and `maintain.rs` keeps saving and reconstructing the closure
from the inferred graph unchanged. `query.rs` sessions use the same tier.

The run and bitmap structures, their property tests and the budget accounting are
routine. The probe interface, the compaction policy, the merge-join step and the
derivation-order guarantees are judgment work. The phase first profiles OWL2Bench(5)
where item 522 asks, so that the gate below has a measured peak to halve.

The gates of this phase are these. The closure of every existing test equals the one
before, and the fact ids give the same order of first occurrence, which the compact
sessions' round boundaries and the non-monotonic tests depend on. The measured bytes per
derived fact fall below 135 and are recorded beside RDFox's 40 to 60. LUBM(100) and
OWL2Bench(1), (2) and (5) under `owl-rl` and `rdfs` are no slower than the build before on
atlas at one thread and all threads, and the peak heap per derived fact at OWL2Bench(5) is
at most half. OWL2Bench(10) completes within 32 GiB or the phase records where the memory
goes. The small incremental runs of `examples/incremental.rs` and the reasoning latency
gate of item 472 show no regression beyond 5%, since query sessions share the tier.

### Phase 2, closure operators in demand programs, item 521's first half

`demand.rs` marks the transitive and equivalence relations of a program, `query.rs`
builds `Closures` for a session with the magic facts as seeds, `closure.rs` gains a seeded
mode that walks from the seeds and emits only the facts the adornment asks for, and the
compact session takes the operators as a full run does. It needs no change to the tier,
so it runs beside Phase 1, and it touches `closure.rs` only in a new mode.

It is judgment work in the seeding and in the equivalence case, where a seed's class must
be closed before the `eq-rep` rules read it, and routine in the session plumbing.

The gates of this phase are these. The four OWL2Bench(1) queries that took 77 to 87 s each
return the same rows faster, every demand test returns the rows it returns today, and the
reasoning latency gate shows no query slower.

### Phase 3, derivation records and compact provenance, item 836

`tier/records.rs` adds the record column and the bindings pool. `engine::run_observed`
records the first instance of each new fact in the merge, `closure.rs` records the
predecessor on a path, `incremental.rs` finds the candidates of a deletion through records
and consults `Search::prove` only for broken facts, `witness.rs` takes the record as the
first support and builds the antichain only on request, `explain.rs` answers a `Why` from
the records when a closure keeps them, and `maintain.rs` keeps the records with the kept
closure and rebuilds them with it. The counts experiment runs on LUBM deletions and its
result goes to `PERFORMANCE-EXPERIMENTS.md`. Persistence of the records with the stored
closure is item 556 and follows item 9 Phase 2's layer.

The record column, pool and tests are routine. The candidate set through records, the
equality of its results with the record-free search and the operator records are judgment
work.

The gates of this phase are these. The differential deletion tests equal a full run on
random histories and on the clique of item 798, whose test must pass within the full run's
memory, with the item 798 bound only as the last resort. Every witness in
`tests/witnesses.rs` and every `Why` answer derives its triple. Records add at most 6
bytes plus 8 per rule variable per fact, measured. The deletion time of the LUBM small
incremental runs falls, and the full runs are no slower than Phase 1 on atlas.

### Phase 4, the shared maintenance core, item 837

`crates/sparkles-core/src/incr/fixpoint.rs` gains the batched `consequences` and
`prove`, the program-owned fact store and the Z-set change output. `fixpoint.rs` in the
reasoner replaces `RuleProgram` with the engine's `Program` over the tier, `incremental.rs`
becomes that program's `prove` and `consequences`, `lib.rs::update_closure` calls
`FixedPoint::advance`, and `maintain::Cache` keeps a `FixedPoint`. The closure operators
become operator-maintained relations of the program, which is item 521's second half.
`incr/footprint.rs`'s `Footprint` replaces the reasoner's watched graphs in `inputs.rs`
and the schema reads of `demand.rs` and `query.rs`, and `InferredFootprint` goes.
The per-list tracking of F16 §7.3 removes a full-run fallback and can be its own routine
lane once the core lands, and stratified `noValue` follows it once the maintainer decides
the full run's negation semantics below. The
`LARGE_DELETION` threshold gives way to the cost estimate of F16 §7.3 using the visit
bound of item 798. The pass emits the tier's change as a Z-set so a view can read the
inferred graph, which item 221 then builds on.

The trait changes, the program and the operator-maintained relations are judgment work
and stay with their planner. The footprint merge and the three fallback removals are
routine against the differential tests.

The gates of this phase are these. Every differential test of both frameworks passes,
including `fixpoint_tests.rs` for path views. The maintained closures after insertion and
deletion on LUBM(1) and OWL2Bench(1) equal Nemo's. The small incremental runs and
`bench:lubm`'s incremental runs are no slower than Phase 3 on atlas, and the deletion of
the item 798 clique stays incremental. No full-run fallback remains for changed lists, nor
for stratified `noValue` once its decision is taken, each with a test that fell back
before.

### Phase 5, rule frontends, item 838

A `shacl-af` reasoning profile reads a shapes graph among the inputs, `sh:TripleRule`
compiles to the engine's current rule form, and to the IR after Phase 6, through a
`shConforms` builtin that calls the SHACL validator,
`sh:SPARQLRule` compiles to a query stratum, SPIN translates to `sh:SPARQLRule`, and the
Fuseki configuration maps TopBraid's rule settings. N3 rules get a parser for the subset
above into the same rule form. SWRL waits for item 71. `sh:TripleRule`, the SPIN
translation and N3 target the engine's current rule form and need only Phase 1, so they
run beside Phases 3 and 4. A `sh:SPARQLRule` whose body is not a basic graph pattern needs
the recompute stratum of Phase 4, so that half follows it.

The translations and their conformance tests are routine. The semi-naive form of a
`sh:SPARQLRule` body and the iteration semantics within an order are judgment work.

The gates of this phase are these. The TopBraid SHACL-AF test cases pass for triple and
SPARQL rules, a maintained `shacl-af` run equals a full run on the differential histories,
and the W3C and Nemo gates are unchanged.

### Phase 6, the native n-ary Datalog core, item 839

`ir.rs` defines `Relation`, `Atom`, `Rule` and `Program` with strata, `parser.rs` and
`engine::compile` become the Jena frontend over it, `tier/` takes arity-n runs, `plan_with`
plans over several relations, a chase module checks weak acyclicity and later MFA and
skolemizes existential heads, and `demand.rs` adorns n-ary atoms. Item 517 lands as its
first consumer, and the stratified aggregates and then the monotone aggregates in
recursion follow as separate lanes. The IR can be defined as soon as Phase 1 lands, and the
n-ary evaluation waits for Phase 4 so the maintainer sees one program shape.

The IR, the frontend and item 517 are routine against the Nemo program, which already
takes lists of any length. The planner over relations, the chase and the aggregates are
judgment work.

The gates of this phase are these. Chains of four and more properties and keys of three
and more derive the same facts as Nemo on test ontologies and the W3C cases. The demand
programs of `owl-rl` shrink below the 50,000 cap with the length rules removed, measured.
Full runs are no slower on atlas, and ChaseBench's open scenarios are the acceptance set
for existential rules.

### Phase 7, convergence and optional kernels

The RDFS rule profile with the shared explainer, explanation routing and entailment
analysis in memory keep the phases and dependencies of the spec, after items 190, 53, 18,
285 and 286. Compiled or specialized kernels for the fixed profiles are a measured
experiment after Phase 1, kept only on a win on atlas and recorded in
`PERFORMANCE-EXPERIMENTS.md` otherwise, since the planner experiment of item 487 showed
that the interpretive dispatch is not the dominant cost it was assumed to be.

## What runs in parallel

Phases 0, 1 and 2 start together, since item 547 and the reference numbers are
measurements, Phase 2 changes the demand engine and a new operator mode, and Phase 1
changes the tier under the engine. Phase 3 follows Phase 1. Phase 4 follows Phase 3 and
the measurement of item 547. Phase 5's triple rules, SPIN and N3 can start after Phase 1
and run beside Phases 3 and 4, and its general `sh:SPARQLRule` bodies follow Phase 4.
Phase 6's IR can start after Phase 1 and its evaluation after Phase 4. Items 518 and 520
are independent routine lanes at any time. A lane that touches `engine.rs` and `graph.rs`
does not run beside Phase 1, and one that touches `incremental.rs` does not run beside
Phase 3 or 4.

## Open decisions for the maintainer

The plan recommends an answer to each question but does not take it, and the phases that
depend on an answer say so.

1. Is the memory gate half the peak heap per derived fact at OWL2Bench(5), as the plan
   proposes, or parity with Nemo's resident set at OWL2Bench(5) and LUBM(100)? Nemo's
   resident set per derived fact at OWL2Bench(1) is about a quarter of Sparkles'.
2. Does the columnar tier come before item 9's derived layer, with the layer written from
   the tier's runs, as the plan proposes? The spec stated the reverse dependency, and the
   proposal leaves `storage: "layer"` waiting on Phase 1.
3. Does a `sh:SPARQLRule` order iterate to a fixed point before the next `sh:order`, as the
   plan proposes with one pass as an option, or run once? The SHACL-AF note allows both
   readings, and the choice is visible in results.
4. Should the full run evaluate a stratifiable `noValue` program by strata, as the demand
   engine does? It changes the result of a program whose negated rule fires before its
   blocker is derived, and it is what lets Phase 4 maintain such programs. The plan
   recommends it as an option that the maintained profiles turn on.
5. Should `makeSkolem` nodes get a stable identity, a skolem IRI from a hash of the rule
   and its arguments as F13 §4.6 gives CONSTRUCT views, so that rules with `makeSkolem`
   heads can be maintained? Each run writes fresh blank nodes today. The plan recommends the
   stable identity under a new option and keeps the full run until then.
6. Are derivation records the only provenance, with counts dropped unless Phase 3's
   measurement shows them faster? F13 §5 lists counts among its annotations, and the
   maintainer may want them kept for views regardless of the reasoner's result.

## The first implementation lane

The columnar derived tier, item 835, is the first lane. Every later phase stores into it,
the reference comparison shows memory per derived fact as the loss, and it unblocks the
derived layer, the records and the shared core. It is judgment-sensitive work for a
planner that implements it, with the run and bitmap structures as its routine portion.
Item 547 and item 521's demand half start beside it.
