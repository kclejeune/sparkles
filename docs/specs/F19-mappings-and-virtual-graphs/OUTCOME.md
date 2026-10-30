# F19: Mapping languages and virtual graphs — Delivery record

Current delivery status is summarized in the [landing page](README.md).
The [design Outcome overview](SPEC.md#outcome) links the supported scope to this record.
The recorded dates and measurements below describe their original delivery stages.

## Historical opening status

> **Status:** implemented in part, most of Phase 1.
>
> **Phases:** Materialized R2RML, RML and YARRRML loading over CSV, TSV, JSON, local
> Parquet and SQLite, CSVW key checks, Tarql templates and stored mappings are available.
> PostgreSQL and DuckDB sources, some mapping expressions and administration surfaces
> remain. Phase 2's virtual SQL, Parquet and Iceberg graphs remain planned.
>
> **User docs:** [Mappings](../../API.md#stored-mappings),
> [usage](../../USAGE.md#mappings-r2rml-rml-and-yarrrml) and
> [features](../../storage/README.md#storage-and-indexes).
>
> This is the design as written before implementation. The [Outcome](SPEC.md#outcome) records
> what shipped and the remaining work.

## Outcome

**Phase 1 partly built** on 2026-10-24 for roadmap item 22. Everything in Phase 1 is
built except the PostgreSQL and DuckDB sources, the UI's Mappings page and runs that
become tasks, which are roadmap items 230, 233 and 234. Phase 2, the virtual graphs, is
roadmap item 43.

* **Engine.** `crates/sparkles-core/src/mapping/` holds the model of §3.1 (`model.rs`),
  the R2RML, rml.io and RML-Core reader (`rdf.rs`), the YARRRML translator
  (`yarrrml.rs`, over `yaml-rust2`), R2RML string templates with the IRI-safe encoding
  and RML-Core's URI-safe one (`template.rs`), a JSONPath subset of RFC 9535
  (`jsonpath.rs`), the functions (`functions.rs`), the plan and the term generation
  (`exec.rs`) and the sources (`source/`). `sparkles::mapping::write` streams quads to
  any writer, and `to_nquads_file` writes them to a temporary N-Quads file that the
  loader then reads as one source, so a failed run commits nothing.
* **Sources.** Each source is read once for all the triples maps that share it, row by
  row through a callback. CSV and TSV go through C05's reader and dialects, and a CSVW
  description named `FILE-metadata.json` next to the file gives the columns datatypes and
  null values. JSON is parsed whole up to `--max-json-mb` (512 MiB by default), and JSON
  Lines is read a line at a time. Parquet is read a row group at a time through the
  `parquet` crate's record API, without Arrow, behind the `mapping-parquet` cargo
  feature. SQLite is read through `rusqlite` with the bundled SQLite, behind the
  `mapping-sqlite` feature, opened read-only with `query_only` set and a progress
  handler that stops a statement at the deadline or on cancellation. Both features are
  in the server's default build and in the `full` feature of the `sparkles` crate. The
  Python wheel leaves them out.
* **Natural types.** Values keep the types their source gives, as R2RML's natural
  mapping says. SQLite columns take theirs from the declared type, Parquet from the
  physical and logical types, JSON from the value, and CSV from the CSVW description.
  Decimals are written in canonical form (`5.0`), and doubles in R2RML's (`8.025E1`).
* **Joins.** A referencing object map whose child and parent read the same rows joins
  within the row. Two SQLite tables of one connection join in one statement, `SELECT …
  FROM (child) AS c JOIN (parent) AS p ON …`, which `sparkles map check` prints. Any
  other join reads the parent into a hash table charged to the run's memory budget, and
  past the budget the run fails with the join's name and the parent's row count. The
  command line runs without a budget, and the server charges the query memory budget.
* **Functions.** RML function maps and YARRRML `function` entries call 48 functions.
  28 are GREL and IDLab functions, with the IRIs and parameter predicates of their
  published FnO descriptions. The other 20 are Sparkles' own
  `urn:x-sparkles:fn:KEYWORD` functions, which apply SPARQL 1.1's string, hash and
  rounding functions to `argN` parameters. §7.3 asked for every SPARQL function by its
  keyword, and the rest of them are roadmap item 236. An unknown function fails when the
  mapping is read and suggests a CONSTRUCT template. `sparkles map check --functions`
  lists them.
* **Command line.** `sparkles map run`, `check`, `convert` and `draft` follow §2.1, and
  `put`, `get`, `list`, `delete` and `run` manage the mappings stored with a database
  (`--loc`) or on a server (`--server` and `--dataset`). `sparkles load --mapping` takes
  every mapping kind, with `--source`, `--connection`, `--csvw-mode`, `--csvw-check`,
  `--lookup` and `--max-json-mb`. `sparkles csv convert` takes the CSVW options and
  leaves engine mappings to `sparkles map run`.
* **CSVW.** `tabular::Options.standard` adds the table group, table and row
  descriptions of §6.1, and the metadata's notes and common properties as JSON-LD
  values. `tabular::KeyRegistry` keeps the primary and foreign key values of a load
  within its budget, and `tabular::key_order` orders the tables by their foreign keys,
  so each referencing table is checked in one pass after the tables it references. A
  cycle of foreign keys or a reference to a table the load lacks fails before anything
  is read.
* **Templates.** `tarql:expandPrefix` and `tarql:expandPrefixedName` resolve prefixes
  of the template, then those of the dataset. A file may hold several CONSTRUCT
  queries, and each runs for every batch. `apf:strSplit` is tested in a template. A
  lookup reads the snapshot of the head when the conversion starts, under the caller's
  view, and the table's report gives `lookupCommit`.
* **Stored mappings and the server.** `crates/sparkles-core/src/stored_mappings.rs`
  keeps `mappings.json`, format 1 in `formats::SETTINGS`, which a reader refuses when it
  is newer. Clones and backups copy it. The routes of §2.2 are in
  `crates/sparkles-server/src/http/mappings.rs`, the Graph Store's table bodies in
  `http/gsp_csv.rs`, and the `connections` settings kind in `settings/connections.rs`.
  A stored run in replace mode replaces every graph its constant graph maps name in one
  commit through `ReplaceTarget::Graphs`, a new variant of `Store::replace`. The library
  has `Dataset::mappings`, `StoredMappings::run` and `Dataset::run_mapping`, and Python
  has `Dataset.mappings` and `Dataset.run_mapping`. The JVM and Node.js bindings also
  expose stored mapping handles and direct mapping runs. Their default builds do not
  enable SQLite or Parquet sources. Native Rust `Dataset::run_mapping_with` accepts
  complete `WriteOptions` through conversion and the final load, and
  `StoredMappings::run` applies the cancellation sources and deadline of its write
  options to conversion in the same way. The JVM and Node.js direct runs call
  `run_mapping_with`, and their stored runs pass the operation's cancellation and
  absolute deadline as write options. The original `run_mapping` signature and
  exhaustive `RunOptions` constructions remain compatible, and the opaque callback
  retains its conversion-only scope. Python mapping runs take no cancellation token
  or timeout, which is roadmap item 404.

Decisions this spec left open, made during implementation:

* The command line parses a mapping file with the file's URL as the base of its own
  relative IRIs, and stored and uploaded mappings with `urn:x-sparkles:mapping`. Only a
  document's `@base`, a triples map's `rml:baseIRI` or `--base` sets the base of
  generated IRIs. A triples map's `rml:baseIRI` wins, as RML-Core requires, and
  `--base` wins over the document's `@base`.
* An R2RML logical table reads the default connection, which is the only connection or
  the one named `default`. `convert` prints that connection as
  `urn:x-sparkles:connection:default`.
* A `d2rq:Database` names a connection by its `d2rq:jdbcDSN`, which a connection's
  `jdbcDsn` declares, and a mapping that carries `d2rq:username` or `d2rq:password` is
  refused, since a mapping never holds credentials.
* The `connections` kind holds one member, `connections`, the object of §4.4, so that it
  merges and locks as the channels of `notifications` do. A SQLite path must be
  absolute.
* YARRRML's join parameters are `str1` for the child and `str2` for the parent, or the
  `s` and `o` of a three-element list. A source list or a subject list expands into one
  triples map per entry. `a` takes IRIs, and the predefined prefixes of the YARRRML
  specification apply.
* The blank node label of a value is `m` and the value escaped, so equal values give one
  node within a run. A blank node subject map without a value gives a new node per row,
  labelled by its triples map and the row's number in its source. A join that reads the
  parent's subject again gets the same node. A hash join reads the parent's source with
  the same statement as the parent's own scan, so both reads see the rows in the same
  order. Such a join is never pushed into SQL, whose joined rows have no row number of
  the parent's source, so two SQL tables whose join has a blank node per row on either
  side join in memory, comparing values as strings rather than with the database's
  collation.
* A term map generates at most 100,000 values for one row, and a join probes or indexes
  at most 100,000 key combinations for one row. Multi-valued JSON references multiply,
  and past the bound the run fails with the triples map and the row. Term maps nest at
  most 32 deep through datatype, language and function maps, so a term map that refers
  to itself fails when the mapping is read.
* A triples map whose graph maps generate no graph for a row generates no triple for
  that row. The default graph applies only when no graph map is declared.
* A JSON number without a fraction or exponent is an `xsd:integer` with its exact
  digits. Any other number is an `xsd:double`, and serde_json prints it in its own form,
  so `1e0` becomes `"1.0"^^xsd:double`. §4.2 asked for the source's lexical form, which
  roadmap item 239 adds.
* In CSVW standard mode the tables of one load are written into one N-Triples file, so
  a table group without an `@id` is one blank node for all its tables.
* A template lookup that binds a blank node of the dataset writes a new blank node,
  because the loader scopes blank node labels per file. Roadmap item 238 keeps the
  dataset's nodes.
* A Graph Store request's body is the file its mapping reads, named by `source=NAME` or
  the mapping's only file source. A `DELETE` with a body removes the mapped triples and
  cannot remove triples with blank nodes. A request on the default graph refuses a
  mapping whose graph maps name a named graph.
* `POST /{ds}/upload` now honours `If-Match` for every upload, as §7.2 assumed it did.
* A run of a stored CSVW or template mapping on the server takes the tables as file
  parts. The library and the command line run stored R2RML, RML and YARRRML mappings,
  and load tables with CSVW and template mappings given as files.

Tests and conformance:

* The W3C R2RML test cases run on SQLite from `testsuite/mapping/r2rml`, vendored from
  `kg-construct/r2rml-test-cases-support` at commit 976098802e9b (2022-01-13) under the
  W3C Software and Document License and Apache-2.0. 59 of 62 pass. R2RMLTC0002f expects
  case-sensitive identifiers that SQLite does not have, R2RMLTC0015b expects a check of
  language tags against the subtag registry, and R2RMLTC0018a expects `CHAR` padding.
* The RML-Core test cases with JSON and CSV sources run from `testsuite/mapping/rml-core`,
  vendored from `kg-construct/rml-core` at commit 82ab28d46803 (2026-04-20) under
  CC-BY-4.0. 70 of 76 pass. RMLTC0027b expects IRIs that oxrdf refuses, and RMLTC0030b to
  RMLTC0030f use join conditions written as expression maps, which roadmap item 232 adds.
* `crates/sparkles-core/src/mapping/tests.rs` covers A1 to A6 on SQLite, CSV, JSON and
  Parquet, conversion round trips, refusals, functions and a 200,000-row run that
  streams. `tabular/tests.rs` covers A7 to A9. `tests/cli_map.rs` runs the command line,
  `http/router_tests/mappings.rs` covers the routes, A11 and the `connections` kind, and
  `crates/sparkles-py/tests/test_mappings.py` covers Python. The Node administration
  and JVM structured administration tests cover mapping revisions, runs and refusals.
  `crates/sparkles/tests/mapping_stored_control.rs` checks that a cancelled or expired
  stored run refuses before conversion in add and replace modes, that a stop during
  conversion takes effect at the next conversion checkpoint and commits nothing, and
  that an unstopped run publishes and records its run.
  The Node administration tests check that the native timeout reaches the write
  options of both mapping runs, that the package timeout refuses both runs without a
  commit, and that both runs time out at writer admission while another transaction
  holds the writer.
  `crates/sparkles/tests/mapping_native_control.rs` covers native conversion checks
  and bounds, every supplied cancellation source, actual final writer waits and
  unchanged state on refusal. Writer-acquisition and committed callbacks latch the
  exact phases for cancellation and deadline checks. The native tests retain
  preconditions, graph authority, previews, commit messages, old exhaustive option
  literals and successful receipts after publication.
* The W3C CSVW test suite is not vendored, which is roadmap item 235.

Measurements:

* On 2026-10-24 on atlas, an Intel i5-13500 shared with other work, a YARRRML mapping
  of a 1,000,000-row CSV table with four columns wrote the same 4,000,000 triples as
  C05's default mapping in 1.61 s against 2.81 s, with peaks of 41 MB and 39 MB. Loaded
  into a new database, the mapping took 2.88 s and the C05 conversion 4.12 s. Each figure
  is the median of three rounds of the release binary. The mapping is faster, so no
  follow-up item was needed.
* A Parquet file of 10,000,000 rows and two columns, 227 MB, mapped to 20,000,000
  triples in 11.6 s with a peak of 46 MB, read one row group at a time.

The A10 refresh, the R2RML cases on PostgreSQL and the BSBM comparison wait for the
PostgreSQL source and Phase 2.
