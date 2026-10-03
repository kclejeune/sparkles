# F01: A Cypher read subset over RDF

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 is the parser, the compiler, `/{ds}/cypher`, the CLI
> and the TCK harness. Phase 2 adds list values in the engine, path enumeration, the MCP
> tool and the UI. Phase 3 adds a Neo4j Query API route and stored Cypher queries.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

This design was written from the openCypher 9 language reference and
its Technology Compatibility Kit (TCK), public summaries of ISO/IEC 39075:2024 (GQL), the
Neo4j Cypher manual and Query API documentation, Amazon Neptune's documentation and
published papers on openCypher over RDF, the Kùzu documentation of RDF graphs, the
neosemantics documentation, Ontotext GraphDB's graph path search documentation, the W3C
RDF 1.2 drafts and the RDF-star Community Group report, academic work on Cypher's
semantics and on translating between property-graph and RDF query languages, and the
Sparkles code. The sources are listed in §17. It builds on budgets
([C01](C01-observability-and-budgets.md)), schema discovery
([C02](C02-schema-discovery.md)), access control ([C09](C09-dataset-access-control.md),
[C12](C12-graph-access-control.md)), the MCP server ([C11](C11-mcp-server.md)), stored
queries ([C16](C16-stored-queries.md)) and point-in-time reads
([F06](F06-snapshots-and-point-in-time.md)).

## 1. Summary, goals, non-goals

Many developers who could use a Sparkles dataset know Cypher and not SPARQL, and many
graph tools, tutorials and language-model agents assume Cypher. RDF already holds the
data a property graph holds: resources with types, literal-valued properties, links
between resources, and, since RDF 1.2, statements about links. What is missing is a
documented reading of RDF as a property graph and a Cypher frontend that queries it
without copying the data.

This spec defines that reading and a read-only Cypher frontend. The frontend parses a
subset of openCypher 9, resolves names to IRIs, and compiles the query to the SPARQL
algebra that `/{ds}/sparql` runs. The planner, the indexes, budgets, access control,
reasoning, point-in-time reads and caches therefore apply unchanged. A thin result
shaper turns the solutions into Cypher values such as nodes, relationships, paths, lists
and maps.

**Goals**

1. A property-graph view of any RDF dataset, defined in §2, that needs no configuration
   and no copy of the data. Nodes are IRIs and blank nodes, labels are `rdf:type`,
   properties are literal-valued triples and relationships are triples between resources.
2. Relationship identity and relationship properties through RDF 1.2 reifiers, so that
   parallel relationships of one type between two nodes are distinct, as in a property
   graph.
3. The read clauses of openCypher 9: `MATCH`, `OPTIONAL MATCH`, `WHERE`, `WITH`,
   `RETURN`, `ORDER BY`, `SKIP`, `LIMIT`, `UNWIND`, `UNION`, `DISTINCT` and aggregation,
   with variable-length relationships and a subset of list and map expressions (§5).
4. Predictable names. A name in a query resolves to an IRI by a fixed order of rules
   (§3), and every name in a result resolves back to the same IRI.
5. A defined mapping of Cypher's types and null to RDF terms and SPARQL's unbound
   variables (§4), with the remaining differences listed as deviations.
6. Clear errors. Anything outside the subset fails at compile time with a code that
   names the feature (§5.6).
7. `POST /{ds}/cypher` with a JSON result modelled on Neo4j's Query API and the SPARQL
   result formats (§7), plus a CLI command, an MCP tool and a UI editor (§8).
8. Queries run as the caller, on the caller's graph view, under the caller's budgets
   (§9, §10).
9. A published subset of the openCypher TCK that passes (§12).

**Non-goals.** Writes (`CREATE`, `MERGE`, `SET`, `REMOVE`, `DELETE`, `DETACH DELETE`,
`FOREACH`, `LOAD CSV`). The Bolt protocol and Neo4j driver compatibility. Procedures and
user-defined functions, apart from three schema procedures in Phase 2. Shortest-path
functions, which wait for the path search of F07. Spatial and full temporal support.
Schema constraints and indexes. A stored property-graph copy of the data.

## 2. The property-graph view of RDF

The view is a fixed reading of the RDF dataset that a query reads. It is not stored. Each
query computes the parts it needs from the ordinary indexes.

### 2.1 Nodes

A **node** is an IRI or a blank node that is the subject of a triple, or the object of a
triple that is a relationship (§2.4). Classes that appear only as the object of
`rdf:type`, predicates, literals and triple terms are not nodes. A node's identity is the
RDF term, so two node variables are the same node exactly when they are bound to the same
IRI or blank node.

A resource with no triples does not exist in RDF, so the view has no isolated node
without labels or properties. A resource whose only triple is `ex:x rdf:type ex:Thing`
is a node with one label and no properties.

### 2.2 Labels

A node's **labels** are the IRIs `c` of its triples `n rdf:type c`. A blank-node class,
such as an OWL restriction, is not a label. The pattern `(n:Person)` is the triple
pattern `?n rdf:type ex:Person`. Labels are names for IRIs (§3), so `labels(n)` returns
names that resolve back to those IRIs.

`rdf:type` triples are labels only. They are never relationships and never properties,
so `MATCH (n)-[r]->(m)` does not return a node's classes. `rdfs:Resource` is never a
label, because RDFS entailment makes every resource an instance of it, but a triple
`n rdf:type rdfs:Resource` still makes `n` a node.

### 2.3 Properties

A node's **properties** are its triples `n p v` where `v` is a literal and `p` is not
`rdf:type`. The property key is the name of `p`, and the value is the literal converted
as in §4.1. An RDF predicate can have both literal objects and resource objects. Each
triple is classified by its object, so `ex:alias "Bobby"` is a property and
`ex:alias ex:bob2` is a relationship, even with the same predicate.

RDF does not limit a predicate to one value per subject, while a Cypher property has one
value. §4.4 defines what `n.p` means when a node has several values.

### 2.4 Relationships and their identity

A **relationship** of type `p` from node `s` to node `o` exists for each asserted triple
`s p o` where `o` is an IRI or a blank node and `p` is neither `rdf:type` nor
`rdf:reifies`. Its identity depends on whether the triple has reifiers.

- **A triple with no reifier** is one relationship. Its identity is the triple term
  `<<( s p o )>>`. RDF has set semantics, so there is at most one such relationship per
  type and pair of nodes.
- **A triple with reifiers** is one relationship per reifier. A reifier is a resource
  `r` with a triple `r rdf:reifies <<( s p o )>>`, which RDF 1.2 calls a reifying
  triple. Each reifier is the identity of one relationship, and the bare triple adds no
  further relationship.

This is how the view represents **parallel relationships**. Two employments of one
person at one company are one asserted triple with two reifiers, and Cypher sees two
`WORKS_FOR` relationships, each with its own properties:

```turtle
PREFIX ex: <http://example.org/>

ex:carol ex:WORKS_FOR ex:acme ~ ex:job1 {| ex:role "Engineer" |} .
ex:carol ex:WORKS_FOR ex:acme ~ ex:job2 {| ex:role "Advisor" |} .
```

RDF 1.2 allows several reifiers for one triple term and does not require them to
denote the same thing, so reading each reifier as one relationship is consistent with it.
It also matches how the openCypher TCK's graphs are loaded in §12, where every created
relationship gets its own reifier.

A reifier whose triple is not asserted is not a relationship. RDF 1.2 states that
reification does not assert the triple, and a relationship that the data does not claim
would be a surprise in `MATCH`. Whether to offer such reifications as relationships is
open question 1.

A reifier is also a resource. It is the subject of `rdf:reifies` and of its annotation
triples, so it is a node with properties, usually without labels or relationships. The
view does not hide it, because doing so would cost an anti-join on every unconstrained
node pattern. Open question 2 asks whether it should.

### 2.5 Relationship properties

A relationship's **properties** are the literal-valued triples of its reifier, other than
`rdf:reifies`. A relationship without a reifier has no properties, and `r.since` on it is
null. A reifier's triples whose objects are resources are not visible as properties or as
relationships of the relationship, because a property-graph relationship has neither.
They remain visible as relationships of the reifier as a node, and through SPARQL.

`startNode(r)` is `s` and `endNode(r)` is `o`. `type(r)` is the name of `p`.

### 2.6 Graphs and reasoning

A Cypher query reads the same RDF graph as a SPARQL query on `/{ds}/sparql` without
`FROM`. That is the default graph, or the union of the named graphs when the dataset is
configured that way. The request parameter `default-graph-uri` selects other graphs, as
it does for SPARQL. Graph names never become labels or properties. GQL's `USE` clause is
not supported.

The reasoning options of `/{ds}/sparql` apply. With materialized inferences
([C08](C08-inference-freshness.md)) or RDFS on read, `(n:Agent)` also matches the
instances of subclasses of `ex:Agent`. Point-in-time reads with `?at=`
([F06](F06-snapshots-and-point-in-time.md)) work as they do for SPARQL.

### 2.7 What the view leaves out

The view loses some RDF detail, and SPARQL remains the way to reach it.

- Triple terms in object position other than in `rdf:reifies` triples are neither
  properties nor relationships.
- RDF lists (`rdf:first`, `rdf:rest`) are ordinary nodes and relationships. They are
  not Cypher lists.
- The datatype of a literal with no Cypher counterpart, such as `xsd:anyURI`, is kept in
  the SPARQL result formats but not in the Cypher JSON format (§4.1).
- The graph a triple came from is not visible.

## 3. Names and IRIs

### 3.1 Forms of a name

Labels, relationship types and property keys are names. A name in a query takes one of
these forms.

| Form | Example | Meaning |
|---|---|---|
| Bare name | `Person`, `` `first name` `` | Resolved by §3.2. A backticked name without `<…>` or a prefix is still a bare name. |
| Full IRI | `` `<http://example.org/Person>` `` | The IRI between the angle brackets. Backticks make it a valid openCypher symbolic name, so the query still parses with any openCypher tool. |
| Prefixed name | `` `ex:Person` `` | A prefix of the query or of the dataset, followed by a local part. |
| Neptune-style prefixed name | `ex::Person` | The same as `` `ex:Person` ``. Amazon Neptune uses this form for openCypher over RDF, and accepting it lets those queries run unchanged. |

A query may start with SPARQL-style prefix declarations, which Neptune also accepts:

```cypher
PREFIX ex: <http://example.org/>
MATCH (p:ex::Person) RETURN p.`ex:name`
```

The dataset's stored prefixes (`/{ds}/prefixes`) are predeclared, and a declaration in
the query overrides one of the same name. A prefixed name with an unknown prefix is a
compile error. Inside backticks, the text `a:b` is a prefixed name only when `a` is a
declared prefix. Otherwise it is a bare name, so a property key that contains a colon
still works.

### 3.2 Resolving bare names

A bare name resolves by the first rule that applies.

1. **The dataset's name map.** The Cypher settings of a dataset (§7.6) may map names to
   IRIs, such as `"KNOWS": "http://xmlns.com/foaf/0.1/knows"`. The map has separate
   entries for labels, relationship types and property keys, so one name can mean a
   class as a label and a predicate as a key.
2. **The dataset's namespace.** When the settings give a namespace, a bare name `N` is
   the IRI `namespace + N`. No lookup takes place, so the meaning of a query does not
   depend on the data.
3. **A unique local name.** Without a namespace, the compiler looks for IRIs whose local
   name equals `N`. For a label, the candidates are the classes the view has instances
   of. For a relationship type or a property key, they are the predicates the view uses.
   The local name is the part after the last `#`, `/` or `:`, compared case-sensitively.
   One candidate resolves the name. Several candidates are a compile error,
   `cypher-ambiguous-name`, which lists them as prefixed names.
4. **No match.** The name resolves to nothing. As in Cypher, a pattern with an unknown
   label or type matches no rows and a property with an unknown key is null. The response
   carries the notification `unknown-name` (§7.4).

Rule 3 makes queries on vocabularies such as FOAF or schema.org work without
configuration. `MATCH (p:Person)-[:knows]->(f) RETURN f.name` resolves `Person`,
`knows` and `name` when the dataset uses `foaf:Person`, `foaf:knows` and `foaf:name` and
no other vocabulary has the same local names. The candidates come from the statistics of
the caller's view (§9), so the lookup is a hash probe into a table that is built once per
snapshot and view.

Case is never folded. RDF vocabularies use `knows` where Cypher style uses `KNOWS`. A
heuristic that maps one to the other would make the meaning of a name depend on what else
the data contains, so a dataset that wants `KNOWS` maps it explicitly.

### 3.3 Names in results

`labels(n)`, `type(r)`, `keys(n)` and the property maps of returned nodes and
relationships write each IRI as the shortest name that resolves back to it.

1. The name of the name map, if one maps to the IRI.
2. The local name, if the IRI is in the dataset's namespace.
3. The local name, if no namespace is set and rule 3 of §3.2 would resolve it uniquely.
4. A prefixed name, `ex:Person`, with the query's or the dataset's prefixes.
5. Otherwise the full IRI in angle brackets, `<http://example.org/Person>`.

Forms 4 and 5 are written exactly as they would appear inside backticks in a query, so
a client can paste a returned name into a query.

## 4. Values, types and null

### 4.1 Type mapping

| Cypher type | RDF terms |
|---|---|
| Integer | `xsd:integer` and the types derived from it. Values keep SPARQL's arbitrary precision (§4.5). |
| Float | `xsd:double` and `xsd:float`. `xsd:decimal` is a Float for type checks and conversions, and arithmetic on it stays exact as in SPARQL. |
| String | `xsd:string`, `rdf:langString` and `rdf:dirLangString`. String functions ignore the language tag and direction, and `lang(x)` returns the tag. |
| Boolean | `xsd:boolean`. |
| Date, LocalDateTime, DateTime, LocalTime, Time | `xsd:date`, `xsd:dateTime` without a timezone, `xsd:dateTime` with one, `xsd:time` without and with one. |
| Duration | `xsd:duration`, `xsd:dayTimeDuration` and `xsd:yearMonthDuration`. |
| Node | An IRI or a blank node in a node position. |
| Relationship | A reifier, or a triple term for a relationship without one (§2.4). |
| Path | Built by the result shaper from the nodes and relationships of a pattern (§6.3). |
| List | A list built by `collect()` or a list literal (§4.7). Not an RDF term. |
| Map | A map literal, a map projection or a parameter. Not an RDF term. |
| null | An unbound variable, or an expression error (§4.2). |

A literal of any other datatype, including an ill-typed literal, is an opaque value. It
equals itself and nothing else, sorts with the strings, and converts to its lexical form
with `toString()`. IRIs in value positions, such as the object of a triple read through a
property expression, do not occur, because a resource object makes a relationship and not
a property.

### 4.2 Null and unbound

Cypher's null is SPARQL's unbound variable. An `OPTIONAL MATCH` that finds nothing leaves
its variables unbound, which Cypher reads as null, and `n.p` for a missing property
compiles to an `OPTIONAL` pattern that leaves its variable unbound.

Inside expressions, SPARQL has errors where Cypher has null. SPARQL's `&&`, `||` and `!`
treat an error as unknown in exactly the way Cypher's three-valued `AND`, `OR` and `NOT`
treat null, and a `FILTER` that errs drops the row as a `WHERE` that is null does. An
expression that errs in a projection leaves its variable unbound, which Cypher reads as
null. The compiler writes a null literal as `fn:error()`, which is always an error.

Cypher raises some errors at run time that SPARQL turns into an unbound value, such as
`toUpper(1)`. Sparkles returns null for these and does not fail the query. This is a
documented deviation, and the TCK scenarios that expect a run-time `TypeError` are
excluded (§12).

### 4.3 Equality, comparison and ordering

Cypher and SPARQL agree on the comparison of numbers, of strings by code point, of
booleans and of dates and times. They differ in four places, and the compiler handles
each.

- **Equality of different types.** In Cypher, `1 = 'a'` is false. In SPARQL it is an
  error. Cypher equality compiles to `COALESCE(a = b, false)` when both sides are bound
  and to null otherwise. Where the compiler can see that both sides are bound and of
  comparable types, it emits plain `=`, which keeps key-level filter evaluation.
- **Language tags.** `'Berlin' = n.name` is false when the value is `"Berlin"@en`,
  because the RDF terms differ. Neptune treats language-tagged literals the same way.
  `toString(n.name) = 'Berlin'` compares the lexical form. Open question 4 asks whether
  equality with a plain string should ignore the tag.
- **Nulls in `ORDER BY`.** Cypher sorts null last in ascending order and first in
  descending order. SPARQL sorts unbound first. `ORDER BY x` compiles to
  `ORDER BY (!BOUND(?x)) ?x`, and `ORDER BY x DESC` to `ORDER BY DESC(!BOUND(?x)) DESC(?x)`.
- **Mixed types in `ORDER BY`.** Cypher orders values of different types by a fixed order
  of types. The Neo4j manual gives it as maps, nodes, relationships, lists, paths, the
  temporal types, strings, booleans and numbers, with null last. When the compiler
  cannot prove that a sort key has one type, it adds a type-rank key before the value.

`DISTINCT`, grouping keys and `UNION` compare values as RDF terms, as SPARQL does. Where
Cypher's equivalence and RDF term identity differ, as they can for numerically equal
values of different number types, Sparkles follows RDF. The difference shows only when
one property holds such values.

### 4.4 Multi-valued properties

A node often has several values for one predicate in RDF data, most commonly labels in
several languages. Cypher expects one. Neptune picks an arbitrary value, which makes
results change from run to run. Sparkles defines the value instead.

1. **Language preference.** The request parameter or dataset setting `languages` lists
   language tags in order of preference, such as `en,de`. Of a node's values for one
   predicate, only those tagged with the first listed language that any of them has
   count. When no value has a listed language, all values count.
2. **In `WHERE` and in inline property maps,** a condition on a multi-valued property
   holds when some choice of values makes it true. `MATCH (n {name: 'Robert'})` and
   `WHERE n.name = 'Robert'` both match a node named "Bob" and "Robert". The compiler
   evaluates such a condition as an `EXISTS` over the property's values, so it never
   multiplies rows.
3. **Everywhere else,** in `RETURN`, `WITH`, `ORDER BY`, aggregates and expressions that
   build values, `n.p` is the least value in SPARQL's `ORDER BY` order under the default
   policy `multi-valued=first`. When a value was chosen from several, the response
   carries the notification `multi-valued-property`, which names the predicate. With
   `multi-valued=error`, a query that meets a node with several values in these positions
   fails with `cypher-multi-valued-property` and names the node and the predicate.
4. **In a returned node or relationship,** and in `properties(n)`, a property with several
   values is a list of all of them in SPARQL order. These values are only output, so a
   list cannot change the meaning of anything else in the query.

Phase 2 adds `multi-valued=list`, under which `n.p` is a list wherever a node has more
than one value, as neosemantics does with its `ARRAY` option. That needs lists in the
engine (§4.7). Whether it should become the default is open question 3.

The plan depends on the data, while the meaning does not. When the statistics of the
caller's view show that a predicate has as many triples as distinct subjects, no subject
has two values, and `n.p` compiles to a plain `OPTIONAL` triple pattern, which is exact
for that snapshot. Otherwise rule 3 compiles to a grouped subquery or to the late lookup
of §6.3 (§6.4).

### 4.5 Arithmetic and strings

| Cypher | Compiled to |
|---|---|
| `a + b` on numbers | SPARQL `+`. |
| `a + b` with a string | `CONCAT` of the operands' string forms, as Cypher concatenates `'a' + 1` to `'a1'`. |
| `a / b` | Integer division when both operands are integers, as `7 / 2 = 3`, and SPARQL division otherwise. Integer division by zero is null, where Cypher raises an error. |
| `a % b`, `a ^ b` | `fn:numeric-mod` and `math:pow`. `^` always returns a Float, as in Cypher. |
| `STARTS WITH`, `ENDS WITH`, `CONTAINS` | `STRSTARTS`, `STRENDS` and `CONTAINS` on the lexical forms. |
| `a =~ 'regex'` | `REGEX(a, "^(?:regex)$")`, because a Cypher regular expression must match the whole string. Constructs that Java's regex syntax has and the engine's does not are a compile error. |

When the compiler knows the operand types from literals or parameters, it emits the plain
SPARQL operator. When it does not, it emits a function in the `spk:` namespace,
`spk:cypherAdd` or `spk:cypherDiv`, that dispatches on the types at run time. These
functions do not block the join planner, but a filter that uses one cannot run on keys
alone, so the compiler prefers the plain operator whenever the types allow it.

Cypher integers are 64-bit and overflow is an error. SPARQL integers do not overflow, and
Sparkles keeps that. A value outside the 64-bit range is written as a JSON number in the
Cypher JSON format and keeps its lexical form in the SPARQL formats.

### 4.6 Parameters

Parameters (`$name`) come from the request's `parameters` object. They are never spliced
into text. Each value becomes an RDF term in the algebra or a `VALUES` block, as stored
query parameters do in [C16](C16-stored-queries.md), so no value can change the shape of
a query.

| JSON value | Becomes |
|---|---|
| string | an `xsd:string` literal |
| integer | an `xsd:integer` literal |
| other number | an `xsd:double` literal |
| boolean | an `xsd:boolean` literal |
| null | null, folded at compile time |
| array of scalars | a list, usable with `IN` and `UNWIND` (§5.1) |
| array of objects | rows for `UNWIND`, whose keys become columns (§6.2) |
| object | a map, usable through `$map.key` |

A parameter cannot be a node. `WHERE elementId(n) = $iri` and `WHERE id(n) = $iri`
compile to a binding of `?n` to that IRI, which is a single index lookup.

### 4.7 Lists and maps

The SPARQL algebra has no list or map values. In Phase 1, lists and maps exist in two
places only.

- **Constants and parameters.** List literals, `range()` with constant bounds and list
  parameters may appear after `IN` and in `UNWIND`, where they compile to SPARQL `IN` and
  `VALUES`. Map literals and map parameters may be read by key, which the compiler folds.
- **Built values.** `collect()` is an engine aggregate, `spk:collect`, that returns a
  literal of the private datatype `spk:list`, whose lexical form is the list of its terms
  in N-Triples syntax. `size()` reads it, and it can pass through `WITH` and be returned.
  List and map literals that contain variables, map projections such as
  `n {.name, .age}`, `labels(n)`, `keys(n)`, `properties(n)`, `nodes(p)` and
  `relationships(p)` are built by the result shaper and may appear only in the final
  `RETURN`.

Phase 2 adds list functions on `spk:list` values, `UNWIND` of a computed list, list
comprehensions, the quantifiers `any`, `all`, `none` and `single`, list indexing and
slicing, and `multi-valued=list`.

## 5. Supported Cypher

### 5.1 Clauses

| Clause | Phase | Notes |
|---|---|---|
| `MATCH` | 1 | Patterns of §5.2. Several comma-separated patterns form one clause, and relationship uniqueness applies across all of them. |
| `OPTIONAL MATCH` | 1 | Compiles to a left join. Its `WHERE` is the left join's condition. |
| `WHERE` | 1 | After `MATCH`, `OPTIONAL MATCH` and `WITH`. Pattern predicates such as `WHERE (a)-[:KNOWS]->(b)` and `WHERE NOT (a)-->()` compile to `EXISTS` and `NOT EXISTS`. |
| `RETURN` | 1 | With `DISTINCT`, aliases, `*`, `ORDER BY`, `SKIP` and `LIMIT`. |
| `WITH` | 1 | With `DISTINCT`, aliases, `*`, aggregation, `WHERE`, `ORDER BY`, `SKIP` and `LIMIT`. Each `WITH` is a subquery. |
| `ORDER BY`, `SKIP`, `LIMIT` | 1 | `SKIP` and `LIMIT` take constant expressions or parameters, not expressions over variables. |
| `UNWIND` | 1 | Of a list literal, a list parameter, `range()` with constant bounds, a list of maps from a parameter, or null. `UNWIND` of a computed list is Phase 2. |
| `UNION`, `UNION ALL` | 1 | The parts must return the same column names. |
| `EXPLAIN`, `PROFILE` | 1 | Prefixes that return the plan without running the query, or the plan with actual counts (§7.3). |
| `CALL db.labels()`, `db.relationshipTypes()`, `db.propertyKeys()` | 2 | Answered from the statistics of the view, with names as in §3.3. |
| `EXISTS { … }`, `COUNT { … }` | 2 | Subquery expressions of Neo4j 5 and GQL. |
| Quantified path patterns, `((a)-[:R]->(b)){1,3}` | 2 | The GQL form of variable-length patterns. |
| `shortestPath`, `allShortestPaths`, `ANY SHORTEST` | 3 | Through the path search of F07. These are Neo4j and GQL extensions, not part of openCypher 9. |

Clauses that change data, `CALL` of other procedures, `CALL { … }` subqueries, `USE`,
`MANDATORY MATCH`, `LOAD CSV`, `START` and `SHOW` commands fail with
`cypher-unsupported` (§5.6). `USING` hints are ignored with the notification
`hint-ignored`, because the planner chooses its own join order.

### 5.2 Patterns

| Pattern | Compiled to |
|---|---|
| `(n)` alone | The node set of §2.1, the distinct subjects and resource objects of relationships. This is a full scan, as it is in a property-graph database. |
| `(n:A:B)` | `?n rdf:type A . ?n rdf:type B`. |
| `(n {k: v})` | `?n k v`, with the existential reading of §4.4. |
| `(a)-[:T]->(b)` | `?a T ?b` with `?b` restricted to IRIs and blank nodes, plus the reifier lookup when identity matters (§6.2). |
| `(a)<-[:T]-(b)` | `?b T ?a`. |
| `(a)-[:T]-(b)` | The union of both directions. |
| `(a)-[:T1\|T2]->(b)` | `?a ?t ?b` with `VALUES ?t { T1 T2 }`. |
| `(a)-[r]->(b)` | `?a ?t ?b` with `?t` neither `rdf:type` nor `rdf:reifies`, and `?b` an IRI or a blank node. |
| `(a)-[r:T {k: v}]->(b)` | The reifier lookup is required, and `?r k v` holds on the reifier. |
| `p = (a)-[:T]->(b)-[:U]->(c)` | The pattern, with the path built by the result shaper. |

Within one `MATCH` clause, two relationship variables, including anonymous ones, never
bind the same relationship. This is Cypher's relationship isomorphism. The compiler adds
an inequality on the relationship identities only where two pattern relationships could
coincide, which needs the same or an unknown type and compatible endpoints. Nodes may
repeat, as in Cypher.

How often a self-loop matches an undirected pattern follows the TCK's `Match` scenarios.

### 5.3 Variable-length relationships

Cypher's `-[:T*n..m]->` matches each trail of `n` to `m` relationships, which is a path
that uses no relationship twice, and returns one row per trail. SPARQL property paths
return each pair of endpoints once and have no bounded repetition. The compiler therefore
has two strategies, and refuses the cases neither covers.

- **Bounded expansion.** When `m` is finite and at most `max-path-length` (default 10,
  §10), the pattern becomes a `UNION` with one branch per length from `n` to `m`. A
  branch of length `k` is a chain of `k` relationship patterns with fresh intermediate
  variables, with inequalities that keep the `k` relationships distinct. A length of 0
  binds the two endpoints to the same node. This gives Cypher's rows exactly, including
  their multiplicity. Property maps on the relationship apply to every step, and a
  relationship-list variable (`-[rs:T*1..3]->`) or a path variable is built by the shaper
  from the chain's variables.
- **Reachability.** When `m` is unbounded, the pattern is directed, `n` is 0 or 1, and the
  relationship has neither a variable, a path variable nor a property map, the pattern
  compiles to the property path
  `T+` or `T*`, with the object restricted to resources. For directed graphs, a node
  reaches another by some trail exactly when it reaches it by some walk, so the set of
  endpoint pairs is the same as Cypher's. Only the multiplicity differs. Under `DISTINCT`,
  in a pattern predicate, or as the input of a `DISTINCT` aggregate, the answer is
  exactly Cypher's. Elsewhere the result has one row per reachable endpoint where Cypher
  has one per trail, and the response carries the notification `path-multiplicity`. With
  `strict=true`, such a query fails with `cypher-unbounded-path` instead.
- **Refused.** An unbounded pattern that is undirected, has a lower bound above 1, binds
  a path or relationship variable, or has relationship properties fails with
  `cypher-unbounded-path`. A bound above `max-path-length` fails with
  `cypher-path-too-long`. Both messages suggest an upper bound. Phase 2 adds a path
  enumeration operator in the engine that walks trails under the budgets of §10, which
  removes these refusals.

Relationship uniqueness reaches across a variable-length pattern too. A bounded expansion
adds inequalities between its steps and the other relationships of the clause that could
coincide with them. The reachability translation has no step variables, so it applies
only when no other relationship pattern in the same `MATCH` clause could share a
relationship with it.

An untyped variable-length pattern, `-[*1..3]->`, uses every predicate except
`rdf:type` and `rdf:reifies`. In a property path this is the negated property set
`!(rdf:type|rdf:reifies)`.

### 5.4 Expressions and functions

| Group | Phase 1 | Notes |
|---|---|---|
| Literals | integers, floats, strings, booleans, null, lists and maps of §4.7 | Hexadecimal and octal integers are accepted. |
| Operators | arithmetic, comparison, `AND`, `OR`, `XOR`, `NOT`, `IS NULL`, `IS NOT NULL`, `IN`, string operators, property access, `n:Label` | As in §4.3 and §4.5. |
| Conditionals | `CASE` in both forms, `coalesce()` | A simple `CASE` compares with Cypher equality, so a null test value falls through to `ELSE`. |
| Identity | `id()`, `elementId()`, `labels()`, `type()`, `startNode()`, `endNode()`, `keys()`, `properties()` | `id()` returns the same string as `elementId()`, as Neptune does, not an integer. |
| Strings | `toUpper`, `toLower`, `trim`, `lTrim`, `rTrim`, `substring`, `left`, `right`, `replace`, `reverse`, `size`, `toString`, `split` | `substring` is zero-based. `split` returns a list and is final-`RETURN` only in Phase 1. |
| Numbers | `abs`, `ceil`, `floor`, `round`, `sign`, `rand`, `sqrt`, `exp`, `log`, `log10`, `e`, `pi`, the trigonometric functions, `toInteger`, `toFloat`, `toBoolean` | Conversions return null for input they cannot convert, as in Cypher. |
| Paths | `length()`, `nodes()`, `relationships()` | `length()` is a constant per expansion branch. |
| Time | `timestamp()` | Constructors and arithmetic for temporal values are Phase 2. |
| RDF | `lang(x)`, `datatype(x)`, `iri(n)` | Sparkles extensions. `iri(n)` returns a node's IRI, and null for a blank node. |

`elementId()` of a node is its IRI, or its blank-node label `_:b…`, which identifies the
same stored blank node when it comes back as a parameter, as for SPARQL initial bindings.
`elementId()` of a relationship is its reifier's IRI or label, or the triple term in
N-Triples syntax, `<<( <s> <p> <o> )>>`.

### 5.5 Aggregation

Grouping is implicit, as in Cypher. The non-aggregate expressions of a `RETURN` or `WITH`
that contains an aggregate are the grouping keys, and they compile to SPARQL `GROUP BY`.

| Cypher | SPARQL | Difference handled |
|---|---|---|
| `count(*)`, `count(x)`, `count(DISTINCT x)` | `COUNT` | None. Both skip null. |
| `sum(x)` | `SUM` | None. Both return 0 over no rows. |
| `avg(x)` | `AVG` | SPARQL returns 0 over no rows and Cypher returns null, so the compiler wraps it in `IF(COUNT(?x) = 0, null, AVG(?x))`. |
| `min(x)`, `max(x)` | `MIN`, `MAX` | Mixed types order as in §4.3. |
| `stDev(x)`, `stDevP(x)` | ARQ's `STDEV_SAMP` and `STDEV_POP` | None. |
| `collect(x)` | `spk:collect` | Skips null, as Cypher does. |
| `percentileCont`, `percentileDisc` | | Phase 2, as new engine aggregates. |

`collect()` keeps the order of its input rows when they come from a `WITH … ORDER BY`.
That requires the engine's grouping to keep input order within a group, which §6.4 lists
as a requirement on the implementation.

### 5.6 Unsupported features and their errors

Every unsupported feature fails at compile time, before any data is read, with a `400`
whose `code` is `cypher-unsupported` and whose `feature` names it.

| Feature | Message points to |
|---|---|
| `CREATE`, `MERGE`, `SET`, `REMOVE`, `DELETE`, `DETACH DELETE`, `FOREACH` | "Cypher queries are read-only. Use SPARQL Update to change data." |
| `LOAD CSV` | the upload and Graph Store routes |
| `CALL` of a procedure other than §5.1's, `CALL { … }` | SPARQL, or the listed procedures |
| `shortestPath`, `allShortestPaths` | variable-length patterns with an upper bound |
| `USE`, `MANDATORY MATCH`, `START`, `SHOW …` | none |
| A Phase 2 feature used in Phase 1 | the phase that adds it |

The messages give the line and column of the construct, as SPARQL parse errors do.

## 6. Compilation

### 6.1 Pipeline

1. **Parse.** A hand-written recursive-descent parser, written from the openCypher 9
   grammar, builds an AST with source positions. Keywords are case-insensitive.
2. **Analyse.** Scopes follow Cypher: each `WITH` and `RETURN` starts a new scope. Each
   variable gets a kind, which is node, relationship, path or value. The analyser reports
   openCypher's compile-time errors, such as an undefined variable or a variable used as
   both a node and a relationship.
3. **Resolve names.** Every label, type and key becomes an IRI or "unknown" by §3.2.
4. **Plan the shape.** The compiler decides, per property access and per variable-length
   pattern, which translation of §4.4 and §5.3 applies, using the statistics of the view.
5. **Emit.** The compiler builds a spargebra `Query` directly. It never prints and
   reparses SPARQL text.
6. **Execute.** `execute_query` runs the algebra with the request's `QueryOptions`, which
   carry the timeout, budgets, graph view, reasoning, `at` and cancellation.
7. **Shape.** The result shaper builds the Cypher values of the final `RETURN` from the
   returned rows (§6.3) and serializes them.

The parser and compiler live in a new crate, `sparkles-cypher`, which depends on the
vendored `spargebra` and on `sparkles` for statistics and terms. The server's `cypher`
feature, on by default, adds the route.

### 6.2 Clause translation

With the namespace `http://example.org/`, this query

```cypher
MATCH (p:Person)-[k:KNOWS]->(f:Person)
WHERE p.age > 30
RETURN p.name AS name, f.name AS friend, k.since AS since
ORDER BY name
LIMIT 10
```

compiles to the algebra of this SPARQL query, when the statistics show that `ex:age` and
`ex:name` have one value per subject and the view has reifiers:

```sparql
PREFIX ex: <http://example.org/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
SELECT ?name ?f ?k WHERE {
  ?p a ex:Person .
  ?p ex:KNOWS ?f .
  ?f a ex:Person .
  OPTIONAL { ?k_r rdf:reifies <<( ?p ex:KNOWS ?f )>> }
  BIND (COALESCE(?k_r, TRIPLE(?p, ex:KNOWS, ?f)) AS ?k)
  ?p ex:age ?p_age .
  FILTER (isLITERAL(?p_age) && ?p_age > 30)
  OPTIONAL { ?p ex:name ?name FILTER isLITERAL(?name) }
}
ORDER BY (!BOUND(?name)) ?name
LIMIT 10
```

`f.name` and `k.since` appear only in the final `RETURN`, so the result shaper reads them
for the ten returned rows (§6.3) and the algebra projects `?f` and `?k` instead.

The translation follows these rules.

- **Labels and types** become triple patterns. The restriction of `?f` to resources is
  dropped, because `?f a ex:Person` already makes `?f` a subject.
- **Relationship identity.** The `OPTIONAL` reifier lookup and the `BIND` give `?k` one
  value per relationship, as §2.4 defines. When the view has no `rdf:reifies` triples,
  the compiler leaves both out and binds `?k` to the triple term. When the relationship
  has no variable, no property map and its multiplicity cannot show, as under `DISTINCT`
  or in a pattern predicate, it leaves out the lookup as well.
- **Null-rejecting conditions.** `p.age > 30` is false when the age is null, so the
  property's `OPTIONAL` becomes a plain join, which the planner can order freely.
- **`WITH`** becomes a subquery that projects the `WITH`'s columns, with `GROUP BY`,
  `HAVING` for a `WHERE` after aggregation, `ORDER BY`, `OFFSET` and `LIMIT`. The clauses
  after it join with the subquery, as Cypher's clause sequence joins each new `MATCH` with
  the table before it.
- **`OPTIONAL MATCH`** is a left join of the table so far with the pattern, and its
  `WHERE` is the left join's expression.
- **`UNWIND`** of constants or parameters is a `VALUES` block. A list of maps from a
  parameter becomes one column per key the query reads, so
  `UNWIND $rows AS row MATCH (n {name: row.name})` is one `VALUES` block with a `?row_name`
  column joined with the pattern.
- **`UNION`** is SPARQL `UNION`, and `UNION` without `ALL` adds `DISTINCT`.
- **Identifiers.** Generated variable names never collide with user variables, and the
  projection keeps the user's column names and order.

### 6.3 Result shaping

The shaper runs on the final rows, after `ORDER BY`, `SKIP` and `LIMIT`. It reads the
solution table by id and builds what the algebra cannot.

- **Nodes.** For the distinct nodes of the returned rows, one batched lookup reads their
  `rdf:type` objects and literal-valued triples through the caller's view.
- **Relationships.** For each relationship, the shaper has the reifier or triple term,
  the endpoints and the type from the algebra, and reads the reifier's literal-valued
  triples.
- **Paths, lists and maps** that the final `RETURN` builds from variables.
- **Late properties.** A property that appears only in the final `RETURN`, and in no
  `WHERE`, `ORDER BY`, `WITH` or aggregate, is not joined in the algebra. The shaper reads
  it for the returned rows only. For `MATCH (n:Person) RETURN n.name LIMIT 10`, that is
  ten lookups instead of a join with every person's name.

The shaper's reads go through the same snapshot and graph view as the query, and they
count toward the `rows-produced` budget.

### 6.4 Statistics-driven choices

The compiler reads three facts from the statistics of the caller's view, which
`sparql::stats` already keeps per generation and corrects for the delta and the graphs
read.

| Fact | When true | When false |
|---|---|---|
| The view has `rdf:reifies` triples. | Reifier lookups as in §6.2. | No lookups. Relationships are triples. |
| A predicate has as many triples as distinct subjects. | `n.p` is a plain `OPTIONAL`. | `n.p` in `WHERE` is an `EXISTS`. Elsewhere it is a grouped subquery with `MIN`, or a late lookup (§6.3). |
| A name has candidates under §3.2 rule 3. | The name resolves, or is ambiguous. | The name is unknown. |

The choices change the plan, never the answer. A query that runs at one commit and again
after a write that adds a second value gets a different plan and an answer that is still
defined by §4.4.

Two requirements fall on the engine. The grouping operator must keep input order within
a group, so that `collect()` after `WITH … ORDER BY` returns a sorted list. The reifier
lookup must bind a triple term from three bound variables with one vocabulary probe,
without scanning every `rdf:reifies` triple.

## 7. HTTP endpoint

### 7.1 Requests

`POST /{ds}/cypher` runs one Cypher query. The body is JSON:

```json
{ "query": "MATCH (p:Person) WHERE p.age > $min RETURN p.name AS name",
  "parameters": { "min": 30 } }
```

`statement` is accepted in place of `query`, as Neo4j's Query API names it. A form body
with `query` and `parameters`, where `parameters` is JSON text, works too, and so does
`GET /{ds}/cypher?query=…&parameters=…`.

The other request parameters, in the query string or the JSON body, are those of
`/{ds}/sparql`: `timeout`, `at`, `reasoning`, `nocache`, `default-graph-uri`, `format`
and the budget overrides `memory-mb`, `max-rows`, `max-rows-produced` and
`max-result-mb`. Four are specific to Cypher.

| Parameter | Values | Default |
|---|---|---|
| `languages` | Language tags in order of preference, such as `en,de`. | The dataset setting, otherwise none. |
| `multi-valued` | `first` or `error`, and `list` from Phase 2. | The dataset setting, otherwise `first`. |
| `namespace` | An IRI that overrides the dataset's namespace for this request. | The dataset setting. |
| `strict` | `true` refuses queries whose answer could differ from openCypher's (§5.3, §4.4). | `false` |

### 7.2 Responses

The `Accept` header or `format` chooses the result format.

**Cypher JSON** (`application/json`, the default) is modelled on the plain JSON of Neo4j's
Query API. With the data of §14,
`MATCH (:Person {name: 'Alice'})-[:KNOWS]->(f) RETURN f.name AS friend, f AS node`
returns:

```json
{
  "data": {
    "fields": ["friend", "node"],
    "values": [["Bob", { "elementId": "http://example.org/bob",
                         "labels": ["Person"],
                         "properties": { "name": ["Bob", "Robert"], "age": 41 } }]]
  },
  "notifications": [
    { "code": "multi-valued-property", "severity": "INFORMATION",
      "message": "ex:name has several values for some nodes. f.name returns the least.",
      "position": { "line": 1, "column": 54 } }
  ],
  "meta": { "commit": 42, "timing": { "compileMs": 0.3, "execMs": 4.1, "totalMs": 5.0 } }
}
```

A relationship is `{elementId, type, startNodeElementId, endNodeElementId, properties}`.
A path is an array that alternates nodes and relationships. Strings, numbers, booleans,
null, lists and maps are JSON values. Language-tagged strings lose their tag, and dates,
times, durations and opaque literals are their lexical forms. Clients that need the exact
RDF terms use a SPARQL format.

**SPARQL results** (`application/sparql-results+json`, `+xml`, `text/csv`,
`text/tab-separated-values`) carry exact RDF terms. A node is its IRI or blank node. A
relationship is its reifier, or a triple term in the SPARQL 1.2 JSON form. A list is a
`spk:list` literal, whose lexical form keeps the RDF term of each element, and a map is
an `rdf:JSON` literal. These formats let SPARQL tooling, and the UI's result table,
read Cypher results.

**`application/x-sparkles+json`** is the UI's format (§8.3). It adds the plan, the
generated SPARQL and the notifications to the SPARQL JSON body.

The response has the headers of a SPARQL query, including the commit and history
headers.

### 7.3 EXPLAIN and PROFILE

A query that starts with `EXPLAIN` is compiled and planned, not run. The response is
`{ "sparql": string, "algebra": string, "plan": PlanNode, "notifications": [...] }`, with
the SSE algebra and the plan of `/{ds}/explain`. The generated SPARQL is text printed from
the algebra, which helps users learn the mapping and move a query to SPARQL. `PROFILE`
runs the query and returns the same object with actual row counts and times in the plan,
plus the results. `/{ds}/explain` does not accept Cypher, so that its input stays a
SPARQL query.

### 7.4 Errors and notifications

Errors have the body of every Sparkles error, `{error, detail, line, column, requestId}`,
with a `code` and an `openCypher` object that names the TCK's error type and detail,
such as `{ "type": "SyntaxError", "detail": "UndefinedVariable" }`.

| Status | `code` | When |
|---|---|---|
| `400` | `cypher-syntax` | The query does not parse. |
| `400` | `cypher-semantic` | Analysis fails: an undefined variable, a kind conflict, an aggregate in `WHERE`, column names that differ across `UNION`. |
| `400` | `cypher-unsupported` | §5.6. The body has `feature`. |
| `400` | `cypher-ambiguous-name` | §3.2 rule 3 found several IRIs. The body lists them. |
| `400` | `cypher-unknown-prefix` | A prefixed name uses an undeclared prefix. |
| `400` | `cypher-unbounded-path`, `cypher-path-too-long` | §5.3. |
| `400` | `cypher-parameter` | A parameter is missing or has a shape its use does not accept. |
| `422` | `cypher-multi-valued-property` | `multi-valued=error` met a node with several values. |
| `408`, `503`, `507` | as for SPARQL | Timeout, cancellation and budgets (§10). |

Notifications are warnings that do not fail the query. The codes are `unknown-name`,
`multi-valued-property`, `path-multiplicity` and `hint-ignored`. Each has a severity, a
message and a position.

### 7.5 Compatibility with Neo4j clients

The Neo4j drivers, Neo4j Browser, Bloom and `cypher-shell` speak the Bolt protocol, and
none of them can use this endpoint. Bolt needs sessions, transactions, bookmarks and
PackStream encoding, and it makes sense only together with writes. It is out of scope
here and would get its own spec.

Clients that can use Cypher on Sparkles in Phase 1 are HTTP clients that send a query and
read JSON. These include `curl`, scripts and notebooks, the Sparkles UI, the CLI, the MCP
server and language-model agents. Phase 3 adds `POST /db/{ds}/query/v2`, which accepts
the request body of Neo4j's Query API and returns its plain JSON. Tools and scripts
written against that API then work unchanged for read queries. A Neptune-compatible
route is open question 6.

### 7.6 Dataset settings

A dataset's Cypher settings live in `<db>/cypher.json`, next to `prefixes.json`, and
travel with backups and clones:

```json
{
  "namespace": "http://example.org/",
  "names": {
    "labels": { "Person": "http://xmlns.com/foaf/0.1/Person" },
    "types": { "KNOWS": "http://xmlns.com/foaf/0.1/knows" },
    "keys": { "name": "http://xmlns.com/foaf/0.1/name" }
  },
  "languages": ["en"],
  "multiValued": "first",
  "maxPathLength": 10
}
```

`GET /$/cypher/{ds}` returns them and needs `read`. `PUT /$/cypher/{ds}` replaces them
and needs `admin`. Settings are configuration, not data, so a change makes no commit. A
dataset's `maxPathLength` cannot exceed the server's `--cypher-max-path-length`.

## 8. CLI, MCP and UI

### 8.1 CLI

```
sparkles cypher --loc DB 'MATCH (n:Person) RETURN n.name LIMIT 5'
sparkles cypher --server URL --dataset NAME --file query.cypher [--param NAME=JSON]…
sparkles cypher --loc DB --sparql 'MATCH …'
```

`sparkles cypher` takes the options of `sparkles query` for output and budgets. `--sparql`
prints the generated SPARQL without running it, which serves as a translation tool.

### 8.2 MCP

Phase 2 adds the read-only tool `cypher_query` to the MCP server of
[C11](C11-mcp-server.md). Its arguments are `dataset`, `query`, `parameters`, `maxRows`,
`atCommit` and `timeoutSeconds`, and its result is rendered like `sparql_query`'s. The
tool description states the mapping of §2 in a few sentences, and `describe_schema`
gains the Cypher names of the classes and predicates it lists, so that an agent can
write a query without guessing names.

### 8.3 UI

Phase 2 adds Cypher to the query page.

- Each editor tab has a language, SPARQL or Cypher. A Cypher tab gets syntax highlighting
  and completion of the labels, types and keys of §3.3, from the dataset's schema report
  ([C02](C02-schema-discovery.md)).
- Results use the existing table. When a result returns nodes or relationships, the graph
  view that draws `CONSTRUCT` results draws them too.
- A **SPARQL** button shows the generated SPARQL and opens it in a new SPARQL tab. The
  plan view shows the plan of `PROFILE`.
- Notifications appear above the results.

## 9. Access control

A Cypher query runs as the caller, exactly as a SPARQL query on `/{ds}/sparql` does.

- `/{ds}/cypher` and the MCP tool count as the `query` endpoint of
  [C12](C12-graph-access-control.md). They read nothing that a SPARQL query cannot, so a
  separate endpoint name would add configuration without adding protection. Open
  question 9 asks whether operators want one anyway.
- The compiled query runs on the caller's graph view, through `QueryOptions::graphs`.
- The statistics behind §6.4 and the name candidates of §3.2 come from the caller's
  view. A principal limited to some graphs never sees a class or predicate that occurs
  only in hidden graphs, in an ambiguity error, in `CALL db.labels()` or in a
  notification. A name that resolves only in hidden graphs behaves as an unknown name.
- The result shaper reads labels and properties through the same view, so a returned
  node shows only the triples the caller may read.
- The dataset's Cypher settings are readable with `read` through the `info` endpoint,
  and changeable with `admin`.
- Rate limits count a Cypher request in the `query` class. The access log and metrics
  record `op="cypher"`.

## 10. Budgets and limits

The query budgets of C01 apply unchanged: the timeout, `memory`, `rows`, `rows-produced`
and `result-bytes`, with the same per-request overrides and the same `507` and `408`
responses. The shaper's lookups count toward `rows-produced`, and the shaped body counts
toward `result-bytes`.

The compiler has its own limits, which bound the size of the algebra before the engine
sees it.

| Limit | Default | Why |
|---|---|---|
| Query text | the query body ceiling, `maxQueryBodyBytes` | as for SPARQL |
| Parameters | the same ceiling, for the whole JSON body | A `VALUES` block from a parameter also counts toward the `rows` budget. |
| Nesting | the nesting limits of SPARQL | The parser has a depth limit, and the generated algebra passes the existing depth check. |
| `max-path-length` | 10 relationships, `--cypher-max-path-length` | Each extra hop multiplies the work of a bounded expansion by the fan-out. |
| Expansion branches | 256 `UNION` branches per query | Several variable-length patterns multiply their branches. More fails with `cypher-path-too-long`. |

A client disconnect cancels a Cypher query, as it cancels a SPARQL query.

## 11. Performance

The Cypher frontend should cost little beyond the SPARQL query it produces.

- **Compilation** of a query under 4 KiB should take under 1 ms. The name table and the
  statistics it reads are built once per snapshot and view and shared by later queries.
- **Overhead against hand-written SPARQL.** On the benchmark data at 1.05M and 10.5M
  triples, a Cypher version of each applicable benchmark query should take at most 10%
  more median time than the SPARQL query a person would write for it. The Outcome will
  record the comparison.
- **Free cases stay free.** Without `rdf:reifies` triples, relationships cost one triple
  pattern. With functional predicates, properties cost one `OPTIONAL` triple pattern.
  The resource restrictions on objects are kind checks on ids, which the engine evaluates
  on keys.
- **Late lookups** make returned properties cost a lookup per returned row instead of a
  join over every match.
- **Operator functions** such as `spk:cypherAdd` cannot run on keys, so the compiler emits
  them only when the operand types are unknown.
- **Reifier lookups** need a vocabulary probe for each candidate triple term and a scan
  of `rdf:reifies` by object. If measurement shows that this dominates queries over
  heavily annotated data, a derived index from a triple to its reifiers can follow. It
  would cost about 32 bytes per reifier on disk and in the block cache, and it is not part
  of Phase 1.
- **Bounded expansions** grow with the fan-out to the power of the bound. The budgets
  stop runaway queries, and `EXPLAIN` shows the branches.

The benchmark gains a Cypher set. It covers the star, path and aggregation queries of the
existing set, the short reads IS1 to IS7 of the LDBC Social Network Benchmark on its
Turtle serialization, and multi-valued and annotated data.

## 12. Conformance: the TCK subset

The openCypher TCK (Apache-2.0) is a set of Gherkin scenarios. Each scenario builds a
graph with `CREATE`, runs a query, and checks the result or the error. A new task,
`mise run test:tck`, runs the scenarios from a pinned checkout, as `test:w3c` runs the
W3C suites.

**Loading the scenario graphs.** The harness has a test-only translator from the TCK's
`CREATE` statements to RDF. It is not reachable through the server. Each created node is
a fresh IRI `urn:tck:n<k>` with the triple `rdf:type rdfs:Resource`, so that a node
without labels or properties exists without gaining a label (§2.2). Each label becomes an
`rdf:type` triple and each property a literal triple, in the namespace `urn:tck:`, which
the run configures as the dataset's namespace. Each created relationship becomes an
asserted triple with its own reifier, so parallel relationships and relationship
properties have exactly the multiplicity the TCK expects. List-valued properties are
encoded as one triple per element, and the scenarios that depend on their order are
excluded.

**Comparing results.** The harness compares nodes by labels and properties and
relationships by type and properties, as the TCK's result notation does. It maps
Sparkles' error codes to the TCK's error types through the `openCypher` field of §7.4.

**Scope in Phase 1.**

| TCK features | In scope |
|---|---|
| `clauses/match`, `match-where` | yes, without shortest paths and unbounded patterns that need enumeration |
| `clauses/return`, `return-orderby`, `return-skip-limit` | yes |
| `clauses/with`, `with-where`, `with-orderBy`, `with-skip-limit` | yes |
| `clauses/unwind`, `clauses/union` | yes, without `UNWIND` of computed lists |
| `expressions/aggregation`, `boolean`, `comparison`, `conditional`, `literals`, `mathematical`, `null`, `string`, `typeConversion`, `graph`, `pattern` | yes |
| `expressions/list`, `map`, `path`, `quantifier`, `existentialSubqueries` | Phase 2 |
| `expressions/temporal` | Phase 2, for the types of §4.1 |
| `clauses/create`, `merge`, `set`, `remove`, `delete`, `call`, and `useCases` | no |

**Exclusions.** A checked-in exclusions file lists every in-scope scenario that does not
pass, with one reason each. The expected reasons are the deviations of this spec: integer
identifiers, run-time type errors and integer division by zero that return null, numeric
equivalence in `DISTINCT`, integer overflow, path multiplicity in default mode, and
features of a later phase. The acceptance bar for Phase 1 is that every in-scope scenario
passes or is excluded for a listed reason, and that no reason is "unknown". The Outcome
will record the counts.

## 13. Phasing

**Phase 1: the read subset.**

- `sparkles-cypher` with the parser, the analyser, name resolution and the compiler.
- The `spk:collect` aggregate, the `spk:list` datatype, the `spk:cypher*` operator
  functions and the order-preserving grouping of §6.4.
- `/{ds}/cypher` with Cypher JSON and the SPARQL formats, `EXPLAIN`, `PROFILE`, errors and
  notifications.
- `cypher.json` with `GET` and `PUT /$/cypher/{ds}`.
- `sparkles cypher`, including `--sparql`.
- The TCK harness and exclusions, the acceptance tests of §14 and the Cypher benchmark
  set.
- `docs/API.md` and `docs/USAGE.md` sections, the README status row and the PROVENANCE
  entry's implementation part.

**Phase 2: lists, paths and clients.**

- Lists in the engine: list functions, `UNWIND` of computed lists, comprehensions,
  quantifiers and `multi-valued=list`.
- A path enumeration operator for unbounded and undirected variable-length patterns,
  with trail semantics, under the budgets.
- Quantified path patterns, `EXISTS { … }` and `COUNT { … }`.
- `CALL db.labels()`, `db.relationshipTypes()` and `db.propertyKeys()`.
- Temporal constructors and arithmetic, and `percentileCont` and `percentileDisc`.
- The MCP tool and the UI.

**Phase 3: compatibility.**

- `POST /db/{ds}/query/v2` for Neo4j Query API clients.
- Stored Cypher queries in the catalog of C16, with parameters typed as there.
- `shortestPath`, `allShortestPaths` and GQL's `ANY SHORTEST` through F07's path search,
  once it exists.

Writes, Bolt and a stored property-graph representation are separate specs if they are
ever wanted.

## 14. Acceptance examples

The examples use this data, loaded into dataset `t` with the namespace
`http://example.org/`:

```turtle
PREFIX ex: <http://example.org/>

ex:alice a ex:Person ; ex:name "Alice" ; ex:age 34 .
ex:bob   a ex:Person ; ex:name "Bob" , "Robert" ; ex:age 41 .
ex:carol a ex:Person , ex:Manager ; ex:name "Carol" .
ex:dave  a ex:Person ; ex:name "Dave" .
ex:acme  a ex:Company ; ex:name "ACME"@en , "ACME GmbH"@de .

ex:alice ex:KNOWS ex:bob {| ex:since 2019 |} .
ex:bob   ex:KNOWS ex:carol .
ex:carol ex:KNOWS ex:dave .
ex:carol ex:WORKS_FOR ex:acme ~ ex:job1 {| ex:role "Engineer" |} .
ex:carol ex:WORKS_FOR ex:acme ~ ex:job2 {| ex:role "Advisor" |} .
```

- **A1. Labels and properties.** `MATCH (p:Person) WHERE p.age > 35 RETURN p.name` returns
  one row, `"Bob"`, with the notification `multi-valued-property` for `ex:name`.
- **A2. Multi-valued properties.** `MATCH (p {name: 'Robert'}) RETURN p.name` returns
  `"Bob"`, the least value. `RETURN p` returns Bob's node with `"name": ["Bob",
  "Robert"]`. With `multi-valued=error`, `RETURN p.name` fails with `422`
  `cypher-multi-valued-property`, while `RETURN elementId(p)` succeeds, because the
  inline map reads the property existentially.
- **A3. Language preference.** `MATCH (c:Company) RETURN c.name` with `languages=de`
  returns `"ACME GmbH"`, with `languages=en` returns `"ACME"`, and neither request has a
  `multi-valued-property` notification.
- **A4. Relationship properties.** `MATCH (a)-[k:KNOWS]->(b) RETURN a.name, b.name,
  k.since ORDER BY k.since` returns Alice, Bob and 2019 first, followed by the two rows
  whose `k.since` is null.
- **A5. Parallel relationships.** `MATCH (:Person {name: 'Carol'})-[w:WORKS_FOR]->(c)
  RETURN w.role ORDER BY w.role` returns `"Advisor"` and `"Engineer"`, and
  `RETURN count(w)` returns 2. The two `elementId(w)` values are the IRIs of `ex:job1`
  and `ex:job2`.
- **A6. Identity of unannotated relationships.** For `MATCH (:Person {name: 'Bob'})-[k]->(m)
  RETURN elementId(k), type(k)`, the identifier is
  `<<( <http://example.org/bob> <http://example.org/KNOWS> <http://example.org/carol> )>>`
  and the type is `KNOWS`.
- **A7. OPTIONAL MATCH and null ordering.** `MATCH (p:Person) OPTIONAL MATCH
  (p)-[:WORKS_FOR]->(c) RETURN p.name, c.name ORDER BY c.name` returns Carol's two rows
  first and the three rows whose company is null last. With `ORDER BY c.name DESC`, the
  null rows come first.
- **A8. WITH and aggregation.** `MATCH (p:Person)-[:KNOWS]->(f) WITH p, count(f) AS n
  WHERE n >= 1 RETURN p.name, n ORDER BY p.name` returns Alice, Bob and Carol, each with
  1. `MATCH (p:Person) RETURN avg(p.height)` returns null, not 0.
- **A9. Bounded variable length.** `MATCH (:Person {name: 'Alice'})-[:KNOWS*1..3]->(x)
  RETURN x.name` returns Bob, Carol and Dave. `EXPLAIN` shows a `UNION` of three chains.
  A bound of `*1..20` fails with `cypher-path-too-long`.
- **A10. Unbounded variable length.** With an added triple `ex:alice ex:KNOWS ex:carol`,
  `MATCH (:Person {name: 'Alice'})-[:KNOWS*]->(x) RETURN DISTINCT x.name` compiles to the
  property path `ex:KNOWS+` and returns Bob, Carol and Dave. Without `DISTINCT` it returns
  the same three rows with the notification `path-multiplicity`. With `strict=true` it
  fails with `cypher-unbounded-path`, and `MATCH p = (a)-[:KNOWS*]->(b) RETURN p` fails
  the same way in every mode.
- **A11. Names.** Without a namespace, against data that uses FOAF and a dataset that
  declares the prefix `foaf`,
  `MATCH (p:Person)-[:knows]->(f) RETURN f.name` resolves through local names. When a
  second vocabulary also defines `Person`, the query fails with `cypher-ambiguous-name`
  and lists both IRIs. `` MATCH (p:`<http://xmlns.com/foaf/0.1/Person>`) `` and
  `MATCH (p:foaf::Person)` succeed in both cases. An unknown label returns no rows with
  the notification `unknown-name`.
- **A12. Round-trip names.** Every name in `labels(n)`, `type(r)` and `keys(n)` of every
  example, put inside backticks in a new query, resolves to the same IRI.
- **A13. Read-only.** `CREATE (n:Person {name: 'Eve'})` fails with `400`
  `cypher-unsupported`, `feature: "CREATE"`, and the dataset's commit does not change.
- **A14. Parameters.** `MATCH (p:Person) WHERE p.name = $n RETURN p` with
  `{"n": "Alice\" }) RETURN 1 //"}` returns no rows and no other data.
  `UNWIND $rows AS row MATCH (p:Person {name: row.name}) RETURN p.age` with two rows
  returns both ages from one `VALUES` block.
- **A15. Null semantics.** `RETURN null = null AS a, 1 = 'a' AS b, null OR true AS c,
  7 / 2 AS d, 7 / 2.0 AS e` returns null, false, true, 3 and 3.5.
- **A16. Access control.** For a principal that may read only a graph without the
  `KNOWS` triples, `MATCH ()-[:KNOWS]->() RETURN count(*)` returns 0 with the
  notification `unknown-name`, exactly as if the predicate did not exist. When a node has
  properties in a hidden graph, `RETURN n` leaves them out.
- **A17. SPARQL formats.** A5 with `Accept: application/sparql-results+json` returns the
  reifier IRIs for `w`, and A6 returns a `triple` term for `k`.
- **A18. Budgets.** A query over a budget fails with `507` and the same body as SPARQL,
  and a timeout gives `408`.
- **A19. TCK.** `mise run test:tck` passes every in-scope scenario or excludes it for a
  listed reason.

## 15. Rejected alternatives

- **Gremlin.** Apache TinkerPop's Gremlin is a traversal language embedded in host
  languages, with a server protocol of its own. It is harder to compile into a
  declarative algebra, because a traversal states its evaluation order, and its
  ecosystem is smaller among the developers and models this feature is for. Stardog
  offered TinkerPop over RDF, and its staff later advised users against it.
- **Write support in this spec.** Cypher writes over RDF must decide how identities are
  minted for new nodes and relationships, whether deleting a relationship deletes the
  asserted triple when other reifiers or plain RDF writes still claim it, what `SET` does
  to a multi-valued property, and how Cypher's statement-level visibility rules map onto
  SPARQL Update. Each needs its own design and tests. Reads are useful without them, and
  a later write spec can build on this view.
- **A stored property-graph copy.** neosemantics imports RDF into Neo4j, and Kùzu's RDF
  graphs load triples into node and relationship tables. A copy doubles the storage,
  needs synchronization with every write and a second enforcement of graph-level access
  control, and drifts from the RDF that SPARQL sees. ISO SQL/PGQ defines property graphs
  as views over existing tables, which is the approach taken here.
- **A separate Cypher executor.** An interpreter for Cypher would need its own planner,
  budgets, cancellation and graph view. Compiling to the SPARQL algebra reuses all of
  them, and the few gaps are filled by the shaper and a handful of `spk:` functions.
- **Generating SPARQL text.** Printing SPARQL and parsing it again costs time, loses
  source positions and invites injection bugs. The compiler builds the algebra, and
  `EXPLAIN` prints text from it for people.
- **One row per value of a multi-valued property.** Plain join semantics would multiply
  rows and change `count()`, which no Cypher user expects.
- **An arbitrary value,** as Neptune chooses. Results would change between runs.
- **Types as relationships,** as neosemantics' `NODES` option does. Classes are labels
  in every property-graph reading of RDF this spec found, and labels are what Cypher
  queries filter on.
- **Every reifier as a relationship, asserted or not.** It would make reifications that
  the data explicitly does not assert visible as facts.
- **Case-insensitive names, or converting `KNOWS` to `knows`.** Either makes the meaning
  of a query depend on which other names the data happens to contain.
- **Bolt in the first phase.** It needs sessions and transactions, which make sense only
  with writes, and drivers need more than a working handshake.
- **ANTLR or a parser generator.** The openCypher ANTLR grammar would need a Rust ANTLR
  runtime, and generated parsers give poor error messages. The vendored spargebra parser
  is hand-written for the same reasons.

## 16. Open questions

1. **Unasserted reifications.** Should a reifier whose triple is not asserted be offered
   as a relationship, perhaps behind a setting, for data that records claims without
   asserting them?
2. **Reifiers as nodes.** Should reifiers be hidden from node patterns? Hiding them costs
   an anti-join on every unconstrained node pattern in datasets that have reifiers.
3. **Default multi-valued policy.** Once `multi-valued=list` exists, should it become the
   default, which is faithful to RDF but gives a property two possible types?
4. **Strings and language tags.** Should `n.name = 'Berlin'` match `"Berlin"@en`?
   Matching would be friendlier for RDF data with tagged labels and would break the rule
   that equality compares RDF terms.
5. **Path multiplicity default.** Is reachability with a notification the right default
   for unbounded patterns, or should the default be strict until Phase 2's path operator
   exists?
6. **Neptune compatibility.** Should Sparkles also accept Neptune's HTTP openCypher
   route and response shape, so that notebook tooling written for Neptune works?
7. **GQL syntax.** How soon should GQL's `MATCH` forms, path modes and selectors be
   accepted next to openCypher's?
8. **Error codes.** Should errors also carry GQL's GQLSTATUS codes?
9. **Endpoint name.** Should C12 gain a separate `cypher` endpoint name, so that an
   operator can allow SPARQL and not Cypher, or the reverse?
10. **Editor support.** Should the UI's Cypher mode be written in-house or adopted from
    a permissively licensed package after a license review?
11. **Reifier index.** What measured cost should trigger the derived index of §11?

## 17. Sources

- openCypher 9, the Cypher Query Language Reference, and the openCypher grammar, from
  opencypher.org, cited from working knowledge. They define the clauses, scoping, null
  semantics, relationship isomorphism and variable-length relationships.
- The openCypher TCK (Apache-2.0), in the `tck/features` directory of the openCypher
  repository. Its directory structure and license were checked on 2026-10-02.
- N. Francis et al., "Cypher: An Evolving Query Language for Property Graphs", SIGMOD
  2018, and its formal semantics, for bag semantics and relationship uniqueness.
- ISO/IEC 39075:2024 (GQL), through public summaries, and A. Deutsch et al., "Graph
  Pattern Matching in GQL and SQL/PGQ", SIGMOD 2022, for path modes, quantified path
  patterns and selectors. ISO/IEC 9075-16:2023 (SQL/PGQ) for property graphs as views.
- The Neo4j Cypher manual, for the orderability of types, `shortestPath` and the
  notification model, and the Neo4j Query API documentation
  (neo4j.com/docs/query-api), checked on 2026-10-02, for the request and response shape.
- Amazon Neptune documentation, checked on 2026-10-02: "openCypher specification
  compliance in Amazon Neptune", for multi-valued properties and string identifiers, and
  "Using RDF data" in the Neptune Analytics guide, for IRIs, language-tagged literals and
  blank nodes. The AWS blog post "Build and deploy knowledge graphs faster with RDF and
  openCypher", for the `prefix::local` form, backticked IRIs and `PREFIX` declarations.
- M. Schmidt et al., "openCypher over RDF: Connecting Two Worlds", ISWC 2024 Posters and
  Demos, CEUR-WS Vol. 3828, and O. Lassila et al., "The OneGraph vision: Challenges of
  breaking the graph model lock-in", Semantic Web 14(1), 2023, for relationship and
  property statements over RDF.
- The Kùzu documentation of RDF graphs (docs.kuzudb.com), for a stored mapping of triples
  to node and relationship tables.
- The neosemantics documentation (neo4j.com/labs/neosemantics), for `handleVocabUris`,
  `handleMultival` and `handleRDFTypes`.
- Stardog community forum answers on its TinkerPop support, for the experience of
  offering Gremlin over RDF.
- Ontotext GraphDB's "Graph path search" documentation, for shortest-path search exposed
  through SPARQL, as background for F07.
- W3C RDF 1.2 Concepts and Abstract Syntax, RDF 1.2 Turtle and SPARQL 1.2 Query drafts,
  for triple terms, reifiers, `rdf:reifies`, the annotation syntax and the triple
  functions. The W3C RDF-star Community Group report "RDF-star and SPARQL-star" (2021),
  and O. Hartig, "Reconciliation of RDF* and Property Graphs" (arXiv:1409.3288), for
  statements about edges as edge properties.
- R. Angles, H. Thakkar and D. Tomaszuk, "Mapping RDF Databases to Property Graph
  Databases", IEEE Access 8, 2020, for direct mappings between the models.
- B. A. Steer et al., "Cytosm: Declarative Property Graph Queries Without Data
  Migration", GRADES 2017, for compiling Cypher onto a non-graph engine through a
  mapping. H. Thakkar et al., "Two for One: Querying Property Graph Databases using SPARQL
  via Gremlinator", GRADES-NDA 2018, and Z. Zhao et al., "S2CTrans: Building a Bridge from
  SPARQL to Cypher", 2023, for translation in the other direction. No published compiler
  from Cypher to SPARQL was found.
- The Sparkles code: `sparql::QueryOptions`, `execute_query`, `sparql::stats`,
  `sparql::aggext`, the vendored `spargebra` algebra and property paths, the prefixes
  store, and the specs C01, C02, C08, C09, C11, C12, C16 and F06.

## Outcome

Nothing is built.
