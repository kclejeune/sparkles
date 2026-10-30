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
* Each iteration merges its results in parallel on a reasoning pool, half the cores by
  default with `serve --reason-threads`, that admits runs in arrival order. A run stops at
  50 million derived triples by default, and `sparkles infer --max-inferred`,
  `serve --reason-max-inferred`, a request's `maxInferred` and the bindings change the
  limit. A run past the limit fails with the code `inferred-limit` and commits nothing.
  `ReasonOptions::max_memory` bounds the estimated heap of the whole run.
* Re-runs, automatic runs and `sparkles infer` update the previous materialization
  incrementally with the backward/forward algorithm, and write what a full run would.
  Rules that are not monotonic or create blank nodes, large deletions and a changed set of
  input graphs run in full. The kept closure holds only the derived facts, which takes 41%
  to 47% of the memory of a closure with the explicit facts on LUBM-style data.
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
  Jena backward rules answer this way. A demand transformation starts from the query's
  constants and bindings and specializes schema relations proven complete. Schema facts
  can follow from derived instance facts, including class and property declarations and
  equality between schema terms. Programs containing `makeTemp` run in one ordinary
  demand program with their normal lower stages. Derived tables are cached per snapshot
  and view, and `maxDerived`, the deadline and the memory budget bound them. Tables that
  contain generated blank nodes are not cached. A tiny positive feedback view whose
  schema expansion creates thousands of rules can use a bounded temporary closure of
  the original rules. This retains intermediate facts within the session and uses the
  same resource limits. EXPLAIN shows a `Derive` node per pattern.
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
* Sparkles records the commit at which inferences were made, and only a later change to a
  graph the run read makes them stale. `GET /$/reason/{ds}`, dataset info, `/$/stats` and
  the `Sparkles-Inferences` header report freshness. Automatic re-runs are opt-in with
  `serve --auto-reason` or `PUT /$/reason/{ds}/auto`.
* `GET /$/reason/{ds}/diagnostics` and `sparkles infer --check` run 17 inconsistency
  checks that cover every OWL 2 RL rule with a `false` conclusion, in JSON or Turtle.
  `literal-not-in-datatype` finds ill-typed literals for the datatypes whose value spaces
  it can decide. The checks never prove consistency.

### SHACL

* `sparkles-shacl` validates SHACL Core and SHACL-SPARQL in parallel over the indexes.
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

The Rust forms library can also read declared nested blank-node layouts, preserving
shared identities, visible graph anchors and cycles within one preparation allowance.
The [forms guide](../visualization/README.md#nested-form-read-models) describes this
read primitive and the remaining atomic editing and HTTP/browser consumers.

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

## Known gaps

These limits name the roadmap items that address them. Current priorities and
dependencies are recorded in the [central roadmap](../ROADMAP.md).

### Reasoning and validation

* Inference is forward materialization, RDFS on read and derivation on demand.
  Derivation on demand has no cost-based choice against a materialization, no OWL 2 QL
  rewriting, no reuse of tables across commits and no dataset setting, and Jena's hybrid
  rules are flattened. Roadmap item 19.
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
  On small views where `owl:sameAs` relates classes and properties, a demand can take
  much longer than materializing the view, because equality is read through the
  `eq-rep-*` rules rather than representatives. Roadmap items 9 and 190.
  Roadmap item 190 covers these costs and matched Forge measurement. The
  [recorded demand measurements](../specs/F17-query-time-reasoning.md#outcome) identify
  their source revision and do not establish current-head performance.
* The derived-table cache's budget accounting is item 192. DESCRIBE is refused under
  derivation on demand, item 193.
* Provenance-based inferred visibility remains roadmap item 18. Query result cells do
  not offer a Why? action,
  item 285. `why_empty` adds hypotheses for a grounded failing intensional pattern.
  Abduction for patterns with unbound target variables remains item 286.
* Derived facts use the ordinary inferred graph. A separate derived-fact layer and
  canonical `owl:sameAs` representatives are not built. Roadmap item 9.
* Incremental maintenance does not cover rules with `noValue`, `now` or new blank nodes,
  or RDF lists that change under OWL 2 RL. Those runs, and the first run after a
  compaction without retained history, recompute everything. Diagnostic checks do not
  prove consistency, and datatype membership that cannot be decided remains unknown.
  Roadmap items 9 and 53.
* Because the kept closure holds only derived facts, changes of 100 triples at 16 threads
  over it take 1.6 to 2.3 times as long as with a closure that also holds the explicit
  facts, and undos at 1 thread take 1.3 to 1.8 times, on LUBM-style data. Roadmap item
  113.
* `sparkles fmt` and the UI's Format button do not format SHACLC. Roadmap item 48.
* Write-time validation validates recursive SHACL shapes, and SHACL-SPARQL constraints not
  anchored at the focus node, in full on every write. Roadmap item 78.
* ShEx keeps no typing when the process is killed or exits without closing the store, so
  the first writes afterwards to a recursive schema over a large connected graph may
  validate in full. Roadmap item 54.
* Validation endpoints refuse callers whose grants cover only some graphs or triples.
  Roadmap item 75.

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
* Read [Constraints layer](../API.md#constraints-layer).
* Read [Schema reports from statistics](../API.md#schema-reports-from-statistics).

## Roadmap

The remaining work described above refers to [item 9](../ROADMAP.md), [item 18](../ROADMAP.md), [item 19](../ROADMAP.md), [item 48](../ROADMAP.md), [item 53](../ROADMAP.md), [item 54](../ROADMAP.md), [item 75](../ROADMAP.md), [item 78](../ROADMAP.md), [item 113](../ROADMAP.md), [item 190](../ROADMAP.md), [item 192](../ROADMAP.md), [item 193](../ROADMAP.md), [item 285](../ROADMAP.md), [item 286](../ROADMAP.md).
The central roadmap owns priority and dependencies. Its
[completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs)
link partially complete specs to their remaining work and acceptance criteria.
