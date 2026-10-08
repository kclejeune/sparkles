# C05: CSV and TSV imports

> **Status:** implemented
>
> **Phases:** Shipped on 2026-10-02 as one phase: the `sparkles::tabular` converter, a
> default mapping, CSVW metadata mappings, CONSTRUCT templates, `sparkles load` and
> `sparkles csv`, and CSV files in `POST /{ds}/upload`.
>
> **User docs:** [Usage: Loading CSV and TSV](../USAGE.md#loading-csv-and-tsv) ·
> [API: CSV and TSV uploads](../API.md#csv-and-tsv-uploads) ·
> [Features](../FEATURES.md#storage-tdb2-equivalent)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This design was written from the W3C CSV on the Web recommendations
(the Model for Tabular Data and Metadata on the Web, the Metadata Vocabulary for Tabular
Data, and Generating RDF from Tabular Data on the Web), RFC 4180, RFC 6570, RFC 7111, the
IANA registration of `text/tab-separated-values`, the public documentation of Tarql, the
RML and YARRRML specifications at the level of their published overviews, and the
Sparkles code. It builds on budgets ([C01](C01-observability-and-budgets.md)), write
previews ([C15](C15-write-previews.md)) and the existing bulk loader.

## 1. Summary

A large share of the data people want in a graph arrives as spreadsheets and database
exports in CSV. Today a Sparkles user has to convert such a file to Turtle or N-Triples
with another tool before `sparkles load` can read it. Jena has no CSV reader in its core
distribution, so Fuseki users face the same detour. The common tools for the conversion
are Tarql, which maps each row through a SPARQL CONSTRUCT query, and CSVW processors,
which read a JSON metadata file that describes the table.

This spec adds a converter from CSV and TSV to RDF triples inside the engine, and wires
it into the places where RDF files are loaded. The converter offers three ways to map a
table to triples.

1. **A default mapping** that needs no configuration. Each row becomes one subject, named
   from a key column or from the row's position in the file, and each column becomes one
   predicate under a namespace.
2. **CSVW metadata.** A JSON file in the W3C metadata vocabulary names the columns, gives
   their datatypes and null values, and builds subjects, predicates and objects with URI
   templates. This is the standard format, and other CSVW tools read the same files.
3. **CONSTRUCT templates** in the style of Tarql. A SPARQL CONSTRUCT query sees each row
   as a solution whose variables are the columns. The engine's own SPARQL evaluator runs
   the query, so every SPARQL function is available to build terms.

**Goals**

1. `sparkles load` accepts `.csv` and `.tsv` files, compressed or not, with
   `--mapping`, `--template`, `--base` and `--key`. A CSV file loads in the same atomic
   commit as any RDF files given with it.
2. `sparkles csv convert` writes the triples as N-Triples, N-Quads or Turtle, and
   `sparkles csv mapping` prints the CSVW metadata of the default mapping as a starting
   point for a mapping file.
3. `POST /{ds}/upload` accepts CSV and TSV files, with the mapping or template as a part
   of the multipart request.
4. The conversion streams. Memory does not grow with the number of rows, and a file
   larger than memory converts and loads.
5. Errors name the file, the row and the column. A value that does not match its
   declared datatype fails the import, and nothing is committed.
6. The import respects the existing limits: the decompressed-size limit, the free-disk
   reserve, write timeouts and cancellation, and the query budgets for templates.

**Non-goals.** RML, R2RML and YARRRML mappings, joins across several tables, lookups in
the target dataset while mapping, CSVW's standard mode with its `csvw:Table` and
`csvw:Row` description triples, foreign-key and primary-key validation, spreadsheets in
other formats such as XLSX, mappings stored on the server, and type inference.

## 2. The converter

The converter lives in a new module, `sparkles::tabular`. Its input is a byte stream, a
mapping and a few options. Its output is a stream of triples written to any
`std::io::Write` in N-Triples, N-Quads or Turtle, together with a count of rows and
triples and a list of warnings.

### 2.1 How it reaches the store

The loader reads `Source` values, which hold RDF bytes or a file of them. The converter
writes N-Triples into a temporary file, and that file is loaded as an ordinary N-Triples
source, placed into `--graph` when one is given. This is the path the server already
takes for Jena's RDF Thrift, RDF Protobuf and RDF/JSON bodies, which it transcodes into
N-Quads before loading. Everything the loader does applies unchanged: the bulk builder
for large inputs, transactional inserts for small ones, atomicity, write-time
validation, dry runs, quotas and commit messages.

The cost of this path is one serialization and one parse of N-Triples, and temporary
disk space for the N-Triples file. The N-Triples parser splits the file and parses it in
parallel, so the parse costs little next to the conversion. A new kind of `Source` that
holds the converter itself would avoid both costs, but every reader of `Source` would
have to learn it, and the conversion could not be shared with `sparkles csv convert`.
That alternative is left for a later phase if measurements show the cost matters.

### 2.2 Reading the table

The `csv` crate reads records. The reader is configured from the CSVW dialect (§3.4),
and by default it follows RFC 4180: commas, double quotes, quotes doubled inside quoted
fields, and either `\r\n` or `\n` line ends. A file named `.tsv`, or a body sent as
`text/tab-separated-values`, uses tabs and no quoting, as the IANA registration of TSV
describes. The input must be UTF-8, and a byte-order mark at its start is skipped.

The first row is the header unless the dialect says otherwise. Header cells give the
columns' titles. Every later row must have as many cells as the table has columns. A row
with more or fewer cells fails the import, as the CSVW model requires, because a short
row usually means a quoting error further up the file.

Rows are numbered as CSVW numbers them. `_row` counts data rows from 1. `_sourceRow`
counts every record of the file from 1, header and skipped rows included, so it equals
the row number of RFC 7111's `#row=` fragment. Error messages use the source row, which
is the line a person finds in an editor when the file has no line breaks inside quoted
cells.

### 2.3 Limits

| Limit | Default | Notes |
|---|---|---|
| Bytes in one record | 16 MiB | A longer record fails with its row number. This stops an unclosed quote from reading the rest of a large file into one cell. |
| Columns | 4,096 | |
| Output bytes | none on the CLI | On the server the N-Triples written for one upload count against `--max-decompressed-mb`, the limit on the bytes the loader parses from any decompressed upload. |
| Free disk | the store's reserve | The server checks `--min-free-disk-mb` while it writes the temporary N-Triples file, as it does while it spools an upload. |
| Rows per template batch | 10,000 | §5.2. |

A compressed CSV file is decompressed as a stream with the same codecs and the same
decompressed-size limit as compressed RDF. The converter calls the write's timeout and
cancellation check every 65,536 rows, so a timed-out upload stops converting.

## 3. CSVW metadata

A mapping file is a CSVW metadata document: a table description, or a table group whose
`tables` lists table descriptions. Sparkles reads the subset below, which covers the
examples of the CSVW primer and of "Generating RDF from Tabular Data on the Web".

```json
{
  "@context": "http://www.w3.org/ns/csvw",
  "url": "people.csv",
  "tableSchema": {
    "aboutUrl": "http://example.org/person/{id}",
    "columns": [
      { "name": "id", "titles": "ID", "datatype": "integer",
        "propertyUrl": "http://example.org/id" },
      { "name": "name", "titles": "Name", "propertyUrl": "schema:name", "lang": "en" },
      { "name": "born", "titles": "Born",
        "datatype": { "base": "date", "format": "dd/MM/yyyy" },
        "propertyUrl": "schema:birthDate" },
      { "name": "tags", "titles": "Tags", "separator": ";",
        "propertyUrl": "schema:keywords" },
      { "name": "employer", "titles": "Employer", "null": ["", "n/a"],
        "propertyUrl": "schema:worksFor",
        "valueUrl": "http://example.org/org/{employer}" },
      { "virtual": true, "propertyUrl": "rdf:type", "valueUrl": "schema:Person" }
    ]
  }
}
```

### 3.1 What is read

- **`@context`** is `"http://www.w3.org/ns/csvw"`, or an array of it and an object with
  `@base` and `@language`. Any other context fails, because Sparkles does not process
  JSON-LD contexts.
- **Table descriptions** take `url`, `tableSchema` (inline), `dialect` (inline),
  `suppressOutput` and the inherited properties. A `tableSchema` or `dialect` given as a
  URL fails with a message that asks for it inline.
- **Schemas** take `columns`, `aboutUrl` and the other inherited properties.
  `primaryKey`, `foreignKeys` and `rowTitles` are accepted and ignored.
- **Columns** take `name`, `titles` (a string, an array or a language map), `virtual`,
  `suppressOutput` and the inherited properties.
- **Inherited properties** are `aboutUrl`, `propertyUrl`, `valueUrl`, `datatype`,
  `default`, `lang`, `null`, `ordered`, `required` and `separator`. They are inherited
  from the table group to the table, the schema and the column, and the nearest one
  wins. `textDirection` is accepted and ignored.
- Other CSVW properties, such as `notes`, `transformations` and the common properties of
  Dublin Core, are ignored with a warning. Properties outside the vocabulary are ignored
  with a warning too, as CSVW asks of processors.

### 3.2 Columns and titles

Without a `name`, a column's name is its first title with characters outside the
unreserved set percent-encoded, as CSVW specifies. A column with neither name nor title
is `_col.N`. Names that start with `_` are reserved for the template variables of §3.5
and fail in a metadata file.

When the file has a header, the header must have as many cells as the metadata has
non-virtual columns, or the import fails. A header cell that matches none of its
column's titles gives a warning and the metadata's column is used, so a file whose
headers were renamed still loads in the right column order.

### 3.3 Cells and datatypes

Each cell goes through the steps of the CSVW model's cell parsing.

1. Unless the datatype is `string`, `json`, `xml`, `html` or `anyAtomicType`, line breaks
   and tabs become spaces, and unless it is also not `normalizedString`, the value is
   trimmed and runs of spaces collapse.
2. An empty value takes the column's `default`.
3. With a `separator`, the value is split into a list, and each item goes through the
   remaining steps. An empty value is an empty list.
4. A value equal to one of the column's `null` strings is null. The default `null` is
   the empty string. A null in a column with `required: true` fails the import.
5. The value is parsed with the datatype and its `format`, and checked against the
   datatype's constraints.

| Datatype | Accepted `format` | Literal written |
|---|---|---|
| `string` (default) and the other string types | a regular expression the whole value must match | the value, with the column's `lang` when the type is `string` and `lang` is set |
| `integer`, `decimal`, `double`, `float`, `number` and the derived integer types | an object with `decimalChar` and `groupChar`. A `pattern` is accepted with a warning and not checked | the canonical form of the number. `%` and `‰` suffixes divide by 100 and 1,000, and `NaN`, `INF` and `-INF` are read for `double` and `float` |
| `boolean` | `"yes|no"`, where the first word is true | `true` or `false` |
| `date`, `dateTime`, `dateTimeStamp`, `time` | the date and time patterns listed by CSVW, such as `dd/MM/yyyy`, `M/d/yyyy`, `yyyyMMdd`, `HH:mm` and `yyyy-MM-ddTHH:mm:ssXXX` | the XSD lexical form |
| `gYear`, `gYearMonth`, `duration` and the other XSD types | none | the value, after checking that it is a valid lexical form |
| `json`, `xml`, `html` | none | the value, typed `csvw:JSON`, `rdf:XMLLiteral` or `rdf:HTML` |
| `anyAtomicType` | none | an `xsd:string` literal |

A datatype object with `base` and `@id` writes literals of the `@id` type after checking
the value against `base`. The constraints `length`, `minLength` and `maxLength` apply to
every type, and `minimum`, `maximum`, `minInclusive`, `maxInclusive`, `minExclusive` and
`maxExclusive` apply to numeric types. A bound on a date or time type fails at load with
a message that it is not supported.

A value that fails its datatype or a constraint fails the import with its row, column
and the reason. CSVW lets a processor go on with the raw string and report an error, but
a loader that keeps going would commit data that silently differs from its declaration.

### 3.4 Dialect

The dialect's `delimiter`, `quoteChar`, `doubleQuote`, `header`, `headerRowCount`,
`skipRows`, `skipColumns`, `skipBlankRows`, `skipInitialSpace`, `commentPrefix`, `trim`
and `encoding` are read. Delimiters, quote characters and comment prefixes must be one
ASCII character, and the encoding must be UTF-8. `lineTerminators` is accepted and
ignored, because the reader takes both line ends. Sparkles trims cells by default and
reads `#` as a comment prefix only when the dialect names it, since a `#` at the start of
a data row is more common in exports than a comment.

### 3.5 Triples

The triples follow the minimal mode of "Generating RDF from Tabular Data on the Web".
For each row of each table that is not suppressed, and each column that is not
suppressed:

- **The subject** is the column's `aboutUrl` expanded for the row, or a blank node for
  the row when the column has none. Columns of one row with the same `aboutUrl` describe
  the same resource, and columns without one share the row's blank node.
- **The predicate** is the column's `propertyUrl` expanded, or the table URL with the
  column's name as its fragment.
- **The object** is the column's `valueUrl` expanded, as an IRI, or the literal of
  §3.3. A list value gives one triple per item, or an `rdf:List` when `ordered` is true.
- A null value or an empty list gives no triple. A virtual column has no cell, and gives
  one triple per row from its `aboutUrl`, `propertyUrl` and `valueUrl`. A virtual column
  without a `valueUrl` fails at load.

URI templates are expanded by RFC 6570 at levels 1 to 4. Their variables are the
columns' names, bound to the values of the row's cells in their canonical form, and
`_row`, `_sourceRow`, `_column`, `_sourceColumn` and `_name`. A null value leaves its
variable undefined. An expanded `propertyUrl` or `valueUrl` that is a prefixed name with
a prefix of the CSVW initial context, such as `schema:name`, `rdf:type` or `xsd:date`,
expands to its IRI. The result is then resolved against the table URL, and a result that
is not an absolute IRI fails with its row and column.

**The table URL** is the table description's `url`, resolved against the metadata's
`@base` or the location of the metadata file. Without a `url`, it is the `--base` option
or `base` parameter, and otherwise the `file:` URL of the CSV file. A server upload has
no file URL, so a relative reference there fails unless the metadata has a `url` or the
request gives `base`.

### 3.6 Which table a file is

`sparkles load data.csv --mapping m.json` applies the mapping to `data.csv`. A mapping
with several tables is matched by the file name of each table's `url`, and a mapping
with one table applies to any file. `sparkles load --mapping m.json` with no CSV file
loads every table of the mapping from its `url`, resolved against the mapping's
location. Only `file:` URLs and relative paths are read. A table group cannot fetch
tables over HTTP.

Without `--mapping` or `--template`, `sparkles load` looks for `data.csv-metadata.json`
next to `data.csv`, the first location CSVW's locating rules name, and uses it when it
exists. The command prints that it did, so the choice is never silent.

## 4. The default mapping

With no mapping, the converter builds CSVW metadata from the header and two options, and
runs it through §3. The namespace `N` is `--base`, or the CSV file's `file:` URL followed
by `#`.

- Each column is a `string` column whose `propertyUrl` is `N` followed by the column's
  name. With the default namespace that is the predicate CSVW itself gives a column, the
  table URL with the name as its fragment.
- With `--key COLUMN`, every column's `aboutUrl` is `N` followed by the key's value, so
  `--base http://example.org/person/ --key id` names the rows `<http://example.org/person/7>`.
  An empty key fails the import, because two rows would merge into one subject.
- Without `--key`, the `aboutUrl` is `N` followed by `row=` and the source row number.
  With the default namespace that is the RFC 7111 IRI of the row in its file, which CSVW's
  standard mode also uses as the row's URL. The subjects stay the same when the same file
  is loaded again, and rows of different files never collide.
- An empty cell gives no triple.

`sparkles csv mapping data.csv` prints this metadata. A user who wants datatypes or other
predicates edits the printed file and passes it with `--mapping`.

Type inference was considered and rejected. A column of zip codes or product numbers
would lose its leading zeros as integers, and a column whose first thousand values look
like dates may hold free text further down. A mapping makes the types explicit.

## 5. CONSTRUCT templates

### 5.1 The query

A template is a SPARQL CONSTRUCT query. For each row, its WHERE clause starts from one
solution in which each column's variable is bound to the cell, and the template's
triples are written for each solution that the WHERE clause produces. The usual Tarql
mapping looks like this.

```sparql
PREFIX schema: <http://schema.org/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
CONSTRUCT {
  ?person a schema:Person ;
    schema:name ?name ;
    schema:birthDate ?born .
}
WHERE {
  BIND (IRI(CONCAT("http://example.org/person/", ?id)) AS ?person)
  BIND (xsd:date(?born_on) AS ?born)
}
```

- **Variables.** Each column's variable is its name from the header, with every
  character that a SPARQL variable name cannot hold replaced by `_`. Duplicate names get
  `_2`, `_3` and so on. Without a header, the columns are `?a`, `?b`, … `?z`, `?aa`, as
  in Tarql. `?ROWNUM` is the data row number as an `xsd:integer`, as in Tarql.
- **Values.** A cell is a plain string literal. An empty cell leaves its variable
  unbound, as in Tarql. With `--mapping` as well as `--template`, the mapping's dialect,
  names, null values and datatypes apply, and each variable is bound to the cell's
  typed literal, or to its IRI when the column has a `valueUrl`. A list cell binds the
  unsplit string.
- **The dataset is empty.** The WHERE clause runs against an empty dataset, so a triple
  pattern matches nothing. Lookups in the target dataset are a non-goal for this phase.
- **Blank nodes** in the template are new for each solution, as CONSTRUCT defines, so a
  blank node never joins two rows.

The query is refused before any row is read if it is not a CONSTRUCT, if it has a
`FROM` or `FROM NAMED` clause, `ORDER BY`, `LIMIT`, `OFFSET`, `GROUP BY` or an aggregate
at its top level, or if it uses `SERVICE`. Each of those would either reach outside the
import or depend on more than one row, and rows are processed in batches (§5.2).
Tarql's extension functions, such as `tarql:expandPrefix` and `apf:strSplit`, are not
provided. SPARQL's string functions, `IRI`, `ENCODE_FOR_URI` and the XSD casts cover the
usual mappings.

### 5.2 Evaluation

Tarql treats the table as a `VALUES` block at the start of the WHERE clause. Sparkles does
the same in its algebra. It parses the query once, then for each batch of 10,000 rows
puts a `VALUES` table of the batch at the start of the WHERE clause's group, joined
before the group's first element, and runs the query with the engine's evaluator. A
`BIND` or `FILTER` of the group therefore sees the row's variables, exactly as if the
`VALUES` block had been written first inside the braces.

Each batch runs with the query budgets of the caller: the memory and row budgets of
[C01](C01-observability-and-budgets.md) on the server, and none on the CLI. A batch that
exceeds a budget fails the import with the range of rows it held. Blank nodes are
relabelled per batch, so two batches never share one.

## 6. Command line

`sparkles load` takes CSV and TSV files next to RDF files. A file is CSV when its name
ends in `.csv` or `.tsv`, before any compression extension, so `orders.csv.gz` is a
gzip-compressed CSV.

```
sparkles load --loc DB people.csv --base http://example.org/person/ --key id
sparkles load --loc DB people.csv --mapping people.csv-metadata.json --graph urn:g
sparkles load --loc DB people.csv --template people.rq
sparkles load --loc DB --mapping tables.json
sparkles load --server https://sparkles.example --dataset ds people.csv --mapping m.json
```

`--mapping`, `--template`, `--base` and `--key` apply to every CSV file of the command.
`--key` conflicts with `--mapping` and `--template`. With `--server`, the CLI converts
each CSV file locally into a temporary N-Triples file and sends that, so a remote load
needs nothing new on the server and resolves relative references against the local
file.

`sparkles csv` converts without loading.

```
sparkles csv convert people.csv --mapping m.json --format ttl --output people.ttl
sparkles csv mapping people.csv --base http://example.org/person/ --key id
```

`convert` writes N-Triples to standard output by default. `--format nq` with `--graph`
writes N-Quads, and `--format ttl` writes Turtle with the prefixes of the CSVW initial
context that the output uses. The CLI code lives in its own module, so that a later
`riot`-style converter can call `sparkles::tabular` directly.

## 7. Server

`POST /{ds}/upload` takes CSV in two forms.

- **A multipart upload** whose file parts have names ending in `.csv` or `.tsv` reads
  them as CSV. A part named `mapping` holds a CSVW metadata document, and a part named
  `template` holds a CONSTRUCT query. Each is limited to 1 MiB. The mapping or template
  applies to every CSV part of the request. RDF files in the same request load with them
  in one commit.
- **A plain body** with `Content-Type: text/csv` or `text/tab-separated-values` uses the
  default mapping.

The `base` and `key` query parameters set the default mapping's namespace and key. A
request with neither a mapping nor `base` fails with `400`, because the server has no
file URL to build the default namespace from. Errors in the table or the mapping answer
`400` with the row and column. The output limit answers `413` and the disk reserve
`507`, as the other upload limits do. Dry runs ([C15](C15-write-previews.md)) work as
for any upload.

The Graph Store endpoint does not take CSV. A graph in the Graph Store protocol is RDF,
and its body has no room for a mapping. Mappings stored on the server, named by a
parameter, are a non-goal for this phase.

## 8. Rejected and deferred alternatives

- **RML, R2RML and YARRRML.** RML generalizes the W3C's R2RML from SQL tables to CSV,
  JSON and XML sources. A mapping is itself RDF, with triples maps, logical sources,
  subject and predicate-object maps, joins between triples maps, and functions through
  the Function Ontology. YARRRML is a YAML syntax that compiles to RML. Supporting them
  well means a mapping engine with joins across sources and a function library, which is
  far larger than this phase. CSVW covers one table at a time with a JSON file that
  spreadsheet users can read, and CONSTRUCT covers the rest with SPARQL, which Sparkles
  users already know. RML remains the right choice if users arrive with existing R2RML
  or RML mappings, and it is deferred until they do.
- **CSVW's standard mode.** It adds a `csvw:TableGroup`, a `csvw:Table` and a `csvw:Row`
  per row, with `csvw:describes` links and row numbers. That is provenance about the
  file, which most users drop after the import. It can be added as an option later.
- **Type inference** (§4).
- **A `Source` that holds the converter** (§2.1).
- **A per-row SPARQL query** for templates. It costs a parse-free but complete query
  evaluation per row, which is about a thousand times slower than one evaluation per
  batch of 10,000 rows.
- **Templates over the target dataset.** Lookups during the import make the result
  depend on the dataset's state, and on the server they would read under the writer's
  feet. A user can load the table first and join with a SPARQL update afterwards.
- **CSV in the Graph Store protocol** (§7).

## 9. Acceptance examples

- **T1.** `people.csv` with the header `id,name` and the rows `7,Ann` and `8,Bob`, loaded
  with `--base http://example.org/p/ --key id`, gives `<http://example.org/p/7>
  <http://example.org/p/name> "Ann"` and three more triples.
- **T2.** The same file without `--key` gives subjects `<file:///…/people.csv#row=2>` and
  `…#row=3`, and loading it twice adds nothing the second time.
- **T3.** The metadata of §3 with the row `7,Ann,03/04/1990,a;b,n/a` gives
  `"1990-04-03"^^xsd:date`, `"Ann"@en`, two `schema:keywords` triples, no
  `schema:worksFor` triple, and `rdf:type schema:Person`, all about
  `<http://example.org/person/7>`.
- **T4.** A row whose `born` value is `31/02/1990` fails with an error that names the
  file, source row 2, column 3 and `born`, and the database is unchanged.
- **T5.** A row with one cell too many fails with its row number. An unclosed quote
  fails with the row where the record started.
- **T6.** The CONSTRUCT template of §5.1 gives the same `schema:name` and
  `schema:birthDate` triples as the metadata for a file whose dates are ISO dates.
- **T7.** A template with `LIMIT 10` or `SERVICE` is refused before any row is read.
- **T8.** A file of 3 million rows converts and loads with memory that does not grow
  with the number of rows.
- **T9.** A multipart upload with `people.csv` and a `mapping` part loads the triples of
  T3. A `text/csv` body without `base` answers `400`.
- **T10.** A TSV file whose cells contain `"` loads the quotes as part of the values.

## 10. Sources

- W3C Recommendation "Model for Tabular Data and Metadata on the Web" (2015), for rows,
  columns, cells, dialects, cell parsing and the locating of metadata, cited from working
  knowledge.
- W3C Recommendation "Metadata Vocabulary for Tabular Data" (2015), for table groups,
  tables, schemas, columns, inherited properties, datatypes and their formats, and URI
  template properties, cited from working knowledge.
- W3C Recommendation "Generating RDF from Tabular Data on the Web" (2015), for the minimal
  mode and the triples of each cell, cited from working knowledge.
- RFC 4180 (CSV), RFC 6570 (URI templates), RFC 7111 (`#row=` fragments of `text/csv`)
  and the IANA registration of `text/tab-separated-values`.
- Tarql's public documentation (Apache-2.0 project), for CONSTRUCT over rows, column
  variables, `?ROWNUM`, unbound empty cells and the `VALUES` view of the table.
- The W3C R2RML Recommendation and the published RML and YARRRML specifications, at the
  level of their overviews, for the rejected alternative.
- The `csv` crate's documentation (Unlicense OR MIT).
- The Sparkles code: `io::Source`, `Store::load_with`, the server's transcoding of
  Jena's binary formats, `upload`, `BodyBudget`, `sparql::execute_query` and `xsd`, and
  the specs C01 and C15.

## Outcome

**Delivered on 2026-10-02**, as designed in one phase.

- `sparkles::tabular` holds the converter. `csvw` reads the metadata subset of §3,
  `datatype` parses cells by datatype and format, `uritemplate` expands RFC 6570
  templates, and `template` runs CONSTRUCT templates. `convert` streams a table to a
  triple sink, `write` serializes the triples, and `to_ntriples_file` writes the
  temporary N-Triples file that `source` turns into a loader `Source`. The `csv` crate
  reads the records, and a guard on its input stops a record that grows past the limit
  before it is read whole.
- `sparkles load` takes `--mapping`, `--template`, `--base` and `--key`, and
  `sparkles csv` has `convert` and `mapping`. Both live in the server crate's `csv_cmd`
  module. A later `riot`-style converter can call `tabular::write` directly.
- `POST /{ds}/upload` converts tables in `http::tabular`, after the Jena binary formats
  are transcoded and before the sources are built, so dry runs, timeouts, grants and
  write-time validation apply unchanged.

**Deviations and additions.**

- The parser projects a CONSTRUCT's WHERE clause onto its variables. The `VALUES` block
  therefore goes inside that projection, and the table's variables are added to it, so
  a template can use a column variable that its WHERE clause never names.
- Comment lines are not records, as in the `csv` crate, so `_sourceRow` and the row
  numbers of messages do not count them. Messages add the line number when it differs
  from the row number.
- `notes`, `@id`, `@type`, `primaryKey`, `foreignKeys`, `rowTitles` and common
  properties such as `dc:title` are accepted without a warning, because they describe
  the table and do not change the triples. `transformations` and properties outside the
  vocabulary are ignored with a warning.
- Turtle output declares `rdf`, `xsd`, the CSVW prefixes that the mapping's
  `propertyUrl` and `valueUrl` use, and the template's `PREFIX` declarations. The prefixes
  of the output cannot be known before it is streamed, so this replaced the design's
  "prefixes that the output uses".
- Two header cells with the same title get the names `title` and `title_2`. Template
  variables are made unique the same way, and a column named `ROWNUM` becomes
  `?ROWNUM_2`.
- `.tab` files are TSV too, and `application/csv` is a CSV media type.
- `--compression` applies to the RDF files of a load. Tables are detected by their magic
  bytes and file names. The decompressed-size limit applies to compressed tables only,
  since a plain upload is already bounded by `--max-upload-mb`.
- The upload's answer lists each table with its `file`, `rows`, `triples` and
  `warnings` under `tables`.
- Under the record limit, an unclosed quote ends at the end of the file and the rest of
  the file becomes one cell, as the `csv` crate reads it. Past the limit it fails with
  the row where the record started.

**Tests at landing.**

- `sparkles::tabular::tests` covers the acceptance examples T1 to T6 and T10, the default
  metadata that `sparkles csv mapping` prints, dialects (skipped rows and columns, two
  header rows, comments, other delimiters, no trimming, blank rows), lists and ordered
  lists, defaults, `_row` and `_column` in templates, suppressed columns, table groups,
  bad metadata, relative IRIs without a table URL, the output limit, compressed tables
  and the decompressed-size limit, a 200,000-row generated table that is never held in
  memory, and a load through the temporary file that leaves the store unchanged when the
  table fails. The `datatype`, `uritemplate` and `template` modules test number, boolean,
  string and date formats, the examples of RFC 6570 §3.2, variable names and the refused
  templates. The W3C CSVW test suite was not available offline, so these cases follow
  the spec's examples instead.
- `csv_cmd::tests` checks the conversion of a load's files, the metadata file found next
  to a table, and the tables of a mapping loaded from their `url`.
- `http::router_tests::tabular` uploads `text/csv` and TSV bodies, multipart uploads with
  a mapping or a template next to an RDF file, the refusals of a key with a mapping, a
  mapping without a table and a template with `SERVICE`, a bad cell that commits nothing,
  and the decompressed-size limit (`413`).

**Cost.** Measured with the release build on a machine with a load average of about 55,
so the times are rough. A 3,000,000-row table of 112 MiB with four columns converted to
12,000,000 triples (551 MiB of N-Triples) in 8.8 s with the default mapping, in 13.3 s
with CSVW metadata that types three columns, and in 31.4 s with a CONSTRUCT template that
casts the same three. The conversion stayed at 30 MiB of resident memory, or 114 MiB with
the template's batches, independent of the number of rows (T8). `sparkles load` of the
table took 64 s in all, against 47 s for loading the converted N-Triples file directly,
so the conversion and the temporary file cost about a quarter of the load. The temporary
file takes about five times the table's size on disk until the load ends.

**Not built.** RML, R2RML and YARRRML mappings, CSVW's standard mode, primary-key and
foreign-key checks, templates over the target dataset, Tarql's extension functions, CSV
in the Graph Store protocol, mappings stored on the server, and CSV loading in the Python
package (`Dataset.load`) were not built. Python was left out because it needs a new
method, its type stubs and tests, and the CLI covers the use until then.

**Later.** The UI's upload form at first sent no `base`, `mapping` or `template`, so a
CSV file uploaded from it was refused with `400`. Once a CSV or TSV file is chosen, the
form now shows a base IRI, an optional key column and an optional mapping or template
file. The base IRI and key go in the query string, a `.rq` or `.sparql` file goes in the
`template` part and any other file in the `mapping` part. The form explains what the
server would refuse, such as the default mapping without a base IRI or a key column next
to a mapping, and keeps the Upload button disabled until it is fixed. It remembers the
base IRI per dataset. `ui/src/lib/upload.test.ts` tests the checks and the parameters,
and `ui/tests/mock/upload-csv.spec.ts` runs the form against the mock server, which maps
tables with the default mapping.
