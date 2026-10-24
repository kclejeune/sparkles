# F19: Mapping languages and virtual graphs

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 is materialized loading through R2RML, RML and
> YARRRML mappings over CSV and TSV, JSON, SQL databases and Parquet, CSVW's standard
> mode with primary-key and foreign-key checks, Tarql's functions, CONSTRUCT templates
> that read the target dataset, mappings stored on the server and CSV bodies in the Graph
> Store protocol. Phase 2 is virtual graphs: SPARQL over SQL databases, Parquet files and
> Iceberg tables through the same mappings, without loading, by unfolding queries into
> SQL.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

This design was written from the W3C Recommendation "R2RML: RDB to RDF Mapping Language"
(2012) and the W3C "R2RML and Direct Mapping Test Cases" note, the RML specification
published at rml.io and the RML-Core and related module drafts of the W3C Knowledge Graph
Construction Community Group, the YARRRML specification, the W3C CSV on the Web
Recommendations (2015), A. Dimou et al., "RML: A Generic Language for Integrated RDF
Mappings of Heterogeneous Data Sources" (LDOW 2014), P. Heyvaert et al., "Declarative
Rules for Linked Data Generation at your Fingertips!" (ESWC 2018 demos), D. Calvanese et
al., "Ontop: Answering SPARQL queries over relational databases" (Semantic Web Journal,
2017), M. Rodríguez-Muro and M. Rezk, "Efficient SPARQL-to-SQL with R2RML mappings"
(Journal of Web Semantics, 2015), D. Lanti et al., "The NPD Benchmark: Reality Check for
OBDA Systems" (EDBT 2015), the public specifications of Apache Arrow, Apache Parquet and
Apache Iceberg, Tarql's public documentation, and the Sparkles code and specs. Ontop is
Apache-2.0, and it is described here from its papers and documentation. No mapping
engine's code was read. §16 lists the sources.

It builds on CSV and TSV imports ([C05](C05-tabular-imports.md)), budgets
([C01](C01-observability-and-budgets.md)), access control
([C09](C09-dataset-access-control.md), [C12](C12-graph-access-control.md),
[C12b](C12b-triple-access-control.md)), write previews ([C15](C15-write-previews.md)),
stored queries ([C16](C16-stored-queries.md)), layered settings and secrets
([C19](C19-layered-settings.md)), the SERVICE enhancer ([G10](G10-service-enhancer.md))
and point-in-time reads ([F06](F06-snapshots-and-point-in-time.md)).

## 1. Summary, goals, non-goals

C05 converts one CSV or TSV table at a time, through a default mapping, CSVW metadata or
a Tarql-style CONSTRUCT template. It left out the mapping languages that data
integration projects already use: R2RML for relational databases, RML for files in
several formats, and YARRRML, a YAML syntax that compiles to RML. A user who arrives
with an R2RML mapping maintained for Ontop, Morph or a commercial store has to rewrite it
for Sparkles today, and data held in PostgreSQL or Parquet has to be exported to CSV
first.

This spec adds one mapping engine with three front ends (R2RML, RML and YARRRML) and
several sources, and uses it in two ways.

1. **Phase 1, materialized.** The mapping runs once, its triples are written to a
   temporary N-Quads file, and the loader commits them, as C05 does with a table. A
   stored mapping can run again later and replace the graphs it wrote.
2. **Phase 2, virtual.** A stored mapping over SQL, Parquet or Iceberg sources is
   declared as a virtual graph. Queries that name the graph are unfolded through the
   mapping into SQL, which runs in the source, and the rows come back as terms that join
   with the stored data.

Phase 1 also finishes the C05 items that its Outcome lists as not built: CSVW's standard
mode, primary-key and foreign-key checks, Tarql's extension functions, templates that
read the target dataset, mappings stored on the server and CSV in the Graph Store
protocol.

**Goals**

1. `sparkles load --mapping m.ttl` and `sparkles map run m.ttl` read R2RML, RML (both the
   rml.io vocabulary and the RML-Core vocabulary) and YARRRML, and produce the same
   triples from the same mapping.
2. Sources: CSV and TSV through C05's reader and dialects, JSON and JSON Lines with
   JSONPath iterators, PostgreSQL, SQLite and (behind a feature) DuckDB through SQL, and
   Parquet files.
3. Joins between triples maps (`rr:parentTriplesMap` with `rr:joinCondition`) work
   across sources, and run in the database when both sides are SQL on the same
   connection.
4. Conformance: the R2RML test cases and the RML-Core test cases pass, or are excluded
   for a listed reason.
5. Memory does not grow with the number of rows of a source, except for the parent side
   of a join, which is bounded by the query memory budget and fails with a clear message
   when it does not fit.
6. Phase 2: a SPARQL query over a virtual graph returns the same solutions as the same
   query over the materialized mapping, for every query of the test lists in §13.
7. Phase 2: on BSBM's relational form, virtual queries are within 1.5× of Ontop's times
   on the same PostgreSQL database for the explore mix, measured by §13.2.

**Non-goals**

- XML sources and XPath iterators. RML defines them, and no Rust XPath engine of
  sufficient coverage was found. They are open question 1.
- The W3C Direct Mapping. `sparkles map draft` (§2.1) prints an R2RML mapping in the
  direct mapping's shape, which a user edits. That covers its practical use.
- Writes to virtual graphs. SPARQL Update against a virtual graph fails (§9.10).
- Ontology-based query rewriting over virtual graphs (OWL 2 QL, as Ontop does). Reasoning
  stays with [C08](C08-inference-freshness.md) and the query-time work of F17.
- Sources fetched over HTTP during a mapping. Every file source is local to the CLI or
  uploaded with the request, and every database is a named connection (§4.4).
- MySQL, SQL Server and Oracle. The SQL printer is per dialect, and each dialect needs its
  own test database. They are open question 2.

## 2. User-visible behaviour

### 2.1 Command line

```
sparkles map run      m.ttl [--source NAME=PATH]… [--connection NAME=URL]… [--format nq|nt|ttl] [--output FILE]
sparkles map check    m.ttl                  # parse, list triples maps, sources and joins, check references
sparkles map convert  m.yaml --to rml        # YARRRML to RML Turtle (or --to r2rml when every source is SQL)
sparkles map draft    --connection NAME=URL [--schema public] [--tables a,b]   # R2RML in the direct mapping's shape
sparkles load --loc DB --mapping m.ttl [--graph G] [--source NAME=PATH]…
sparkles load --server URL --dataset ds --mapping m.ttl     # converts locally, as C05 does
```

The kind of a mapping file is read from its content, not its name. A Turtle, TriG or
N-Triples file with a resource typed `rr:TriplesMap` or `rml:TriplesMap`, or with
`rr:logicalTable`, `rml:logicalSource` or the RML-Core equivalents, is R2RML or RML. A
YAML file with a `mappings` key is YARRRML. A JSON file with `@context` naming CSVW is
CSVW metadata, and a SPARQL CONSTRUCT is a template, both as in C05. `--mapping-kind`
overrides the guess.

`--source NAME=PATH` binds a logical source to a file when the mapping's `rml:source`
names a file that is not next to the mapping, and `--connection NAME=URL` gives a
database URL, `postgres://…`, `sqlite:PATH` or `duckdb:PATH`. A password never goes on
the command line. It is read from `PGPASSWORD` or a prompt with echo off, as
`sparkles secrets set` reads a value.

### 2.2 Uploads and stored mappings

`POST /{ds}/upload` accepts a `mapping` part holding R2RML, RML or YARRRML, next to the
file parts that its logical sources name. A source is matched by file name, as C05
matches the tables of a CSVW table group. A mapping whose source is a database needs a
connection declared on the server (§4.4), and a mapping whose file source has no part
fails with `400 mapping-source-missing` before anything is read.

Mappings can be stored per dataset, in `<db>/mappings.json`, with versions, as
[C16](C16-stored-queries.md) stores queries.

| Method | Path | Result |
|---|---|---|
| GET | `/$/mappings/{ds}` | The stored mappings: name, kind, sources, target graphs, versions, last run. |
| PUT | `/$/mappings/{ds}/{name}` | Stores a mapping. The body is the mapping text, with `Content-Type` naming its kind (`text/turtle`, `application/yaml`, `application/csvm+json`, `application/sparql-query`). |
| GET | `/$/mappings/{ds}/{name}` | The mapping text and metadata. `?version=N` reads an older version. |
| DELETE | `/$/mappings/{ds}/{name}` | Deletes it. A mapping that a virtual graph uses cannot be deleted (`409 mapping-in-use`). |
| POST | `/{ds}/mappings/{name}/run` | Runs it and commits (§5.5). Takes multipart file parts for file sources, `mode=add|replace`, `dryRun`, and `Sparkles-Commit-Message`. Runs longer than 10 s become tasks of `/$/tasks`. |

Storing a mapping needs `admin` on the dataset, because a mapping names connections and
can read any table those connections can. Running one needs `write`, and the commit is
subject to the caller's graph grants as any write is. `POST /{ds}/upload` and the Graph
Store protocol take `mapping=NAME` to use a stored mapping instead of a part.

`sparkles map` gains `--server URL --dataset ds` forms of `put`, `get`, `list`, `delete`
and `run`.

### 2.3 CSV in the Graph Store protocol

`PUT`, `POST` and `DELETE` with a body on `/{ds}/data?graph=G` accept `text/csv` and
`text/tab-separated-values` when the request names a stored mapping with `mapping=NAME`
or a default mapping with `base` (and optionally `key`). `PUT` replaces graph G with the
mapping's triples, through `Store::replace` with `ReplaceTarget::Named`, as a `PUT` of
Turtle does. `POST` adds them. The triples go into G whatever graph maps the mapping
declares, and a mapping with a graph map other than the default graph is refused with
`400 mapping-graph-conflict`, because a Graph Store request addresses one graph. A body
without `mapping` or `base` is `415`, which is the answer the endpoint gives today to
any body it cannot read as RDF.

### 2.4 Virtual graphs (Phase 2)

A virtual graph is a named graph IRI bound to a stored mapping whose sources are all
SQL, Parquet or Iceberg sources.

```
PUT /$/virtual/{ds}/{name}
{ "graph": "urn:x-sparkles:virtual:orders", "mapping": "orders",
  "cache": { "ttlSeconds": 0 }, "consistency": "per-query" }
```

A query reaches it by naming its graph: `GRAPH <urn:x-sparkles:virtual:orders> { … }`,
`FROM <…>`, or `FROM NAMED <…>` with `GRAPH ?g`. A virtual graph is never part of the
default graph, the union default graph or a `GRAPH ?g` over the dataset's own named
graphs, so a query that does not name it never sends SQL anywhere. The SPARQL Service
Description of the dataset lists the virtual graphs as named graphs with an
`spk:virtual true` marker, and `/$/schema/{ds}` lists their classes and predicates from
the mapping, not from the data.

## 3. The mapping model

### 3.1 One intermediate form

The three languages are read into one intermediate form in `sparkles::mapping::model`.

```rust
pub struct Mapping { pub maps: Vec<TriplesMap>, pub sources: Vec<SourceDecl>, pub base: Option<String> }
pub struct TriplesMap {
    pub id: String,                          // the map's IRI or YARRRML name, for messages
    pub source: SourceRef,                   // index into sources, plus an iterator
    pub subject: TermMap, pub classes: Vec<NamedNode>,
    pub graphs: Vec<TermMap>,                // empty: the default graph
    pub predicate_objects: Vec<PredicateObject>,
}
pub struct PredicateObject { pub predicates: Vec<TermMap>, pub objects: Vec<ObjectMap>, pub graphs: Vec<TermMap> }
pub enum ObjectMap { Term(TermMap), Join { parent: usize, conditions: Vec<(Reference, Reference)> } }
pub struct TermMap { pub value: Value, pub term_type: TermType, pub datatype: Option<TermMapOrIri>,
                     pub language: Option<TermMapOrString> }
pub enum Value { Constant(Term), Reference(Reference), Template(Template), Function(FunctionCall) }
pub enum TermType { Iri, BlankNode, Literal }
```

`Reference` is a column name for CSV and SQL, a JSONPath expression relative to the
iterator for JSON, and a column path for Parquet. `Template` is R2RML's string template,
whose `{name}` parts are references and whose `\{` and `\\` are escapes. It is not an
RFC 6570 template, so it is kept apart from the C05 `uritemplate` module.

### 3.2 R2RML

The R2RML vocabulary is read as the Recommendation defines it: `rr:logicalTable` with
`rr:tableName` or `rr:sqlQuery` (and `rr:sqlVersion`, accepted and ignored), subject,
predicate, object and graph maps with `rr:constant`, `rr:column` or `rr:template`, the
constant shortcuts `rr:subject`, `rr:predicate`, `rr:object` and `rr:graph`,
`rr:termType`, `rr:datatype`, `rr:language`, `rr:class`, `rr:parentTriplesMap` with
`rr:joinCondition`, `rr:child` and `rr:parent`, and `rr:defaultGraph`.
`rr:inverseExpression` is read and used only by Phase 2's template inversion (§9.3).

These rules from the Recommendation govern the output.

- A term map whose referenced column is NULL in a row generates no term, and no triple
  that needs the term.
- The default term type is `rr:IRI` for subjects, predicates and graphs, and for objects
  generated by `rr:column` it is `rr:Literal`. A template object is an IRI by default.
- In an IRI template, each value is percent-encoded with the IRI-safe encoding of the
  Recommendation before it is substituted. A literal template substitutes the plain
  value.
- A column value without `rr:datatype` becomes a literal by the natural mapping of its
  SQL type: integers to `xsd:integer`, `DECIMAL` and `NUMERIC` to `xsd:decimal`, `REAL`,
  `FLOAT` and `DOUBLE PRECISION` to `xsd:double`, `BOOLEAN` to `xsd:boolean`, `DATE` to
  `xsd:date`, `TIME` to `xsd:time`, `TIMESTAMP` to `xsd:dateTime`, binary types to
  `xsd:hexBinary`, and everything else to a plain string, in the canonical lexical form.
- A referencing object map without join conditions, whose parent has the same logical
  table, takes the parent's subject from the same row.
- An IRI that is not absolute after resolution against the mapping's base, a literal
  that is not valid for its declared datatype, or a language tag that is not well
  formed, is a data error. R2RML lets a processor report data errors. Sparkles fails the
  run with the triples map, the row and the term map, for the reason C05 §3.3 gives.

### 3.3 RML

RML generalizes the logical table to a logical source with a source, a reference
formulation and an iterator. Sparkles reads both vocabularies in use.

- **The rml.io vocabulary** (`http://semweb.mmlab.be/ns/rml#` with `ql:` reference
  formulations): `rml:logicalSource`, `rml:source`, `rml:referenceFormulation` with
  `ql:CSV` or `ql:JSONPath`, `rml:iterator` and `rml:reference`, used inside R2RML's term
  maps. `rml:source` is a file path string, a `csvw:Table` description with a
  `csvw:dialect`, or a `d2rq:Database` with `d2rq:jdbcDSN`. The D2RQ description's
  `d2rq:username` and `d2rq:password` are refused (§4.4).
- **The RML-Core vocabulary** of the W3C Knowledge Graph Construction Community Group
  (`http://w3id.org/rml/`), in which the same constructs, including the term maps, live
  in one namespace. A mapping may not mix the two vocabularies.
- **Functions** through RML's function modules (FNML in the rml.io form, the functions
  module in the Community Group's form): a function map names an FnO function IRI and
  its parameters. Sparkles implements a fixed table of functions (§7.3) and fails at
  load on any other.

`ql:XPath` and XML sources are refused with a message (non-goals). RML-star, RML's
collections and containers module and logical views are open question 3. RML's logical
targets are ignored with a warning, because the target is always the dataset or the
output file of `sparkles map run`.

### 3.4 YARRRML

A YARRRML document is translated into the same model, in the way its specification
describes the translation to RML. Sparkles reads `prefixes`, `base`, `sources` (the
`[path~csv]`, `[path~jsonpath, iterator]` shorthands and the long form with `access`,
`referenceFormulation`, `iterator`, `query` and `queryFormulation`), `mappings` with
`sources`, `s` or `subjects`, `po` or `predicateobjects`, `graphs`, `$(reference)`
references, `~iri` and `~lang` suffixes, `p`/`o` long forms with `datatype` and
`language`, `mapping` references with `condition` of the `equal` function for joins,
`fn`/`function` with `pms`/`parameters`, and `authors`, which is ignored. Unknown keys
fail with their path in the document, because YARRRML has no extension mechanism to
ignore safely. `sparkles map convert --to rml` prints the translation as RML-Core
Turtle, so a user can check what Sparkles understood.

### 3.5 CSVW and templates

CSVW metadata and CONSTRUCT templates keep C05's converter. They are not translated into
the model, because C05's CSVW datatypes and formats and its batched template evaluation
already work and have tests. They gain the additions of §6 and §7. A table group's CSVW
metadata can also describe a CSV logical source of RML, which then uses the dialect, the
null values and the datatypes of the column with the referenced name.

## 4. Sources

### 4.1 CSV and TSV

A CSV source reads records with C05's reader (`tabular::open` and its dialect handling),
including compression, the record-size limit and the BOM rule. A reference is a column
title from the header. An empty cell is NULL unless a CSVW description of the source
names other null values, which follows RML's convention that a CSV cell has no type and
CSVW's that the empty string is null.

### 4.2 JSON and JSON Lines

The iterator is a JSONPath expression in the syntax of RFC 9535, and each match is one
iteration. References are JSONPath expressions relative to the match, and a reference
that matches several values generates one term per value, as RML specifies for
multi-valued references. A JSON number keeps its lexical form and is typed
`xsd:integer`, `xsd:decimal` or `xsd:double` by that form. `true` and `false` are
`xsd:boolean`. `null` is NULL.

A JSON document is parsed whole, because a JSONPath iterator can match anywhere in it.
Its size is limited to 512 MiB by default (`--max-json-mb`). A file whose name ends in
`.jsonl` or `.ndjson`, or whose reference formulation names JSON Lines, is read one line
at a time, with the iterator applied to each line. That keeps memory flat for large
exports, which are usually in that form.

### 4.3 Parquet

A Parquet file is read with the `parquet` crate's Arrow reader, one row group at a time,
projected to the referenced columns. A reference is a column name, or a dotted path into
a struct column. Arrow types map to literals as the R2RML natural mapping does for the
corresponding SQL types: integer types to `xsd:integer`, `Decimal128` to `xsd:decimal`,
floats to `xsd:double`, `Boolean`, `Date32`, `Time64` and `Timestamp` (to
`xsd:dateTime`, with the zone when the column has one), `Binary` to `xsd:hexBinary`, and
`Utf8` to a plain string. A list column generates one term per element. A Parquet file
can be local or in an object store reached through a named connection of kind `s3`,
`gcs` or `azure`, built with the `object_store` crate that backup repositories
([F05](F05-snapshot-repositories.md)) already use.

### 4.4 SQL connections

A database is never described inside a mapping with credentials. The server holds named
connections in a server-wide settings kind, `connections`, managed like C21's channels
through `/$/settings` and the settings file, with secret references for passwords.

```json
{ "orders-db": { "kind": "postgres", "url": "postgres://reader@db.internal:5432/shop",
                 "password": { "secret": "orders-db" }, "tls": "require",
                 "maxConnections": 4, "statementTimeoutMs": 30000 } }
```

A mapping refers to a connection by `rml:source <urn:x-sparkles:connection:orders-db>`,
by YARRRML's `access: urn:x-sparkles:connection:orders-db`, or, for compatibility with
existing RML mappings, by a `d2rq:Database` whose `d2rq:jdbcDSN` equals a connection's
`jdbcDsn` alias. On the CLI, `--connection NAME=URL` declares one for the run.

| Kind | Crate | Feature | In the server build |
|---|---|---|---|
| `postgres` | `tokio-postgres`, with rustls | `mapping-postgres` | yes |
| `sqlite` | `rusqlite` with the bundled SQLite | `mapping-sqlite` | yes |
| `duckdb` | `duckdb` with the bundled DuckDB | `mapping-duckdb` | no, because it compiles DuckDB's C++ sources |
| `s3`, `gcs`, `azure`, `file` | `object_store` | `mapping-parquet` | yes |

A PostgreSQL host goes through the outbound policy (`outbound.rs`) as SERVICE requests
do, so an operator's CIDR allowlist also limits which databases a mapping reaches.
Connections are read-only by construction: Sparkles opens every PostgreSQL session with
`SET default_transaction_read_only = on` and SQLite and DuckDB files in read-only mode,
and runs only the statements its printer generates or the `rr:sqlQuery` text of a stored
mapping, wrapped as a derived table (§5.2).

## 5. Materialization

### 5.1 Plan

`sparkles::mapping::exec` turns a mapping into a plan before reading any row. The plan
lists, for each logical source, the triples maps that read it and the references they
need. Each source is read once, and every triples map that shares the source generates
its triples from the same iteration. R2RML and RML define the output as a set, so a
triple generated twice is written twice and the loader's deduplication removes it.

Each generated quad goes to the same sink as C05's tables: a temporary N-Quads file that
`tabular::source` turns into a loader `Source`, so the bulk path, atomicity, write-time
validation, dry runs, quotas, timeouts and cancellation apply unchanged. The C05 Outcome
measured this path at about a quarter of a load's time, and the alternative of a
`Source` that holds the converter remains open there.

### 5.2 SQL sources

A logical table becomes a derived table: `rr:tableName t` reads `SELECT <columns> FROM
t`, and `rr:sqlQuery q` reads `SELECT <columns> FROM (q) AS m0`, with only the columns
the triples maps reference. Rows are fetched with a server-side cursor in PostgreSQL and
a stepped statement in SQLite and DuckDB, 10,000 at a time, so memory stays flat. The
run's deadline sets `statement_timeout` for PostgreSQL and an interrupt for SQLite and
DuckDB, and a cancelled run cancels the statement.

### 5.3 Joins

A referencing object map joins the child's rows with the parent's on the join
conditions and generates the parent's subject as the object.

- **Both sides on one SQL connection.** The join runs in the database:
  `SELECT c.<child refs>, p.<parent subject refs> FROM (child) c JOIN (parent) p ON c.a =
  p.b AND …`. NULL never joins, as R2RML requires.
- **Otherwise.** The parent's subject terms are built into an in-memory hash table keyed
  by the parent's join values, and the child's rows probe it. The table is charged to the
  query memory budget of [C01](C01-observability-and-budgets.md) on the server and is
  unbounded on the CLI. A run over the budget fails and names the join and the parent's
  row count, and suggests putting both sides on one database.

The engine's batch hash join of X08 Phase 2 can replace this table once it spills to
disk. Until then, a join whose parent does not fit fails rather than swapping.

### 5.4 Limits and errors

| Limit | Default | Notes |
|---|---|---|
| JSON document | 512 MiB | `--max-json-mb`. JSON Lines is unbounded. |
| Rows per SQL fetch | 10,000 | |
| Parent rows of an in-memory join | the memory budget | §5.3 |
| Mapping text | 1 MiB | as C05's mapping part |
| Triples maps | 10,000 | |

Every error names the mapping file, the triples map, the source, the row (the source row
for CSV, the iteration number for JSON, the row group and row for Parquet, and nothing
for SQL, where the error quotes the key columns' values instead) and the term map. A
failed run commits nothing.

### 5.5 Running a stored mapping again

`POST /{ds}/mappings/{name}/run` with `mode=add`, the default, adds the triples. With
`mode=replace`, the run replaces every graph the mapping's graph maps can generate, which
must all be constant graph maps, through one `Store::replace` per graph inside one
commit. A mapping with a template graph map cannot use `replace`, because the set of
graphs it would clear is not known before the run. The commit message defaults to
`mapping NAME (version V)`, and the stored mapping records the commit of its last run.
This is the refresh that a materialized copy of a database needs, and the change feed
then carries only the net change, because a replace that writes the same quads changes
nothing.

## 6. CSVW: standard mode and key checks

### 6.1 Standard mode

`--csvw-mode standard` (and `csvwMode=standard` on uploads) writes the triples of the
standard mode of "Generating RDF from Tabular Data on the Web" around the minimal-mode
triples that C05 writes: a `csvw:TableGroup` resource with `csvw:table` links, a
`csvw:Table` per table with `csvw:url`, and a `csvw:Row` per row with `csvw:rownum`,
`csvw:url` (the RFC 7111 row IRI) and `csvw:describes` links to the subjects the row
generated, plus the table's notes and common properties as their JSON-LD values where
those are plain strings, IRIs or language maps. Common properties that need full JSON-LD
processing fail with a message, because Sparkles does not process JSON-LD contexts
(C05 §3.1). Minimal mode stays the default.

### 6.2 Primary keys

A schema's `primaryKey` names one or more columns. With `--csvw-check keys` (on by
default in standard mode, off in minimal mode to keep C05's behaviour), the converter
checks that no two rows have the same key values and that no key cell is null. It keeps
a hash set of the keys' canonical values, charged to the memory budget, and a duplicate
fails the import with both source rows. A set that passes the budget fails with a
message. An external sort through the index builder's run files is open question 4.

### 6.3 Foreign keys

A schema's `foreignKeys` lists column references to a resource (a table of the group,
by `url`) or a schema reference, and the referenced columns. Each table referenced by a
foreign key is read first, its referenced column values are collected into a set, and
every row of the referencing table must find its values there, or be null in a column
of the reference. This needs the referenced table to be in the same table group and to
be a file the run can read. A violation fails the import with the referencing row, the
column and the value. With both checks on, a group's tables are ordered by their foreign
keys, and a cycle fails at load.

## 7. Templates: Tarql functions and target-dataset lookups

### 7.1 Tarql's functions

CONSTRUCT templates gain the functions that Tarql documents in its namespace
`http://tarql.github.io/tarql#`:

- `tarql:expandPrefix(?prefix)` returns the namespace IRI of a prefix, from the
  template's `PREFIX` declarations and then the dataset's declared prefixes
  ([C20](C20-declared-prefixes.md)).
- `tarql:expandPrefixedName(?qname)` returns the IRI of a prefixed name by the same
  lookup.

ARQ's `apf:strSplit`, which Tarql's documentation uses to split a cell, is already an
ARQ property function of the engine (`sparql/arqpf.rs`), and templates run on the
engine's evaluator, so it needs a test and no new code. Tarql's handling of several
CONSTRUCT queries in one file is accepted: each query runs over the same batches and the
triples are the union. Tarql's command-line options for headers and delimiters map to
C05's dialect options.

### 7.2 Templates that read the target dataset

C05 ran templates against an empty dataset and rejected lookups in the target dataset,
because they would read under the writer. A template may now read the dataset with
`--lookup` on the CLI, or `lookup=true` on an upload or a stored mapping's run.

- The WHERE clause runs against the snapshot of the dataset's head when the conversion
  starts, under the caller's view (C12 and C12b), with `GRAPH` blocks allowed. The rows'
  `VALUES` block still comes first. The batch evaluation of C05 §5.2 is unchanged, so a
  lookup joins one batch of rows with the snapshot per evaluation.
- The conversion runs before the writer lock is taken, as today. If other commits land
  in between, the import still commits on top of them, as a load would. The receipt
  records `lookupCommit`, the commit the template read. `If-Match` with the entity tag
  of that commit, which upload and Graph Store writes already read as a precondition on
  the head, makes the commit fail with `412` when the head has moved.
- A template that reads the dataset counts its batches against the caller's query
  budgets, as C05's do.

The common use is linking: a row's `country` code becomes the IRI of the country already
in the dataset, through `?c ex:isoCode ?code`, without a second pass.

### 7.3 Functions in RML and YARRRML

RML function maps and YARRRML `fn` entries name FnO functions. Sparkles implements a
fixed set by mapping each to a SPARQL built-in or a C05 helper: the GREL string
functions for case, trimming, replacing, substring, splitting into several values,
concatenation and string comparison, `idlab-fn:`'s equality and the IRI-safe encoding,
and Sparkles' own `spk:` functions under `urn:x-sparkles:fn:` that name any SPARQL 1.1
function by its keyword. The exact function IRIs come from the published GREL and IDLab
function descriptions during implementation, and the list is printed by
`sparkles map check --functions`. A function the table lacks fails at load, with the
suggestion to use a CONSTRUCT template, where every SPARQL function is available.

## 8. Stored mappings

`<db>/mappings.json` holds the stored mappings in the shape of `queries.json`
(`stored.rs`): per name, the kind, the text, a description, the versions with time,
author, message, the head commit when saved and a digest chain, and the last 100
versions. Its writes take the same file lock and atomic rename as stored queries. The
mapping is parsed and checked (§2.1's `check`) before it is stored, so a stored mapping
always parses. A mapping refers to connections by name, and a missing connection is a
warning when the mapping is stored and an error when it runs, so a mapping can be stored
before the operator declares its connection.

Stored mappings stay per dataset and are shared by its branches, as stored queries are
([F09 §5.1](F09-branches-and-merges.md)). A clone of a persistent dataset copies the
file with the other configuration files, and an in-memory clone leaves it out, as
`Store::clone_to_memory` leaves out stored queries. Backups include the file.

The UI gains a Mappings page in the dataset's settings that
lists them, edits them in the query editor's Turtle and YAML modes, runs them with a
dry run first, and shows the last run's counts and warnings.

## 9. Virtual graphs (Phase 2)

### 9.1 What the planner sees

The planner (`sparql/plan.rs`) already recognizes special `SERVICE` IRIs before it plans
a generic `Kind::Service` leaf, for history (`history_svc`) and path search
(`pathsearch`). A virtual graph is recognized one level up: when the planner builds a
pattern under `GRAPH <g>` or with a dataset that names `g`, and `g` is a virtual graph of
the dataset, it hands the largest subtree that lies inside that graph to the unfolder
(§9.2), and puts the result into the plan as a new leaf, `Kind::VirtualScan(Box<VirtualSpec>)`.
The leaf holds the generated SQL, the output columns and how each variable's term is
built from them. A `GRAPH ?g` over `FROM NAMED` graphs that include a virtual graph is a
union of the stored part and one `VirtualScan` per virtual graph, with `?g` bound to the
graph's IRI.

The rows that come back become terms in the query's local term table, as SERVICE results
do today, so the rest of the engine (joins, filters, aggregates, result writers) treats
them as any other solutions.

### 9.2 Unfolding

Unfolding follows the approach the Ontop papers describe, without its ontology
rewriting.

1. **Candidate maps per triple pattern.** For each triple pattern, the unfolder keeps the
   (triples map, predicate-object map) pairs whose constant predicate and class can match
   the pattern's predicate and object. A pattern with a variable predicate gets every
   pair. `rdf:type` with a constant class matches the `rr:class` declarations.
2. **Pattern to relational algebra.** Each candidate becomes a projection of its logical
   table onto the columns of its subject and object term maps, with `IS NOT NULL` on
   those columns. A pattern is the `UNION ALL` of its candidates.
3. **Joins on templates, not strings.** Two patterns that share a variable join on the
   columns behind the variable when both term maps build it with the same template: the
   join is `a.id = b.id`, never a comparison of concatenated IRIs. Two templates that
   cannot produce the same IRI (different constant prefixes, or a literal against an IRI)
   make that branch of the union empty, and it is pruned. This is what keeps generated
   SQL sargable.
4. **Self-join elimination.** Two patterns over the same logical table joined on columns
   that form a key (a declared primary key, a unique constraint read from the database's
   catalogue, or `spk:key` declared in the mapping for derived tables) become one scan.
   A star of properties of one subject therefore becomes one `SELECT` of several columns.
5. **OPTIONAL, UNION, FILTER, MINUS.** OPTIONAL becomes a `LEFT JOIN`, UNION a `UNION
   ALL` with NULL padding, and MINUS a `NOT EXISTS`. A FILTER moves into SQL when every
   function in it has an exact SQL counterpart for the column types involved (§9.4), and
   otherwise stays in the engine above the leaf.
6. **Modifiers.** `DISTINCT`, `ORDER BY`, `LIMIT`, `OFFSET`, `GROUP BY` and the
   aggregates `COUNT`, `SUM`, `MIN`, `MAX` and `AVG` move into SQL when the whole query
   lies inside one virtual graph and the ordering is defined by columns with the same
   order as SPARQL's for those types. Otherwise the engine applies them.

The relational form is `sparkles::mapping::vsql::Rel` (scan, select, project, join, left
join, union, not-exists, distinct, aggregate, order and slice), printed per dialect for
PostgreSQL, SQLite and DuckDB, and handed to DataFusion as a logical plan for Parquet and
Iceberg (§9.6). It is never assembled from strings of the query, and constants go in as
bound parameters.

### 9.3 Template inversion

A constant IRI in a pattern, or an IRI that a join with stored data passes in (§9.5),
is matched against each candidate's subject or object template. A template
`http://ex.org/order/{id}` inverted against `http://ex.org/order/42` binds `id` to `42`
after percent-decoding, and the scan gets `WHERE id = $1`. An IRI that matches no
template removes the candidate. Templates whose parts are ambiguous to invert (two
adjacent references with no constant between them) are inverted only when
`rr:inverseExpression` is given, and otherwise compare the generated string in SQL, with
a notice in `EXPLAIN`.

### 9.4 Types and filters

A column's literal is built after the SQL runs, so filters push down only when the SQL
comparison means what the SPARQL comparison means.

| SPARQL | Pushed into SQL when |
|---|---|
| `=`, `!=`, `<`, `<=`, `>`, `>=` | both sides are numeric columns or constants, or both are date or dateTime columns with time zones handled as §4.3 maps them, or both are plain-string columns compared with `=` or `!=` |
| `<`, `>` on strings | the column's collation is binary (`C` in PostgreSQL), because SPARQL orders strings by code point |
| `BOUND`, `!`, `&&`, `\|\|` | their operands are pushed |
| `STRSTARTS`, `CONTAINS` | the column is a string column, as `LIKE` with escaped wildcards |
| `REGEX` | never in Phase 2. Regular expression dialects differ. |
| `LANG`, `DATATYPE`, `STR` | resolved at compile time from the term map |

Everything else is evaluated by the engine on the rows. `EXPLAIN` shows which filters
moved.

### 9.5 Joins with stored data

A query can join a virtual graph with the dataset's own graphs.

```sparql
SELECT ?customer ?name ?total WHERE {
  ?customer ex:name ?name ; ex:segment ex:enterprise .           # stored
  GRAPH <urn:x-sparkles:virtual:orders> { ?o ex:customer ?customer ; ex:total ?total }
}
```

The planner chooses between two strategies by the estimates of §9.7.

- **Bind join.** The stored side runs first, and its distinct values of the shared
  variables are sent to the virtual leaf in batches of up to 1,000, inverted through the
  templates into column values and passed as an array parameter (`= ANY($1)` in
  PostgreSQL) or a `VALUES` derived table. This is G10's `bulk` mode applied to SQL.
- **Hash join.** The virtual leaf runs once with its own filters, and the engine joins
  its rows with the stored side. This wins when the stored side is large or unselective.

### 9.6 Parquet and Iceberg

A virtual graph over Parquet or Iceberg sources runs its relational form on DataFusion,
which reads Parquet with row-group and page pruning from statistics, and reads Iceberg
tables through `iceberg-rust`'s DataFusion integration with partition pruning from the
table's manifests. Iceberg tables are named by a catalogue connection (`iceberg-rest`, or
a metadata file location in an object store) and a table identifier. An Iceberg query
reads one table snapshot, chosen at the start of the query (§9.8).

### 9.7 Cost estimates

The planner's join ordering needs row estimates for the virtual leaf. They come from
PostgreSQL's `pg_class.reltuples` and column statistics (`pg_stats`), SQLite's
`sqlite_stat1` when `ANALYZE` has run, Parquet row-group metadata, and Iceberg manifest
record counts. Estimates are cached per connection for ten minutes. Without statistics a
scan is estimated at 100,000 rows and a key lookup at 1. The leaf's cost per row is
higher than a stored scan's by a factor measured per connection kind in §13.2.

### 9.8 Consistency

A query reads its stored data at one commit of the dataset, and its virtual data at the
source's state when the SQL ran. The two are not one snapshot, and no design can make
them one without a transaction spanning both systems. Within those limits:

- **Per query, per source.** Every statement a query sends to one PostgreSQL connection
  runs in one `REPEATABLE READ READ ONLY` transaction, so the virtual part of a query is
  a consistent state of that database. SQLite and DuckDB reads run inside one read
  transaction. An Iceberg query pins one table snapshot. Parquet files are read by their
  object version or ETag when the store has one, and a file that changes during the
  query fails it with `409 virtual-source-changed`.
- **Point-in-time reads.** `?at=` applies to the stored data as F06 defines. For Iceberg,
  `at=time:T` also selects the table snapshot current at T, through Iceberg's time
  travel. For SQL sources, `at` other than `head` fails with `400
  virtual-at-unsupported` unless the virtual graph sets `"at": "ignore"`, in which case
  the response carries the notice `virtual-at-ignored`.
- **Entity tags and caches.** A response that read a virtual graph has no strong entity
  tag, because the dataset's commit does not determine it. The result cache of the
  engine is bypassed for such queries unless the virtual graph sets a cache (§9.9).

### 9.9 Caching

A virtual graph may cache the rows of its leaves, keyed by the mapping version, the
generated SQL and its parameters, and the source's version token. The token is exact
for Iceberg (the snapshot id) and Parquet (ETag or size and modification time), so those
caches are always on and never stale. PostgreSQL and SQLite have no cheap token that
covers a set of tables, so their caches use `cache.ttlSeconds`, which defaults to 0
(off). The cache lives in the server's result cache memory and its size limit (X08 Phase
1), and `POST /$/cache/clear/{ds}` clears it with the other cached results.

### 9.10 Access control and writes

- **Graph grants.** C12 grants name virtual graph IRIs like any other graph. A principal
  whose view does not include the virtual graph sees it as a graph that does not exist,
  and no SQL is sent.
- **Protections of triples.** A C12b protection by predicate compiles into the unfolder
  by removing the candidates that generate the predicate. Protections by subject class
  and by pattern would need a join with the class's maps or an evaluation of the pattern
  per row. In Phase 2, a query by a principal with such a protection that reaches a
  virtual graph fails with `403 virtual-protection-unsupported` rather than return data
  the protection should hide.
- **Rows per caller.** A logical table's `rr:sqlQuery` may use the parameters
  `:principal` and `:groups`, bound to the caller's principal name and group list, as
  C12b binds the caller in its patterns. This is how a virtual graph restricts rows per
  user, inside the database.
- **Database credentials** are the connection's, never the caller's.
- **Writes.** An update whose quad patterns name a virtual graph, a Graph Store write to
  one, and an RDF Patch row in one fail with `403 virtual-graph-read-only`. Loading the
  same mapping with `POST /{ds}/mappings/{name}/run` is the way to copy its data into the
  dataset.

### 9.11 Budgets and explanation

The query's deadline sets the statement timeout of each SQL statement, and cancellation
sends the database's cancel request. Rows fetched count against the row budget. A query
whose virtual leaf would fetch more than `virtualMaxRows` (dataset setting, default
1,000,000) rows without a LIMIT pushed into SQL fails with `507`. `/{ds}/explain` shows
each `VirtualScan` with its SQL, its parameters, which filters and modifiers moved into
it, the estimate, and with `analyze` the rows and the time spent in the database.

## 10. Architecture

| Place | Contents |
|---|---|
| `crates/sparkles-core/src/mapping/mod.rs` | The public entry points: `parse(text, kind, base) -> Mapping`, `check`, `run(mapping, sources, sink, opts) -> Stats`. |
| `mapping/model.rs` | §3.1's intermediate form. |
| `mapping/r2rml.rs` | R2RML and both RML vocabularies, read from an `oxrdf` graph parsed with `oxttl`. |
| `mapping/yarrrml.rs` | YARRRML to the model, and the model to RML-Core Turtle for `convert`. |
| `mapping/template.rs` | R2RML string templates, IRI-safe encoding, inversion. |
| `mapping/source/{csv,json,parquet,sql}.rs` | Iterators of rows by references. `csv` wraps `tabular::open` and the C05 dialect. |
| `mapping/exec.rs` | The materialization plan, the joins and the quad sink. |
| `mapping/functions.rs` | §7.3's function table. |
| `mapping/vsql/` (Phase 2) | `Rel`, the unfolder, filter translation, dialect printers and the DataFusion bridge. |
| `crates/sparkles-core/src/sparql/plan.rs`, `exec.rs` | `Kind::VirtualScan` and its executor, next to `Kind::Service`. |
| `crates/sparkles-core/src/tabular/` | Standard mode and key checks in `csvw.rs`, Tarql functions and lookups in `template.rs`. |
| `crates/sparkles-core/src/stored_mappings.rs` | `mappings.json`, in the manner of `stored.rs`. |
| `crates/sparkles-server/src/map_cmd.rs` | `sparkles map`, and the `--mapping` additions in `csv_cmd`. |
| `crates/sparkles-server/src/http/mappings.rs`, `http/tabular.rs` | Stored mapping routes, runs, uploads with RML parts, Graph Store CSV. |
| `crates/sparkles-server/src/settings/` | The `connections` kind and `/$/virtual/{ds}`. |
| `ui/src/routes/…/mappings` | The Mappings page. |

The SQL drivers and DataFusion sit behind the cargo features of §4.4 and `virtual`, so
the library's default build and the Python wheel gain no C or C++ code. The Python and
Node bindings expose `Dataset.run_mapping(text, sources=…)` in Phase 1.

## 11. Phases

**Phase 1 (roadmap item 22).** The model, R2RML, RML and YARRRML readers, CSV, JSON,
JSON Lines, Parquet, PostgreSQL and SQLite sources, joins, functions, `sparkles map`,
`--mapping` for these kinds in `load` and uploads, CSVW standard mode and key checks,
Tarql's functions, lookups in templates, stored mappings with runs and `mode=replace`,
the `connections` settings kind, Graph Store CSV, and the R2RML and RML-Core test cases.
DuckDB is in Phase 1 behind its feature if the build time is acceptable in CI.

**Phase 2 (roadmap item 43).** Virtual graphs over PostgreSQL and SQLite first, with
unfolding, template joins, self-join elimination, filter and modifier pushdown, bind and
hash joins with stored data, consistency per query, access control and `EXPLAIN`. Then
DataFusion for Parquet and Iceberg, caching by version token, Iceberg time travel with
`at`, and the BSBM comparison with Ontop.

## 12. Acceptance examples

The examples use a PostgreSQL database with these tables, and the R2RML mapping below.

```sql
CREATE TABLE customer (id int PRIMARY KEY, name text NOT NULL, country char(2));
CREATE TABLE orders (id int PRIMARY KEY, customer int REFERENCES customer, total numeric(10,2), placed date);
INSERT INTO customer VALUES (1, 'Ann', 'NO'), (2, 'Bob', NULL);
INSERT INTO orders VALUES (10, 1, 99.50, '2026-01-02'), (11, 1, 5.00, '2026-02-03'), (12, 2, 12.00, '2026-02-04');
```

```turtle
@prefix rr: <http://www.w3.org/ns/r2rml#> . @prefix ex: <http://ex.org/> .
<#C> rr:logicalTable [ rr:tableName "customer" ] ;
  rr:subjectMap [ rr:template "http://ex.org/customer/{id}" ; rr:class ex:Customer ] ;
  rr:predicateObjectMap [ rr:predicate ex:name ; rr:objectMap [ rr:column "name" ] ] ,
                        [ rr:predicate ex:country ; rr:objectMap [ rr:template "http://ex.org/country/{country}" ] ] .
<#O> rr:logicalTable [ rr:tableName "orders" ] ;
  rr:subjectMap [ rr:template "http://ex.org/order/{id}" ] ;
  rr:predicateObjectMap [ rr:predicate ex:total ; rr:objectMap [ rr:column "total" ] ] ,
    [ rr:predicate ex:customer ; rr:objectMap [ rr:parentTriplesMap <#C> ;
        rr:joinCondition [ rr:child "customer" ; rr:parent "id" ] ] ] .
```

- **A1. R2RML load.** `sparkles load --loc DB --mapping shop.ttl --connection
  shop=postgres://…` commits 12 triples: two `rdf:type`, two names, one country (Bob's
  NULL country gives none), three totals typed `xsd:decimal` with values `99.5`, `5.0`
  and `12.0`, and three `ex:customer` links. `EXPLAIN`-style output of `sparkles map
  check` shows the join as one SQL statement.
- **A2. Same triples three ways.** The same mapping written in RML-Core and in YARRRML,
  over CSV exports of the two tables, gives the same 12 triples as A1, with the totals
  typed `xsd:decimal` only when a CSVW description of the CSV source says so, and as
  plain strings otherwise.
- **A3. NULL and errors.** A row of `orders` with a NULL `customer` gives no `ex:customer`
  triple. A template object that resolves to a relative IRI fails with the triples map
  `<#O>`, the key `id = 13` and the term map.
- **A4. JSON.** A JSON file `{"people":[{"id":7,"tags":["a","b"]}]}` with iterator
  `$.people[*]` and an object reference `tags[*]` gives two `ex:tag` triples for
  `<http://ex.org/p/7>`, and `id` is typed `xsd:integer` when referenced as a literal.
- **A5. Parquet.** A Parquet file of 10 million rows in 100 row groups maps with resident
  memory below 200 MiB.
- **A6. Join without a database.** A CSV child of 1 million rows and a parent of 100,000
  rows joins in memory. With a memory budget of 1 MiB, the run fails with a message that
  names the join and the parent's row count, and nothing is committed.
- **A7. Standard mode.** C05's T3 file in standard mode adds a `csvw:TableGroup`, one
  `csvw:Table` with its `csvw:url`, and one `csvw:Row` with `csvw:rownum 1`, `csvw:url
  <…people.csv#row=2>` and `csvw:describes <http://example.org/person/7>`.
- **A8. Keys.** A table whose `primaryKey` is `id` and has two rows with `id` 7 fails with
  both source rows. An `orders.csv` whose foreign key `customer` names a missing
  `customer.csv` id fails with its row, column and value.
- **A9. Tarql functions and lookups.** A template with `BIND(IRI(CONCAT(
  tarql:expandPrefix("ex"), ?code)) AS ?c)` resolves `ex` from the template's `PREFIX`.
  With `--lookup`, `?country ex:isoCode ?code` joins each row with the dataset, and the
  receipt's `lookupCommit` is the head at the start.
- **A10. Stored mapping refresh.** Storing A1's mapping, running it with `mode=add`, then
  updating `customer` 1's name in PostgreSQL and running it with `mode=replace` makes a
  commit with `inserted: 1, deleted: 1`.
- **A11. Graph Store CSV.** `PUT /ds/data?graph=urn:g&mapping=people` with a CSV body
  replaces `urn:g`. The same request without `mapping` or `base` is `415`. A stored
  mapping with `rr:graph <urn:other>` is `400 mapping-graph-conflict`.
- **A12. Virtual star.** With A1's mapping as virtual graph `urn:v`, `SELECT ?n ?c WHERE {
  GRAPH <urn:v> { ?x a ex:Customer ; ex:name ?n ; ex:country ?c } }` sends one SQL
  statement over `customer` with no self-join, and returns one row (Ann).
- **A13. Constant IRI.** `GRAPH <urn:v> { <http://ex.org/order/10> ex:total ?t }` sends
  `WHERE id = $1` with `$1 = 10`. `<http://ex.org/other/10>` sends no SQL.
- **A14. Join with stored data.** The query of §9.5 with 10 enterprise customers stored
  and 1 million orders in PostgreSQL uses a bind join and fetches only those customers'
  orders.
- **A15. Equivalence.** Every query of §13.1's list returns the same solutions over the
  virtual graph as over the materialized copy of A1.
- **A16. Access.** A principal without a grant on `urn:v` gets no rows and causes no SQL.
  A principal with a class protection gets `403 virtual-protection-unsupported`. An
  `INSERT DATA { GRAPH <urn:v> { … } }` gets `403 virtual-graph-read-only`.
- **A17. Consistency.** A query with two virtual leaves over one PostgreSQL connection
  runs both in one transaction. An Iceberg query with `at=time:T` reads the table
  snapshot current at T.

## 13. Testing and measurement

### 13.1 Conformance and equivalence

- **R2RML.** The W3C R2RML test cases, run against PostgreSQL and SQLite in CI, from a
  pinned copy of the test suite. Each case compares the produced dataset with the
  expected one by graph isomorphism, and the cases that expect an error expect one.
- **RML.** The RML-Core test cases of the Knowledge Graph Construction Community Group,
  and the earlier rml.io test cases for CSV and JSON, excluding XML.
- **YARRRML.** Each YARRRML test translates to RML and compares with the expected RML
  graph, then runs.
- **CSVW.** The W3C CSVW test suite, which C05 could not run offline, is vendored as test
  data for the RDF tests in minimal and standard mode.
- **Virtual equivalence.** A generated query list over the A1 schema and over BSBM's
  relational form: every combination of up to four triple patterns, OPTIONAL, UNION,
  FILTER and the modifiers, run over the virtual graph and over the materialized
  mapping, with results compared as multisets. Differences fail the test.

### 13.2 Performance

- **Materialization.** Triples per second from PostgreSQL, Parquet, CSV through RML, and
  JSON Lines, against C05's numbers for the same CSV through CSVW.
- **BSBM on SQL.** The BSBM generator's relational form at 1M and 10M products loaded into
  PostgreSQL, with the BSBM explore and business intelligence query mixes run against
  (a) Sparkles virtual graphs, (b) Ontop as an external endpoint over the same
  database, with its default settings and the same R2RML mapping, (c) Sparkles with the
  mapping materialized, and (d) BSBM's SQL queries run directly. The comparison is
  published in BENCHMARKS.md with the versions and settings, and a target of 1.5× of
  Ontop for the explore mix (goal 7).
- **NPD.** The NPD benchmark's queries over its relational data, for coverage of
  realistic mappings, when its license allows redistribution of the data to CI.
- **Join with stored data.** A14 at 10, 1,000 and 100,000 stored customers, to fit the
  crossover between bind and hash joins.

## 14. Rejected alternatives

- **Translating R2RML and RML into CSVW or CONSTRUCT templates.** CSVW has no joins
  across tables and no JSON sources, and templates run per batch of one table.
- **Embedding an existing mapping engine.** The mature RML engines are written in Java
  or Python. A JVM or Python process beside the server would need its own deployment,
  memory and security model, and would not share the engine's budgets.
- **Ontop as the virtual graph engine.** Running Ontop as a SPARQL endpoint behind
  SERVICE works today with no new code. It cannot see the stored data's statistics, so
  joins between stored and virtual data are decided without them, and every row crosses
  HTTP twice. It remains the comparison baseline.
- **Virtual graphs in the default graph.** Every query would reach every database. A
  user can add `FROM` or set up a stored query that names the graphs.
- **Pushing SPARQL into SQL as string comparisons of IRIs.** Joins on concatenated
  templates defeat indexes. §9.2's template joins are the core of the design.
- **One SQL dialect through `sqlparser`.** Parsing and rewriting SQL text would let
  `rr:sqlQuery` be inlined, but the derived-table wrapping works on every database, and
  the generated SQL needs a printer per dialect in any case.
- **DuckDB for Parquet and Iceberg.** DuckDB reads both, but it is a C++ build, and its
  Iceberg support is an extension loaded at runtime. DataFusion is Rust, Apache-2.0, and
  `iceberg-rust` integrates with it.
- **Writable virtual graphs.** Translating SPARQL Update into SQL through a mapping is
  ambiguous for templates and joins, and the database's own constraints would reject
  writes the dataset's validation accepted.

## 15. Open questions

1. **XML sources.** Is a subset of XPath (child and attribute steps, predicates on
   attributes) enough for the XML mappings users bring?
2. **More databases.** MySQL and SQL Server first, if users ask, each with a CI
   database.
3. **RML modules.** RML-star to RDF 1.2 triple terms, collections and containers, and
   logical views: which first?
4. **Large key checks.** Should CSVW key checks and in-memory joins spill to the index
   builder's run files before X08 Phase 2's spilling join exists?
5. **Cache tokens for PostgreSQL.** Is a token from `pg_stat_user_tables` change counters
   or logical replication positions reliable enough to make PostgreSQL caches exact?
6. **Virtual graphs in schema discovery.** Should `/$/schema/{ds}` sample the database
   for counts, or report only what the mapping declares?
7. **Ontology rewriting.** When F17 adds OWL 2 QL rewriting, should it apply to virtual
   graphs, as Ontop does?

## 16. Sources

- W3C Recommendation "R2RML: RDB to RDF Mapping Language" (2012), and the W3C Working
  Group Note "R2RML and Direct Mapping Test Cases" (2012), cited from working knowledge.
- The RML specification at rml.io, and the RML-Core and related module drafts of the W3C
  Knowledge Graph Construction Community Group, cited from working knowledge at the level
  of their vocabularies.
- A. Dimou, M. Vander Sande, P. Colpaert, R. Verborgh, E. Mannens and R. Van de Walle,
  "RML: A Generic Language for Integrated RDF Mappings of Heterogeneous Data Sources",
  LDOW 2014.
- The YARRRML specification (rml.io/yarrrml/spec), and P. Heyvaert, B. De Meester, A.
  Dimou and R. Verborgh, "Declarative Rules for Linked Data Generation at your
  Fingertips!", ESWC 2018 Satellite Events.
- W3C Recommendations "Model for Tabular Data and Metadata on the Web", "Metadata
  Vocabulary for Tabular Data" and "Generating RDF from Tabular Data on the Web" (2015).
- D. Calvanese et al., "Ontop: Answering SPARQL queries over relational databases",
  Semantic Web Journal 8(3), 2017, and M. Rodríguez-Muro and M. Rezk, "Efficient
  SPARQL-to-SQL with R2RML mappings", Journal of Web Semantics 33, 2015, for unfolding,
  template joins and self-join elimination.
- D. Lanti, M. Rezk, G. Xiao and D. Calvanese, "The NPD Benchmark: Reality Check for OBDA
  Systems", EDBT 2015. C. Bizer and A. Schultz, "The Berlin SPARQL Benchmark",
  International Journal on Semantic Web and Information Systems 5(2), 2009, for BSBM and
  its relational form.
- RFC 9535 (JSONPath), the Apache Arrow and Apache Parquet format specifications, and the
  Apache Iceberg table specification.
- Tarql's public documentation (Apache-2.0), for `tarql:expandPrefix`,
  `tarql:expandPrefixedName` and `apf:strSplit`.
- The Sparkles code: `tabular` (`mod.rs`, `csvw.rs`, `template.rs`), `stored.rs`,
  `Store::replace` and `ReplaceTarget`, `sparql/plan.rs` (`Kind::Service`, the history
  and path-search SERVICE leaves), `sparql/enhancer.rs`, `sparql/arqpf.rs`,
  `outbound.rs`, and the specs C01, C05, C12, C12b, C16, C19, C20, C21, F05, F06 and G10.

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
  has `Dataset.mappings` and `Dataset.run_mapping`. The JVM and Node.js bindings are
  planned under roadmap item 231.

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
  parent's subject again gets the same node, and such a join is never pushed into SQL,
  whose joined rows have no row number of the parent's source. Across a hash join over
  a SQL source, the node depends on the database returning the parent's rows in the
  same order to both reads.
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
  `crates/sparkles-py/tests/test_mappings.py` covers Python.
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
