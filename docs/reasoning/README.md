# Reasoning and validation capabilities

This page describes the available capabilities and their current limits. The
[capability matrix](../FEATURES.md#capability-status) records delivery status within
each stated scope.

## Reasoning, validation and schema

Inferences are materialized and kept current or derived on demand, shapes validate on
request or on every write, and the schema report counts what the data holds.

### Reasoning

* RDFS, OWL 2 RL and Jena rule syntax materialize through `sparkles-reasoner`,
  `/$/reason` and `sparkles infer`. The rules read their input graphs from the
  compressed indexes, so a run's memory follows the facts it derives. LUBM(1000) under
  RDFS, 138 million triples, materializes in 91 s at 15.2 GiB of RSS on 16 cores
  ([F16](../specs/F16-materialization-at-scale.md#outcome), [BENCHMARKS.md](../BENCHMARKS.md)).
* The `owl-rl` profile covers the rules of the OWL 2 RL/RDF tables of OWL 2 Profiles
  §4.3, and a test checks each one ([F23](../specs/F23-reasoner-core/SPEC.md#outcome)).
  Rules whose conclusion is `false` are reported by the inconsistency checks below
  rather than materialized. The profile leaves out four rules. `eq-ref` would make every
  term `owl:sameAs` itself, `dt-type2` would type every literal, and `dt-eq` and `dt-diff`
  would relate every pair of literals with equal values and with different values. Literal types are read where a rule needs them, as
  `owl:someValuesFrom` with a datatype does. Property chains have rules for 2 and 3
  properties and keys for 1 and 2, because each longer rule adds thousands of rules to
  the demand programs of query-time reasoning.
* The materialized closure is checked triple for triple against Nemo, a Datalog engine
  that runs the OWL 2 RL/RDF rules written from the recommendation. The closures are equal
  on LUBM, OWL2Bench and the W3C OWL 2 RL test cases, and the
  [reasoning benchmark](../benchmarks/reasoning/README.md#the-owl-2-rl-reference)
  describes the comparison.
* Materialization closes transitive relations, `rdfs:subClassOf`, `rdfs:subPropertyOf`
  and `owl:sameAs` with closure operators instead of joins. An operator condenses the
  relation's strongly connected components and works out each component's reachable
  set once, and `owl:sameAs` closes its classes with union-find. An operator merges its
  pairs into the derived facts in batches of four million, so a closure larger than the
  facts derived so far does not wait in memory beside them. OWL2Bench with 5
  universities materializes under OWL 2 RL in 44 s at 9.5 GiB of RSS, and with 2 in 5 s
  instead of 53 s ([results](../benchmarks/reasoning/RESULTS.md#owl2bench)). The rules stay in the
  program, so explanations and incremental maintenance still name them, and a run
  derives the same facts as one that evaluates them as joins. A program with a rule that
  is not monotonic, or one that reads lists and builds them in a rule head, evaluates
  every rule as a join. A run whose rules read lists and derive list facts through a
  property, such as a subproperty of `rdf:first`, starts again without the operators.
* A rule body of three or more atoms is ordered from sampled bindings where that is
  clearly cheaper than the order the statistics give. The planner follows up to 16
  bindings through the facts the plan reads and counts each atom's candidates under
  them, which matters where the rule itself binds a predicate or a class, as `prp-key`
  does with the keyed class and its key property. On a benchmark worker the rules of
  OWL2Bench with 5 universities take 29.9 s instead of 51.7 s at one thread and 9.6 s
  instead of 12.6 s at 20
  ([results](../benchmarks/reasoning/RESULTS.md#sampled-rule-body-planning)). A run
  derives the same facts in either order. A rule in which a builtin binds a variable
  that an atom also holds keeps the statistical order, and so does every rule while the
  derived facts hold one that a deletion removed. Planning costs a few milliseconds per
  run. Roadmap items 944 and 945 cover these two limits and that cost.
* Derived facts live in a columnar tier, each fact one row by a dense id, with each
  predicate's facts sorted by subject and by object. OWL2Bench with 5 universities holds
  its 53 million derived facts in about 52 bytes each with the triple index, against about
  135 in the hash lists before the tier. Full materializations take the time they took
  before, and incremental runs on LUBM(100) are about 2% slower
  ([roadmap item 938](../ROADMAP.md)).
* Each iteration merges its results in parallel on a reasoning pool, half the cores by
  default with `serve --reason-threads`, that admits runs in arrival order. A run stops at
  50 million derived triples by default, and `sparkles infer --max-inferred`,
  `serve --reason-max-inferred`, a request's `maxInferred` and the bindings change the
  limit. A run past the limit fails with the code `inferred-limit` and commits nothing.
  `ReasonOptions::max_memory` bounds the estimated heap of the whole run.
* Any later run, whether a re-run, an automatic run or `sparkles infer`, updates the
  previous materialization incrementally with the backward/forward algorithm and writes
  what a full run would. A run is full when rules are not monotonic or create blank nodes,
  when deletions are large, or when the set of input graphs changed. A deletion also runs
  in full when its maintenance passes an allowance set by the size of the closure, which
  approximates the cost of a full run. The allowance is four matched rule instances for
  each fact of the closure and as much memory beyond what the run held when the deletion
  started as the closure's own heap, with floors of about a million instances and 64 MiB.
  The maintenance compares its count and the run's charges with the allowance every 1,024
  instances, so it can pass the allowance by a few batches before it stops. The kept
  closure holds only the derived facts, which takes 41% to 47% of the memory of a closure
  with the explicit facts on LUBM-style data. The update is a pass of the fixed-point
  operator that property paths in incremental views also use, in its exact mode, which
  keeps no derivation records.
* `ReasonOptions::inferred_change` makes an incremental run report what it committed to
  the inferred graph, each added triple with weight `+1` and each removed one with weight
  `-1`, in the ids of the snapshot that the report's receipt names. A full run, including
  one that an incremental run fell back to, reports no change, and a reader then reads the
  inferred graph again. The change is held in memory until the report is dropped.
* A materialization reads the default graph unless data and ontology graphs are named,
  and follows `owl:imports` to graphs of the dataset through a Jena-style location
  mapping ([API.md](../API.md#input-graphs-and-imports)). With `imports: fetch`, missing
  imports are loaded into the graph of the import IRI, so later runs work offline.
* RDFS on read matches Fuseki's `--rdfs` and Jena's `ja:DatasetRDFS`
  ([API.md](../API.md#rdfs-on-read)). Queries see the RDFS closure of each graph against a
  schema graph by rewriting the query, and the answers equal Jena 6.2's on 55 fixture
  queries. `/$/rdfs/{ds}`, `serve --rdfs` and `query --rdfs` turn it on.
* Query-time reasoning with `reasoning=demand` derives the facts a query's patterns need
  as it runs, from RDFS, RDFS simple, OWL 2 RL or a Jena rule file, without the inferred
  graph ([API.md](../API.md#query-time-reasoning), [F17](../specs/F17-query-time-reasoning.md#outcome)).
  Jena backward rules answer this way. `reasoning=auto` reads a fresh materialization
  with the same profile and derives over a stale or absent one or for rules that
  materialization skips, and the dataset's query-time setting makes it the default
  ([API.md](../API.md#automatic-choice)). The `owl-ql` profile rewrites a query against
  the ontology's OWL 2 QL axioms with PerfectRef instead of deriving, so an existential
  restriction answers a pattern whose variable the query does not use elsewhere, and a
  rewriting past `budget.maxUnion` branches falls back to the `owl-rl` rules with a
  warning ([API.md](../API.md#owl-2-ql-rewriting)). The UI's query page chooses the mode and the
  profile in its **Options** menu ([query page](../visualization/README.md#query-page)).
  A demand transformation starts from the query's
  constants and bindings and specializes schema relations proven complete. Schema facts
  can follow from derived instance facts, including class and property declarations and
  equality between schema terms. Programs containing `makeTemp` run in one ordinary
  demand program with their normal lower stages. Derived tables are cached per snapshot
  and view, and a later commit keeps them when no quad it changed is one the rules read or
  give or a schema, list or `owl:sameAs` fact. `maxDerived`, the deadline and the memory
  budget bound them. Tables that
  contain generated blank nodes are not cached. A tiny positive feedback view whose
  schema expansion creates thousands of rules can use a bounded temporary closure of
  the original rules. This retains intermediate facts within the session and uses the
  same resource limits. EXPLAIN shows a `Derive` node per pattern.
* Demand tables share a process-wide retained-table estimate budget. The global
  `--derived-table-cache-mb` flag sets it to 256 MiB by default in both memory profiles,
  and zero disables retention. Library callers use `sparkles_reasoner::tables::set_capacity`
  and read a coherent report with `tables::snapshot`. Operators with the metrics
  permission see `derivedTableCache` in `/$/stats` and `caches.derivedTable` in
  `/$/server/memory`, with capacity, estimated bytes, table count, hits and misses.
  A shared chunk counts for each cached table retaining it. A query may keep a table
  alive after eviction, so retained-cache estimates are not allocator usage or RSS.
  The estimator counts 24 bytes per fact, 64 bytes per chunk, each local term's
  byte length plus 40 bytes, and 64 bytes per table. It omits spare vector capacity,
  cache-key strings, hash-table capacity and exact allocator metadata. Measuring and
  calibrating these allowances is planned. Roadmap item 757.
  Derivation and replay still use the query's own memory budget, and the host-derived
  query pool leaves out this configured cache beside the storage caches.
* The library names the reasoning of a query with `sparkles::reasoning::ReasoningMode`,
  which is `Off`, `Materialized`, `Demand` with a profile and a limit, or `Auto`.
  `Dataset::inference_view` turns a mode into an `InferenceView`, which gives the query
  options that read the selected data, the inferred data that incremental views read and
  the source of an explanation. A mode is read from the `reasoning` parameter of the HTTP
  endpoints, the `--reasoning` flag of `sparkles query` and the `reasoning` argument of
  the MCP tools `sparql_query` and `explain_query`.
* The Fuseki converter maps a GenericRuleReasoner's `jr:ruleMode` to the dataset's
  query-time default: `auto` for hybrid, Jena's default, `demand` with nothing
  materialized for backward, and materialized reasoning for forward
  ([G08](../specs/G08-fuseki-configuration.md#rule-modes-and-query-time-reasoning)).
  The hybrid mode's rule text starts with `@hybrid jena .`.
* Jena's hybrid rules nest backward rules in a forward rule's head, as in
  `(?p rdf:type ex:Symmetric) -> [(?x ?p ?y) <- (?y ?p ?x)]`. By default the
  materialization flattens such a rule into `B1, B2 -> H`. With `@hybrid jena .` in the
  rule text, the materialization keeps the forward head and the nested rule becomes the
  query-time rule `H <- B1, B2`, which `demand` and `auto` derive when a query asks. A
  rule with a functor term, a builtin that has a side effect or makes terms, `noValue`,
  a backward head variable that no body clause binds or a more deeply nested backward
  rule cannot be split this way and is refused with an error that names it
  ([F17 §5.2](../specs/F17-query-time-reasoning.md#52-hybrid-rules)).
* A rule text can declare relations and write rules over them, the subset of the Datalog
  syntax of [C24 §6.2](../specs/C24-json-query-languages.md#62-syntax) that rule files
  take. `.decl name(col, …)` declares a relation of that many columns, before or after
  its rules. An atom `name(args)` of a declared relation can stand in a body or a head
  wherever a triple pattern can, `not` before a triple pattern or a relation atom in a
  body negates it, and `head :- body.` writes a forward rule whose head is one relation
  atom or triple pattern. In a relation atom or a negated atom, `_` and `?_` match any
  value. A relation's facts are not RDF and never reach the dataset, and an atom whose
  predicate is a variable does not read them. The following program derives the
  property chains of any length.

  ```text
  .decl chain(x, h, rest, y)
  chain(?x, ?h, ?r, ?y) :- (?h owl:propertyChainAxiom ?l), (?l rdf:first ?p), (?l rdf:rest ?r), (?x ?p ?y).
  chain(?x, ?h, ?r2, ?z) :- chain(?x, ?h, ?r, ?y), (?r rdf:first ?p), (?r rdf:rest ?r2), (?y ?p ?z).
  (?x ?h ?y) :- chain(?x, ?h, rdf:nil, ?y).
  ```

  A text that uses relations or `not` is one program, its `@include`d rule sets
  included, and it must stratify, which means that no relation or predicate depends on
  its own negation through recursion. A program that does not is refused with the cycle
  named. Its strata run in order, so a `noValue` of a Jena rule in such a program reads
  the complete lower strata, and the run warns once for each such rule with the rule set
  it came from. Materialization, explanations, witnesses, query-time reasoning and
  why-not answers read a program as they read Jena rules, and an explanation shows a
  relation fact as a node `name(args)`. A program without `not` is maintained
  incrementally, and one with `not` runs in full on every update. A query-time demand
  binds only the first column of a relation of three or more columns, so writing a
  relation with the column that queries bind first keeps their demand narrow
  ([F23](../specs/F23-reasoner-core/SPEC.md#rule-ir)).
* Embedded query providers implement `ReasoningSession::intensional` with a
  `Result<bool>` return. Constant interning can refuse a memory reservation, and
  callers propagate that refusal before preparing demands. Implementations that
  previously returned a Boolean wrap successful values in `Ok` and return allocation
  failures with `Err`. `AnalysisContext::new` returns `anyhow::Result<Arc<Self>>`.
  A local reasoning limit still supports downcasting to `MemoryLimitExceeded`.
* `POST /$/reason/{ds}/explain` returns derivation trees from materialized inferences,
  RDFS on read or the demand engine ([API.md](../API.md#explanations),
  [F18](../specs/F18-explanations-and-entailment-diffs.md#outcome)). A tree has well-founded
  premises, the rule's variable bindings and passed builtins. Generalized intermediate
  facts and default geometries are explained, and a bounded partial tree marks premises
  that could not be expanded. RDFS-on-read trees can include materialized derivations.
  Schema builds honor cancellation and the deadline.
* The same explanation route accepts `mode: "whyNot"`. It lists candidate rules,
  conditions that block them and small sets of missing facts, with a verified derivation
  under each hypothesis. Fresh blank-node placeholders stand for missing resources.
  `complete` reports whether the bounded search exhausted its candidates. The CLI,
  library, MCP, Python and Node bindings expose both modes, and Explore has a Why-not
  form beside a resource's properties.
* Why-not hypotheses report whether their missing facts introduce a new inconsistency.
  An unfinished check remains unknown. Existing unrelated findings are excluded.
* Entailment diffs compare branch heads or retained commits under one program, with
  complete counts, bounded samples, predicate and class counts, and new findings.
  JSON write and merge previews can include reasoning changes over the complete
  candidate. Merge and review pages display them ([API.md](../API.md#entailment-diffs)).
* Minimal justifications list asserted facts and their schema or assertion role for
  inconsistency findings and unsatisfiable classes. Alternative supports share bounded
  oracle checks. Diagnostics can attach a first support to each finding, and the UI
  previews a selected support fact's removal on a new branch before explicit application
  ([API.md](../API.md#minimal-justifications)).
* A materialization can keep witnesses for each fact of its closure: up to a bound of
  sets of input quads, each of which derives the fact, and whether others may exist
  ([F18](../specs/F18-explanations-and-entailment-diffs.md#reasoner-witnesses-item-416)).
  `ReasonOptions::witnesses` sets the bound, and incremental runs maintain them, deletions
  included. A fact that is not truncated keeps exactly its minimal supports. They live
  with the closure kept in memory, where `Cache::witnesses` and
  `Cache::witness_dependents` read them, and they are off by default, so a run without
  them does no work for them. Turning them on costs memory and time. At a bound of 8,
LUBM with 10 universities took about ten times as long to materialize under RDFS and
about forty times as long under OWL 2 RL, and the witnesses held 574 MiB and 1.75 GiB
([results](../benchmarks/reasoning/RESULTS.md#reasoner-witnesses)).
* Sparkles records the commit at which inferences were made, and only a later change to a
  graph the run read makes them stale. Freshness is reported by `GET /$/reason/{ds}`,
  dataset info, `/$/stats` and the `Sparkles-Inferences` header. Automatic re-runs are
  opt-in with `serve --auto-reason` or `PUT /$/reason/{ds}/auto`.
* `GET /$/reason/{ds}/diagnostics` and `sparkles infer --check` run 17 inconsistency
  checks that cover every OWL 2 RL rule with a `false` conclusion, in JSON or Turtle.
  `literal-not-in-datatype` finds ill-typed literals for the datatypes whose value spaces
  it can decide. The checks never prove consistency.

### SHACL rules

* The `shacl-af` profile materializes the rules of SHACL Advanced Features. A
  `sh:TripleRule` infers the triples of its `sh:subject`, `sh:predicate` and `sh:object`
  node expressions at each focus node, and a `sh:SPARQLRule` runs its CONSTRUCT query
  once per focus node with `$this` pre-bound. Focus nodes are the shape's target nodes
  that conform to every shape of the rule's `sh:condition`. Deactivated shapes and rules
  do not run. The node expressions are those of the 2017 note, which are `sh:this`,
  constants, `sh:path` with `sh:nodes`, `sh:filterShape`, `sh:intersection`, `sh:union`
  and calls of `sh:SPARQLFunction`s over every combination of their arguments, and the
  SHACL 1.2 `shnex:` expressions also work. A rule of another type fails the run with an
  error that names it. TopBraid's own rule tests pass.
* Rules run in groups of equal shape `sh:order` and rule `sh:order`, from the lowest.
  The rules of a group run in rounds over the input graphs and every fact derived before
  the round, and the group ends when a round adds nothing. `mode=one-pass` runs each group
  once, which is TopBraid's evaluation, so that the rules of a group do not see each
  other's facts. A group that has not reached its fixed point after `max-rounds` rounds,
  100 by default, fails the run with an error that names its order, and the run changes
  nothing. A blank node in a CONSTRUCT template is the same node for the same rule and
  solution in every round, so such a rule reaches a fixed point. The run's report and its
  record give the round cap and the rounds each group took.
* The terms the rules compute, such as a new literal or IRI, are added to the dataset's
  vocabulary as the run goes, so that later rounds read them. A run that fails, is
  cancelled or passes its memory budget before its commit removes them again. A failure
  of the commit itself leaves them, as it does for every write. Each round sets aside a
  quarter of the run's remaining memory budget for the SPARQL queries of its rules, and
  cancelling the run stops it between queries and within large cartesian products.
* A blank node that a query makes, through `BNODE()` in a SPARQL rule or in a
  `sh:SPARQLFunction`, is a new node for each focus node, call and round, so a rule that
  derives such nodes from facts of its own group does not reach a fixed point. A function
  call gives the binding of the first solution, and an `sh:optional` parameter without an
  argument value is left unbound. The queries of rules follow the pre-binding rules of
  SHACL-SPARQL, which refuse `VALUES`, `MINUS`, `SERVICE` and an `AS` that assigns
  `$this`.
* The input record of a run lists the shapes graphs that `shapes=` names among the
  graphs it watches, so a change to the rules marks the inferences stale.
* The shapes are read from the input graphs, as TopBraid's `shaclinfer` reads them without
  a shapes file, or from the graphs that `shapes=` names. The options follow the profile
  name, as in `sparkles infer --profile 'shacl-af?shapes=urn:x:shapes&mode=one-pass'`.
  A run reads its shapes graph into memory and decodes the nodes each rule produces, so
  the profile suits rule sets over data that a SHACL validation handles. Each derived
  fact also holds about 450 bytes in the view the rules read, beside its place in the
  derived facts.
* SPIN rules in the shapes graph run as SPARQL rules. A query attached to a class with
  `spin:rule`, or a subproperty of it, runs with `?this` bound to each instance of the
  class, and a query that does not mention `?this` runs once. The query is read from
  `sp:text` and gets declarations of the RDF, RDFS, OWL, XSD, SHACL and SPIN prefixes.
  Other prefixes need their own declarations in the text, since a store does not keep
  the prefixes of the file it loaded.
* A Fuseki configuration whose reasoner has `ja:reasonerURL sh:Rules` converts to the
  `shacl-af` profile. This reasoner URL is a Sparkles convention, since neither Jena nor
  TopBraid defines an assembler for SHACL rules, and `sh:Rules` is the IRI under which
  TopBraid applies rules as an entailment. `sh:shapesGraph` on the reasoner names the
  shapes graph, and `tosh:maxIterations` sets one pass when it is 1 and the round cap
  otherwise. The converter reports a shapes graph that declares `sh:entailment sh:Rules`,
  because validation does not run rules first.

### SHACL

* `sparkles-shacl` validates SHACL Core and SHACL-SPARQL in parallel over the indexes.
* Sparkles checks a shapes graph before it runs, through `sparkles lint`, `POST /$/lint`,
  the MCP `lint` tool and the shapes editor. It lints the SPARQL in `sh:select`, `sh:ask`,
  `sh:construct` and `sh:update` literals with the prefixes of `sh:prefixes` and
  `sh:declare` and with SHACL's pre-bound variables, such as `$this`, `$value` and
  `$currentShape`, counted as bound. A finding is placed inside the literal. Turtle,
  TriG, JSON-LD and SHACL Compact Syntax shapes are all linted, and the compact syntax
  has no syntax for these literals ([USAGE.md](../USAGE.md#linting)).
* SHACL 1.2 support covers `sh:targetWhere`, the list constraints `sh:memberShape`,
  `sh:minListLength`, `sh:maxListLength` and `sh:uniqueMembers` with `sh:detail` results,
  and node expressions in the Working Group's `shnex:` vocabulary. Node expressions work as
  targets, constraint parameters, `sh:deactivated`, `sh:values` and `sh:defaultValue`,
  and as constraints.
* Fuseki's `/{ds}/shacl` endpoint, with `graph=default|union|<iri>`, and `sparkles shacl`
  return the report as Turtle, N-Triples, JSON-LD or JSON. Validation covers the data and
  the inferences.
* The SHACL Compact Syntax, `text/shaclc`, works wherever shapes are read or drafted
  ([API.md](../API.md#shacl-compact-syntax-shaclc)). That includes `/{ds}/shacl`,
  write-time validation, the CLI, the MCP tools `validate_shacl` and `draft_shapes`, the
  Python bindings and the UI's shapes editor. `sparkles shacl parse` converts shapes files
  between SHACLC and the RDF syntaxes, and the writer refuses a graph that SHACLC cannot
  express and names the triples.

### ShEx

* `sparkles-shex` validates ShEx 2.2 with the `shex` cargo feature, which is on by default
  ([API.md](../API.md#shex-validation)). Schemas can be ShExC, ShExJ or ShExR, and ShExR can
  be in any RDF syntax Sparkles reads. `EXTENDS` and `ABSTRACT` are supported.
* Imports can be inline, files under `--load-dir`, or http(s) URLs fetched through the
  outbound policy. `EXTERNAL` shapes, annotations and the Test semantic-action extension
  are supported.
* Shape maps can be compact or JSON, with `{FOCUS p o}` selectors and
  `SPARQL """SELECT …"""` selectors that run within the query budgets and without
  SERVICE. Recursion and negation use stratified greatest-fixed-point typing without deep
  stacks.
* `POST /{ds}/shex` returns JSON, ShapeMap JSON, compact or text reports.
  `sparkles shex validate|parse` accepts Jena's flag names, and `parse --out shexr`
  writes Turtle.

### OWL 2 ontologies

* `sparkles-owl` holds the OWL 2 structural model of entities, class and property
  expressions, data ranges, axioms, annotations and the ontology header. It reads the
  model from triples by the reverse mapping of the OWL 2 Mapping to RDF Graphs and writes
  it back by the forward mapping. Its functional-style writer renders axioms and
  documents, and its functional-style reader reads them. The `ontology` cargo feature,
  which is on by default in the server, builds it into the library and the server.
* An OWL 2 functional-style document loads as the triples its axioms map to, one axiom
  at a time. A `.ofn` file, compressed or not, is read by `sparkles load`,
  `sparkles ontology check FILE`, the library's and the bindings' file loaders, SPARQL
  `LOAD` and the configuration's data files. Graph Store writes and uploads also read a
  body sent as `text/owl-functional`. The predefined prefixes `owl:`, `rdf:`, `rdfs:` and
  `xsd:` need no declaration, and a literal typed `rdf:PlainLiteral` reads as the plain or
  language-tagged literal it stands for. A syntax error names the file, the line and the
  column. The document is held in memory while it is read. Sparkles does not write the
  syntax as a dataset dump, and the axiom listings render it.
* A read takes the graphs a caller names, the default graph unless told otherwise, and
  follows `owl:imports` to graphs of the dataset that the caller can read. A strict read
  keeps every triple the mapping does not cover as unmapped and refuses shared list
  cells. A lenient read, the default, also types undeclared entities from their use and
  records each guess as a diagnostic. Triples that map to no axiom are counted and
  listed, never dropped silently.
* The profile checks test the OWL 2 DL structural restrictions and the grammars of
  OWL 2 EL, QL and RL. Each violation names its rule and section in the recommendations
  and the axiom it concerns. A report on a profile also lists the DL violations, since
  each profile is a subset of OWL 2 DL. The checks cover the assertions the store holds
  unless the caller leaves them out.
* The DL check, and so every profile report, also checks literals. A literal of a
  datatype in the OWL 2 datatype map needs a lexical form of that datatype, such as an
  `xsd:byte` from -128 to 127 or an `owl:rational` such as `1/3`, and no literal can be
  of type `owl:real` or of a datatype that a datatype definition introduces. XML
  literals require balanced content with QName components, scoped namespace bindings
  and unique expanded attribute names. Normalized namespace names must be URI
  references, with relative references accepted and their spelling preserved. The
  check retains noncanonical spellings. Full XML canonical lexical and value semantics
  are not implemented. Roadmap item 663. Namespace
  checking uses scratch proportional to active declarations, element depth and the
  current tag's attributes, including decoded namespace strings. The standalone
  lexical checker does not charge this scratch to a caller-provided memory budget.
  A datatype restriction can use only the facets its datatype supports, with values in the value
  space each facet takes, a pattern that parses as an XML Schema regular expression and
  a language range of RFC 4647. Annotation literals and the annotations of the headers
  of the ontology and its imports are checked as well. An edit can still write an ill-typed literal, and a check reports it.
* Entity accessors answer what an ontology says about an entity: its kinds, labels and
  annotations, the superclasses, subclasses, equivalent and disjoint classes and keys of
  a class, the domains, ranges, hierarchy, inverses and characteristics of a property,
  and the types and property assertions of an individual. The instances of a class and
  the assertions about an individual stream from the store, through its indexes, since a
  read keeps assertions out of memory. Rust has them as methods of the read ontology and
  of the open handle, and the HTTP routes and the bindings answer them as one entity
  report.
* An edit adds and removes axioms in one commit. Added axioms declare the entities they
  use unless the caller turns this off. A removal deletes the axiom's triples and the
  anonymous structures it alone uses, and keeps any structure that another axiom or
  triple still refers to. A removal deletes only the declarations it names, and
  `remove_entity` names an entity's declarations, its axioms and the assertions about it.
* A read with `inferences` also reads the materialized inferences while the dataset
  records a materialization, so that an axiom listing about an entity includes the
  materialized closure, such as the transitive superclasses of a class. The answer names
  the materialization's profile and says whether it is stale, as the
  `Sparkles-Inferences` header does for a query. Without a materialization the read
  covers the asserted axioms and says so, and it never runs the reasoner. The inferred
  graph holds the inferences of every input of the run, so such a read also includes
  what the run derived from other graphs. Edits and diffs read the asserted axioms only.
* A materialization under `owl-rl` warns when its input graphs hold axioms outside the
  OWL 2 RL grammar, with their count and the first of them, since the OWL 2 RL rules are
  complete only for ontologies in the profile. It also warns about axioms in OWL 2 RL
  whose rule the rule set covers only up to a size, property chains of more than three
  properties and keys of more than two, which
  `crates/sparkles-reasoner/rules/owl-rl.gaps` lists beside the rules. The warnings
  appear in the run's report, the reasoning record and the output of `sparkles infer`.
  The check reads the terminology of the inputs in the state the run reads, once the run
  holds the writer lock, with the imports that resolve to graphs of the dataset.
* `sparkles_owl::profiles::partition` splits an ontology's axioms by one profile's
  grammar, without the OWL 2 DL global restrictions. It is the OWL 2 QL check that
  query-time OWL 2 QL rewriting is to use, and the RL warning uses it too.
* `sparkles_owl::classify` classifies an ontology by OWL 2 EL with a consequence-based
  calculus in the manner of ELK. It reads the axiom model only, saturates its contexts
  on several threads and computes the class taxonomy: the classes grouped into
  equivalence classes, their direct superclasses and the unsatisfiable classes. It
  covers OWL 2 EL, with intersections, existential restrictions, `owl:Thing` and
  `owl:Nothing`, individuals in class expressions and assertions, the property
  hierarchy with the top and bottom properties, property chains, transitive and
  reflexive properties, domains, ranges, disjointness, `ObjectHasSelf`, the datatypes,
  literals and data ranges of the profile, functional data properties, datatype
  definitions, keys and negative property assertions. A class whose instances reach an
  individual through a context that holds only where the class has an instance is
  classified again in a world of its own. An ontology where only an individual's own
  context derives the individual, and that has no keys or universal roles, skips that
  step, and so does every ontology without individuals. In an ontology without
  individuals, functional data properties or top properties, the Horn rules of Kazakov's
  calculus for Horn-SHIQ take the Horn axioms outside OWL 2 EL: inverse properties,
  universal restrictions, at-most-one and at-least restrictions, functional and
  inverse-functional properties, disjoint, asymmetric and irreflexive properties and
  complements that move to the other side as their duals. Universal and at-most
  restrictions create contexts for conjunctions of classes, which OWL 2 RL and QL
  ontologies never need, so these stay polynomial. A run with the Horn rules has a
  memory budget of 64 times its index and axioms, at least 1 GiB and no more than the
  caller's. A run that passes it classifies again by the EL rules alone, leaves the Horn
  axioms out and reports them, and sets `Stats::horn_over_budget`. No ontology measured
  reaches it, since a run composes only the property chains that a negative existential
  restriction needs and unfolds the others by their automata, and composes each pair of
  links into the least properties of a chain's result, so that a symmetric transitive
  property, which is its own inverse, derives each link once. In an ontology
  without individuals, data properties, self restrictions, reflexive properties or top
  properties, axioms that need case analysis, such as unions, complements and universal
  restrictions on the left, take the disjunctive rules of the consequence-based calculus
  for ALCHI, with transitive properties and chains through the same automata. Each
  context keeps clauses under one total order of literals, and a named subsumer that only
  a case analysis gives is found by a test context for the class with the candidate's
  complement. `Limits::expansion` chooses the cautious or the eager successor strategy,
  which give the same answers. These runs share the Horn rules' memory budget, and a run
  that passes it classifies again by the Horn rules, leaves the disjunctive axioms out and
  reports them, and sets `Stats::dl_over_budget`. A run that neither counts nor is rooted
  also has a bound of 384 derived conclusions for each class of the signature, at least
  2,097,152, `Limits::dl_attempt_work`, past which it falls back in the same way and sets
  `Stats::dl_attempt_over` too. The attempts that finish on the ORE 2015 DL development
  set stay far within it, and those that cannot finish pass it well before their memory
  budget. On several threads a schedule can decide the outcome of a run that derives
  about the bound. With `Limits::partial`, a run of the
  disjunctive rules that its deadline, cancellation or memory budget stops returns the
  subsumptions it proved, with `Classification::complete` false, the classes it may have
  left incomplete in `incomplete`, and a consistency of consistent, inconsistent or
  unknown, which leaves every class incomplete when unknown. The same rules can count, but
  counting is off by default. `Limits::counting` turns it on. A counting run without
  individuals orders its number restrictions by entailment unless `Limits::number_order`,
  which is on by default, is set to false. The run tells itself that `≥m r.F` entails
  `≥n s.G` when `m ≥ n`, `r ⊑ s` and `F ⊑ G` are told and that it contradicts `≤k s.G`
  when `m > k`, and a class that holds a minimum makes no witnesses for a minimum that it
  entails. Its answers are those of a counting run that finishes without the order, and
  it finishes on ontologies with many related cardinalities where that run passes its
  bound. It keeps tables of the related restrictions, which raise the peak memory of the
  two Protein Ontology files of the ORE 2015 DL sample by 4 to 19% over the run without
  counting. A run that passes its bound with the order as without it can take longer to
  do so. The classification of ore_ont_6485 of that sample with counting requested takes
  1.14 times as long with the order, which roadmap item 896 follows with the default
  of counting.
  Number restrictions, functional and inverse functional properties, disjoint properties
  and asymmetric properties then take equalities of successors, and a run that counts has
  a bound of 262,144 derived conclusions, `Limits::dl_work`. Without it those axioms are
  left out and reported. A run that passes it classifies
  again without counting, leaves the axioms that need counting out and reports them, and
  sets `Stats::dl_counting_over` and `Stats::dl_over_budget`. The fallback is always
  reported and never a different complete answer. A run with such a bound, which a run
  that counts and a rooted run have, proceeds in steps that derive the same conclusions
  on every number of threads, so that it answers in full or falls back in the same way
  on every schedule. `Limits::dl_steps` controls the steps and is on by default. Set to
  false, it gives the run the driver of a run without a bound, with which the thread
  schedule can decide near the bound whether the run answers in full or falls back. The
  steps cost 4 to 5% on the two Protein Ontology files on four threads in the build
  measured on `atlas`, and more on
  ontologies that classify in a few milliseconds, which roadmap item 950 follows. A
  rooted run and a run with the eager strategy that counts take one thread for their
  saturations with the steps, which roadmap item 951 follows. In an
  ontology with individuals or nominals, `Limits::rooted` lets the disjunctive rules take
  nominals, class and property assertions, equalities and inequalities of individuals and
  negative assertions through a root context where the individuals' facts meet, with the
  four departures from the published calculus that F24 records. It is off by default,
  since the completeness of the departed calculus is not proved, and every answer it gives
  is entailed. A rooted run without counting has a bound of 524,288 derived conclusions,
  `Limits::dl_work`, past which it classifies again without the rooted rules, gives the
  default answer and sets `Stats::dl_rooted_over` and `Stats::dl_over_budget`. It takes
  keys over object properties and the universal object property by rewriting them into
  class axioms over fresh names, which never reach an answer, and a run of the rewritten
  axioms rebuilds after every edit. A key whose rewriting passes its budget, about 60
  individuals for a key with one property, and a cardinality, functionality or
  disjointness of the universal property leave the ontology to the default paths, as do
  keys over data properties, the top data property and functional data properties.
  Roadmap items 822, 823 and 1061. Without the option, any other axiom
  outside OWL 2 EL that needs case analysis, such as a union, is left out and reported
  with its rule, while the Horn axioms run with the individuals. An axiom with a literal
  or datatype that the classifier cannot compare, such as an XML literal, an ill-typed
  literal or a datatype outside the OWL 2 datatype map, or with a property chain through
  the top object property, is used as far as the calculus goes and reported as partial.
  Every answer is entailed, and it is complete for named classes when nothing is reported.
  For eligible EL additions and removals, the classifier keeps its contexts and saturates
  again only those the changes can reach. A classification with the Horn rules of an
  ontology without individuals, and one with the disjunctive rules that does not count,
  keep their contexts in the same way. After an edit of class axioms the next run clears
  the contexts that hold an expression whose rules changed, with the contexts that reach
  them, and its answer is that of a classification that starts again. A change to the
  property hierarchy, chains, reflexivity or ranges, an
  axiom with an individual, a data property, a datatype, a key or a self restriction, and
  a change that moves the ontology to other rules, saturates everything again. So does
  any change to a Horn ontology with individuals, to a counting or a rooted run, and to a
  disjunctive run with the eager strategy. Roadmap items 753 and 953. A kept disjunctive run
  counts the conclusions of the runs since the last full one against the bound above and
  starts again in full past it, and a run that starts again can derive more than that
  count, so near the bound a kept run can answer in full where a full run falls back.
  Roadmap item 1010. A kept classification holds
  the last index beside the new one while it compares them, and starts again once it
  keeps more than twice the expressions of its last full run. A run stops at its deadline, its
  memory budget or a cancellation, which it checks while it indexes, builds the property
  closure, saturates and builds the taxonomy. Integer, decimal and rational literals
  use exact arbitrary-precision values, so equal reduced fractions and equivalent
  spellings compare equally beyond 128 bits. Numeric construction polls these controls
  and admits scratch and overlapping copies before growth. Large values share immutable
  storage that remains counted until the last alias drops. A numeric memory,
  cancellation or deadline refusal is returned as a typed error through the Horn and
  DL paths, rather than making the value opaque or reporting a completed fallback.
  XML namespace validation retains lexical spellings and does not establish canonical
  XML value equality. Its standalone scratch is not charged by the numeric budget.
  Full XML semantics and shared hierarchical ownership remain open. Roadmap items 663
  and 675.
  `Stats::memory` recounts retained index
  and context storage when saturation ends, using the same estimates for fresh and
  unchanged reused state. This adds a traversal of the retained tables. The final
  estimate can differ from the allowances charged as conclusions arrive, while the
  saturation peak remains in `Stats::peak_memory`. Any final growth past that peak
  must fit the hard budget and the pool before the run returns. These are heap
  estimates, not allocator measurements or RSS. A run of 16,384 axioms or more on several
  threads normalizes chunks of the axioms in parallel, with the same ids and taxonomy as
  on one thread. The chunks hold the names they meet until they merge and count against
  the memory budget, and a run whose chunks would pass it normalizes on one thread. This
  and the saturation's message buffers raise the peak resident set on eight threads by
  up to 13% on some ontologies in exchange for the speed. On the benchmark host GO
  classifies in 91 ms and ChEBI lite in 0.39 seconds on eight threads, and the
  classifier is 3.1 to 20 times as fast as ELK on GO, ChEBI lite and the largest ORE
  2015 EL ontologies on one and eight threads. GO's `InverseObjectProperties(part_of
  has_part)` takes the Horn rules, which add the 14 subsumptions that Konclude and HermiT
  entail and ELK misses and make GO 12% slower to classify on one thread than with the EL
  rules alone. Roadmap item 693. The classifier's direct hierarchy of GO is the one GO's
  release publishes with those 7 direct edges added, and its taxonomy of ChEBI entails
  every subsumption ChEBI asserts and gives no direct subsumption that ChEBI does not
  assert ([results](../benchmarks/el-classification/RESULTS.md)).
* `ds.ontology().classify()`, `POST /$/ontology/{ds}/classify`, `sparkles ontology
  classify` and the bindings' `classify` run it on an open ontology. The answer says
  whether the ontology is consistent and complete and lists the unsatisfiable classes,
  the excluded axioms, one class's place in the hierarchy and the taxonomy's nodes. A
  run with `target` writes the taxonomy to a named graph in one commit, replaces the
  taxonomy that graph held, and records PROV-O provenance on the graph's ontology
  header. The dataset keeps its last classification of an ontology for callers who read
  every graph, so a compatible same-commit call can reuse a retained result. At a later
  commit, eligible EL additions and removals reuse affected contexts, as do edits of
  class axioms in Horn ontologies without individuals and in disjunctive runs that do
  not count. Horn runs with individuals and counting or rooted runs rebuild after edits.
  Roadmap items 753 and 953 under item 603. Finer invalidation for EL individual, merge, key and world
  changes is open. Roadmap item 664. Same-commit reuse is separate from incremental
  reclassification. That kept state holds the classifier's contexts, index
  and taxonomy in memory while the dataset is open and reads the axioms of the open
  ontology in place. It is about 74 MB for GO by the estimate, which a counted heap
  matches, and it trades that memory for fast reclassification. `sparkles ontology classify FILE` classifies a file without a
  database and exits with status 1 when the ontology is inconsistent or has an
  unsatisfiable class.
* A kept classification has an immutable source and completed taxonomy separate from
  the saturation a later DL query can change. `GET /$/ontology/{ds}/classification`,
  Rust `kept_classification`, Python `kept_classification`, Node.js `keptClassification`
  and JVM `keptClassification` read availability and bounded named-class direct parents,
  equivalents and unsatisfiability without classifying on a miss. They preserve the exact
  branch, commit, input/options and producer metadata, distinguish running, mismatch and
  absence, and expose every captured source/import warning through typed pages. Continue
  with the result ID and exact commit. Restricted views and synthetic snapshots do not
  share this cache, and a retained result cannot resurrect pruned history.
  The new JVM read and frame-projection conveniences refuse pinned dataset wrappers
  and active transactions before native work. Use a live dataset with `OntologyRead.at`
  for retained committed history. Wrapper-source integration is open. Roadmap item 588.
* An explicit classify request with `projection: "frame"` returns bounded clause and
  diagnostic samples with totals, outcome and actual source identity. Missing/null
  projection keeps legacy output. False nodes, zero/null offset and null target defaults
  are harmless, while effective nodes, offset, target or OFN output are incompatible.
  An isolated restricted result remains useful but advertises no shared continuation.
  General entity/axiom reads with classification, property/individual entailments and
  justifications are not available. Roadmap items 588, 622 and 609, and F24's later
  phases.
* Retention counts auxiliary source/exclusion metadata and a published taxonomy alongside
  a DL query's newer saturation. Readers and an active replacement can keep an older
  publication alive. This memory tradeoff means the between-run one-gigabyte estimate
  does not cap combined live RSS. Bounded pages limit output/name buffers, not server
  scans or arbitrary scalar sizes. See [API.md](../API.md#ontology) for the read states,
  typed sources, pins and projection selectors.
* Every classification answer gives `tier`, the rules the axioms need, and `consistency`,
  which is `consistent`, `inconsistent` or `unknown`. `partial` on the request, `--partial`
  on the command line and the bindings' `partial` option ask a run of the disjunctive rules
  that its deadline or cancellation stops to answer what it proved. Such an answer has
  `partial` true and `complete` false, lists the classes whose superclasses it may miss
  under `incomplete` and gives every other class its full answer. Its consistency is
  `unknown` when the run stopped before deciding it, and then every class is incomplete.
  After a stopped run, immutable source hashing and response serialization finish over
  its already produced partial result without resuming classification or resetting shared
  cancellation flags. This read-only work can extend the response beyond the compute
  deadline. Completed results and subsequent GET reads retain their cancellation controls.
  The dataset never keeps a partial answer, and a classification that writes its taxonomy
  to a graph, the functional-style answer and the ROBOT-compatible commands refuse or
  ignore `partial`, since none of them can say that the taxonomy is partial. `sparkles
  ontology classify` takes `--timeout SECONDS` for the deadline, prints a note on stderr
  for a partial answer and exits with status 2 when the answer is partial and otherwise
  clean. A usage error also exits with status 2, so a script tells a partial answer apart
  by that note or by `partial: true` in the JSON output. `expansion` picks the cautious
  or the eager strategy of the disjunctive rules, which give the same answer.
* `sparkles ontology classify FILE` and `sparkles ontology check FILE` read a file of one
  graph straight into memory, with no store, and give the answers of a store that loaded
  the file. RDF/XML, Turtle, N-Triples and N3 parse on several threads, and so does a
  functional-style document, whose axioms the read keeps as written. A file that holds
  named graphs, a strict read of a functional-style document and a file whose answer
  depends on what only a store reproduces, such as its own blank node labels, go through
  a store in memory, and so does a file that fails to parse, with the loader's
  error. A functional-style document goes through the store when it has an anonymous
  individual, an undeclared entity or an `ObjectInverseOf` outside a plain property
  assertion, among the other cases that the
  [F24 Outcome](../specs/F24-owl-classification/SPEC.md#outcome) lists. ChEBI lite
  classifies from its functional-style file in 1.13 s on the benchmark host, and the
  reverse mapping of the triples bounds a read of RDF/XML
  ([results](../benchmarks/el-classification/RESULTS.md#file-classification)).
* The Rust handle `ds.ontology()` opens a snapshot's ontology, checks it and edits it,
  and keeps the last four reads of up to two million triples each, keyed by commit, for
  callers who read every graph. `/$/ontology/{ds}` serves the summary, the axioms, the
  checks and edits over HTTP with the caller's grants
  ([API.md](../API.md#ontology)). `sparkles ontology summary|axioms|check|edit` does the
  same for a database directory or a server, and `sparkles ontology check FILE` checks
  an ontology file without a database. The check exits with status 1 when the ontology
  is outside a profile it names.
* `sparkles ontology reason`, `reduce` and `relax` do what ROBOT 1.9.11's commands of the
  same names do in OBO release pipelines, with ROBOT's options, values and defaults.
  `reason` writes the whole ontology with the inferred axioms and, by default, without the
  asserted subclass axioms that are not direct. `reduce` removes the subclass axioms that
  other subclass axioms entail, between named classes and existential restrictions alike,
  and `relax` adds the subclass axioms that equivalences with intersections and
  existential restrictions imply. They read a file with `--input` and no database, as
  `sparkles ontology classify FILE` does, or a database with `--loc`, and write the
  ontology with `--output` as functional-style syntax (`.ofn`), RDF/XML (`.owl`), Turtle
  (`.ttl`) or N-Triples (`.nt`), each optionally gzipped. They chain in one process as
  ROBOT's commands do, as in
  `sparkles ontology reason --input x.owl relax reduce --output y.owl`. `classify` and
  `check` can join a chain to classify or check the ontology at that point, and a chain
  can start from `classify FILE` or `check FILE`, which answer as they do alone. A chain
  from a database starts with `reason`, `reduce`, `relax` or `materialize` and `--loc`.
  Without `--reasoner`, `reason` and `reduce` also take the Horn axioms outside OWL 2 EL,
  such as GO's inverse properties, so `reason` adds the subsumptions they entail.
  `--reasoner ELK`, which `materialize` takes by default as ROBOT's does, and `emr`, which
  ROBOT builds on ELK, run the OWL 2 EL rules alone and leave those axioms out, as ELK
  does, so the output is ROBOT's with ELK. `hermit`, `jfact`, `whelk` and `structural` run
  the default classifier with a warning. The library has the same switch as
  `ClassifyRequest::el_only` and `Limits::el_only`. The queries ELK does not answer, such
  as the disjoint classes and the property hierarchy, add no axioms, as they add none in
  ROBOT. Errors and the exit status 1 follow ROBOT's, and a malformed command line exits
  with status 2, as every `sparkles` command does. With `--reasoner ELK` the outputs equal
  ROBOT's axiom for axiom on GO and ChEBI lite
  ([ROBOT comparison](../benchmarks/el-classification/RESULTS.md#robot)). The commands
  read no `--server`. Roadmap item 685. They write no OWL/XML, Manchester, OBO or OBO
  Graphs JSON. Roadmap item 686. They refuse `--prevent-invalid-references true`. Roadmap
  item 687.
* `sparkles ontology materialize` does what `robot materialize` does. It asserts each
  class's direct superclasses of the form `ObjectSomeValuesFrom(p C)` for the properties
  of `--term` and `--term-file`, together with the direct named superclasses that are not
  asserted, and with no term it takes every object property of the ontology. A term is a
  full IRI or a CURIE with one of the OBO prefixes of ROBOT's default context, such as
  `BFO:0000050`, and a term file holds one a line with `#` comments. A term that is
  neither a URN nor a URL that Java's URL parser accepts, such as an IRI with a malformed
  port, is left out with a warning, as ROBOT leaves it out. A superclass is
  direct in the taxonomy that has a class for each such restriction, so a superclass is
  left out when another superclass of the class, named or a restriction, lies below it.
  The command validates the ontology first, as `reason` does, and takes `--reasoner` as
  `reduce` does, with ELK by default, so it runs the OWL 2 EL rules alone unless another
  reasoner is named. `--create-new-ontology true` adds nothing, because ROBOT's command saves
  the ontology without the axioms it found, and `--remove-redundant-subclass-axioms` and
  `--annotate-inferred-axioms` are accepted and ignored, as ROBOT ignores them. It runs
  from a file or a database and joins a chain, as in
  `sparkles ontology reason --input go.owl materialize --term BFO:0000050 reduce --output y.owl`.
  On GO with part of and ChEBI lite with has role its output equals ROBOT's axiom for
  axiom. The classifier names each restriction by a fresh class equivalent to it, as
  ROBOT's expression materializing reasoner does, but only for the classes that can be
  fillers, so one property takes about as long as `reason`. Each further property holds
  more memory, about 280 MiB on ChEBI lite, where all nine properties take 5,084 MiB of
  peak resident memory against 2,506 MiB for `reason`
  ([materialize results](../benchmarks/el-classification/RESULTS.md#materialize)).
* `sparkles ontology check FILE` reports the profile and the axioms outside it, and
  `sparkles ontology axioms` lists the axioms of a database or a server.
* Python's `Dataset.ontology`, Node.js's `dataset.ontology` and the JVM's
  `DatasetGraphSparkles.ontology()` and `SparklesOnt` run the summary, listing, check,
  edit, entity removal and diff in the JSON forms of the routes
  ([integrations](../integrations/README.md#libraries-and-bindings)). Python's `Axiom`
  is a frozen axiom with structural equality, and Node.js lists axioms as an async
  iterable in batches. `SparklesOnt.model` runs Jena's `jena-ontapi` over a Sparkles
  graph that one native read copies into memory, with writes sent to the graph in the
  caller's transaction.
* `GET /$/ontology/{ds}/search?q=` finds entities by label, synonym, short form and
  definition with typos tolerated, ranked for a typeahead, and the bindings' ontology
  handles call it as `search`. See [entity search](../search/README.md#entity-search) for
  its matching, ranking and index.
* `GET /$/ontology/{ds}/diff?from=&to=` lists the axioms that differ between two
  commits, with the graph of each, as JSON or as `+` and `-` lines. The terminology of
  each state is read as the summary reads it and the two axiom sets are compared. With
  `assertions=true`, the assertions that the triples changed between the commits carry
  are listed too, so an unchanged assertion costs nothing to diff. A diff of two states
  whose graphs did not change opens neither. The diff needs the commits in the dataset's
  retained history and the `diff` endpoint for a caller whose grants limit endpoints.
* The MCP tool `ontology_axioms` lists axioms in functional-style syntax. On a server
  that allows updates it also adds and removes axioms, given in functional-style syntax
  or as JSON, with the preview, `ifHead` precondition and commit message of
  `sparql_update`. Each added axiom is annotated with PROV-O provenance:
  `prov:wasGeneratedBy` the call's activity, `prov:generatedAtTime` and
  `prov:wasDerivedFrom` the source the call names. Annotation assertions describe the
  activity, its principal, its message and the software agent. The PROV-O properties are
  declared as annotation properties, so the ontology stays in OWL 2 DL, and an ontology
  that types them as object or data properties refuses the provenance. Removals are
  recorded by the activity and the commit, since an annotation cannot describe an axiom
  that is gone.
* `sparkles_owl::manchester` reads OWL 2 Manchester syntax against an ontology's
  prefixes, local names and labels. `POST /$/ontology/{ds}/parse` and the bindings'
  `parse` read a class expression, data range, individual, literal or one-line axiom
  with the caller's grants, and a name that several entities share is an error that
  lists them, with the span of the name ([API.md](../API.md#ontology)).
* A DL query answers a class expression's equivalent classes, direct and inferred
  superclasses and subclasses, and instances, as Protégé's DL Query tab does.
  `ds.ontology().dl_query()`, `POST /$/ontology/{ds}/dl-query` and the bindings'
  `dl_query` or `dlQuery` run it. Inside OWL 2 EL it adds the expression as a fresh
  class to the dataset's kept classification, saturates only what that class reaches and
  removes it again, so the answers are complete and the kept taxonomy is unchanged.
  Outside EL the OWL 2 RL rules answer it by demand reasoning over a copy of the
  ontology in memory. Their instances are complete for an ontology in OWL 2 RL, and
  their class answers can miss entailed subsumptions. Every answer names the engine,
  why it was chosen, whether it is complete and the axioms outside the profile. The
  instances come from a classification with the assertions, which the dataset does not
  keep, so an instance query classifies the ontology again.
* The web UI's **Ontology** page, `/ui/workbench/ontology`, edits an ontology graph over these
  routes ([U12](../specs/U12-ontology-workbench/SPEC.md#outcome)). It has trees of
  classes, object, data and annotation properties and individuals, an entity frame with
  the entity's axioms in Manchester syntax, an annotation editor with language tags and
  annotations of axioms, a header editor and an axiom-level usage view. The **Inferred**
  switch shows the EL classifier's class taxonomy beside the asserted tree. The page
  creates entities with IRIs from a per-dataset IRI policy, renames and deletes them,
  and runs every edit as a previewed dry run before it commits with `If-Match`, so a
  commit by another writer in between refuses the edit. **Start session** edits on a
  branch of its own, an undo stack for the browser session undoes and redoes commits by
  reverting them, which restores exactly the triples each commit changed, and the change
  feed reloads the views after another writer's commit. The frame's add fields take
  Manchester syntax with completion of names and labels, and the **DL Query** tab runs
  DL queries with the reasoning label beside the answers.

  ![The ontology editor with the class tree and the frame of Professor](../images/ontology-editor.png)

  ![The DL Query tab with the answers of a class expression and the frame of a subclass](../images/ontology-dl-query.png)

  ![The asserted and inferred class trees side by side](../images/ontology-inferred.png)

  ![The preview of a new subclass, with the axioms it adds and the triples it changes](../images/ontology-preview.png)

* The W3C OWL 2 test cases run as `mise run test:owl`, with the known differences and
  their reasons in `crates/sparkles-owl/tests/w3c_owl_known.txt`. The 78
  functional-style ontologies of the cases all parse, read back from the writer's
  output unchanged, and give the axioms of the cases' RDF/XML forms apart from five
  listed differences in the cases' data.

### Write-time validation

* SHACL and ShEx can validate the post-state of every commit before anything is written.
  In `reject` mode a failing write is refused with `422`, and in `warn` mode it is
  committed and the results are reported. Grandfather mode, in both languages, blocks only
  the results a write introduces.
* SHACL shapes come from dataset graphs, a file, or both. ShEx uses a schema, which can
  also live in ShExR graphs of the dataset, and a query shape map expanded per write.
  When a schema is set, its imports are resolved and copied into the database. A copied
  schema's digest is verified before installation or reuse in a settings change. A
  mismatch refuses the guard. Legacy ShEx configurations without a copy digest receive
  a full validation under the writer lock each time the dataset opens, and a validation
  that exceeds `timeoutSeconds` leaves writes refused. They gain the digest when their
  settings are saved, including from the UI's keep-source option.
* `/$/validation/{ds}`, `sparkles validation` and `serve --validate` configure it in
  `validation.json`. The CLI's `--no-validate` skips it, and writes that bypass
  validation are flagged `unvalidated` in the commit log. If a dataset's guard cannot be
  loaded, its writes are refused rather than accepted unvalidated.
* A write validates only the focus nodes or associations it can affect, against the
  exact result counts of the head kept in `validation-status.json`. It skips writes that
  touch no relevant graph or only predicates no shape reads, and falls back to a full
  validation when the head's state is unknown, the shapes change or too much is affected.
  SHACL-SPARQL constraints over ordinary stored predicates anchored at the focus node,
  `sh:targetWhere` targets and
  `rdfs:subClassOf` changes validate incrementally. For recursive ShEx schemas the guard
  keeps the head's typing and validates only where typings change. Computed SPARQL
  predicates use full validation when their actual graph reads cannot be localized,
  including list property functions and potentially rewritten spatial relations.
* Results show in `sparkles_validation_*` metrics, the `validation` and `validation_ms`
  access-log fields, CLI summaries and `sparkles stats`.

Editing forms also edit declared nested blank-node children, SHACL 1.2 lists and
`sh:or` or `sh:xone` value alternatives, and save each form's changes as one guarded
commit. See the [nested form model](../visualization/README.md#nested-form-read-models)
for its allowances and the editors.

### Schema discovery and drafted shapes

* `GET /$/schema/{ds}`, `sparkles schema` and `sparkles::schema` list classes and
  predicates with exact counts per graph ([API.md](../API.md#constraints-layer)). The counts
  cover triples, distinct subjects and objects, object kinds, datatypes, languages and the
  most objects per subject, kept apart from the RDFS and OWL declarations. The report
  also gives subClassOf roots and cycles, and `detail=subjectClasses` lists the classes of
  each predicate's subjects.
* Pagination uses cursors bound to one snapshot, and time and entry budgets make the
  report fail rather than truncate. An RDF `Accept` header or
  `sparkles schema --format void|turtle` returns a VoID description with class and
  property partitions.
* A constraints layer lists the SHACL constraints per class and says whether each is
  enforced on write, reported on write, or checked only on request. Anonymous
  superclasses, equivalent classes, domains and ranges are rendered in the OWL 2
  Manchester Syntax.
* After a write, the server updates its kept report from the changes instead of
  recounting. On stores above ten million quads, the schema that grounds an Ask comes
  from the index statistics ([API.md](../API.md#schema-reports-from-statistics)).
* Class profiles list, for each class, the predicates its instances use, with how many
  instances have a value, values per instance, object kinds, datatypes and value classes,
  and the predicates that point at its instances ([API.md](../API.md#class-profiles)).
  Schema diffs compare the reports of two commits, times or snapshots field by field.
  `/$/schema/{ds}/profiles`, `/$/schema/{ds}/diff`, `sparkles schema --profiles` and
  `--diff` and the MCP tools `describe_schema` and `diff_schema` serve them.
* Drafted shapes give each class a SHACL node shape, or a ShEx shape with a query shape
  map, computed from exact per-class profiles ([API.md](../API.md#drafted-shapes)). A draft
  can use cardinalities, node kinds, datatypes, `sh:class`, `sh:in`, language
  constraints, patterns, observed ranges, inverse property shapes, sequence paths through
  blank nodes and optionally `sh:closed`. A constraint is drafted when the share of
  instances that satisfy it reaches a support threshold, and at support 1 the current
  data conforms. `GET /$/schema/{ds}/shapes`, `sparkles schema --draft-shapes` and the MCP
  tool `draft_shapes` draft them, reading only the caller's visible graphs.

### OML and openCAESAR models

* openCAESAR's adapter writes an Ontological Modeling Language model as OWL 2 ontologies
  with DL-safe SWRL rules, and a vocabulary bundle as disjointness axioms over its
  concepts and relation entities. Sparkles reads that OWL form from files or from a
  dataset that holds each ontology in the named graph of its IRI, and follows the imports
  between those graphs. On the OWL that openCAESAR's build writes for its Kepler16b
  sample, the classifier's taxonomy equals the one that `owl-reason` gets from Openllet.
  Its named types of individuals equal Openllet's apart from types it gives the SWRL
  rules' variables, and the classifier finds the
  inconsistencies that the bundle's disjointness, keys, functional properties, universal
  restrictions and property characteristics raise
  ([F24](../specs/F24-owl-classification/SPEC.md#oml-and-opencaesar-models)).
* `sparkles serve --fuseki-config` starts from an openCAESAR project's `.fuseki.ttl`,
  with its in-memory dataset, union default graph, query timeout and SHACL endpoint. It
  answers the requests that openCAESAR's `owl-load`, `owl-query` and `owl-shacl` send,
  which are a Graph Store `PUT` of each ontology and of each entailment ontology into
  the graph of its IRI, the project's queries on the `sparql` service and shapes posted
  to `shacl?graph=default`. The queries then read the entailments that `owl-reason`
  wrote, not Sparkles' own reasoning.
* The rule parser refuses the Jena rules file that the adapter writes with
  `generateRules`, because its `@prefix` lines give the namespace IRIs without angle
  brackets. Once the brackets are added, `sparkles infer --rules` derives the facts that
  Jena's rule engine derives from the file.
* The SWRL rules are not read, so a reified relation's forward property and the facts of
  an OML rule are not derived, and the OWL RL materialization answers a query that needs
  them with fewer rows. Rule-derived inconsistencies, enumerations beside keys, OWL 2 DL
  datatypes such as `xsd:double` and `xsd:boolean`, and openCAESAR's closed-world
  cardinality check are not covered. The known gaps below name the roadmap items.

## Known gaps

These limits name the roadmap items that address them. Current priorities and
dependencies are recorded in the [central roadmap](../ROADMAP.md).

### Reasoning and validation

* A rule program cannot have an existential head, a head variable that no body atom
  binds, until the restricted chase of item 839, and a query `?-` and comparisons such as
  `?x < 5` belong to item 72's Datalog query language. A demand that binds a later column
  of a relation of three or more columns derives every fact of the relation, which keeps
  `owl-rl`'s chains and keys in rules per length. Roadmap items 1102 and 517. An
  explanation of a triple whose derivation passes a relation fact that was derived from
  a generalized fact, one with a literal subject, finds no derivation. Roadmap item 1103.
* An update of a `shacl-af` materialization runs in full, and the rules are evaluated
  per focus node in each round rather than semi-naively. Native maintenance of triple
  rules and of SPARQL rules whose bodies are basic graph patterns, and a recompute stratum
  for other bodies, follow the shared maintenance core of item 837. Roadmap item 958.
  Query-time reasoning, explanations and witnesses do not cover the profile.
* SPIN rules given only in SPIN's RDF syntax, without `sp:text`, and calls of SPIN
  templates are refused. Roadmap item 959.
* The `owl-rl` profile has property chains of 2 and 3 properties and keys of 1 and 2.
  Roadmap item 517. It leaves out `eq-ref`, `dt-type2`, `dt-eq` and `dt-diff`. Roadmap
  item 518. Its rule tests follow the Profiles tables rather than the OWL 2 test cases.
  Roadmap item 520.
* Closure operators serve full materialization only. Incremental updates and query-time
  reasoning evaluate transitive and equivalence rules as joins. Roadmap item 521.
* A deletion in a large clique of a symmetric and transitive property, where every fact
  follows through every member, passes the bound on maintenance and runs in full. The
  bound leaves out the witness preparation of a deletion, which is roadmap item 815. A run
  that falls back reports the full run's peak memory and not the higher peak of the update
  it abandoned, which is roadmap item 816.
* Inference is forward materialization, RDFS on read and derivation on demand.
  Under `@hybrid jena` the query-time program evaluates forward and backward rules in one
  fixpoint, so a forward rule reads what a backward rule derives at query time, where
  Jena's forward engine does not.
* OWL 2 QL rewriting leaves out reflexive properties, inclusions with `owl:Thing` on the
  left, data existentials on the left with a datatype range and triples about undeclared
  properties, and its rewritten patterns read the query's default graph, not the data of
  named input graphs. It decides which variables may stand for an unnamed individual by
  name over the whole query rather than by scope, and its fallback to the `owl-rl` rules
  can use axioms outside OWL 2 QL. Roadmap item 985.
  `reasoning=auto` reads a materialization that can answer rather than compare costs per
  pattern. Roadmap item 910.
* A view's tables carry over to another commit only while the rules cannot read or give
  any quad that differs between them. The `rdfs` profile's `rdfs4a` and `rdfs4b` read
  every fact, as do `owl-rl`'s `eq-rep-*` rules once the data holds `owl:sameAs`, so such
  views derive again after every commit to their input graphs. `rdf1` reads which
  predicates are in use, so a commit that brings a predicate into use or takes the last
  quad of one away derives again. A table that a later commit's query derives under an
  earlier commit's tables, and that holds a term the earlier commit lacked, is not kept.
  An anchor keeps a clone of its commit's delta, which retains the key-set nodes that
  later writes replaced.
* Demand reasoning has per-query schema analysis and program setup costs. Schema
  completeness analysis can stream the visible inputs twice, including when schema
  tables are warm, and reserves a conservative workspace against the memory budget.
  Analysis caps keep memory bounded and leave unproven relations for ordinary demand.
  Programs containing `makeTemp` forgo schema specialization and reuse. Implicit fact
  replay can allocate a temporary generator term before refusing an old output.
  A bounded temporary closure can avoid evaluating the expanded schema program for a
  tiny positive feedback view, but can retain more intermediate facts than selective
  evaluation.
  Structural list terms, negation and term-generating rules exclude this route.
  The equality stages keep their facts, which do not depend on the query, once per view
  in the derived-table cache. This trades memory for time, since the table can hold
  many more facts than a selective query derives.
  On small views where `owl:sameAs` relates classes and properties, a demand can take
  much longer than materializing the view, because equality is read through the
  `eq-rep-*` rules rather than representatives. Roadmap items 9 and 190.
  Roadmap item 190 covers these costs and matched Forge measurement. The
  [recorded demand measurements](../specs/F17-query-time-reasoning.md#outcome) identify
  their source revision and do not establish current-head performance.
* DESCRIBE is refused under derivation on demand. Roadmap item 193. Hierarchical
  budgeting that links reasoning, incremental views and the query pool is not built.
  Roadmap item 675.
* Provenance-based inferred visibility, which reads the reasoner's witnesses, is not
  available. Roadmap item 18. Witness builds and updates take much longer than the
  closure. Roadmap item 555. Their memory and their rebuild after the closure is read back
  from the dataset are further costs. Roadmap item 556. Every fact of a program with
  `noValue`, `listNotContains`, default geometries, head actions or a list that is not
  plain is always truncated. Roadmap item 557. Query result cells do not offer a Why?
  action. Roadmap item 285. `why_empty` adds hypotheses for a grounded failing
  intensional pattern. Abduction for patterns with unbound target variables is not
  available. Roadmap item 286.
* Derived facts use the ordinary inferred graph. A separate derived-fact layer and
  canonical `owl:sameAs` representatives are not built. Roadmap item 9.
* Incremental maintenance does not cover rules with `noValue`, `now` or new blank nodes,
  or RDF lists that change under OWL 2 RL. Those runs, and the first run after a
  compaction without retained history, recompute everything. Diagnostic checks do not
  prove consistency, and datatype membership that cannot be decided remains unknown.
  Roadmap items 9 and 53.
* Because the kept closure holds only derived facts, OWL 2 RL changes of 100 to 10,000
  triples over it take 1.2 to 2.3 times as long at one thread as with a closure that also
  holds the explicit facts, on LUBM-style data. Roadmap item 941. The first change of 100 RDFS
  triples to a part of the data takes 2.6 times as long. Roadmap item 942. RDFS undos and
  repeated changes take 0.85 to 1.08 times as long on that data.
* Write-time validation validates recursive SHACL shapes, and SHACL-SPARQL constraints not
  anchored at the focus node, in full on every write. Roadmap item 78.
* ShEx keeps no typing when the process is killed or exits without closing the store, so
  the first writes afterwards to a recursive schema over a large connected graph may
  validate in full. Roadmap item 54.
* Validation endpoints refuse callers whose grants cover only some graphs or triples.
  Roadmap item 75.
* An ontology read does not fetch an `owl:imports` target that is not a graph of the
  dataset. Roadmap item 559.
* The MCP ontology tool and the CLI's ontology commands read the asserted axioms only,
  without the materialized inferences that the library, the bindings and the HTTP routes
  can include. Roadmap item 565.
* The CLI's ontology commands and the MCP ontology tool have no entity listing or entity
  report. Roadmap item 578.
* An ontology read has no size budget of its own beyond the query deadline, and its cost
  on large ontologies is not measured. Roadmap item 539.
* The EL classifier compares integer, decimal and rational values exactly at arbitrary
  precision under its resource controls. XML literals, ill-typed literals, datatypes
  outside the supported OWL 2 datatype map and chains through the top object property
  remain partial. XML namespace conformance preserves lexical spellings and does not
  provide canonical value equality. Roadmap item 663.
* An EL classification after a change to an axiom with an individual, a data property
  or a key saturates everything again, and so does any change after a run that merged
  a context with an individual.
  Roadmap item 664.
* In an ontology with individuals, the Horn rules leave out an at-least restriction
  above one and report it, since the unfolding they use elsewhere is argued for
  tree-shaped models. Roadmap item 867.
* On an ontology with many property assertions, the Horn rules take 1.8 to 2.0 times the
  EL rules' time on one thread and derive about 2.2 times the conclusions. Every context
  takes the subsumers of `owl:Thing`, a successor is told twice that it has an instance,
  and the states of the automata and existential restrictions repeat. Roadmap item 1025.
* GO's inverse properties take the Horn rules, which classify it 12% slower than the EL
  rules on one thread and raise its peak resident set from 196 to 223 MiB. On two
  Uberon-based ontologies of the ORE 2015 DL sample the Horn rules take about five times
  the EL rules' time, since each context of a conjunction derives its successor's
  subsumers again. Roadmap item 693.
* The same two Uberon-based ontologies take the disjunctive rules first, whose attempt
  passes its bound on derived conclusions before the Horn rules answer. On four threads on
  `atlas` the bound ends the attempt so that `ore_ont_9663` classifies in 7.5 to 8.8 s and
  `ore_ont_14817` in 7.9 to 11.3 s. Konclude 0.7.0 classifies them in 8.9 to 9.7 s on four
  threads on the laptop. Roadmap items 693 and 1011.
* Three large ORE 2015 DL files whose disjunctive attempt finishes, `ore_ont_10140`,
  `ore_ont_6952` and `ore_ont_11460`, classify 1.06 to 1.15 times slower in the EL part of
  their saturation with the build that bounds the attempt, and as slow with the bound
  turned off. Roadmap item 1012.
* Horn ontologies whose successors no other successor dominates classify up to 6% slower
  than without the check for dominated successors, which costs more there than it saves.
  Roadmap item 991.
* The EL saturation of one ORE 2015 DL ontology with 2.5 million existential
  restrictions over a transitive symmetric role derives each link of the transitive
  closure once per intermediate class, so it sends three billion messages for 32 million
  conclusions. On `atlas` its saturation takes 8.9 s on four threads, 7.4 s on twelve and
  24.6 s on one. Konclude 0.7.0 classifies it in 9.5 s on the same host at 6.7 GiB, with
  any number of workers. Close to half of a four-thread run's cycles go to the checks of
  the repeated messages against the contexts' tables, which roadmap item 956 follows. The ontology
  takes the Horn rules, which classify it in 13.9 s on four threads with a peak resident
  set of 6.0 GiB on `atlas`, as the
  [EL classification results](../benchmarks/el-classification/RESULTS.md#symmetric-transitive-properties-with-the-horn-rules)
  show.
* A small ontology's first classification in a process on several threads starts its
  helper threads after 0.5 ms of work, and later ones after as long as the last start of
  the helpers took. Warm runs of some small files on eight threads take up to 0.2 ms
  longer than a start at once would. Roadmap item 819.
* The ELK mode, which classifies by the OWL 2 EL rules alone, is an option of the Rust
  library and the ROBOT-compatible commands only, and HTTP, the other CLI commands and
  the bindings always take the Horn axioms. Roadmap item 694.
* By default, axioms outside OWL 2 EL that need nominals, and axioms that need case
  analysis or counting in an ontology with individuals, are left out, since the rooted run
  that takes them is an option whose completeness is not proved, and five of the 50
  nominal fixtures of its gate pass its work bound. The rooted run also passes its bound
  on ore_ont_14924, ore_ont_15803 and many generated ontologies with keys or the universal
  property, and then gives the default answer after the work. Roadmap items 822, 823
  and 1061. Axioms that
  need case analysis in an ontology with data properties are left out, and an external DL
  reasoner bridge is planned. Roadmap item 603. The disjunctive rules need a
  datatype checker with data cardinalities before they take data properties, and 10 of
  the 97 disjunctive development files of the ORE 2015 DL sample wait on it. Roadmap
  item 768.
* Counting is off by default. Five files of the ORE 2015 DL development set attempt
  counting. With the order between number restrictions, which a counting run uses by
  default, the two versions of the Protein Ontology, ore_ont_6952 and ore_ont_10140,
  finish with counting and classify in 1.2 s on one thread and 0.4 s on four on `atlas`,
  against 0.9 and 0.3 s without counting and 2.3 to 2.5 s for Konclude 0.7.0. The other
  three pass the bound of derived conclusions and get the answer without counting. The
  order makes the failed attempt of ore_ont_6485 dearer. Its classification with counting
  requested, the attempt and the run without it together, takes 213 ms on one thread with
  the order and 188 ms with `Limits::number_order` set to false, 1.14 times as long,
  against 7 ms without counting and 40 ms for Konclude. Roadmap items 896 and 729.
* BioTop, ore_ont_12141 of the ORE 2015 DL sample, classifies in 23 ms on four threads,
  the time Konclude takes, which is short of the classifier's aim of three times
  Konclude's speed. Roadmap item 712.
* ore_ont_3215 of the ORE 2015 DL sample passes the disjunctive rules' bound on derived
  conclusions and gets the Horn rules' answer without its 18,323 unions. Roadmap item 713.
* `partial` applies only to runs of the disjunctive rules, and runs of the EL and Horn
  rules still fail at a deadline on every surface. Roadmap item 714.
* The Workbench does not ask for a partial classification, so it shows no partial answers
  or incomplete classes. Roadmap item 719.
* Cold retained-snapshot preparation polls primary cancellation and deadlines. Additional
  composed cancellation is checked around preparation, and during strict materialization
  freshness queries and later index/hash work. Propagation through all cold resolution,
  cache, decoding and replay work and its request/server admission is planned. Roadmap
  item 539. Historical reads remain available, with this interruption limitation.
* No MCP tool classifies. Roadmap item 586. Bounded kept reads supply named-class
  roots, direct parents and children, equivalence and unsatisfiability. General entity
  accessors, inferred axiom listings and subclasses of a class expression are not
  available. Roadmap item 588. Browser materialized views are separate recorded-profile
  facts and do not answer from the class-only classifier or establish arbitrary-axiom
  proofs.
* A classification runs within one request's deadline and is not kept up to date as
  commits land. Roadmap item 589.
* A Horn classification of an ontology with individuals, keys or data restrictions
  rebuilds after every axiom insertion and deletion. Roadmap
  [item 753](../ROADMAP.md) under item 603.
* A classification that counts and a rooted one rebuild after every edit, since the
  bound on their derived conclusions depends on the order of the derivations. Roadmap
  [item 953](../ROADMAP.md).
* A disjunctive classification with the eager strategy rebuilds after every edit.
  Roadmap [item 753](../ROADMAP.md).
* A classification that keeps its contexts after an edit still normalizes every axiom
  and builds its index and its taxonomy again, which bounds what a small edit saves.
  Roadmap [item 955](../ROADMAP.md).
* An edit that clears most contexts of a kept classification costs more than a
  classification that starts again, by the comparison of the two indexes. Roadmap
  [item 954](../ROADMAP.md).
* A classification with the assertions saturates everything on each run, since the
  dataset keeps only classifications of the terminology. Roadmap item 590.
* The ontology readers map SWRL rules in RDF to assertions about anonymous individuals,
  and their variables to named individuals. Roadmap item 970. DL-safe SWRL rules do not
  run in materialization, query-time reasoning or the classifier. Roadmap item 971.
* A rooted run refuses keys over data properties and functional data properties, which
  OML vocabularies declare for their identifiers, as the Kepler16b sample and its
  metrology library do. Roadmap item 768. The classifier leaves out the literals and
  ranges of datatypes outside OWL 2 EL, such as `xsd:double` and `xsd:boolean`. Roadmap
  item 972.
* A lenient read maps no axiom for an `owl:AllDisjointClasses` with an undeclared member
  and reports no warning. Roadmap item 973. The `all-disjoint-classes` check of
  `sparkles infer --check` passes its intermediate-row limit on a 55-member list with a
  few thousand triples. Roadmap item 974.
* openCAESAR's closed-world cardinality check, its entailment outputs and the OML
  textual syntax have no counterpart. Roadmap items 975 to 977.
* The Jena rule parser refuses an `@prefix` whose namespace IRI has no angle brackets,
  which Jena's parser accepts and openCAESAR's adapter writes. Roadmap item 1020.

## Configuration and usage

The following guides describe configuration and supported UI, command-line, HTTP
or library workflows. Exact HTTP contracts remain in the API reference.

* Read [Input graphs and imports](../API.md#input-graphs-and-imports).
* Read [RDFS on read](../API.md#rdfs-on-read).
* Read [Query-time reasoning](../API.md#query-time-reasoning).
* Read [Explanations](../API.md#explanations).
* Read [Entailment diffs](../API.md#entailment-diffs).
* Read [Minimal justifications](../API.md#minimal-justifications).
* Read [SHACL Compact Syntax (SHACLC)](../API.md#shacl-compact-syntax-shaclc).
* Read [ShEx validation](../API.md#shex-validation).
* Read [Ontology](../API.md#ontology).
* Read [Constraints layer](../API.md#constraints-layer).
* Read [Schema reports from statistics](../API.md#schema-reports-from-statistics).

## Roadmap

The remaining work described above refers to [item 9](../ROADMAP.md), [item 18](../ROADMAP.md), [item 19](../ROADMAP.md), [item 48](../ROADMAP.md), [item 53](../ROADMAP.md), [item 54](../ROADMAP.md), [item 75](../ROADMAP.md), [item 78](../ROADMAP.md), [item 190](../ROADMAP.md), [item 192](../ROADMAP.md), [item 193](../ROADMAP.md), [item 285](../ROADMAP.md), [item 286](../ROADMAP.md), [item 539](../ROADMAP.md), [item 559](../ROADMAP.md), [item 565](../ROADMAP.md), [item 578](../ROADMAP.md), [item 768](../ROADMAP.md), [item 941](../ROADMAP.md), [item 942](../ROADMAP.md), [item 953](../ROADMAP.md), [item 954](../ROADMAP.md), [item 955](../ROADMAP.md), [item 958](../ROADMAP.md), [item 959](../ROADMAP.md), [item 970](../ROADMAP.md), [item 971](../ROADMAP.md), [item 972](../ROADMAP.md), [item 973](../ROADMAP.md), [item 974](../ROADMAP.md), [item 975](../ROADMAP.md), [item 976](../ROADMAP.md), [item 977](../ROADMAP.md).
The central roadmap owns priority and dependencies. Its
[completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs)
link partially complete specs to their remaining work and acceptance criteria.
