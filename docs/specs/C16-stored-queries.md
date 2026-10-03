# C16: Stored, parameterized queries

> **Status:** implemented
>
> **Phases:** Shipped on 2026-10-02 as one phase: the `sparkles::stored` catalog, the
> admin API and runs over HTTP, `sparkles queries`, the MCP tools and the query page's
> saved queries.
>
> **User docs:** [API: Stored queries](../API.md#stored-queries) ·
> [Usage: Stored queries](../USAGE.md#stored-queries) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This design was written from the SPARQL 1.1 Query and Protocol
specifications, Apache Jena's documentation of `ParameterizedSparqlString` and of query
execution with substitutions, the public documentation of Stardog's stored queries and
of GraphDB's saved queries, the OpenAPI notion of parameterized operations, and the
Sparkles code. It builds on commit identity ([CI](CI-commit-identity.md)), budgets
([C01](C01-observability-and-budgets.md)), access control
([C09](C09-dataset-access-control.md), [C12](C12-graph-access-control.md)) and the MCP
server ([C11](C11-mcp-server.md)).

## 1. Summary

Applications and agents that read a Sparkles dataset tend to run the same few queries
with different values: the orders of a customer, the people born after a date, the
labels of a concept. Today each client keeps its own copy of the query text and splices
values into it. That has three problems.

- **Injection.** Building a query by string concatenation lets a value that contains
  `"` or `}` change the query's shape, the SPARQL equivalent of SQL injection. Jena's
  `ParameterizedSparqlString` documentation warns about exactly this.
- **No shared definition.** A fix to a query has to reach every client. Nothing records
  who changed a query, when or why.
- **Agents see raw SPARQL only.** An MCP client gets `sparql_query` and has to write
  SPARQL itself, even for questions an operator has already answered with a reviewed
  query.

This spec adds named queries stored per dataset. Each has typed parameters, a
description and a default result format. A client runs one with
`GET /{ds}/queries/{name}?param=…`, and an MCP client sees each one as a tool whose input
schema lists the parameters.

**Goals**

1. Stored queries are managed through an admin API, `PUT`, `GET` and `DELETE
   /$/queries/{ds}/{name}`, and through the CLI.
2. Parameters are typed. A value is checked against its type, turned into one RDF term
   and bound to the query's variable as an initial binding. It never becomes query text,
   so no value can change the query's shape.
3. A run uses the caller's credentials, graph view, budgets and rate limits, exactly as
   a query sent to `/{ds}/sparql` would.
4. Every change is a new version with commit-identity style metadata: a sequence
   number, the time, the author, a message, the dataset's head commit and a digest that
   chains the versions.
5. Each stored query is an MCP tool with a JSON Schema of its parameters.
6. The UI's query page lists the stored queries, runs them with a form for their
   parameters, and saves the editor's query as a stored query.

**Non-goals.** Stored updates (parameterized writes), parameters that bind several
values at once (lists bound through a multi-row `VALUES`), per-user private queries,
query scheduling, and result caching beyond the engine's own cache.

## 2. Storage: a settings file, not a reserved graph

Stored queries live in `<db>/queries.json`, next to `validation.json`, `text.json` and
`prefixes.json`. An in-memory dataset keeps them in memory. A reserved named graph such
as `urn:x-sparkles:queries` was considered and rejected for these reasons.

- **A query is configuration, not data.** In a graph, the definitions would show up in
  `GRAPH ?g` patterns, the union graph, dumps, Graph Store reads, schema reports, the
  VoID export, the change feed and reasoning input. Each of those would need an
  exception.
- **Changing a query would be a data commit.** It would advance the commit sequence,
  pass through write-time validation, mark inferences stale and appear in diffs, none of
  which has anything to do with the data.
- **Access would follow the graph, not the role.** Anyone allowed to write the graph
  could change a query that other principals then run with their own credentials. A
  settings file changes only through the admin API, which needs `admin` on the dataset,
  as `validation.json` does.
- **History has a different lifetime.** The data's history is subject to retention and
  compaction ([F06](F06-snapshots-and-point-in-time.md)). A query's versions are small
  and stay until they are replaced, up to a fixed number.

A graph would have had two advantages: definitions would travel in N-Quads dumps, and
SPARQL could query them. The file covers the first need by being part of backups and
clones, as `validation.json` is, and `GET /$/queries/{ds}` exports the definitions. The
second need is small. Stardog keeps stored queries in its system catalog and GraphDB
keeps saved queries in its workbench settings, both outside the data, for the same
reasons.

## 3. Model

### 3.1 Definitions

```json
{
  "query": "PREFIX ex: <http://ex.org/>\nSELECT ?name WHERE { ?p ex:age ?age ; ex:name ?name FILTER(?age >= ?minAge) }",
  "description": "People at least minAge years old",
  "parameters": {
    "minAge": { "type": "integer", "default": 18, "description": "Youngest age" }
  },
  "results": "json",
  "mcp": true
}
```

| Field | Meaning |
|---|---|
| `query` | One SPARQL query (`SELECT`, `ASK`, `CONSTRUCT` or `DESCRIBE`), self-contained: the dataset's prefixes are not predeclared. Updates are refused. |
| `description` | Text shown in listings, in the UI and as the MCP tool's description. |
| `parameters` | Variables of the query, by name without `?`, with their types (§3.2). |
| `results` | The result format of runs that ask for none: `json`, `xml`, `csv` or `tsv`, or `turtle`, `ntriples`, `jsonld` or `rdfxml` for `CONSTRUCT` and `DESCRIBE`. |
| `mcp` | Whether MCP lists the query as a tool. The default is `true`. |

A name has 1 to 64 characters from `[A-Za-z0-9_-]` and starts with a letter or digit.

### 3.2 Parameters

| `type` | A value is | Term |
|---|---|---|
| `iri` | an absolute IRI, `<iri>`, or a prefixed name of the dataset's prefixes | an IRI |
| `string` | any text | a simple literal, or a language-tagged one with `language` |
| `integer`, `decimal`, `double`, `boolean`, `date`, `dateTime` | a lexical form of that XSD type | a typed literal |
| `literal` | a literal in SPARQL syntax (`"x"@en`, `"5"^^xsd:int`, `5`), or with `datatype` the lexical form of that datatype | a literal |
| `term` | an IRI, a prefixed name or a literal in SPARQL syntax | an IRI or a literal |

A parameter may also have:

- `default`, used when a run gives no value;
- `required`, which defaults to `true` without a default. An optional parameter without
  a default leaves its variable unbound;
- `enum`, the values a run may give;
- `description`.

Typed literals must be well formed, and blank nodes are never accepted. For `literal`
and `term`, the value is parsed as the only term of a `VALUES` block, and anything other
than exactly one IRI or literal is refused. A definition is refused when a parameter is
not a variable of the query, when the query assigns it (`BIND … AS ?p`, `VALUES ?p`, or
`(… AS ?p)` in a projection or aggregate), when its name is a reserved request
parameter (§4.2), or when its default or allowed values do not fit its type.

### 3.3 Binding

A run's values become `QueryOptions::initial_bindings`. The engine substitutes each term
for every occurrence of its variable in the parsed query, as Jena's
`QueryExec.substitution` does, and a projected parameter reports its value in the
results. Because the substitution happens on the algebra, a value is always one term.
The alternatives are discussed in §8.

### 3.4 Versions

Every change that alters a definition adds a version:

```ts
type Version = {
  version: number;          // 1, 2, 3, … per query
  parent?: number;          // the version it replaced
  created: string;          // RFC 3339 UTC
  author?: string;          // the principal's name
  message?: string;         // from the body's `message` or the Sparkles-Commit-Message header
  datasetCommit?: number;   // the dataset's head commit when it was saved
  digest: string;           // hex SHA-256 of the parent's digest and the definition's JSON
};
```

A `PUT` of the current definition adds no version, as a write with no net effect adds
no commit. The last 100 versions of each query are kept. `If-Match` with the version's
entity tag makes a `PUT` or `DELETE` conditional, and `If-None-Match: *` makes a `PUT`
create only.

## 4. HTTP

### 4.1 Admin API

| Method | Path | Need | Result |
|---|---|---|---|
| GET | `/$/queries/{ds}` | `read` | `{dataset, queries: Summary[]}` |
| GET | `/$/queries/{ds}/{name}` | `read` | the definition with its version; `?version=N` for an older one |
| GET | `/$/queries/{ds}/{name}/versions` | `read` | the kept versions, newest first |
| PUT | `/$/queries/{ds}/{name}` | `admin` | `201` for a new query, `200` otherwise, with `changed` |
| DELETE | `/$/queries/{ds}/{name}` | `admin` | `204`, or `404` |

`GET` answers carry `ETag: "v<version>"`. Errors are `400` for a bad definition, with
the message naming the parameter, `404` for an unknown query, `412` for a failed
precondition and `403` on a `--read-only` server.

### 4.2 Running

`GET` or `POST /{ds}/queries/{name}` runs the query. Parameter values come from the
query string, from a form body, or from a JSON object body (`application/json`), where
numbers and booleans may be JSON values. `$name=value` is accepted as well as
`name=value`. The other request parameters are those of `/{ds}/sparql`: `timeout`,
`reasoning`, `nocache`, `at`, `format`, the budget overrides and `send`, plus
`version` to run an older version. These names are reserved. An unknown parameter is a
`400` that lists the query's parameters.

The run goes through the code path of `/{ds}/sparql`. It has the same content
negotiation, budgets, timeouts, rate-limit class, metrics (`op="query"`), commit and
history headers and graph view. When the request names no format and accepts anything,
the definition's `results` decides. The response adds `Sparkles-Query-Version`.

| Status | When |
|---|---|
| `400` | A value does not fit its type, a required value is missing, or a parameter is unknown. |
| `404` | The dataset or the query does not exist, or the version is no longer kept. |

### 4.3 Permissions

Listing and reading definitions needs `read` on the dataset, through the `info`
endpoint of C12. Running needs `read` through the `query` endpoint, and the query runs
on the caller's graph view, so a principal limited to some graphs gets the answers of
those graphs. Changing definitions needs `admin`.

## 5. MCP

Each stored query that has `mcp` set and that the caller may run becomes a tool.

- **Name.** `<dataset>__<query>`, with characters outside `[A-Za-z0-9_-]` replaced by
  `_`, so that names fit the common client limit of `^[A-Za-z0-9_-]{1,64}$`. A name
  longer than 64 characters is cut and ends in a short hash. Two queries that would get
  the same name are both left out, and the server logs a warning. Built-in tools have no
  `__`, so the names never collide with them.
- **Description.** The query's description, followed by the query's kind and dataset.
- **Input schema.** One property per parameter, with `integer`, `number`, `boolean` or
  `string` as its JSON type, the parameter's description, its default and its `enum`.
  Required parameters are listed in `required`. `maxRows`, `offset`, `format`,
  `atCommit` and `timeoutSeconds` are added with the meaning they have in
  `sparql_query`.
- **Result.** As `sparql_query`: a compact table or JSON, capped and paged.
- **Listing.** `tools/list` varies with the stored queries, so its cache lifetime drops
  from an hour to a minute.

## 6. CLI

```
sparkles queries --loc DB list
sparkles queries --loc DB get NAME [--version N]
sparkles queries --loc DB put NAME --query FILE [--param NAME:TYPE[=DEFAULT]]…
                 [--description TEXT] [--results FORMAT] [--no-mcp] [--message TEXT]
sparkles queries --loc DB delete NAME
sparkles queries --loc DB run NAME [--set NAME=VALUE]… [--results FORMAT]
```

`put` can also read the whole definition as JSON with `--json FILE`. `run` prints the
results in the format of `sparkles query`.

## 7. UI

The query page gets a **Saved queries** list beside the editor. Choosing one loads its
text into the editor and shows a form with one field per parameter, filled with the
defaults. **Run** sends the form to `/{ds}/queries/{name}`. An admin can save the
editor's text with a name, a description and parameters picked from the query's
variables.

## 8. Rejected alternatives

- **Text substitution with escaping** (Jena's `ParameterizedSparqlString`). It is safe
  only if every escaping rule is right in every position, and it reparses the query on
  every run.
- **A trailing `VALUES` clause.** A `VALUES` block after the query joins with its
  results, so a `FILTER (?age >= ?minAge)` inside the pattern would see `?minAge`
  unbound and fail. A `VALUES` block in front of the pattern does not reach nested
  groups or subqueries either. Substitution reaches every occurrence.
- **Stardog-style `$name` parameters only.** They are accepted, but plain names are
  easier to read in URLs and match the parameter names in the definition.
- **A reserved graph** (§2).
- **One generic MCP tool, `run_stored_query`.** A tool per query gives the model a
  description and a typed schema for each question, which is the point of storing them.
- **Per-user saved queries.** A shared, reviewed catalog per dataset is what the use
  cases need. Personal drafts stay in the browser, as the query page's history does.

## 9. Acceptance examples

- **Q1.** `PUT /$/queries/t/adults` with the definition of §3.1 answers `201` with
  version 1. `GET /t/queries/adults` lists the people of 18 and over, and
  `?minAge=40` those of 40 and over.
- **Q2.** `?minAge=old` answers `400` naming `minAge`. A string parameter given
  `Ann" } UNION { ?s ?p ?o } #` matches nothing and returns no other data.
- **Q3.** A second `PUT` with a new description answers `200` with version 2, parent 1
  and a new digest. The same `PUT` again adds no version. `If-Match: "v1"` then answers
  `412`.
- **Q4.** A definition whose parameter is assigned by `BIND`, or that is an update,
  answers `400`.
- **Q5.** A principal limited to some graphs runs the query and gets only the answers of
  those graphs. A principal without `admin` gets `403` on `PUT`.
- **Q6.** The MCP `tools/list` includes `t__adults` with an `integer` property
  `minAge`, and calling it returns the same rows as the HTTP run.
- **Q7.** A clone and a backup of the dataset keep the stored queries.

## 10. Sources

- W3C SPARQL 1.1 Query Language (variables, `VALUES`, `BIND`, substitution in
  `EXISTS`) and SPARQL 1.1 Protocol, cited from working knowledge.
- Apache Jena documentation (Apache-2.0) of `ParameterizedSparqlString`, with its
  warning about injection, and of `QueryExecution` with substitutions and initial
  bindings, cited from working knowledge.
- Stardog documentation of stored queries, which keeps named queries in the server's
  catalog and binds `$`-prefixed request parameters, cited from working knowledge.
- Ontotext GraphDB documentation of saved queries in the workbench, cited from working
  knowledge.
- The OpenAPI Specification's model of operations with typed, documented parameters, and
  the MCP specification's tools with JSON Schema inputs, cited from working knowledge.
- The Sparkles code: `QueryOptions::initial_bindings`, the query endpoint, `auth`,
  `mcp`, `guard::config` and the backup file lists; and the specs CI, C01, C09, C11 and
  C12.

## Outcome

**Delivered on 2026-10-02**, as designed in one phase.

- `sparkles::stored` holds the catalog. `Definition::check` parses the query without
  predeclared prefixes, refuses updates, and checks each parameter: its name, that the
  query mentions it as a whole variable, that no `Extend`, `Values` or aggregate of the
  algebra assigns it, and that its default and allowed values convert. `Definition::bind`
  turns request values into terms. `literal` and `term` values are parsed as the single
  term of a `VALUES` block, and the parse is accepted only when the algebra is exactly one
  variable with one IRI or literal. `Catalog` keeps the versions in memory and writes
  `queries.json` atomically after each change, and backups and clones include the file.
- The server's `http::queries` module serves `/$/queries/{ds}`, `/$/queries/{ds}/{name}`,
  `…/versions` and `/{ds}/queries/{name}`. The query endpoint's body after the query text
  became `run_query`, which both share. It sets the bindings as
  `QueryOptions::initial_bindings` and keys the quick-query memory by the bindings too.
  The run route counts as the `query` endpoint of C12, the `query` rate-limit class and
  `op="query"` in the metrics.
- `mcp::stored` lists and runs the tools. A call becomes the arguments of `sparql_query`
  plus the bindings, so it pages, caps and renders like `sparql_query`.
- The UI's query page has a **Saved** menu, a parameter bar for a tab opened from a
  stored query, and a save dialog for admins.

**Deviations and additions.**

- A `queries.json` that cannot be read does not stop the dataset from opening. The
  server logs the error, lists no queries, and refuses changes with `409` until the file
  is fixed or removed, so a broken file is never overwritten.
- `If-None-Match: *` creates only, and `If-Match: *` requires the query to exist, as in
  HTTP. `ETag` is `"v<version>"`.
- The CLI gained `sparkles queries versions`. It works on a database directory only,
  with no `--server` mode. `put` and `delete` take the database's lock, so they refuse a
  database a server holds, and the author of a CLI version is `$USER`.
- `serve --mcp-no-stored-queries` and `sparkles mcp --no-stored-queries` leave the
  tools out. An MCP tool needs the caller's `query` endpoint as well as `read`. The
  MCP rate limit charges a stored-query call to the dataset named in its tool name.
- A stored query run through MCP keeps `sparql_query`'s limit of 65,536 characters on
  the query text.
- The UI's save dialog sets each parameter's type and default. Descriptions per
  parameter, `enum`, `results` and `mcp` are set through the API or the CLI's `--json`.

**Tests at landing.**

- `sparkles::stored::tests` checks binding by type, values that try to break out of a
  literal or a term, defaults, allowed values, the definition checks, versions with
  their digests and preconditions, persistence, and the cap of 100 versions.
- `http::queries::tests` runs definitions over HTTP: creation and runs with defaults,
  query-string, `$name`, form and JSON values, type errors, injection attempts, missing
  and unknown parameters, versions and `If-Match`, deletion, the definition's result
  format for solutions and graphs, and a clone that keeps the queries.
- `router_tests::auth::graphs::stored_queries_run_on_the_view` runs a stored query as a
  full reader and as a reader of some graphs, and checks the endpoint and `admin`
  refusals.
- `mcp::tests::stored_queries_are_tools` checks the tool listing, input schema, calls,
  argument errors and a query kept out with `mcp: false`. `stored_tool_names_fit_clients`
  checks the names.
- The UI's Vitest tests cover variable extraction, parameter forms and the save checks,
  and a Playwright test against the mock server saves a query and reopens it.
- Crate and server tests, Clippy over all targets, `mise run lint:features` and
  `mise run ci` pass.

**Cost.** A run parses its values and then follows the query endpoint's path. On 1.18M
triples, with the release build on a machine with a load average of about 30, a stored
query that filters by two parameters and returns 100 rows took a minimum of 7.7 ms over
HTTP, against 6.8 ms for the same query with constants sent to `/{ds}/sparql`. Both
results bypassed the result cache. The medians were too noisy on that machine to compare.

**Not built.** Stored updates, parameters that bind several values, per-user queries and
a `--server` mode for `sparkles queries` were not built.

**Later additions (2026-10-03).** MCP clients learn that a stored query was saved or
deleted through `notifications/tools/list_changed`, each stored query offered as a tool
is also a resource with its definition, and the `run_stored_query` prompt explains its
parameters ([C11](C11-mcp-server.md#outcome)).
