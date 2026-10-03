# C03: GraphQL read adapter

> **Status:** implemented in part (Phase 1)
>
> **Phases:** Phase 1 shipped on 2026-10-02: the adapter, the schema configuration,
> schema drafts, `/{ds}/graphql` and the CLI. Of Phase 2, the MCP tool `graphql_query`
> came on 2026-10-03 with [C11](C11-mcp-server.md#outcome), and the UI and stored
> GraphQL queries are not built. Phase 3 is subscriptions, built only if a workload asks
> for them.
>
> **User docs:** [API: GraphQL](../API.md#graphql) · [Usage: GraphQL](../USAGE.md#graphql) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This design was written from the GraphQL specification (the October
2021 edition and the current working draft), the GraphQL over HTTP working draft, the
GraphQL Cursor Connections specification, GraphQL-LD and its `graphql-to-sparql`
library, HyperGraphQL, the public documentation of Stardog's GraphQL support and of
Ontotext's Semantic Objects (the SOML-based GraphQL service for GraphDB), the
conventions of Apollo, Hasura, PostGraphile and GitHub's GraphQL API, the W3C Bridging
GraphQL and RDF Community Group, published work on the semantics and cost of GraphQL
queries, the documentation of the Rust crates `apollo-compiler`, `async-graphql` and
`juniper`, and the Sparkles code. It builds on schema discovery and drafted shapes
([C02](C02-schema-discovery.md)), budgets ([C01](C01-observability-and-budgets.md)),
access control ([C09](C09-dataset-access-control.md), [C12](C12-graph-access-control.md)
and the protections of triples that C12's second phase adds), write-time validation
([C10](C10-write-time-validation.md)), stored queries ([C16](C16-stored-queries.md)),
point-in-time reads ([F06](F06-snapshots-and-point-in-time.md)) and the MCP server
([C11](C11-mcp-server.md)).

## 1. Summary

Front-end and application developers often know GraphQL and not SPARQL. To read a
Sparkles dataset today they have to learn SPARQL, write one query per screen, and turn
flat result rows back into the nested objects their code wants. Tools such as GraphQL
code generators, typed clients and schema explorers do not work with SPARQL at all.

This spec adds a read-only GraphQL endpoint per dataset, `/{ds}/graphql`. An
administrator installs a GraphQL schema whose types and fields are mapped to RDF classes
and predicates with `@rdf` directives. The server can draft that schema from the
dataset's SHACL shapes or from the observed schema report, and the administrator reviews
the draft before installing it. A request is parsed and validated against the schema,
compiled to SPARQL algebra, and run by the existing engine on one snapshot, with the
caller's credentials, graph view, budgets and rate limits.

**Goals**

1. A GraphQL query over any installed schema runs as a fixed number of engine
   executions. The number depends on the shape of the document, never on the number of
   nodes in the answer, so there is no N+1 problem.
2. The schema states only what the data guarantees. A field is non-null only when a
   write-time guard enforces a value, and the server says so when an administrator
   declares more.
3. Every request runs as the caller, with the same graph views, protections, budgets and
   rate limits as a SPARQL query, because every engine execution carries the caller's
   `QueryOptions`.
4. Pagination is cursor-based and exact. A cursor names the commit it was issued at, so
   the next page reads the same state.
5. Values never become query text. Arguments and variables are converted to RDF terms
   and placed in the algebra, as stored query parameters are in C16.
6. A request that is too large is refused before it runs, with an estimate of its size.
   One that grows too large while it runs fails as a whole, as a SPARQL query does.
7. The endpoint follows the GraphQL over HTTP draft, supports full introspection, and
   works with standard clients and with GraphiQL-style tools.

**Non-goals.** Mutations (§12), subscriptions in Phase 1 (§13), Apollo Federation,
schema stitching across datasets, aggregates beyond `totalCount`, a schema that changes
by itself when the data changes, and a SPARQL passthrough field.

## 2. Overview

A request goes through five steps.

1. **Parse and validate.** The document is parsed and validated against the API schema
   (§3.4) with `apollo-compiler`. Variables are coerced to their declared types. The
   result of these steps is cached by the hash of the document and the schema version.
2. **Estimate.** The planner computes the document's depth and an upper bound on the
   number of nodes it can return (§7.1). A document over a limit is refused here.
3. **Plan.** The planner splits the selection tree into fetch groups (§6.2). Each group
   becomes one `spargebra::Query` built directly as algebra. Introspection fields are
   answered from the schema and never reach the engine.
4. **Execute.** The groups run in order, parents before children, on one
   `Arc<Snapshot>`. Each run goes through `sparql::execute_query` with the caller's
   options. The ids of a group's nodes seed its child groups.
5. **Assemble.** The rows of all groups are merged into the response tree in document
   order. Values are converted to GraphQL scalars (§4.3), and execution errors are
   recorded with their paths (§9).

The adapter is a new crate, `sparkles-graphql`, so that the GraphQL parser stays out of
the core engine crate. The server and the CLI use it behind a default-on `graphql`
feature.

## 3. Schemas

### 3.1 The mapping schema is configuration

The installed schema lives in `<db>/graphql.json`, next to `queries.json` and
`validation.json`. An in-memory dataset keeps it in memory. It is not stored in a graph,
for the reasons C16 §2 gives for stored queries. A schema is configuration, a change to
it must not be a data commit, and only `admin` may change it. Backups and clones carry
the file.

```json
{
  "format": 1,
  "sdl": "schema @rdf(vocab: \"http://example.org/\") @prefix(name: \"ex\", iri: \"http://example.org/\") { query: Query }\ntype Person @rdf(iri: \"ex:Person\") { … }",
  "dataGraph": "default",
  "reasoning": null,
  "introspection": true,
  "persistedOnly": false,
  "limits": { "maxDepth": 12, "maxNodes": 100000, "defaultFirst": 100, "maxFirst": 1000 }
}
```

| Field | Meaning |
|---|---|
| `sdl` | The mapping schema, in GraphQL SDL with the directives of §3.2. |
| `dataGraph` | `"default"`, `"union"` or a list of graph IRIs, chosen as in C10's `dataGraph`. Every query of the adapter reads this dataset, intersected with the caller's view. |
| `reasoning` | `true` or `false` to fix whether the inferred graph is included, or `null` to follow the SPARQL endpoint's default. |
| `introspection` | Whether `__schema` and `__type` are answered (§8). The default is `true`. |
| `persistedOnly` | When `true`, `/{ds}/graphql` runs only stored GraphQL queries for callers without `admin` (§11). |
| `limits` | The limits of §7, each at most the server's own. |

Every change adds a version with the metadata of C16 §3.4: a number, the parent, the
time, the author, a message, the dataset's head commit and a chained digest. The last
20 versions are kept. A `PUT` of the current configuration adds no version.

### 3.2 Directives

The mapping schema uses four directives. They are declared by the server, so the SDL
does not declare them.

```graphql
directive @prefix(name: String!, iri: String!) repeatable on SCHEMA
directive @rdf(
  iri: String          # class, predicate or enum value IRI, absolute or prefixed
  vocab: String        # on SCHEMA: the namespace of unmapped names
  inverse: Boolean = false
  subclasses: Boolean = true
) on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION | ENUM_VALUE
directive @lang(prefer: [String!]!) on SCHEMA | FIELD_DEFINITION
directive @single(onMany: OnMany = ERROR) on FIELD_DEFINITION
enum OnMany { ERROR MIN }
```

- `@prefix` declares a prefix for the IRIs written in the SDL. The SDL is
  self-contained. The dataset's own prefixes are not predeclared, as in C16 §3.1,
  because a change to them must not change the meaning of an installed schema.
- `@rdf(iri:)` on an object type or interface names its class. On a field it names the
  predicate, and `inverse: true` reads the predicate from object to subject. On an enum
  value it names the IRI the value stands for.
- `@rdf(vocab:)` on the schema maps every type and field without its own `@rdf` to the
  vocabulary IRI followed by its name. This is JSON-LD's `@vocab` idea, as GraphQL-LD
  uses it through a JSON-LD context. Without `vocab`, every mapped type and every field
  of a mapped type must carry `@rdf`, except `id`.
- `subclasses: false` on a type makes its members the direct instances of the class
  instead of the instances of the class and its subclasses (§5.1).
- `@lang` sets the language preference of string fields (§4.5).
- `@single` changes what a single-valued field does when it finds several values
  (§4.4). It is the only way to relax that check, and it must be written by hand.

A field typed with an object type, an interface or a union is an object field, whose
values are IRIs or blank nodes. A field typed with a scalar or an enum is a value field.
The field `id: ID!` of every mapped type is the node itself and has no predicate. The
server adds it to a mapped type that does not declare it.

### 3.3 Validation of a mapping schema

A `PUT` parses the SDL and checks it. It is refused with `400` and a message that names
the type, field and line when:

- the SDL does not parse or does not validate as a GraphQL schema;
- a mapped type or field has no IRI and the schema has no `vocab`;
- an IRI is not absolute after prefix expansion, or uses an undeclared prefix;
- a field declares arguments, since the server generates them (§5);
- a field uses a scalar the adapter does not know (§4.3), or an enum has a value
  without an IRI;
- a name collides with a generated name (§4.1), such as a type named `PageInfo` or
  `PersonFilter`, or a field named `id` with a predicate;
- `inverse: true` is set on a value field, since literals cannot be subjects;
- a hand-written `Query` field has a type the planner cannot serve (§5.1).

The `PUT` also returns warnings that do not block it. The main one concerns non-null
fields. A field declared non-null, such as `name: String!`, is backed only when the
dataset's write-time guard runs in `reject` mode with a `strict` baseline, covers the
adapter's `dataGraph`, and has a shape that targets the type's class (or a superclass)
with `sh:minCount 1` on the field's path. Without that, the warning says that a node
without a value will make the field an execution error at run time (§9.2). The adapter
never removes a `!` the administrator wrote, and the drafts of §3.5 write one only when
it is backed.

### 3.4 The API schema

The schema a client sees is derived from the mapping schema. The server adds:

- the `Node` interface with `id: ID!`, implemented by every mapped object type, and a
  `Resource` type that implements it with `id` and `_types: [ID!]!` (§5.4);
- the scalars of §4.3 that the schema uses, each with `@specifiedBy` naming the XSD
  datatype's definition;
- for each mapped type `T`, the input types `TFilter` and the enum `TOrderBy` (§5.5,
  §5.6), and the types `TConnection` and `TEdge` (§5.2);
- the arguments of list fields and language-sensitive fields (§5.3);
- the `Query` type of §5.1 when the SDL has none, and the `node` field in any case.

The mapping directives are not visible through introspection, because the October 2021
specification exposes no applied directives. `GET /{ds}/graphql/schema` returns the API
schema as SDL without them, and `GET /$/graphql/{ds}` returns the mapping schema with
them.

### 3.5 Drafts

`GET /$/graphql/{ds}/draft` drafts a mapping schema. It never installs anything. A draft
is SDL with comments that explain each decision, for an administrator to edit and
install with `PUT /$/graphql/{ds}`. The two sources are:

**From SHACL shapes** (`source=shapes`). The shapes are the dataset's write-time guard
shapes by default, or the named graph given as `shapesGraph`.

| SHACL | GraphQL |
|---|---|
| A node shape with `sh:targetClass C`, or a node shape that is also an `rdfs:Class` | An object type with `@rdf(iri: C)`. Shapes without a class target are skipped with a comment. |
| A property shape with an IRI `sh:path` | A field. `sh:inversePath` gives a field with `inverse: true`. Other paths are skipped with a comment. |
| `sh:name`, `sh:description`, `sh:order` | The field's name, description and position |
| `sh:maxCount 1` | A single value, `T`. Otherwise a list, `[T!]!`. |
| `sh:minCount ≥ 1` | `T!`, only when the guard enforces the shape as §3.3 describes. Otherwise a comment. |
| `sh:datatype D` | The scalar for `D` (§4.3) |
| `sh:class K` or `sh:node` with a class-targeted shape | The type of `K` |
| `sh:nodeKind sh:IRI` or `sh:BlankNodeOrIRI` without a class | `Resource` |
| `sh:in` of IRIs | An enum, one value per IRI, with `@rdf(iri:)` on each value |
| `sh:languageIn` | `@lang(prefer:)` with the listed languages, then `"*"` |

Shapes for a class and for its subclass give two object types. The drafter writes no
interfaces, because a class with instances of its own needs an object type, and GraphQL
keeps object types and interfaces apart. An administrator can add an interface by hand
and list it in `implements`.

**From the data** (`source=observed`). The drafter first drafts shapes with C02 §11 at
the requested `support`, which defaults to 1, and then applies the table above with two
differences. No field is non-null, because nothing enforces an observation. A field whose
values were observed at most once per instance becomes single-valued, and the draft says
above it `# observed at most one value per instance at commit N; nothing enforces this`.
If a second value appears later, the field reports `MULTIPLE_VALUES` (§4.4) until the
administrator changes it to a list. Single-valued drafts trade that risk for the types
developers expect: `name: String` instead of `name: [String!]!`. The administrator sees
the trade in the comment and decides.

The drafter takes the selection parameters of C02 (`graph`, `reasoning`, `class`,
`minInstances`) and the caller's view, and needs `read` through the `info` endpoint. It
names types and fields with §4.1 and writes a `@prefix` for every prefix of the dataset
it uses. A draft of a dataset without classes is an empty schema with a comment.

## 4. Mapping rules

### 4.1 Names

GraphQL names match `[_A-Za-z][_0-9A-Za-z]*`, and names that start with `__` are
reserved for introspection. The drafter derives a name from an IRI in four steps.

1. Take the local name, the part after the last `#`, `/` or `:`. An empty local name
   uses the whole IRI.
2. Replace each character outside `[_0-9A-Za-z]` with `_`, collapse runs of `_`, and
   reduce leading underscores to one. Put `_` in front of a leading digit. Case is kept,
   so `foaf:Person` gives `Person` and `schema:birthDate` gives `birthDate`.
3. If two IRIs give the same name in one scope, both names take the dataset prefix of
   their IRI and an underscore in front, as Stardog writes `foaf_name`. The scopes are
   the whole schema for types and enums, and one type for fields.
4. If names still collide, or an IRI has no prefix, the name ends with `_` and the first
   six hexadecimal digits of the SHA-256 of the IRI.

The generated names are reserved. A type cannot be named `Query`, `Node`, `Resource`,
`PageInfo` or a scalar of §4.3, or end in `Connection`, `Edge`, `Filter` or `OrderBy`
when the stem is a mapped type. A field cannot be named `id` unless it is the node's id.
A colliding IRI takes step 3. Inverse fields are named after `owl:inverseOf` when the
data declares an inverse property, and otherwise after the predicate with `Of` appended,
so the inverse of `ex:author` is `authorOf`. Hand-written SDL may use any valid name.

### 4.2 Identifiers

`id` is the node's IRI, always absolute, never compacted. A blank node's `id` is the
label the SPARQL endpoint reports for a stored blank node (`_:b…`), which the engine
accepts back as that node, so a blank node can be looked up by its `id` with the same
stability it has in SPARQL results. An `ID` argument accepts an absolute IRI, a
prefixed name with a prefix of the schema, or a stored blank node label. Anything else is
a `BAD_USER_INPUT` error.

### 4.3 Scalars

| Datatype or term | GraphQL scalar | Serialized as |
|---|---|---|
| `xsd:string`, a simple literal | `String` | its lexical form |
| `rdf:langString`, `rdf:dirLangString` | `String`, or the `LangString` object type | its lexical form, or `{ value, language, direction }` |
| `xsd:boolean` | `Boolean` | `true` or `false` |
| `xsd:integer` and every derived integer type | `Int` | a JSON number. A value outside the 32-bit range is an `INVALID_VALUE` error, as the specification requires. |
| the same datatypes | `Integer` | a JSON string holding the canonical lexical form, exact at any size |
| `xsd:decimal` | `Decimal` | a JSON string holding the canonical lexical form |
| `xsd:double`, `xsd:float` | `Float` | a JSON number. `NaN` and the infinities are `INVALID_VALUE`. |
| `xsd:dateTime`, `xsd:dateTimeStamp` | `DateTime` | the lexical form, which keeps a missing time zone missing |
| `xsd:date`, `xsd:time` | `Date`, `Time` | the lexical form |
| `xsd:duration` and its two subtypes | `Duration` | the lexical form |
| `xsd:anyURI`, or an IRI in a value field | `IRI` | the IRI as a string |
| any other datatype, such as `geo:wktLiteral` or `spk:vector` | `String` | the lexical form |
| any term | `RDFTerm` object type | `{ kind, value, datatype, language, direction }` |

A value field of type `Int`, `Float` or `Boolean` reads values of the matching
datatypes. The drafts map `xsd:integer` to `Int` because Turtle writes every integer
literal as `xsd:integer`, and a schema whose ages and counts are strings is hard to use.
The draft from data picks `Integer` for a predicate with an observed value outside the
32-bit range. `xsd:decimal` maps to `Decimal`, a string, because a JSON number loses
exactness in most clients, and decimals usually hold money or measurements where that
matters. An administrator may map a decimal predicate to `Float` instead.

A value that does not fit its field is an `INVALID_VALUE` execution error for that
field: a literal in an object field, an IRI or blank node in a scalar field other than
`IRI`, an ill-formed literal, a literal of a datatype that does not convert, or a number
out of range. Values are never dropped silently. A guard with `sh:datatype` keeps such
values out of the data, and the error names the node, the predicate and the value when
it is not.

Enum fields read IRIs, mapped through the values' `@rdf(iri:)`. An IRI without a value
is `INVALID_VALUE`.

### 4.4 Single and multiple values

A field typed `T` or `T!` is single-valued. A field typed `[T!]!` is multi-valued and
returns its values as a list, with an empty list for none. The adapter accepts `[T!]`
and `[T]` in hand-written SDL as well and treats them like `[T!]!`.

RDF has no single-valued properties unless something enforces them. A single-valued
field that finds several distinct values for one node is a `MULTIPLE_VALUES` execution
error at that field, with the node and the number of values. The field becomes null and
the error propagates as the specification says (§9.2). `@single(onMany: MIN)` returns
the smallest value in SPARQL's `ORDER BY` order instead, which is deterministic. It is
never written by the drafter, so a lenient field is always a decision someone made in
the SDL.

Order of list values follows the field's `orderBy` argument, or the engine's term order
when there is none, which makes responses repeatable at one commit.

### 4.5 Language-tagged literals

String fields over predicates whose values may carry language tags get a `lang`
argument, `lang: [String!]`. It lists language ranges in order of preference, matched
with basic filtering as in RFC 4647 and SPARQL's `langMatches`. `"*"` matches any tag,
and `""` matches literals without one. For each node the field returns the values of the
first range that has any. A single-valued field with two values in that range, such as
two English labels, is `MULTIPLE_VALUES` as in §4.4.

Without the argument, the field's `@lang(prefer:)` applies, then the schema's, then
`["*"]`. The drafts write `@lang(prefer: ["en", "", "*"])` on the schema when the data
has English labels, as a starting point the administrator can change. The adapter does
not read `Accept-Language`, because a response that varies with a header the request
cache does not key on is hard to reason about.

A field typed `LangString` or `[LangString!]!` returns the tag and the base direction of
each value, for clients that need them.

### 4.6 Inverse relations

`@rdf(iri: "ex:author", inverse: true)` on `Book.writtenBy`'s counterpart,
`Person.authorOf: [Book!]!`, reads `?book ex:author ?person` from the person's side.
Inverse fields are always object fields. They take the same arguments as forward ones,
and the planner compiles them to the same scans with subject and object swapped, which
the engine's permutations serve as cheaply as forward ones.

## 5. The query API

### 5.1 Root fields

When the SDL has no `Query` type, the server generates one. For each mapped object type
`T`, it adds a lookup field named after the type with a lowercase first letter, and a
collection field named `all` followed by the type name:

```graphql
type Query {
  person(id: ID!): Person
  allPerson(filter: PersonFilter, orderBy: [PersonOrderBy!], first: Int, after: String,
            last: Int, before: String, offset: Int): PersonConnection!
  node(id: ID!): Node
}
```

Names are not pluralized, because English plurals of arbitrary class names go wrong and
the administrator can rename them. A hand-written `Query` type may name its fields
freely. The planner recognizes them by their types: a field of type `T` with an `id: ID!`
argument is a lookup, a field of type `TConnection!` is a collection with cursors, and a
field of type `[T!]!` is a collection without them, with `filter`, `orderBy`, `first`
and `offset`.

The members of a type are the nodes with an `rdf:type` whose class is the type's class
or reaches it over `rdfs:subClassOf` in the data, as SHACL defines the instances of a
class. This is the default because drafts from shapes target SHACL instances. With
`subclasses: false` only direct types count. When the dataset's reasoning overlay is
active, the materialized types give the same answer.

A lookup returns null when the node is not a member of the type, whether it does not
exist, has another type, or is hidden from the caller. The three cases cannot be told
apart. `node(id:)` returns the node as the mapped type that §5.4 chooses for it, or as
`Resource` when it belongs to no mapped type. It returns null only for a node that
appears in no visible triple.

### 5.2 Connections

Collection fields return connections as the Cursor Connections specification defines
them, with the `nodes` shortcut that PostGraphile and GitHub add and a `totalCount`:

```graphql
type PersonConnection { edges: [PersonEdge!]! nodes: [Person!]! pageInfo: PageInfo! totalCount: Int! }
type PersonEdge { cursor: String! node: Person! }
type PageInfo { hasNextPage: Boolean! hasPreviousPage: Boolean! startCursor: String endCursor: String }
```

`first` and `last` must be between 0 and `maxFirst`. A negative value is
`BAD_USER_INPUT`, as the connections specification requires, and a value above the
limit is too. Without either, the page has `defaultFirst` items. `after` and `before`
take cursors from this connection, and `offset` skips items after them.

`totalCount` is computed only when selected. It costs one more engine execution, a
`COUNT` over the collection's pattern without the slice.

**Cursors.** A cursor is base64url JSON:

```json
{ "v": 1, "c": 1287, "h": "4f2a…", "o": 200 }
```

`c` is the commit the page was read at. `h` hashes the field's path in the document, its
arguments other than the cursors and slices, and the schema version. `o` is the
position after the item. A request with a cursor reads at the cursor's commit, through
the point-in-time reads of F06, so items do not shift or repeat between pages while the
data changes. Every cursor in one request must name the same commit, and a request with
`at` must agree with them, or the request is `CURSOR_INVALID`. A cursor whose `h` does
not match its field is `CURSOR_INVALID` too.

A commit that is no longer readable answers `CURSOR_EXPIRED`, with the oldest readable
commit in `extensions`. Commits since the last compaction are always readable, and the
retention window of F06 keeps older ones, so an operator who needs long-lived cursors
sets a retention window. Positions are offsets in the ordered result at that commit.
The engine sorts the whole collection to serve any page anyway, and repeated pages of
one collection at one commit hit the result cache. The tradeoff is that a deep page
costs as much as reading every item before it when the cache is cold. Keyset seeks that
avoid this would need the engine to seek on arbitrary sort keys, and §17 explains why
that was not chosen.

Cursors are not signed. Forging one only selects another position or commit, and any
caller with `read` may already read past commits with `at`.

### 5.3 Arguments of nested fields

A multi-valued object field takes `filter`, `orderBy`, `first` and `offset`, the
arguments of a plain list collection. Nested fields return lists, not connections, in
Phase 1. Per-parent cursors are an open question (§19). `first` defaults to
`defaultFirst` and is limited by `maxFirst`, so a property with ten thousand values per
node never returns all of them by accident. A multi-valued value field takes `first`,
`offset`, and `orderBy` with the value `ASC` or `DESC`. Fields over language-tagged
values take `lang` as §4.5 describes.

### 5.4 Interfaces, unions and type conditions

A field typed with an interface or a union returns, for each node, the first object type
in SDL order that is a possible type of the field and whose class the node is a member
of. A node that matches none is an `UNRESOLVED_TYPE` execution error, except under
`Node`, where it becomes `Resource`. `Resource` exposes the node's `id` and its direct
`rdf:type` IRIs as `_types`.

An inline fragment or fragment spread with a type condition selects its fields only for
nodes that are members of that type's class. The planner adds a membership test for each
type condition to the group's pattern as an optional flag column (§6.2). `__typename`
is answered from the same decision and costs nothing more.

### 5.5 Filters

`filter` takes the type's `TFilter` input:

```graphql
input PersonFilter {
  and: [PersonFilter!]  or: [PersonFilter!]  not: PersonFilter
  id: IDFilter
  name: StringFilter     age: IntFilter     email: StringFilter
  knows: PersonFilter    worksFor: OrganizationFilter
}
input StringFilter {
  eq: String  ne: String  in: [String!]  notIn: [String!]  lt: String  lte: String
  gt: String  gte: String  startsWith: String  contains: String
  regex: String  flags: String  lang: String  exists: Boolean
}
```

Numeric, date and time filters have `eq`, `ne`, `in`, `notIn`, `lt`, `lte`, `gt`, `gte`
and `exists`. `IDFilter` has `eq`, `in` and `notIn`. Enum filters have `eq`, `in` and
`notIn`. The operators use the camelCase names common in GraphQL APIs, close to
PostGraphile's filter plugin, rather than Hasura's `_eq` form.

The semantics follow SPARQL, so a filter means what the same `FILTER` would mean.

- The fields of one filter object combine with `and`. Each operator of a field filter
  combines with `and` too.
- A filter on a field means that some value of the field satisfies it, as Ontotext's
  filters and SPARQL's `EXISTS` do. `exists: false` means the field has no value. A
  filter on an object field applies the nested filter to some value.
- Comparisons use SPARQL's operators on typed values, so `age: { gt: 30 }` compares
  numbers, and a value that cannot be compared does not match.
- `regex` takes SPARQL's `REGEX` pattern and flags. The pattern is a term, not query
  text, and the engine's regex limits apply.
- `lang` restricts the values compared to a language range.

Each filter compiles to a `FILTER` or `FILTER EXISTS` in the group's algebra. Values
from arguments and variables become literals or IRIs of the field's type. A string such
as `Ann" } UNION { ?s ?p ?o } #` is one literal and matches nothing else.

### 5.6 Ordering

`orderBy` takes a list of the type's `TOrderBy` enum, whose values are each orderable
field followed by `_ASC` or `_DESC`, as in PostGraphile, plus `ID_ASC` and `ID_DESC`.
Only single-valued value fields are orderable, as in Ontotext's service, because the
order of a node with several values is not defined. The planner appends `ID_ASC` to every
order, so the order is total and pages are exact. Nodes without a value sort as SPARQL
sorts unbound values, first in ascending order. Values of different datatypes sort in the
engine's `ORDER BY` order. Without `orderBy` the order is `ID_ASC`.

## 6. Compilation and execution

### 6.1 One snapshot

All groups of a request run on one `Arc<Snapshot>`: the head, the state named by the
request's `at` parameter, or the cursors' commit. The response carries
`Sparkles-Commit`, and `extensions.sparkles.commit` repeats it for clients that cannot
read headers.

### 6.2 Fetch groups

The planner walks the selection tree after fragments are inlined and `@skip` and
`@include` are applied with the request's variables. It starts a fetch group at each root
field, each multi-valued field and each object field. A group is one SPARQL query.

- **Single-valued value fields** of a group's nodes join the group's own query, each as
  an `OPTIONAL` with one triple pattern. Each adds a column, not rows, as long as the data
  has at most one value. If a node comes back on more than one row, the extra values
  decide `MULTIPLE_VALUES` for the fields that differ.
- **Multi-valued value fields** of a group become one child group per parent group. Its
  query is a `UNION` with one branch per field, each branch tagged with a constant field
  number by `BIND`. The rows are the sum of the fields' values, not their product.
- **Object fields**, single or multi-valued, become a child group each. The child's
  query starts from `VALUES ?parent { … }` with the ids of the parent group's nodes,
  joins the field's triple pattern, applies the field's filter and order, and adds the
  child's own single-valued fields as `OPTIONAL`s.
- **Type conditions** add an `OPTIONAL` membership test that binds a flag per type.
- **`totalCount`** adds a group with a `COUNT`.

The number of engine executions is the number of groups, which the document fixes. A
query for 1,000 people with their names and the names of the people they know runs two
queries, where a resolver per field would run 1,001.

Child groups order their rows by parent and then by the field's order. The assembler
applies a nested field's `first` and `offset` per parent while it reads the rows, since
SPARQL has no per-group `LIMIT`. The engine therefore produces every value of the field
for every parent. The node estimate of §7.1 counts the values the response can hold,
and the engine's row budgets limit the values it produces. A per-group top-k operator
in the engine would remove that cost. It is in Phase 2.

Phase 1 passes parent ids as terms in `VALUES`, which the engine looks up in its
dictionary again. Phase 2 adds `QueryOptions::seed`, a table of engine ids bound to a
variable before planning, so the lookup disappears.

The queries are built as `spargebra` algebra, never as text. Each one can be rendered as
SPARQL for explanation (§6.4).

### 6.3 Engine options and shared budgets

Every group runs with the `QueryOptions` the SPARQL endpoint would build for the same
caller and request: the graph view of C12 and the protections that follow it, the
reasoning overlay, RDFS on read, the outbound policy (no group uses `SERVICE`), the
budgets of C01 and the cancel flag of the connection. Three values are shared by the
groups of a request, so that splitting a request into groups does not multiply its
allowance.

- **Time.** One deadline is fixed when the request starts. Each group gets the time that
  remains.
- **Rows produced.** One counter, as the WHERE clauses of an update share one. Phase 1
  adds `QueryOptions::work`, an `Arc` counter shared like `outbound_budget`.
- **Result bytes.** The serialized response counts against `--max-result-mb`.

The memory and intermediate-row budgets apply to each group, as they apply to each
query.

### 6.4 Explanation

With `explain=true` in the query string, the response adds
`extensions.sparkles.plan`: each group's path in the document, its SPARQL text, its
parent group, its row count and its time. For a caller with a restricted view, the plan
carries no estimates, as C12 §7.3 requires. Stardog's `graphql explain` command offers
the same view of its translation, and it is the first thing a developer needs when a
GraphQL query is slow.

### 6.5 Caching

Parsed and validated documents are cached by the SHA-256 of the document text, the
operation name and the schema version, in an LRU of 256 entries per dataset. Each group
goes through the engine's result cache like any query. Root groups repeat across
requests and hit it. Child groups usually differ by their `VALUES` and rarely do.

## 7. Limits

GraphQL lets a short document ask for a very large response. Hartig and Pérez showed
that the size of a GraphQL response can grow exponentially with the nesting of the
query, and that the size can be computed beforehand in polynomial time. GitHub's API
limits requests by the nodes they can return. The adapter combines both ideas: it bounds
every list, computes an upper bound before running, and counts again while running.

### 7.1 Before execution

| Limit | Default | Error |
|---|---|---|
| Document size | 64 KiB | `400`, `GRAPHQL_PARSE_FAILED` |
| Depth of the selection tree, outside introspection | `maxDepth`, 12 | `QUERY_TOO_COMPLEX` |
| Fetch groups | 64 | `QUERY_TOO_COMPLEX` |
| Estimated nodes | `maxNodes`, 100,000 | `QUERY_TOO_COMPLEX` |
| `first`, `last` | `maxFirst`, 1,000 | `BAD_USER_INPUT` |

The estimate follows GitHub's rule. Each list contributes its `first` or `last`, or
`defaultFirst`, times the estimate of its parent, and a single-valued object field
contributes one per parent. A query for 50 people with 20 friends each, and 10 friends
of each friend, estimates 50 + 1,000 + 10,000 = 11,050 nodes. Aliases count each
occurrence, so aliasing a field a thousand times multiplies the estimate instead of
evading it. The error reports the estimate and the limit, and the path that contributes
most.

The server's flags `--graphql-max-depth`, `--graphql-max-nodes`,
`--graphql-default-first` and `--graphql-max-first` set the ceilings. A schema's
`limits` may lower them but not raise them.

### 7.2 During execution

The assembler counts the nodes it adds to the response, and reaching `maxNodes` fails
the request. The estimate is an upper bound, so this count only guards against a
mistake in the estimate. The budgets of §6.3 apply as for SPARQL. Any of these
failures fails the whole request with no `data`, as a SPARQL query over its budget
returns no partial result. GraphQL allows partial results, but a response cut off by a
budget would look complete to a client that does not check `errors`.

Requests count against the `query` rate-limit class, as one request each, whatever their
number of groups.

## 8. Introspection

The adapter answers `__schema`, `__type` and `__typename` from the API schema with
`apollo-compiler`'s introspection support. These fields never reach the engine and do not
count against depth or nodes, because the standard introspection query of GraphiQL is
deeper than useful data queries. Introspection documents are limited to 64 KiB like any
other.

The schema is the same for every caller. A caller restricted to some graphs sees every
type, including types whose instances it cannot read. The schema is configuration, as
the definitions of stored queries are, and it is visible to anyone who may use the
endpoint. With `introspection: false`, `__schema` and `__type` are refused with a
validation error for callers without `admin`, which some operators want in production.
`__typename` stays, because clients need it.

## 9. Errors

### 9.1 Response format

Errors follow the GraphQL specification. Each has `message`, `locations` and `path`
where they apply, and `extensions.code` from this list:

| Code | Meaning |
|---|---|
| `GRAPHQL_PARSE_FAILED` | The document does not parse. |
| `GRAPHQL_VALIDATION_FAILED` | The document does not validate against the API schema. |
| `BAD_USER_INPUT` | A variable or argument does not coerce, an `ID` is malformed, or `first` is out of range. |
| `QUERY_TOO_COMPLEX` | A limit of §7.1. `extensions` has `limit`, `estimate` and `path`. |
| `PERSISTED_QUERY_REQUIRED` | The schema is `persistedOnly` and the request is not a stored query. |
| `CURSOR_INVALID`, `CURSOR_EXPIRED` | §5.2. |
| `BUDGET_EXCEEDED` | A budget of §6.3. `extensions` has the C01 fields `budget`, `limit` and `requested`. |
| `TIMEOUT` | The deadline passed. |
| `MULTIPLE_VALUES`, `MISSING_VALUE`, `INVALID_VALUE`, `UNRESOLVED_TYPE` | Execution errors of one field (§4, §5.4). `extensions` names the node and the predicate. |

The first three codes are Apollo Server's, and `PERSISTED_QUERY_REQUIRED` follows the
pattern of its persisted-query codes, so clients that already handle them need no
changes.

### 9.2 Execution errors and null propagation

`MULTIPLE_VALUES`, `MISSING_VALUE`, `INVALID_VALUE` and `UNRESOLVED_TYPE` are execution
errors, which the October 2021 edition calls field errors. The field becomes null. If it
is non-null, the null propagates to the nearest nullable parent, exactly as the
specification describes, and the error is reported once with the path of the field
that failed. `MISSING_VALUE` is the error of a non-null field without a value, the case
§3.3 warns about.

The current draft discusses letting clients choose how errors propagate. The adapter
follows the October 2021 behaviour until that lands in an edition.

### 9.3 HTTP status

| Situation | `application/graphql-response+json` | `application/json` |
|---|---|---|
| Executed, with or without execution errors | `200` | `200` |
| The request is malformed, such as a body that is not JSON or has no `query` | `400` | `400` |
| Document does not parse | `400` | `200` |
| Document does not validate, or a limit of §7.1 | `422` | `200` |
| Mutation over `GET` | `405` | `405` |
| A budget | `507` | `507` |
| Timeout | `408` | `408` |
| Cancelled, or over a concurrency limit | `503` | `503` |
| Unknown dataset, no schema installed, or not visible | `404` | `404` |
| Endpoint not allowed (§10.2) | `403` | `403` |

The body of every response, including `4xx` and `5xx`, is a GraphQL response with
`errors`, so a client needs one parser. Under `application/json`, the GraphQL over HTTP
draft asks for `200` on any well-formed HTTP request, and the adapter follows it for
documents that do not parse or validate. Budgets, timeouts and cancellations are server
conditions, not GraphQL errors. They keep the status codes they have on `/{ds}/sparql`
under both media types, so that proxies, metrics and retry logic see them.

The draft also proposes `294` for responses with both data and errors. The adapter
answers `200` until an edition settles it (§19).

## 10. HTTP and access control

### 10.1 Routes

| Method | Path | Need | Result |
|---|---|---|---|
| GET, POST | `/{ds}/graphql` | `read` through the `graphql` endpoint | A GraphQL response |
| GET | `/{ds}/graphql/schema` | `read` through the `graphql` endpoint | The API schema as SDL, `text/plain` with `charset=utf-8` |
| GET | `/$/graphql/{ds}` | `read` through `info` | The configuration with its version |
| GET | `/$/graphql/{ds}/versions` | `read` through `info` | The kept versions, newest first |
| PUT | `/$/graphql/{ds}` | `admin` | `201` or `200`, with `changed` and `warnings` |
| DELETE | `/$/graphql/{ds}` | `admin` | `204` |
| GET | `/$/graphql/{ds}/draft` | `read` through `info` | A drafted mapping schema as SDL, or JSON with its decisions |

`POST /{ds}/graphql` takes `application/json` with `query`, `operationName`,
`variables` and `extensions`, as the GraphQL over HTTP draft defines it, and
`application/graphql` with the document as the body for older clients. `GET` takes the
same parameters in the query string and runs queries only. The Sparkles parameters of
`/{ds}/sparql` go in the query string for both methods: `at`, `timeout`, `reasoning`,
`nocache`, `explain` and the budget overrides `memory-mb`, `max-rows`,
`max-rows-produced` and `max-result-mb`. The response is
`application/graphql-response+json` when the client accepts it and `application/json`
otherwise.

`PUT /$/graphql/{ds}` takes the configuration of §3.1, accepts `If-Match` and
`If-None-Match: *` as C16 does, and is refused with `403` on a `--read-only` server. A
change that breaks a stored GraphQL query (§11) is refused with `409` and the names of
the broken queries, unless the request has `force=true`, after which those queries
answer `409` until they are fixed.

Without an installed schema, `/{ds}/graphql` answers `404` with a message that points to
`/$/graphql/{ds}/draft`.

### 10.2 Permissions

The adapter runs as the caller. Every group carries the caller's view, so filters,
lookups, `totalCount`, orderings and type tests see only the graphs and triples the
caller may read. A hidden node is indistinguishable from a missing one (§5.1).

GraphQL requests belong to a new C12 endpoint name, `graphql`. A grant whose
`endpoints` list `query` also covers `graphql`, as `gsp-rw` covers `gsp-r`, so every
existing grant keeps working. A grant that lists only `graphql` lets an application read
through its schema without reaching `/{ds}/sparql`. A grant without `endpoints` covers
both, as before. `graphql` is added to the endpoint table of C12 §2.2, and
`/$/whoami` lists it like the others.

Browsers can send cookies, so the session rules of C09 apply. `GET` runs queries only.
`POST` with `application/json` needs a CORS preflight from another origin. The adapter
accepts no form-encoded bodies.

### 10.3 Observability

Requests are logged and measured as `op="graphql"`. A histogram,
`sparkles_graphql_groups`, records the groups per request, and the access log records
the operation name, the document hash and the number of groups.

## 11. Stored GraphQL queries

C16's catalog gains GraphQL definitions:

```json
{
  "language": "graphql",
  "document": "query PeopleNamed($name: String!) { allPerson(filter: { name: { eq: $name } }) { nodes { id name } } }",
  "operationName": "PeopleNamed",
  "description": "People with a given name",
  "mcp": true
}
```

The parameters are the operation's variables, with their GraphQL types, defaults and
non-null markers. A definition is validated against the installed API schema when it is
saved. `GET` or `POST /{ds}/queries/{name}` runs it with values from the query string, a
form body or a JSON body, and answers with a GraphQL response. Values are coerced as
GraphQL variables are. Versions, permissions and the CLI work as C16 describes.

With `persistedOnly: true`, `/{ds}/graphql` refuses documents that are not stored
queries for callers without `admin`, with `PERSISTED_QUERY_REQUIRED`. A client can still
run a stored query there by sending `extensions: { "sparkles": { "storedQuery": "NAME" } }`
with its variables. This is the trusted-documents practice of production GraphQL
deployments: the set of documents is reviewed, and an unreviewed document cannot run.
Apollo's automatic persisted queries, which store any document a client sends, are not
adopted, because they do not limit what runs.

An MCP tool per stored GraphQL query is generated as C16 §5 describes. Its input schema
maps GraphQL types to JSON Schema: `Int` to `integer`, `Float` to `number`, `Boolean` to
`boolean`, the other scalars and `ID` to `string`, enums to `enum`, input objects to
objects, and lists to arrays.

## 12. Mutations are out of scope

The adapter serves reads. The API schema has no `Mutation` type, so a mutation fails
validation, and over `GET` it is refused with `405` as the GraphQL over HTTP draft
requires. Writes stay with SPARQL Update, the Graph Store Protocol and uploads, for these
reasons.

- **The mapping is not reversible.** A field may read an inverse predicate, a class with
  its subclasses, a language-preferred value or the smallest of several values. None of
  these says what a write should do. Writing `name: "Ann"` could replace every name,
  replace the English one, or add one.
- **RDF writes need choices GraphQL inputs do not carry.** Which graph receives the
  triples, whether a new node is an IRI or a blank node, and what to do with the values
  of other languages all need extra arguments that rebuild a write language.
- **Writes already have one checked path.** Validation (C10), previews (C15), commit
  messages, receipts, preconditions and the write rules of C12 all apply to the existing
  write endpoints. A second write path would have to repeat each of them, and the
  protections of triples refuse writes by the triples asked for, which a mutation would
  have to compute first.

Ontotext's service does generate mutations from SOML. A mutation design would be a
separate spec, and it would start from these three problems.

## 13. Subscriptions

Subscriptions are not part of Phases 1 and 2, and the API schema has no `Subscription`
type. The change feed of F06 makes a design possible, which Phase 3 would build if a
workload asks for it.

- A subscription is a query document whose answer is sent again after each commit that
  could change it, the semantics usually called a live query.
- The server reads `Store::subscribe_commits`. For each commit it checks the predicates
  of the commit's changes, filtered by the caller's view, against the predicates and
  classes the document reads, and re-runs the document only on a match.
- The transport is server-sent events, as the change feed's stream already is, with the
  GraphQL over SSE protocol's event names. WebSocket protocols are not adopted, because
  the server has no WebSocket endpoint today.
- Each subscription is a long-lived reader. The number per principal and per server is
  limited, and each re-run counts as a request against the rate limit.

## 14. User interfaces

### 14.1 Command line

```
sparkles graphql --loc DB run [--query FILE] [--variables JSON] [--operation NAME] [--at SEL] [--explain]
sparkles graphql --loc DB schema get [--api]
sparkles graphql --loc DB schema put FILE [--message TEXT] [--force]
sparkles graphql --loc DB schema delete
sparkles graphql --loc DB schema draft [--source shapes|observed] [--support S] [--shapes-graph IRI]
```

`run` reads the document from standard input without `--query` and prints the JSON
response. It exits with status 2 on a budget, as `sparkles query` does, and with
status 1 when the response has any other error. `schema put` takes the database's lock, as
`sparkles queries put` does.

### 14.2 Web UI (Phase 2)

A **GraphQL** page sits next to the query page. It has an editor for the document built
on the existing CodeMirror 6 editor with `cm6-graphql`, which gives highlighting,
validation and completion from the introspected API schema. It also has a variables
pane, a JSON result view, a **SPARQL** tab that shows the plan of §6.4, and a schema
browser that lists the types and their fields with their descriptions. These are the
parts of GraphiQL that developers use, built into the existing UI instead of embedding
GraphiQL's React application (§17).

The dataset page gets a **GraphQL schema** section for admins. It shows the installed
mapping schema in an editor, drafts one from shapes or from the data with the options of
§3.5, shows the warnings of a `PUT` before saving, and lists the versions.

### 14.3 Library

`sparkles_graphql::execute(snapshot, &schema, request, &options)` runs a request
against a snapshot. The Python bindings may expose it later. The schema type is built
once from a configuration and shared between requests.

## 15. Performance expectations

The adapter adds three costs to the engine's work: parsing and validating the document,
planning the groups, and assembling the response. The targets below are for the release
build on the 1M-triple benchmark data, measured as medians against the same groups run
as SPARQL through the library.

- **Parse, validate and plan.** Under 1 ms for documents up to 4 KiB, and nothing for a
  cached document beyond the planning, which depends on the variables.
- **Overhead per request.** At most 10%, or 0.5 ms, more than running the groups' SPARQL
  directly, whichever is larger.
- **Against hand-written SPARQL.** A request with `g` groups costs about `g` queries. A
  developer who writes one SPARQL query with nested `OPTIONAL`s may be faster for a
  shallow request and much slower when sibling lists multiply rows (§17). The benchmark
  reports both.
- **No growth with the answer's breadth.** The number of engine executions is fixed by
  the document. A test with 10, 100 and 1,000 root nodes checks that the groups stay the
  same and the time grows with the rows, not with the nodes times the fields.

**Memory.** Batching holds the ids of a whole level and the whole response tree in
memory, instead of streaming nodes one by one. At the default `maxNodes` of 100,000 the
tree takes on the order of tens of megabytes, and the result budget bounds it as well.
This trades higher peak memory per request for one engine execution per group instead
of one per node. A server that runs many large GraphQL requests at once should lower
`maxNodes` to bound the sum.

**Child groups and nested limits.** In Phase 1, a nested `first` limits the response,
not the engine's work (§6.2). A field with many values per parent costs every value
until Phase 2 adds a per-group top-k.

## 16. Phasing

### Phase 1: the adapter

- The `sparkles-graphql` crate: the mapping schema, its validation and the derived API
  schema, the planner and fetch groups, filters, ordering, connections with
  commit-bound cursors, nested lists, inverse fields, languages, scalars, interfaces and
  type conditions, introspection, limits and errors.
- `QueryOptions::work`, the shared counter of produced rows.
- `graphql.json` with versions, the admin routes, drafts from shapes and from data, and
  `/{ds}/graphql` with `/{ds}/graphql/schema`.
- The `graphql` endpoint in C12's grants, metrics and the access log.
- `sparkles graphql` with `run`, `schema` and `draft`.
- Tests: A1 to A15 and A17, a differential test that compares each group's result with
  the same SPARQL run directly, the access-control matrix of C12 extended with GraphQL
  requests, and the benchmark of §15.

### Phase 2: tools and speed

- The UI's GraphQL page and schema editor (§14.2).
- Stored GraphQL queries, `persistedOnly`, and their MCP tools (§11), with A16.
- An MCP tool `graphql_query` that runs a document on a dataset and returns the JSON
  compacted as `sparql_query` compacts tables, and `graphql_schema`, which returns the
  API schema.
- `QueryOptions::seed`, so child groups bind parent ids without the dictionary.
- A per-group top-k operator in the engine for nested `first`.

### Phase 3: subscriptions

Live queries over the change feed (§13), only if a workload asks for them.

## 17. Rejected alternatives

- **One SPARQL query per document**, as GraphQL-LD's `graphql-to-sparql` builds. Sibling
  lists multiply into a cross product: ten emails and ten friends per person give a
  hundred rows per person. A `LIMIT` per nested list cannot be expressed, and the flat
  rows must be folded back into a tree. Groups keep the row count to the sum of the
  values.
- **A resolver per field**, the model of `juniper` and of `async-graphql`'s dynamic
  schemas. Without a batching layer it runs one query per node. With one, it batches per
  field and level and arrives at the groups of §6.2, but without the whole document in
  view, so filters, orders and type conditions cannot be planned together. Both crates
  also bring their own execution engine and async machinery, of which the adapter needs
  none. `apollo-compiler` parses, validates and introspects, and leaves execution to the
  planner.
- **A schema that follows the data.** Stardog can generate a schema automatically from
  the data. A schema that changes when the data changes breaks generated clients without
  warning, and a schema derived from observations claims guarantees nothing enforces.
  The draft endpoint gives the same start in one call, with a review in between.
- **Generating SPARQL text and parsing it.** Building algebra directly needs no escaping,
  cannot be injected into, and skips a parse per group.
- **Keyset cursors for every order.** They would need the engine to seek into a sorted
  result on arbitrary keys. Offsets at a fixed commit are exact, and the result cache
  serves repeated pages. Keyset cursors at the head for `ID_ASC` order, which the index
  serves directly, are an open question.
- **Cursors at the head.** Pages would shift when the data changes between requests, and
  a client would skip or repeat items without knowing it.
- **Compacted identifiers.** `ex:alice` is shorter than the full IRI, but its meaning
  changes when a prefix changes, and clients store ids. Inputs accept compact forms.
- **A schema per principal**, with types hidden from callers who cannot read them. Types
  are configuration, the data is filtered by the view, and one schema per view would
  multiply the caches and break generated clients that several principals share.
- **Embedding GraphiQL.** It is a React application of several megabytes, and the UI is
  written in Svelte with CodeMirror 6. `cm6-graphql` gives the editor features at a small
  cost.
- **Hasura's operator names** (`_eq`, `_and`). They are well known, but camelCase
  operators match the GraphQL naming conventions and PostGraphile's filters, and the
  leading underscore suggests something private.
- **Apollo's automatic persisted queries** (§11).
- **A `sparql(query:)` field.** It would bypass the schema and its limits. The SPARQL
  endpoint is there for that.

## 18. Acceptance examples

**Fixture.** A dataset `org` holds about a thousand `ex:Person` nodes with `ex:name`
(one string each), `ex:age` (`xsd:integer`), `ex:email` (zero to three strings),
`ex:knows` (other people), `ex:worksFor` (an `ex:Org`) and `rdfs:label` in English and
French. `ex:Employee rdfs:subClassOf ex:Person`. Some `ex:Org` nodes have `ex:name` and
`ex:population`, one of which is 3,000,000,000. A SHACL guard in `reject` mode with a
`strict` baseline requires exactly one `ex:name` per `ex:Person`.

- **A1. Draft from shapes.** `GET /$/graphql/org/draft?source=shapes` drafts
  `type Person @rdf(iri: "ex:Person")` with `name: String!` and no other non-null field,
  because only the name is enforced. A shape for `ex:Employee` gives a separate
  `Employee` type. Employees are members of `Person` as well, because membership
  includes subclasses (§5.1), and `allPerson` lists each of them once.
- **A2. Draft from data.** `source=observed` drafts `name: String` with the "observed at
  most one value" comment, `email: [String!]!`, `knows: [Person!]!`, and
  `population: Integer` because one value exceeds 32 bits.
- **A3. Install.** `PUT /$/graphql/org` with the draft answers `201` with version 1. A
  second `PUT` that declares `age: Int!` answers `200` with a warning that names
  `Person.age`.
- **A4. Lookup.** `{ person(id: "http://example.org/p1") { id name age } }` returns the
  person. The same lookup for an `ex:Org` IRI, for an IRI that does not exist, and for a
  person in a graph the caller cannot see return `null` with no error, identically.
- **A5. No N+1.** `{ allPerson(first: 1000) { nodes { name knows { name } } } }` runs
  exactly two engine executions, as `extensions.sparkles.plan` and the
  `sparkles_graphql_groups` histogram show. Adding `email` adds one group, whatever the
  number of people.
- **A6. Pages.** `allPerson(first: 2)` returns two nodes and an `endCursor`. A commit
  then inserts a person that sorts first. The next page with `after` returns the third
  and fourth people of the earlier commit, and `Sparkles-Commit` names that commit. After
  a compaction that drops the commit, the same cursor answers `CURSOR_EXPIRED`.
- **A7. Filters.** `allPerson(filter: { age: { gt: 30 }, knows: { name: { eq: "Ann" } } })`
  returns the people over 30 who know someone named Ann, the same set as the equivalent
  SPARQL. `name: { eq: "Ann\" } UNION { ?s ?p ?o } #" }` matches nothing.
- **A8. Multiple values.** After the guard is switched to `warn` and a write gives a
  person two names, the person's `name` is `null` with a `MULTIPLE_VALUES` error whose
  path ends in `name`. If `name` is `String!`, the error propagates to the nearest
  nullable parent, and the other people are unaffected. With `@single(onMany: MIN)` the
  smaller name is returned.
- **A9. Languages.** `label(lang: ["fr", "en"])` returns the French label where there is
  one and the English one elsewhere. `label(lang: ["de"])` returns null for every
  person.
- **A10. Numbers.** An `Int` field over the population of 3,000,000,000 is an
  `INVALID_VALUE` error. Declared `Integer`, it returns `"3000000000"`.
- **A11. Limits.** `allPerson(first: 1000) { nodes { knows(first: 1000) { name } } }` is
  refused before execution with `QUERY_TOO_COMPLEX`, `estimate: 1001000` and
  `limit: 100000`, and `422` under `application/graphql-response+json`. `first: -1` is
  `BAD_USER_INPUT`.
- **A12. Budgets.** A request with `memory-mb=1` over a large collection answers `507`
  with `BUDGET_EXCEEDED`, the C01 budget fields, and no `data`.
- **A13. Access.** A principal limited to some graphs gets the `totalCount`, filters and
  nested lists of those graphs only, and the results equal those over a store that holds
  only those graphs. A principal whose grant lists only `graphql` gets results from
  `/org/graphql` and `403` from `/org/sparql`. A grant that lists only `query` covers
  `/org/graphql` too.
- **A14. Introspection.** GraphiQL's standard introspection query succeeds although it
  is deeper than `maxDepth`. With `introspection: false`, it fails validation for a
  reader and succeeds for an admin.
- **A15. Mutations.** `mutation { x }` fails validation with `GRAPHQL_VALIDATION_FAILED`.
  Sent with `GET`, it answers `405`.
- **A16. Stored queries.** A stored GraphQL query runs over
  `/org/queries/PeopleNamed?name=Ann` and through its MCP tool with the same results.
  With `persistedOnly: true`, an ad-hoc document from a reader answers
  `PERSISTED_QUERY_REQUIRED`, and the stored query still runs. A schema `PUT` that
  removes `Person.name` answers `409` and names `PeopleNamed`.
- **A17. Explanation.** `explain=true` returns each group's SPARQL. Run on
  `/org/sparql`, each text returns the rows the group returned.

## 19. Open questions

1. Should responses with both data and errors answer `294`, as the GraphQL over HTTP
   draft proposes, once the draft becomes stable?
2. Should nested lists also be offered as connections, with cursors per parent? They
   are rarely used and need a cursor that names the parent as well.
3. Should a keyset cursor at the head be offered for `ID_ASC` order, where the index can
   seek, for clients that prefer the latest state to a stable one?
4. Should the adapter offer aggregates beyond `totalCount`, such as Hasura's
   `_aggregate` fields with counts, sums and groups?
5. Should RDF 1.2 annotations be exposed on edges? A connection edge is a natural place
   for the annotations of the triple it stands for, the edge properties that property
   graph users expect.
6. Should full-text and vector search (F03, F04) get root fields, such as
   `searchPerson(text:)`, ranked by score?
7. Should `orderBy` accept multi-valued fields with an explicit minimum or maximum?
8. Should introspection hide types whose classes the caller has no visible instances of,
   at the cost of one schema per view?

## 20. Sources

- **GraphQL specification**, October 2021 edition and the current working draft
  (`spec.graphql.org/October2021/` and `spec.graphql.org/draft/`, the draft fetched
  2026-10-02): names and the `__` reservation, the built-in scalars and the 32-bit range
  of `Int`, `@specifiedBy`, `@oneOf` in the draft, validation, execution, field (in the
  draft, execution) errors with `message`, `locations`, `path` and `extensions`, null
  propagation and introspection.
- **GraphQL over HTTP**, working draft (`http-spec.graphql.org/draft/`, fetched
  2026-10-02): the request parameters, `GET` and `POST`, `405` for mutations over `GET`,
  `application/graphql-response+json`, `422` for validation failures, the proposed `294`
  status, and `200` for `application/json`.
- **GraphQL Cursor Connections Specification** (`relay.dev/graphql/connections.htm`,
  fetched 2026-10-02): connection and edge types, `PageInfo`, `first`, `after`, `last`,
  `before` and the error for negative values.
- **GraphQL-LD**: Taelman, Vander Sande and Verborgh, "GraphQL-LD: Linked Data Querying
  with GraphQL", ISWC 2018 Posters and Demos, cited from working knowledge, and the
  README of `graphql-to-sparql` (MIT, fetched 2026-10-02), for the JSON-LD context as a
  mapping, `@reverse`, the single query with chained triple patterns, `@optional`,
  `@single` and `@plural`, `first`, `offset` and `totalCount`. No code was used.
- **HyperGraphQL** (Apache-2.0): the repository's README, fetched 2026-10-02, for SDL
  with mapping directives and JSON-LD responses. Its documentation site no longer
  resolves, and the rest is cited from working knowledge.
- **Stardog's GraphQL documentation** (`docs.stardog.com/query-stardog/graphql`, fetched
  2026-10-02): classes as types and properties as fields, prefixed names written with
  `_` such as `foaf_name`, automatic schemas with `graphql.auto.schema`, the `@filter`,
  `@optional`, `@type`, `@bind`, `@hide` and `@lang` directives, `first`, `skip` and
  `orderBy`, and `graphql explain`.
- **Ontotext Semantic Objects documentation** (`platform.ontotext.com/semantic-objects/`,
  SOML pages on queries and properties, fetched 2026-10-02): root queries per object,
  `ID`, `where`, `orderBy` over single-valued chains, `limit` and `offset`, the
  operators, implicit `EXISTS` across relations, `lang` preferences, cardinalities that
  decide single and multiple values, `inverseAlias`, and the limits of 15 levels and
  100,000 triples.
- **Hasura** filter documentation (`hasura.io/docs/2.0/queries/postgres/filters/`,
  fetched 2026-10-02) and **PostGraphile**'s conventions (`allX` connections, `nodes`,
  `totalCount`, `orderBy` enums such as `NAME_ASC`, the connection-filter plugin), cited
  from working knowledge.
- **Apollo Server** error codes and persisted queries, and the trusted-documents practice,
  cited from working knowledge.
- **GraphQL over Server-Sent Events**, the protocol of the `graphql-sse` project, for the
  transport sketched in §13, cited from working knowledge.
- **GitHub's GraphQL API** limits (`docs.github.com/en/graphql/overview/rate-limits-and-query-limits-for-the-graphql-api`,
  fetched 2026-10-02): `first` or `last` required, values from 1 to 100, at most 500,000
  nodes, and the rule that multiplies them down nested connections.
- **W3C Bridging GraphQL and RDF Community Group** (`w3.org/community/graphql-rdf`,
  fetched 2026-10-02), which surveyed approaches to combining the two and closed on
  2023-04-07.
- **Research**: Hartig and Pérez, "Semantics and Complexity of GraphQL", WWW 2018, for
  exponential response sizes and size computation before execution. Cha, Wittern,
  Baudart, Davis, Mandel and Laredo, "A Principled Approach to GraphQL Query Cost
  Analysis", ESEC/FSE 2020, for static bounds from list arguments. Both are cited from
  working knowledge.
- **Rust crates**, from crates.io and docs.rs, fetched 2026-10-02: `apollo-compiler`
  1.33.0 and `apollo-parser` 0.8.6 (MIT OR Apache-2.0) for SDL, executable documents,
  validation and introspection; `async-graphql` 7.2.1 (MIT OR Apache-2.0), whose
  `dynamic` module builds schemas at run time with per-field resolvers; `juniper` 0.17.1
  (BSD-2-Clause). Licenses were read from the crates' metadata.
- **npm packages** `cm6-graphql` 0.2.2, `graphql` 17.0.2 and `graphiql` 5.4.0, all MIT,
  from the npm registry, fetched 2026-10-02.
- **W3C** SHACL, RDF 1.1 and 1.2 Concepts, XML Schema 1.1 Datatypes, SPARQL 1.1 Query
  (`langMatches`, `ORDER BY`, `EXISTS`, `VALUES`), RFC 4647 and RFC 4648 (base64url),
  cited from working knowledge.
- **The Sparkles code**: `sparql::execute_query` and `QueryOptions`, the vendored
  `spargebra` algebra, `schema::draft`, `stored`, `access`, the server's query, stored
  query and MCP handlers, and the specs C01, C02, C09, C10, C11, C12, C16 and F06.

## Outcome

**Phase 1 delivered on 2026-10-02.** Phases 2 and 3 are not built.

- The crate `sparkles-graphql` holds the adapter. `mapping` parses the SDL with
  `apollo-compiler` next to a prelude that declares the four directives, the scalars the
  SDL does not declare, `LangString`, `RDFTerm` and `Node`, and checks it as §3.3 says.
  Every error names its line. `api` writes the API schema as SDL, and `apollo-compiler`
  validates it. `plan` walks the selection tree with the specification's CollectFields
  and makes the fetch groups. `algebra` builds each group as `spargebra` algebra. `exec`
  runs the groups in order on one snapshot and assembles the response. `config` keeps
  `graphql.json` with 20 versions, and `draft` reads SHACL shapes or C02's drafted shapes
  and writes the commented SDL.
- The engine gained `QueryOptions::work`, the row count of `--max-rows-produced`, shared
  by every group of a request. The groups also share one deadline, and the serialized
  response counts against `--max-result-mb`.
- The server's `http::graphql` module serves `/{ds}/graphql`, `/{ds}/graphql/schema`,
  `/$/graphql/{ds}`, `…/versions` and `…/draft`, behind the default-on `graphql` feature.
  Grants gained the endpoint `graphql`, which `query` covers. Requests count against the
  `query` rate-limit class, are measured as `operation="graphql"`, fill the histogram
  `sparkles_graphql_groups`, and log the operation name, the document hash and the
  number of groups. `serve` has `--graphql-max-depth`, `--graphql-max-nodes`,
  `--graphql-default-first` and `--graphql-max-first`. `sparkles graphql --loc DB` has
  `run`, and `schema get|put|delete|versions|draft`. Backups and clones of a persistent
  dataset carry `graphql.json`.

**Deviations and additions.**

- The response is assembled by `apollo-compiler`'s `resolvers::Execution`, whose
  resolvers read the groups' rows and never query. It gives the specification's null
  propagation, `__typename` and introspection. A field error carries its code through a
  table the resolvers fill, since the executor's errors have a message only.
- `VALUES` cannot hold blank nodes, so each blank node parent of a child group is one
  `UNION` branch whose variable is bound through `QueryOptions::initial_bindings`. The
  explained text of such a group names a variable bound outside it, so A17 holds for
  groups whose parents are IRIs.
- An order key sorts a node by its smallest value, or its largest one for `_DESC`, with
  `GROUP BY` and `MIN` or `MAX`. A node with two values of an order field then appears
  once instead of twice.
- String filters compare the lexical form of literals, `STR(?x)`, so `eq: "Ann"` matches
  `"Ann"@en` as well. The other scalars compare typed values.
- `TFilter` has no `exists` for object fields. `not: { worksFor: {} }` says that a node
  has no value, since an empty filter on an object field holds when some value exists.
- The default language range `"*"` matches every value, tagged or not, so a `String`
  field over plain literals needs no `@lang`. A `lang` argument that names other ranges
  filters as §4.5 says.
- Drafts type a field whose class has no drafted type, and a field with `sh:nodeKind
  sh:IRI`, as `Node` rather than `Resource`. Such a value is answered as its mapped type
  when it has one, and as `Resource` otherwise. Mapping schemas accept fields typed
  `Node` for the same reason.
- Without `source`, a draft reads shapes when a shapes graph is named or a SHACL guard is
  installed, and the data otherwise. A non-null field is backed when the guard runs in
  `reject` mode with a `strict` baseline over exactly the schema's `dataGraph`.
- A5's document, `allPerson(first: 1000) { nodes { name knows { name } } }`, estimates
  1,000 + 1,000 × 100 = 101,000 nodes with the default `defaultFirst`, one more than
  `maxNodes` allows. The tests run it with `knows(first: 10)` or a raised limit.
- `persistedOnly: true` is refused at `PUT`, because stored GraphQL queries are Phase 2.
  The CLI's `--force` is accepted and does nothing yet.
- A cursor's `h` hashes the field's name, not its alias, with its filter and order and
  the schema version.
- A `PUT` with `application/graphql` replaces the SDL and keeps the other fields of the
  installed configuration.
- A malformed HTTP request has the code `BAD_REQUEST`, and an operation name that the
  document does not have is `GRAPHQL_VALIDATION_FAILED`. A `403` from the access layer
  has Sparkles' JSON error body, not a GraphQL response.
- `explain=true` also returns `extensions.sparkles.timing`, the time of parsing,
  planning, the groups and assembly, and `groups`, the number of engine executions.
- The configuration of an in-memory dataset is not in its backups, as for stored queries.

**Tests.** The crate's tests cover A4 to A11, A14, A15 and A17, abstract types, blank
nodes, mapping errors with their lines and the non-null warnings. A differential test
runs 400 random documents over 40 random graphs, with nested `and`, `or`, `not` and
object filters, orders, slices, nested lists and multi-valued fields, and compares each
answer with hand-written SPARQL. The server's tests cover A1 to A3, A6 with
`CURSOR_EXPIRED` after a compaction, A11, A12, A14, A15 and A17 over HTTP, the status
codes of both media types and the metrics. The access-control tests cover A13: a view of
some graphs answers as a store holding only those graphs, a hidden node is null like a
missing one, a `graphql`-only grant reads `/{ds}/graphql` and gets `403` from
`/{ds}/sparql`, a `query` grant covers `graphql`, and protections of triples apply to
lookups, collections and `totalCount`. A CLI test drafts, installs and runs.

**Measurements.** The release build ran on the 1.05M-triple data of
`scripts/gen-data.py` in memory, with mimalloc preloaded and pinned to cores 0–11, with
`crates/sparkles-graphql/tests/bench.rs`. The machine was shared with other builds, with
a load average of 45 to 49 on 16 cores, so absolute times are inflated and vary by up to
2× between runs. Each value is the median of 21 runs that alternate the four
measurements, with the result cache off.

| Document | Groups | GraphQL ms | Groups as SPARQL ms | Adapter ms | One SPARQL query ms |
|---|---|---|---|---|---|
| `person(id:) { name age knows { name } }` | 2 | 2.58 | 2.35 | 0.09 | 10.98 |
| `allPerson(first: 100) { nodes { id name age } }` | 1 | 26.40 | 25.13 | 0.25 | 24.09 |
| `allPerson(first: 1000) { nodes { name knows(first: 10) { name } } }` | 2 | 56.39 | 39.31 | 7.10 | 47.35 |
| the same page with `filter`, `orderBy`, `totalCount` and `worksFor` | 3 | 23.13 | 21.67 | 0.32 | 52.75 |
| a page of 1,000 with two sibling lists, `knows` and `authorOf` | 3 | 49.79 | 33.29 | 8.82 | 40.83 |

"Groups as SPARQL" runs the groups' explained text through `sparkles::sparql::query`.
"Adapter" is parsing, planning and assembly. "One SPARQL query" is the query a developer
would write with nested `OPTIONAL`s, without per-parent limits. For lookups, pages and
filtered pages, the adapter adds 0.1 to 0.3 ms and stays within §15's target. For a
response of about 3,600 nodes, assembly takes 7 to 9 ms, about 2 µs per node, and the
request is about 40% slower than its groups run as SPARQL, which misses the 10% target.
Assembly runs the generic executor once per object and builds a JSON value per field,
and it was not profiled further. The single SPARQL query was slower than the adapter for
the lookup and the filtered page, and faster for the two pages of 1,000 people.

**Not built.** Phases 2 and 3 were not built. Phase 2 holds the UI's GraphQL page and
schema editor, stored GraphQL queries, `persistedOnly`, the MCP tools,
`QueryOptions::seed` and the per-group top-k, and Phase 3 holds subscriptions. Nested
lists are not connections, and a nested `first` limits the response while the engine
produces every value of the field.

**Later additions (2026-10-03).** The MCP server gained `graphql_query`, which runs a
document on a dataset with a schema installed, refuses mutations, and returns the API
schema when called without a document ([C11](C11-mcp-server.md#outcome)). It answers
with the GraphQL response as JSON, not compacted as tables are, and there is no separate
`graphql_schema` tool. Stored GraphQL queries and their tools are still not built.
