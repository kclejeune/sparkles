# LUBM and reasoning

The LUBM measurements cover query timing over a materialized closure and the cost of producing it. The report also covers larger materializations and incremental reasoning under their separate contracts.

Read the [results](RESULTS.md), [run catalog](RUNS.json) and
[shared reference setup](../reference/README.md). The
[harness guide](../../BENCHMARKING.md) documents commands and output formats.

The reasoning gate of the [rewrite gates](../../BENCHMARKING.md#rewrite-gates) records its baseline in [the results](RESULTS.md#query-time-reasoning-latency). A change to the reasoner's query path, derivation on demand or RDFS on read runs `mise run bench:reasoning-latency` on forge and compares the record with that baseline through `scripts/bench-gate.py compare`.

The result report is a manual import. A linked raw record supports only the measurements
and metadata it contains. Missing build revisions, host details, durability settings or
answer evidence remain unknown and must be supplied by a new run before a broader claim
is generated. Existing raw records retain their original bytes and paths.

## The OWL 2 RL reference

Nemo, the Datalog engine of TU Dresden, is the reference for the correctness and speed of
the `owl-rl` materialization. Nemo is licensed under Apache-2.0 or MIT, and the flake
builds version 0.10.1 as `.#nemo` from the project's release binaries, which
`nix/reasoners` pins by the SHA-256 sums that the release publishes. RDFox is not a
reference. Its evaluation agreement allows benchmark results to be published only after
an application to its vendor at least 30 days in advance, and not under its name without
the vendor's approval, so its numbers cannot appear here.

Nemo has no built-in OWL 2 RL. `scripts/rl-reference/owl2rl.rls` writes the OWL 2 RL/RDF
rules of [OWL 2 Profiles section 4.3](https://www.w3.org/TR/owl2-profiles/#Reasoning_in_OWL_2_RL_and_RDF_Graphs_using_Rules)
as a Nemo program, transcribed from the recommendation's tables with their rule names.
The program follows these conventions, which Sparkles' rule set shares.

- The closure is an RDF graph. A rule is not applied when its conclusion would have a
  literal as subject or a literal or blank node as predicate, so no generalized triple is
  derived. Each conclusion of a rule with several conclusions is checked on its own.
- The rules eq-ref, dt-eq and dt-diff are left out, and no rule derives a reflexive
  `owl:sameAs` triple, including the rules whose conclusion has a variable property.
- A list is read only when it ends at `rdf:nil`, as the LIST expansion of the
  recommendation requires. The rules whose conclusion is false check consistency and derive no
  triple, so they are left out too.
- The rule dt-type2 is kept as a relation that the rule cls-svf1 reads for data values.
  The relation gives each literal the datatypes of OWL 2 RL whose value space holds its
  value.
- The rules prp-spo2, prp-key and cls-int1 take lists of any length.

The program changes how two rules are evaluated, not what they derive. The schema
vocabulary is read through binary relations, which keeps Nemo's joins on small tables.
The rule prp-trp extends a path by one triple that another rule derived, instead of
joining two derived paths. On OWL2Bench's symmetric and transitive
`hasSameHomeTownWith`, whose closure is a clique of about 1,100 people, the join of two
paths ran out of a 12 GiB limit, and with the linear form the whole program needs less
than 1 GiB.

Nemo normalizes literal values, so that `"01"^^xsd:int` and `"1"^^xsd:long` become one
value, and it renames blank nodes. `scripts/rl-reference/rl_terms.py` therefore replaces
each literal and blank node of the input with an IRI before Nemo reads it, describes each
literal in a second file, and maps the IRIs back in Nemo's output. The input is the
default graph as Sparkles stores it after the load, so both engines read the same terms.

`scripts/rl-reference/compare.sh` compares the derived graphs triple for triple in two
ways. The aligned comparison adds `scripts/rl-reference/sparkles-extensions.rls` to the
program, which restates what Sparkles' `owl-rl` rule set adds to the OWL 2 RL rules, as
section 4.3 allows. These are axiomatic triples of RDFS and of the OWL vocabulary, the
RDFS rules that make every property an `rdf:Property` and every class a subclass of
itself and of `rdfs:Resource`, the symmetry of `owl:inverseOf` with the domains, ranges
and functional characteristics it carries over, functional and inverse functional
subproperties, and the `owl:equivalentClass` and `owl:equivalentProperty` of classes and
properties that are `owl:sameAs`. Sparkles states a reflexive `owl:equivalentClass` and
`owl:equivalentProperty` only for declared classes and properties, so the aligned program
also leaves out the cases of scm-eqc2 and scm-eqp2 whose two classes or properties are the
same. The aligned closure must equal Sparkles' closure. The core comparison runs the W3C
program alone, and each of its triples must be in Sparkles' closure. A difference that is
understood is listed in `scripts/rl-reference/known.tsv` with its explanation, and any
other difference fails the comparison. The encoding refuses input whose IRIs use the
encoded namespaces. It checks each literal's lexical form against its own datatype, with
calendar and time zone bounds for dates and the alphabets of the binary types, and gives an
ill-typed literal no datatype. The value of a valid literal then decides which datatypes'
value spaces hold it.

`mise run test:rl-reference` compares the closures of the premise ontologies of the W3C
OWL 2 test cases that list the RL profile. `mise run bench:rl-reference lubm 1` or
`owl2bench 1` generates the data with the suite's runner, compares the closures, and
then times both engines for `--runs` rounds.

- A full materialization runs `sparkles infer --full` on a fresh copy of the loaded store,
  on one thread and on all cores, and Nemo's aligned program on the encoded input. Nemo
  evaluates its rules on one thread and has no setting for threads.
- Incremental maintenance takes every hundredth data triple outside the RDF, RDFS and OWL
  vocabularies as the change. It inserts the change into the materialized store without
  it and deletes it from the materialized store with it, and times `sparkles infer` after
  each update. Nemo has no incremental maintenance, so its baseline is a full run on the
  data after the update. The first round compares each maintained closure with Nemo's
  closure of the same data.
- GNU time gives each run's wall time and peak resident memory, and each run has its own
  memory-limited systemd scope and a deadline. A run is recorded as out of memory only
  when the scope's `memory.events` records an out-of-memory kill in the scope. The
  closure check and the materializations that prepare the incremental runs have the same
  limit and deadline. At the deadline the engine gets SIGTERM and, 30 seconds later, SIGKILL, and
  no process of a run outlives it. Sparkles' time starts from a loaded store and includes
  writing the derived graph to it. Nemo's time includes reading the encoded N-Triples and
  writing the derived triples, and its own report gives the reasoning time apart.

The runner writes every run to `results/rl-reference.json` and the medians to
`results/summary.md` in its working directory.
