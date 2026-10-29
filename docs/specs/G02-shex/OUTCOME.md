# G02: ShEx validation — Delivery record

Current delivery status is summarized in the [landing page](README.md).
The [design Outcome overview](SPEC.md#outcome) links the supported scope to this record.
The recorded dates and measurements below describe their original delivery stages.

## Historical opening status

> **Status:** implemented in part, Phases 1 and 2 and most of Phase 3.
>
> **Phases:** ShExC, ShExJ, ShExR, imports, shape maps, interval matching and write-time
> validation are available, with persisted typing, incremental propagation and
> grandfather mode. ShEx 2.2 `ABSTRACT` and `EXTENDS` are supported. `RESTRICTS` and
> `EXACTLY` reference syntax remain outside the supported language subset. Combined
> validation guards and the remaining selector controls are planned under item 54.
>
> **User docs:** [API: ShEx validation](../../API.md#shex-validation) ·
> [API: Write-time validation](../../API.md#write-time-validation) ·
> [API: MCP server](../../API.md#mcp-server) · [Features](../../reasoning/README.md#reasoning-validation-and-schema)
>
> This is the design as written before implementation. The [Outcome](SPEC.md#outcome) section at
> the end records how it landed.

## Outcome

**Delivered.** The spec was written on 2026-09-30. Phases 1 and 2 landed on 2026-10-01 as
designed in §2–§6. Phase 1 brought the crate, the endpoint, the CLI, the harness,
`bench:shex` and the rudof differential script. Phase 2 brought the format 2 guard with the
predicate relevance skip, ShExR, SPARQL selectors, the UI switch and `bench:shex-write`.
The MCP tools `validate_shacl` and `validate_shex` were taken early from Phase 3. No new
third-party crates were added.

**Deviations from the spec.**
* **Matcher.** The interval method replaced bag derivatives as the only matcher (see the
  implementation note above). It is exact for every expression, because each triple
  constraint occurs once after `&include` expansion. Its cost is linear in the
  expression's size whatever the counts, while derivatives copy group bodies. The change
  was approved during implementation. The brute-force partition enumerator stays the
  property-test oracle.
* **Told blank nodes.** The store does not keep blank-node labels. The harness therefore
  skips the `LexicalBNode` validation tests, counting them, and lists one more test in
  `known-failures.txt`.
* This delivery wrote `validation.json` format 2 with `"language"` for both languages
  and retained format 1 reads for SHACL. ShEx subsequently moved to format 3 for
  [verified schema copies](../C10-write-time-validation.md#known-gap-schema-copy-digest-2026-10-16),
  retaining format 2 reads through full validation at installation. SHACL keeps format 2.
* Node-constraint annotations and semantic actions stay rejected, because ShExJ 2.1 has
  no slot for them. An exhausted outbound budget while fetching imports answers `507`,
  not `400`. Imports are capped at 64 schemas and 16 MiB per validation by default.

**Decided by the maintainer.**
* The `sparkles_validation_*` series gain a `language` label. The SHACL series become
  `language="shacl"`.
* SPARQL selectors are refused in the write-time guard (`400` when the configuration is
  set) and allowed on demand.
* Server flags for the import limits, and CLI `--outbound-*` flags for imports, are
  deferred. The import-limit flags came later. `--shex-max-imports`,
  `--shex-max-import-mb` and `--shex-import-timeout` apply to `serve`, `shex validate`
  and `validation`. The CLI `--outbound-*` flags for imports are still open.

**Chosen during implementation** (open to revision).
* The SHACL `Sparkles-Validation` header is unchanged, and ShEx adds `lang=shex`.
* Summaries and `GET /$/validation/{ds}` gain an additive `language` field.
* The stored schema is the text as given when it has no imports. Otherwise it is the
  closed schema as ShExJ, with its base and prefixes.
* The PUT body takes no inline imports or externs, so `EXTERNAL` is a `400`.
* An empty shape map is refused.
* SPARQL selectors do not inherit the prefixes of the map or the schema.
* `--schema` and `--shape-map` imply `--lang shex`.
* The MCP tools are on by default. They are read-only, use `maxResults` 20 and refuse
  imports.
* The UI gets no write-time validation panel in this phase. The dataset page's
  Write-time validation panel came later, and it shows ShEx guards as well as SHACL ones.

**Conformance at landing.** Sparkles passes 100 % of the syntax, negative-syntax,
negative-structure, representation and ShExR tests. It passes 99.9 % of the validation
tests from ShExC, ShExJ and ShExR: 42 blank-node-label tests are skipped, and one more is
listed in `known-failures.txt`.
[FEATURES.md](../../reasoning/README.md#reasoning-validation-and-schema) has the
current numbers. SHACL stayed at 98/98 + 20/20 through the shared-code moves.

**Performance.** [BENCHMARKS.md](../../BENCHMARKS.md) now publishes ShEx numbers from
`forge` on 2026-10-08. Full validation of 152,000 shape associations over 1.05M triples
took 1.532 s in parallel and 2.192 s sequentially, medians of 5. Both miss the §9
targets of 350 ms and 1.5 s. On the same run SHACL took 449 ms and 933 ms, so ShEx is
3.4 and 2.3 times slower than SHACL, where §9 allowed twice. The write-time comparison
meets its target, because `reject` stays within full validation plus 10 ms.

**Incremental guard validation** landed on 2026-10-02 with C10 Phase 2. The guard keeps
the counts of the head's result map, in memory and in `validation-status.json`. A write
validates the associations of the nodes it can affect, in the states before and after
it, and moves the counts by the difference.
* The affected nodes are found per node rather than per pair. They are the endpoints of
  changed triples whose predicate a triple constraint reads, plus the nodes that reach
  them backwards over triple constraints whose value expression refers to a shape. This
  over-approximates the pair dependencies of the design, and needs no typing of the
  states.
* The map's associations at those nodes are recomputed in each state. Term selectors
  select their node in both, and `{FOCUS p o}` and `{s p FOCUS}` selectors are checked
  against the state's arcs.
* The fallbacks are those of C10 that apply: an unknown state, `reject` on a head that
  does not conform, bulk writes, more than 50,000 affected nodes, and SPARQL selectors,
  which the guard refuses anyway.
* A recursive reference over a large connected graph makes every node that reaches the
  changed one affected. With `foaf:knows @ex:Person *` in `bench:shex-write`, a new
  `foaf:knows` arc between two people reaches more than 50,000 of 100,000 people, so that
  write is validated in full. Propagating only typings that change avoids this. It was
  not built with this step and landed later, as described below.
* At 1.05M triples, the benchmark's other write took 8 to 12 ms with `warn` and
  `reject`, against 2.1 to 2.7 s before.
* A property test checks the counts and the listed associations against full
  validations, over recursive, inverse and negated references with and without a
  `CLOSED` shape.

**Grandfather mode** landed on 2026-10-02 with C10 Phase 3. It has the semantics of
SHACL's: `"baseline": "grandfather"` in the configuration, `--grandfather` in the CLI,
`reject` allowed on data that does not conform, and a write blocked only by the
nonconformant associations it introduces. Associations are matched by node and shape as a
multiset, in the state before the write and in the state after it. Summaries gain
`introduced`, and the new associations are listed first. A full validation in this mode
also validates the state before the write. A schema changes only through a new
configuration, and setting one never compares two schemas, so the rule that a shapes
change compares with the old shapes' results has no ShEx case. The property test runs in
`warn` and in grandfather `reject`, and checks every decision, count and listing.

**Typing-change propagation** landed on 2026-10-02 too. For schemas whose references
follow arcs, the guard keeps the typing of the head in memory: every pair the validations
of the map discovered, with its value. A full validation sets it, and every write moves
it. It is dropped by a restart, a compaction (the generation's terms are numbered anew),
a bypassed write and any commit the guard did not judge. With it, a write is validated
as follows.

1. The region starts with the nodes whose neighbourhood the write changed and the nodes
   a changed selector arc may select.
2. The region's pairs, and the map's associations at its nodes, are typed in the state
   after the write. Every other pair the head's typing has as `true` is read as `true`
   and not discovered. A pair it has as `false`, or does not have, is discovered and
   typed.
3. If a typed pair's value differs from the head's, its node and every node that reaches
   it backwards over referring constraints join the region, and step 2 runs again.
4. When no value outside the region changes, the associations of the region's nodes are
   the only ones whose results the write can change. Their results before the write come
   from the head's typing, and those after it from the last typing.

The result is exact. Every pair outside the region keeps the head's value, which is a
fixed point of the state after the write. It is also the greatest one. A pair outside
the region that the head had as `false` and could turn `true` is read by no pair of the
region, since those are typed, and it reads only unchanged pairs. Setting it `true`
would therefore give a larger fixed point of the state before the write, which
contradicts the head's typing. Negation reads lower strata, which are exact first.
Joining the whole backward reach of a changed pair at once, rather than one reader per
round, keeps a change that runs along a chain to one more typing.

On data that mostly does not conform, typing every `false` pair the region reads soon
reaches most of the graph. The first build did so in `bench:shex-write`'s `warn` mode,
where the person write took 2.6 s. The typing therefore also records which `false` pairs
failed in discovery with every reference unknown, such as a person whose age breaks a
node constraint. Only a change to such a pair's own neighbourhood can make it `true`, so
outside the region it is read as `false` and not typed again. A person who knows one of
them is then decided `false` without going further. The design's plan of a
typing of both states was not needed, because the head's typing stands in for the state
before. The old walk over every node that reaches a changed one remains for writes when
the typing is unknown.

With `foaf:knows @ex:Person *`, a new `foaf:knows` arc between two people who conform
validates one association, where it validated everyone who reaches them, or fell back to
a full validation beyond 50,000 people. A unit test runs the four cases on a ring of
people. The property test now runs mostly through this path and passed 20,000 cases with
the final code, and 50,000 before the `false` pairs were split. shexTest stayed at 99.9 %
of the validation tests.

At 1.05M triples (152,000 associations), on a machine with a load average near 30 from
other builds, `bench:shex-write`'s person write took 16 ms in `reject` over the schema
the data conforms to, 18 ms in `warn` and 28 ms in grandfather `reject` over the schema
it mostly does not conform to. That write was validated in full before, in 2.1 to 2.7 s.
With validation off it took 20 ms. The first such write after a restart is still
validated in full, because the typing is not persisted, unless the server was started
with `--validate`.

**ShExR schemas in named graphs** landed on 2026-10-02, as §10's Phase 3 item described
them. The configuration's `schema.graphs` names graphs of the dataset that hold the
schema in ShExR, instead of a copied file.

* The schema is read from the state being validated. The union of the named graphs'
  triples is parsed as ShExR, checked and compiled, and the shape map is parsed against
  it. A configuration's `schema.prefixes` stand in for the prefixes a graph cannot hold,
  so the map may use them. Nothing is copied into the database, and imports are refused
  because a write never fetches anything.
* The schema graphs are never part of the data graph, and a `dataGraph` list that names
  one is refused.
* A write that changes a schema graph reads the schema it leaves and validates the state
  after it in full, with the fallback reason `schema`. In grandfather mode the state
  before is judged under the old schema, as SHACL compares a shapes change with the old
  shapes' results. The new schema, its shape map and what it reads replace the old ones
  when the write commits, together with the typing of the full validation. A write whose
  schema does not parse, check or define the map's labels is rejected with the reason
  in `shapesError`, in `warn` mode too.
* `sparkles validation` gained `--schema-graph IRI` and `--schema-prefix PREFIX=IRI`, and
  a restart reads the schema from the graphs again.

To swap a schema at commit time, the guard keeps its compiled schema, map, read
predicates and incremental plan together behind a lock, and a pending write carries the
new set. The property test of incremental validation now also runs with the schema in a
graph, and passed 1,500 cases with the final code. `guard::tests::schemas_in_graphs`
covers a schema change, a rejected change and a restart,
`http::validation::shex_tests::schema_in_a_graph` the HTTP side, and
`tests/cli_validation.rs` the CLI.

**Deferred or rejected.** SHACL and ShEx guards on one dataset and a schema in graphs
merged with a file are not built. A persisted typing and ShEx 2.2 (`EXTENDS`,
`ABSTRACT`) were built on 2026-10-13, as the first of the roadmap follow-ups below
records.

### Roadmap follow-ups (2026-10-12)

**ShEx 2.2 and a persisted typing** (item 13, Tier 2, [ROADMAP](../../ROADMAP.md)).
*Deferred or rejected* above lists both.

* **ShEx 2.2.** The language adds `EXTENDS` (a shape that includes another's
  constraints, with the extended shape's triple expression matched on its own share of
  the neighbourhood) and `ABSTRACT` (a shape that a node conforms to only through a
  non-abstract extension). The parser reads both in ShExC, ShExJ (`extends`,
  `abstract`) and ShExR, and the compiler checks that extensions are acyclic and that
  an abstract shape is never the target of a shape map entry. The validator matches an
  extending shape by splitting the node's arcs between the shape's own triple
  expression and each extended one, the partition the 2.2 draft specifies, and a
  reference to an abstract shape succeeds when the node conforms to one of its
  extensions. *Typing-change propagation* above treats an extended shape as referred
  to by its extensions. The jena-shex copy of shexTest has no 2.2 cases (Open question
  11); the tests come from the upstream shexTest repository's 2.2 cases where they
  exist, and from the draft's examples otherwise, each listed with its source.
  Acceptance: shexTest's 2.1 results stay at 99.9 %, and every 2.2 case adopted passes
  or is listed with a reason.
* **Persisting the typing.** The head's typing lives in memory and is dropped by a
  restart and by a compaction, which renumbers terms, so the first write after either
  is validated in full (*Typing-change propagation* above). The guard writes the typing
  to `validation/typing.bin` in the dataset's directory when the store syncs before a
  generation switch and when it closes: a header with the commit, the generation, the
  hash of the compiled schema and shape map, then the pairs as (node, shape label,
  value, failed-in-discovery) with nodes as vocabulary keys, not ids, so a compaction
  does not invalidate them. At open, the guard reads the file when the commit, the
  schema hash and the shape map match the head, and maps the keys to the new ids;
  any mismatch discards it and the next write runs in full as now. A commit the guard
  did not judge still drops the typing. Acceptance: with `foaf:knows @ex:Person *` at
  1.05M triples, the first person write after a restart takes the same 16 to 28 ms as
  the next ones, against a full validation of 2.1 to 2.7 s before.

**Outcome of the ShEx 2.2 and persisted typing work (2026-10-13).**

*ShEx 2.2.* `ABSTRACT` and `EXTENDS @label` are read and written in ShExC, ShExJ
(`abstract`, `extends`) and ShExR (`sx:abstract`, `sx:extends`). `RESTRICTS` is still a
syntax error. The `shex22` directory under `crates/sparkles-shex/tests` holds the 2.2
cases of the upstream shexTest repository, and `tests/shextest.rs` runs every group over
the Jena copy and over them. The harness now also reads the upstream layout, so it runs
against a clone of shexTest too.

* The compiler (`check.rs`, `compile.rs`) rejects an extension cycle, an `EXTENDS` target
  that is not defined, and a reference to an abstract shape that nothing concrete
  extends. A reference to a shape reaches every shape that extends it. The shapes of a
  declaration that has a parent become an `ExtPlan` of frames and parts, where each part
  is one ancestor's own shape together with the other conjuncts of its declaration.
* The evaluator (`ext.rs`) reads all outgoing arcs of the node and the incoming arcs of
  every inverse predicate the schema uses. It searches the ways to give each arc to one
  part, matches the triple expression of each part against its own arcs, and checks the
  other conjuncts of an ancestor against the arcs of the part and of its ancestors. An
  arc that no part takes may remain only when the extending shape lists its predicate
  in `EXTRA` and the arc satisfies no triple constraint on it. When the typing is not yet
  known for the arc's value, leaving the arc over is undecided and not a success.
  `CLOSED` forbids an outgoing arc that no part can match. The search spends the
  `max_partitions` budget once per candidate assignment and once per abandoned option,
  and running out of it is an error, never a false result. The search keeps its own stack, so a node with tens of
  thousands of arcs does not exhaust the thread's stack, and it checks the timeout and
  cancellation as it goes. Failures of an extending shape are reported as one `NoMatch`.
* The write guard treats an extending shape as reading every outgoing arc of its nodes,
  so no write is skipped for it, and it validates in full instead of incrementally.
* *Results.* On the 2.2 cases validation passes 100 of 100 from ShExC and from ShExJ, and
  77 of 77 from ShExR, where 23 cases have no ShExR form. Representation, ShExR reading
  and ShExR writing pass 22 of 22 each, syntax 25 of 25, negative structure 4 of 4 and
  negative syntax 1 of 1. The 2.1 numbers of the Jena copy are unchanged. On the
  upstream suite every `EXTENDS` and `ABSTRACT` validation case passes. The remaining
  upstream failures are not about 2.2 and were not adopted, namely float and double
  infinities, the print format of semantic actions, annotations on node constraints,
  relative shape labels, `includeWithCardinality` and a duplicate-label negative syntax
  test.
* *Limits.* The `EXACTLY` reference syntax is not read. The 2.2 draft's extension
  coherence rule and its rule that `EXTENDS` appears only in extendable expressions are
  not enforced, because upstream cases (`ExtendsRepeatedP`, `extends-abstract-or`)
  contradict them. A declaration that is extended must be a shape or a conjunction with a
  shape, and the compiler says so otherwise. A declaration that is a disjunction offers
  one frame per disjunct, at most 64 frames. Only the extending shape's own `EXTRA`
  applies to the arcs that no part takes. The ShExR writer wraps a declaration in
  `sx:ShapeDecl` only when it is abstract or a bare reference.

*Persisted typing.* The guard keeps the typing of the head across a restart and across a
compaction. It differs from the design above in where and when it writes.

* The file is `validation-typing.bin` in the database directory, next to
  `validation-status.json` and outside the list of files a clone or a backup copies. The
  store calls a new `CommitGuard::close` hook when it is dropped cleanly, under the
  writer lock, and the guard writes the file there when the typing is that of the head.
  The file is not written at a generation switch.
* A compaction calls a new `CommitGuard::rekey` hook after it publishes the new
  generation, still holding the writer lock. The guard translates the typing's nodes to
  the new generation's ids in memory, by vocabulary key, and drops the typing if it
  cannot. This avoids a file round trip, and a failed compaction loses nothing.
* The header names the dataset, the head commit with its time and quad count, the SHA-256
  of `validation.json` and a fingerprint of the compiled schema's pair kinds. The
  fingerprint also covers the SHA-256 of the schema copy, so an edited copy never takes
  the typing of the schema it replaced. A SHA-256 of the whole file ends it. That digest
  detects damage and not tampering. The file is trusted like the rest of the database
  directory, because anyone who can write it can also write `validation.json` or the
  schema copy. Nodes are stored as the store's vocabulary key, or
  as the raw id for inline values and blank nodes, whose ids come from their labels. A
  term the store no longer has is dropped with its pairs, which a changed store can
  only make smaller. At install the guard reads the file when every field matches the
  head and leaves it in place.
* The first commit, bypass or schema write after an open removes the file, so a head
  that moved on can never be mistaken for the one in the file. Replacing or removing the
  configuration removes it too. A guard whose schema is in graphs, and an in-memory
  dataset, keep no file.
* *Memory and speed.* The typing was in memory already after the first full validation,
  so resident memory is the same once the file is read. While it is read, the file's
  bytes are held as well, about the size of the typing, and freed at once. Closing the
  store costs one write of that size. The first write after a restart no longer
  validates the dataset in full. The timings of the acceptance example, 16 to 28 ms
  against 2.1 to 2.7 s, were not measured again at 1.05M triples. The test
  `guard::tests::the_typing_survives_a_restart` checks a restart, a compaction, a
  changed file, a file of an older commit and a replaced configuration.
* A process that exits without dropping the store, or one that is killed, leaves no file
  or an old one that is never trusted, and the first write validates in full as before.
* The file has a magic number and a format number of its own, and it is registered in the
  file-format registry that [X12](../X12-release-engineering.md) adds as the derived kind
  `validation-typing`. A file of a version this build does not read is discarded and the
  typing is rebuilt by a full validation, which is logged.
* *Limitation.* The schema copy's own digest in `validation-status.json` is not checked
  when the guard loads, which is older than this work. The typing file no longer relies on
  it, since it carries a digest of the copy it was written for. The gap is recorded under
  the follow-ups of [C10](../C10-write-time-validation.md).

**Guards, schema sources and CLI controls** (item 54, Tier 5,
[ROADMAP](../../ROADMAP.md)).

* **SHACL and ShEx guards on one dataset.** `validation.json` holds one `language`
  (`GuardLanguage` in `crates/sparkles-core/src/guard.rs`). It becomes a list of
  guards, each with its own language, schema, mode and baseline; the single-guard form
  stays valid and is read as a list of one. A write runs each guard on the same pending
  state, in the listed order, and is rejected if any `reject` guard rejects; the
  report holds one section per guard. The incremental paths are per guard, and a bulk
  write's fallback applies to each. Metrics already carry the language label.
* **A schema in graphs merged with a file.** `schema.graphs` and a copied schema file
  exclude each other today. Both may be given: the file's ShExC or ShExJ is compiled
  with the graphs' ShExR as one schema, labels from either may refer to the other, and
  a label defined in both is an error at configuration time and at any write that
  changes a schema graph. Imports stay refused for the graph part only.
* **CLI outbound controls.** `sparkles shex validate` builds its import policy in
  `shex_cmd.rs` with `allow_private: true` and the default outbound settings, and the
  Outcome lists the `--outbound-*` flags as open. The command flattens the
  server's `OutboundArgs` (`crates/sparkles-server/src/outbound.rs`:
  `--outbound-allow-private` or `--outbound-block-private`, `--outbound-allow`,
  `--outbound-timeout`, `--outbound-max-mb` and the per-request limits) and builds its
  import policy from them. The local default stays `allow_private: true`, so existing
  invocations keep working, and `--outbound-block-private` refuses imports from
  private addresses.
