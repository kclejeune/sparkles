# X20: Formal verification

> **Status:** specified.
>
> **Roadmap:** [item 1223](../../ROADMAP.md) holds this work. Each candidate phase below
> gets its own numbered roadmap item when it is ready to start.

The design covers model checking, bounded proofs and mechanized proofs for the parts of
Sparkles whose faults ordinary tests are least likely to find.

## Problem and goal

Sparkles checks its answers against conformance suites, reference engines and
differential tests over corpora. Agreement on a corpus shows that the engine answers
those inputs correctly, but it can miss a whole class of wrong answers that no input in
the corpus reaches. The rooted calculus of the OWL classifier showed this, since a
calculus that agreed with HermiT across the benchmark corpus still needed a completeness
argument for its departures from the published rules. Durability and concurrency faults
hide in a similar way, because they appear only at a crash point or an interleaving
that an ordinary test does not choose.

The goal is to check the properties that matter most against a model or a bounded
input space, so that a class of faults is ruled out instead of sampled. Each phase
targets one subsystem, names the property it checks and runs as part of the build.

## Scope

The work covers three kinds of evidence. Model checking with TLA+, P or Alloy explores
every state of a small model of a protocol. Bounded proofs with Kani and exhaustive
concurrency tests with Loom or Shuttle check the Rust code itself within stated bounds.
Mechanized proofs in Lean or Isabelle cover algorithms whose correctness does not reduce
to a bounded search. Property tests and fuzz targets accompany each phase, since they
tie a model or a proof to the code that implements it.

The rooted calculus first gets a rigorous paper proof with an adversarial review as part
of items 823, 1061, 1075 and 1220, outside this spec. Its mechanized proof is a later
phase here.

## Candidate phases

**Commit log, crash recovery and group commit.** A TLA+ or P model of acknowledgement
and recovery checks that an acknowledged commit survives a crash at any point and that
recovery yields a prefix of the commit sequence. A model-based crash test in Rust drives
the real log through the model's crash points and compares the recovered state with the
model's. It serves [CI](../CI-commit-identity.md) and
[X09](../X09-group-commit-by-default.md).

**Snapshot reads, commit and compaction concurrency.** Loom or Shuttle tests explore the
interleavings of the publish path and reader pinning, so that a reader never sees a
partly published generation and a pinned generation is never freed. Further tests run
compaction beside readers and `sparkles check`. It serves
[X16](../X16-storage-consolidation/README.md) and
[X17](../X17-tiered-segments/README.md).

**On-disk format decoders and version refusal.** Kani bounded proofs show that each
decoder neither panics nor reads out of bounds on any input within the bound. Round-trip
properties check that decoding an encoded value returns it, and cargo-fuzz targets
extend the search beyond the bound. The proofs also enforce the rule that a reader
refuses a format version it does not know. It serves
[X16](../X16-storage-consolidation/README.md) and the codec specs
[X01](../X01-compression-codecs.md) and
[X11](../X11-index-compression-and-predicate-patterns.md).

**Resource admission and memory budgets.** Kani proofs on the admission core show that
nothing is allocated after a refusal and that every reservation returns its credit, so
that the budget returns to its baseline when a query ends. It serves
[C01](../C01-observability-and-budgets.md).

**Access control.** An Alloy or TLA+ model of the policy language checks that grants,
deny rules and inheritance mean what the specs say. Differential tests then show that no
query operator, property path or reasoning step returns a quad that the principal cannot
read. It serves [C09](../C09-dataset-access-control.md),
[C12](../C12-graph-access-control.md) and [C12b](../C12b-triple-access-control.md).

**Incremental maintenance.** A mechanized proof in Lean or Isabelle shows that
derivation counts for non-recursive views and the backward/forward algorithm for
recursive ones keep a view equal to its recomputation after any sequence of changes. Property tests check the same equality on the
implementation. It serves [F13](../F13-incremental-views-and-provenance.md) and the
shared maintenance core of [F23](../F23-reasoner-core/README.md).

**Demand and magic-set rewriting.** A proof shows that a demand program answers exactly
what the full program answers for the query that seeded it. Differential property tests
compare the two programs on generated rule sets and data. It serves
[F23](../F23-reasoner-core/README.md).

**The rooted calculus.** A mechanized completeness proof covers the calculus with its
[departures from the published rules](../F24-owl-classification/SPEC.md#item-804s-departures-from-the-journals-rules).
It starts once the design is stable after items 823, 1061, 1075 and 1220. It serves
[F24](../F24-owl-classification/README.md).

**Branch merges and replication.** A TLA+ model of the merge and replication protocols
checks convergence and that no committed change is lost. The model is built when one of
those protocols next changes. It serves [F09](../F09-branches-and-merges.md) and
[F10](../F10-replication.md).

**Query rewrites.** Property tests check each algebraic rewrite of the planner against
the reference evaluator on generated queries and data. It serves
[X14](../X14-unified-executor/README.md) and
[X15](../X15-join-algorithms-and-estimation/README.md).

## Dependencies

* The commit log and crash recovery phase follows the commit sequence of CI and the
  group commit of X09 as they stand when it starts.
* The concurrency and format phases follow the shared publish tail and the format cutoff
  of [item 455](../../ROADMAP.md), so that the models check the consolidated code.
* The incremental maintenance and demand phases follow F23's shared maintenance core.
* The rooted calculus phase follows the paper proof of items 823, 1061, 1075 and 1220.

## Acceptance gates

* Each phase states its property, its model or bound, and the tool that checks it, in
  its Outcome section.
* Each model, proof or bounded check runs in CI or as a mise task, and a failure stops
  the build in the same way a failing test does.
* Each model or proof is shown to catch at least one seeded fault of the class it
  targets, so that a vacuous model does not pass.
* Each phase keeps the differential and conformance tests of the subsystem it covers.

## Non-goals

* The work does not verify the whole engine.
* A proof does not replace differential tests. It covers a model or a bounded property,
  and the tests still check that the code matches it.
* The work proves nothing about performance, or about resource use beyond the admission
  properties above.

## Design constraints

The tools run in CI or as a mise task, with pinned versions, so that a proof that once
passed is checked again on every change it covers. A model lives beside the code it
checks, in the same crate or a sibling directory, so that a reviewer sees both. A change
to an on-disk format or a protocol that a model covers changes the model in the same
commit.

## Provenance

This design derives from Sparkles' existing specs. It also draws on the public
documentation of TLA+, P, Alloy, Kani, Loom, Shuttle, cargo-fuzz, Lean and Isabelle. No
code from another project is copied.

## Outcome

Nothing is built. [Item 1223](../../ROADMAP.md) tracks the work.
