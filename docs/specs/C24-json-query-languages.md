# C24: JSON queries, JSON-LD frames and Datalog queries

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 is the JSON query language (node patterns, filters,
> optional and alternative patterns, aggregates, ordering and paging) compiled to the
> SPARQL algebra, with `/{ds}/query` accepting it, `sparkles query --language json`,
> stored JSON queries and EXPLAIN. Phase 2 adds nested selections that return JSON-LD
> trees, JSON-LD 1.1 framing as a query at `/{ds}/frame`, the MCP tool argument and the
> UI editor mode. Phase 3 adds Datalog programs and queries with predicates of any arity,
> evaluated through the demand transformation of [F17](F17-query-time-reasoning.md), and
> relation atoms in JSON queries.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

This design was written from the W3C recommendations JSON-LD 1.1, JSON-LD 1.1 Processing
Algorithms and API, JSON-LD 1.1 Framing, SPARQL 1.1 Query Language and SPARQL 1.1 Query
Results JSON Format; the public documentation of Fluree's JSON-LD query language, of
TerminusDB's WOQL and its JSON-LD form, of Apache Jena ARQ's `JSON` query form and rule
syntax, and of the Soufflé Datalog system; Abiteboul, Hull and Vianu, *Foundations of
Databases* (Addison-Wesley, 1995), for Datalog with stratified negation; Calì, Gottlob and
Lukasiewicz, "A general Datalog-based framework for tractable query answering over
ontologies" (Journal of Web Semantics, 2012), for Datalog±; Fagin, Kolaitis, Miller and
Popa, "Data exchange: semantics and query answering" (Theoretical Computer Science, 2005),
for weak acyclicity; and the Sparkles code. Fluree is licensed under BUSL-1.1; only its
public documentation was read, and no code. It builds on the GraphQL adapter's algebra
builder ([C03](C03-graphql/README.md)), the Cypher frontend's compiler design
([F01](F01-cypher.md)), query-time reasoning and its rule syntax decision
([F17](F17-query-time-reasoning.md) §9), stored queries ([C16](C16-stored-queries.md)),
the MCP server ([C11](C11-mcp-server.md)), budgets ([C01](C01-observability-and-budgets.md))
and access control ([C12](C12-graph-access-control.md),
[C12b](C12b-triple-access-control.md)).

## 1. Summary, goals, non-goals

### 1.1 What the gap is

[FEATURES.md](../FEATURES.md#known-gaps) lists "no JSON query language" under query
languages, and [COMPARISON.md](../COMPARISON.md#vs-fluree) explains it against Fluree:
Fluree offers "a JSON-LD query and transaction language" besides SPARQL, while in
Sparkles "JSON-LD is an RDF format". Sparkles takes queries as SPARQL text, Cypher text
once F01 lands, and GraphQL through a reviewed mapping (C03). A program that builds
queries as data has to generate SPARQL text and escape terms correctly, or use one of the
query builders of the bindings.

What other stores offer, from their public documentation:

| Store or standard | JSON query surface |
|---|---|
| Fluree | A query is a JSON-LD object with an `@context`, a `where` clause of node patterns whose keys are properties and whose values are variables (`"?name"`) or constants, a `select` that is a list of variables or a nested selection that returns JSON-LD trees ("graph crawl"), and filter expressions. Rules and transactions have JSON forms too. |
| TerminusDB | WOQL, a query language whose queries clients build in JavaScript or Python and send as JSON-LD documents. |
| JSON-LD 1.1 Framing | A frame is a JSON-LD document that matches nodes by type, properties and values and shapes the output tree with embedding rules. It is a recommendation for reshaping a JSON-LD document; processors that hold an RDF graph can apply it as a query over that graph. |
| Apache Jena ARQ | The `JSON { "name": ?n } WHERE { … }` query form: SPARQL syntax with a JSON template for output. It is listed with the other ARQ extensions under [ROADMAP](../ROADMAP.md) item 69 and is not part of this spec. |

The second half of roadmap item 72 is Datalog. [F17](F17-query-time-reasoning.md) §9
decides that Jena's rule language is the Datalog syntax for rules over triples, and
leaves "n-ary predicates that are not triples, a separate Datalog query form, and a
syntax such as Soufflé's `.decl` and `.output`" to a later spec. This is that spec (§6).

### 1.2 Goals

1. A JSON query language whose patterns are JSON-LD node objects with variables, so a
   query reads like the data it matches, compiled to the same SPARQL algebra the engine
   runs (§3, §4). Every engine feature then applies unchanged: the planner, budgets,
   views, reasoning modes, point-in-time reads and caches.
2. A defined, total mapping from every JSON construct to algebra, and from SPARQL 1.1
   query features to JSON constructs, so that any SPARQL SELECT, ASK or CONSTRUCT whose
   features §3 lists can be written in JSON.
3. Results as SPARQL JSON results, a table, or JSON-LD trees shaped by the query (§3.7).
4. JSON-LD 1.1 framing as a query: a frame selects and shapes nodes of the dataset
   (§5).
5. A Datalog surface for queries and for predicates of any arity, compatible with Jena
   rule files, evaluated through F17's demand transformation (§6).
6. Clear errors with a JSON pointer to the offending member (§3.8).

**Non-goals.** Updates in JSON in Phases 1 and 2 (open question 2). Fluree's syntax
itself: the language here is designed for Sparkles and documented on its own, so that
neither project's changes bind the other, although simple Fluree queries translate
mechanically (§12). JSON-LD contexts fetched from the network: a remote `@context` IRI
is refused unless it is one of the contexts the server preloads. Datalog with
aggregates in recursion, or with function symbols beyond Jena's builtins.

## 2. User-visible behaviour

```
POST /ds/query
Content-Type: application/vnd.sparkles.query+json

{ "@context": { "ex": "http://ex.org/", "name": "http://xmlns.com/foaf/0.1/name" },
  "select": ["?person", "?name"],
  "where": [
    { "@id": "?person", "@type": "ex:Professor", "name": "?name", "ex:age": "?age" },
    ["filter", [">", "?age", 40]]
  ],
  "orderBy": ["?name"],
  "limit": 10 }
```

The answer is `application/sparql-results+json` by default, chosen by `Accept` as for
SPARQL. `/{ds}/sparql` and `/{ds}` take the same body with the same content type. The
media type for Datalog is `application/vnd.sparkles.datalog` (§6), and frames go to
`POST /{ds}/frame` (§5).

`sparkles query --language json|datalog|sparql` reads the query from `--query FILE` or
standard input; the language defaults from the file extension (`.json`, `.dl`, `.rq`).
`--explain` prints the compiled SPARQL text (§4.4) above the plan. Stored queries (C16)
gain `"language": "json"` and keep parameters as variables bound by name, as for SPARQL.
The MCP `query` tool takes `language` (Phase 2). The UI's query editor gets a JSON mode
with schema completion of property names from C02's predicate listing (Phase 2).

## 3. The JSON query language

### 3.1 Document

A query is a JSON object with these members. Unknown members are errors.

| Member | Type | Meaning |
|---|---|---|
| `@context` | JSON-LD context | Prefixes and terms for the query. The dataset's declared prefixes ([C20](C20-declared-prefixes.md)) apply under it. |
| `select` | array, `"*"` or object | The projected variables and aggregates, or a nested selection (§3.6). |
| `ask` | `true` | An ASK query instead of `select`. |
| `construct` | array of node patterns | A CONSTRUCT template instead of `select`. |
| `where` | array or one pattern | The graph pattern (§3.3, §3.4). |
| `distinct`, `reduced` | boolean | Solution modifiers. |
| `groupBy`, `having`, `orderBy`, `limit`, `offset` | | Solution modifiers (§3.7). |
| `values` | object | Inline data joined with the whole pattern, as a trailing `VALUES`. |
| `from`, `fromNamed` | IRI or array | The dataset of the query, as `FROM` and `FROM NAMED`. |
| `rules` | string | A Datalog program whose relations the pattern may use (Phase 3, §6.6). |

### 3.2 Terms and variables

A string that starts with `?` and is a valid SPARQL variable name is a variable. Other
strings in a value position are literals unless the context's term definition gives the
property `"@type": "@id"` or `"@type": "@vocab"`, as in JSON-LD. `{"@id": "ex:x"}` is an
IRI, `{"@value": "?x"}` is the literal string `?x`, and `{"@value": "5",
"@type": "xsd:integer"}` and `{"@value": "chat", "@language": "fr"}` are typed and
language-tagged literals. JSON numbers become `xsd:integer`, `xsd:decimal` or
`xsd:double` and booleans become `xsd:boolean`, by the JSON-LD 1.1 rules for native
values. `"_:b1"` in an `@id` is a blank node of the query, which SPARQL treats as a
variable that is not projected.

### 3.3 Node patterns

A node pattern is a JSON-LD node object in which variables may appear wherever an IRI or
a literal may: as `@id`, as a value of `@type`, as a property value and as a key. It is
expanded with the query's context by the JSON-LD 1.1 expansion algorithm, with variables
kept, and each expanded property value gives one triple pattern:

- `{"@id": "?p", "name": "?n"}` gives `?p foaf:name ?n`.
- An array value gives one triple pattern per element, joined.
- A nested node object gives its own patterns, joined, with its `@id` (or a fresh blank
  node) as the object of the outer property.
- `"@reverse": {"ex:advisor": "?s"}` gives `?s ex:advisor ?p`.
- A key that is a variable, `{"@id": "?p", "?pred": "?o"}`, gives `?p ?pred ?o`.
- A key of the form `"path:…"` holds a property path in SPARQL syntax:
  `{"@id": "?a", "path:ex:knows+": "?b"}`.
- `"@graph"` inside a pattern object with an `@id` gives a `GRAPH` block, as JSON-LD
  named graphs do.

### 3.4 Clauses

The `where` array joins its elements in order. An element that is an array is a clause
whose first member names it:

| Clause | Algebra |
|---|---|
| `["optional", p…]` | `LeftJoin` with the patterns `p…` |
| `["union", [p…], [p…], …]` | `Union` of the alternatives |
| `["minus", p…]` | `Minus` |
| `["filter", e]` | `Filter` over the group, as SPARQL places it |
| `["bind", "?v", e]` | `Extend` |
| `["values", {"?a": [..], "?b": [..]}]` | `Values` at that point |
| `["graph", g, p…]` | `Graph` with an IRI or a variable |
| `["exists", p…]`, `["notExists", p…]` | `Filter` with `EXISTS` / `NOT EXISTS` |
| `["select", subquery]` | a subquery, an object of §3.1 without `@context` |
| `["service", iri, p…]` | `Service`, under the federation permission of C09 |
| `["relation", name, args…]` | a Datalog relation (Phase 3, §6.6) |

### 3.5 Expressions

An expression is a constant, a variable, or an array whose first member is an operator
or a function: `[">", "?age", 40]`, `["&&", e, e]`, `["regex", "?n", "^A"]`,
`["lang", "?n"]`, `["ex:distance", "?a", "?b"]`. The operators are SPARQL's operators,
and the function names are SPARQL's built-in function names in lower camel case and any
IRI or prefixed name for extension functions (P03, G06, F04, G01). `{"sparql": "?age >
40 && lang(?n) = 'en'"}` holds an expression in SPARQL syntax, for people who prefer it.

### 3.6 Selections

- `"select": ["?a", "?b"]` projects variables. `"*"` projects every in-scope variable in
  the order SPARQL's `SELECT *` uses.
- An aggregate is `{"?count": ["count", "?x", {"distinct": true}]}` in the list, with
  `groupBy` naming the grouping variables.
- **Nested selection (Phase 2).** `"select": {"?p": ["name", {"ex:knows": ["name"]}]}`
  returns, for each binding of `?p`, a JSON-LD node object with the listed properties,
  following `ex:knows` to a nested object with its `name`. `"*"` in a list takes every
  property of the node. `{"ex:knows": {"select": [...], "limit": 5}}` limits a nested
  list. The depth is at most 8 and the output is compacted with the query's context.

### 3.7 Modifiers and results

`orderBy` is a list of variables or `["desc", e]` / `["asc", e]`. `groupBy` is a list of
variables or `["as", e, "?v"]`. `having` is an expression. `limit` and `offset` are
non-negative integers. A flat selection answers in any SPARQL results format. A nested
selection answers `application/ld+json` with a top-level array, or with `@graph` and the
query's `@context` when the query sets `"compact": true`.

### 3.8 Errors

A query that does not parse or compile is a `400` with `code: "json-query"`, a message,
and a JSON pointer (RFC 6901) to the member at fault, such as `/where/1/1`. Runtime errors
are those of SPARQL.

## 4. Compilation

### 4.1 Pipeline

The body is parsed with `serde_json`. The context is processed by the JSON-LD 1.1
context processing algorithm, with remote contexts refused (§1.2). Node patterns are
expanded with variables preserved, which needs an expansion that treats strings starting
with `?` as opaque in key and `@id` positions; the expansion is written for this
compiler over the expanded-form rules of JSON-LD 1.1, since it only handles node objects
without lists of contexts, `@nest` or `@included` (they are errors). The compiler then
builds a `spargebra::Query` directly, as the GraphQL adapter does
(`crates/sparkles-graphql/src/algebra.rs`), never SPARQL text, so no escaping question
arises.

### 4.2 Scoping

Variables follow SPARQL's scoping. A variable that appears only inside an `optional`
and is used in a later filter behaves as SPARQL defines. The compiler runs
`sparkles_core::sparql::validate_scoping` on the result, so a JSON query and the SPARQL
text it prints are refused for the same reasons.

### 4.3 Execution

The algebra goes to `sparkles_core::sparql::execute_query`, which runs parsed queries.
Nested selections compile into one query for the root bindings and one batched query per
nested level, each joined with a `VALUES` block of the parent nodes, as C03's fetch
groups are. A nested level is cut at its `limit` in the engine, per parent, rather than after the
fetch. The GraphQL adapter today fetches every value of a nested field and then slices
it (`slice` in `crates/sparkles-graphql/src/exec.rs`), which FEATURES lists as a gap.

### 4.4 EXPLAIN and printing

`spargebra::Query` prints as SPARQL text, so EXPLAIN and `sparkles query --explain` show
the SPARQL a JSON query is equal to. This doubles as documentation: every JSON query has
a SPARQL reading.

## 5. JSON-LD frames as queries (Phase 2)

`POST /{ds}/frame` with an `application/ld+json` body takes a JSON-LD 1.1 frame and
answers `application/ld+json;profile="http://www.w3.org/ns/json-ld#framed"`. The frame
options of the framing recommendation (`@embed`, `@explicit`, `@omitDefault`,
`@requireAll`, `@default`, `@omitGraph`) apply, and query parameters `at=`, `graph=`,
`timeout=` and `limit=` apply as for queries.

The frame is evaluated in two steps:

1. **Candidate selection.** The top-level frame's `@id`, `@type` and property matches are
   compiled to a SELECT of candidate subjects: `@type` values to `rdf:type` patterns,
   a property with a non-empty frame to a pattern on that property (joined when
   `@requireAll` is true, unioned otherwise), `[]` for "must not have" to `NOT EXISTS`,
   and value patterns to filters. The selection is a superset of the frame's matches,
   because the compilation drops conditions it cannot express exactly.
2. **Framing.** The candidates' descriptions are fetched to the depth the frame embeds,
   by batched queries as in §4.3, into an in-memory flattened node map, and the framing
   algorithm of JSON-LD 1.1 Framing runs over that node map. The algorithm itself decides
   the final matches, so the superset of step 1 is safe.

The number of top-level matches is limited by `limit` (default 1,000) and the embedded
depth by 8, and the answer says when either cut. There is no permissively licensed Rust
crate that this spec relies on for framing; the algorithm is written in `sparkles-core`
from the recommendation, tested against the W3C framing test suite (§11).

## 6. Datalog queries and predicates (Phase 3)

### 6.1 Relation to F17

F17 makes Jena's rule language the syntax of rules over triples. This section keeps that
decision: **a Datalog program here is a Jena rules file, plus three additions** that
Jena's syntax lacks: declared relations of any arity, clauses written in the
`head :- body.` form over those relations, and a query form `?- body.` Any Jena rules
file is a valid program, and the parser extends `crates/sparkles-reasoner/src/parser.rs`
rather than adding a second one. Programs compile to the reasoner's `Rule` values, so
the evaluation is F17's.

### 6.2 Syntax

```
@prefix ex: <http://ex.org/> .

.decl ancestor(a, d)
.decl cousin(a, b, via)

ancestor(?a, ?d) :- (?a ex:parent ?d).
ancestor(?a, ?d) :- (?a ex:parent ?m), ancestor(?m, ?d).
cousin(?x, ?y, ?g) :- (?x ex:parent ?p), (?y ex:parent ?q), (?p ex:parent ?g),
                      (?q ex:parent ?g), ?p != ?q.

[sym: (?a ex:knows ?b) -> (?b ex:knows ?a)]

?- ancestor(ex:n10, ?d), not cousin(ex:n10, ?d, ?_).
```

- `.decl name(col, …)` declares a relation and its arity, as Soufflé's `.decl` does,
  without types; terms are RDF terms.
- An atom is a triple pattern in Jena's form `(s p o)`, a relation atom `name(args)`, a
  Jena builtin call, `not` before a relation or triple atom (stratified negation), or a
  comparison `?x < 5` that compiles to the builtin `lessThan`.
- The head of a `:-` clause is a relation atom or a triple pattern; a triple head
  derives a triple for the query's tables, as a backward Jena rule does.
- Variables are written `?x` as in Jena rules and SPARQL. `?_` is anonymous.
- `?- body.` is the query. A program has at most one, and its answer has one column per
  named variable of the body, in order of first appearance.
- Rules in Jena's bracket form are kept as they are, forward or backward.

### 6.3 Semantics

A program is accepted when every clause is safe (each variable of the head, of a negated
atom or of a comparison occurs in a positive atom of the body) and the program is
stratified (no relation depends negatively on itself through recursion). The answer is
the query body evaluated over the perfect model of the program on the snapshot the
request reads, as Abiteboul, Hull and Vianu define it. Relations derived for a query
are not written to the dataset; they live in F17's per-query tables.

### 6.4 Compilation

- **Relation encoding.** The rule engine works on triples (`crates/sparkles-reasoner/src/
  engine.rs`). A binary relation `r(a, b)` becomes the triple `(a, <urn:x-sparkles:dl:r>,
  b)`, and a unary one `r(a)` becomes `(a, rdf:type, <urn:x-sparkles:dl:r>)`. A relation
  of arity three or more becomes a tuple node made by the existing `makeSkolem` builtin
  from the relation name and the arguments, with one triple per column
  (`<urn:x-sparkles:dl:r#1>` and so on). The `urn:x-sparkles:dl:` namespace is reserved,
  and a program or a query that names it directly is refused.
- **Non-recursive programs** are unfolded into the query: each relation atom becomes the
  union of its clauses' bodies with renamed variables, and the result compiles to SPARQL
  algebra as a JSON query does. No rule engine runs.
- **Recursive programs** go to F17's demand path: the query body's bound arguments give
  the adornments, the demand transformation runs on the compiled rules, and the `Derive`
  operator of F17 §10 supplies the relation atoms to the algebra. This needs F17 Phase 1.
- **Negation** of a relation compiles to `NOT EXISTS` over its derived table, in a
  higher stratum.

### 6.5 Existential rules (Datalog±)

A head variable that does not occur in the body, `.decl advisor(s, a)` with
`advisor(?s, ?a) :- (?s a ex:PhDStudent).`, asks for a new node for each student, as a
tuple-generating dependency of Datalog±. The compiler accepts it only when the program
is weakly acyclic (Fagin, Kolaitis, Miller and Popa), so that the chase terminates, and
compiles the existential to `makeSkolem` over the body's frontier variables. Otherwise
it is refused with the dependency cycle named. The fact budget of F17 §8 still applies.

### 6.6 Relations in JSON queries

A JSON query with a `rules` member uses its relations through
`["relation", "ancestor", "ex:n10", "?d"]` clauses, compiled as in §6.4. This gives JSON
clients recursion without a separate request.

## 7. Access control and budgets

JSON queries, frames and Datalog queries are queries: they need `read` on the dataset
and the `query` endpoint in C12's endpoint lists, and they run on the caller's view with
its protections (C12b), so a frame cannot embed a hidden triple. They use the query
timeout, memory budget and result limits of C01; framing's node map is charged to the
memory budget. Derived relations follow F17 §4.5: they are computed from the caller's
view and cached per view.

## 8. Architecture

| Crate / file | Change |
|---|---|
| `crates/sparkles-core/src/jsonq/` (new) | `parse.rs` (the document, with JSON pointers), `context.rs` (JSON-LD context processing without remote fetches), `expand.rs` (node pattern expansion with variables), `compile.rs` (to `spargebra::Query`), `nested.rs` (nested selections). |
| `crates/sparkles-core/src/jsonld_frame/` (new) | The framing algorithm over a node map and the candidate query (Phase 2). |
| `crates/sparkles-reasoner/src/parser.rs` | `.decl`, `:-` clauses, `?-` queries, `not`, infix comparisons (Phase 3). |
| `crates/sparkles-reasoner/src/datalog.rs` (new) | Safety, stratification, relation encoding, unfolding, weak acyclicity (Phase 3). |
| `crates/sparkles/src/dataset.rs` | `Dataset::query_json`, `Dataset::frame`, `Dataset::query_datalog`, each with `QueryOptions`. |
| `crates/sparkles-server/src/http.rs` | Content-type dispatch on `/{ds}/query`, `/{ds}/sparql` and `/{ds}`; `POST /{ds}/frame`; OpenAPI entries. |
| `crates/sparkles-server/src/main.rs` | `sparkles query --language`. |
| `crates/sparkles-server/src/mcp/` | The `language` argument of the `query` tool (Phase 2). |
| `crates/sparkles-server/src/http/queries.rs` and the stored-query store | `"language": "json" \| "datalog"` (C16). |
| `crates/sparkles-py`, `crates/sparkles-node`, `crates/sparkles-ffi` | `query_json` taking a dict, an object or a string, mapped by P06's test. |
| `ui/src/routes/query/` | The JSON mode of the editor (Phase 2). |

## 9. Phases

**Phase 1 (M).** §3 except nested selections and `rules`, §4, the content-type dispatch,
the CLI, stored JSON queries, the bindings' `query_json`.

**Phase 2 (M).** Nested selections, `/{ds}/frame`, the MCP argument, the UI mode.

**Phase 3 (M, after F17 Phase 1).** §6: the Datalog parser additions, the compiler, the
`application/vnd.sparkles.datalog` media type, `sparkles query --language datalog`, and
relations in JSON queries.

## 10. Acceptance examples

The examples use `ex:` = `http://ex.org/` and the query context of §2.

**A1. Same answers as SPARQL.** The query of §2 returns the same solutions, in the same
order, as `SELECT ?person ?name WHERE { ?person a ex:Professor ; foaf:name ?name ;
ex:age ?age FILTER(?age > 40) } ORDER BY ?name LIMIT 10`, and `--explain` prints that
query.

**A2. Literals and IRIs.** With `"ex:advisor": {"@type": "@id"}` in the context,
`{"@id": "?s", "ex:advisor": "ex:ann"}` matches the IRI `ex:ann`; without it, the same
pattern matches the string literal `"ex:ann"`.

**A3. Optional and aggregate.** `"select": ["?dept", {"?n": ["count", "?p"]}]`,
`"where": [{"@id": "?p", "ex:dept": "?dept"}, ["optional", {"@id": "?p", "ex:email":
"?e"}]]`, `"groupBy": ["?dept"]` returns one row per department with its member count.

**A4. Errors point at members.** `"where": [["filter", ["~", "?a"]]]` fails with `400`,
`code: "json-query"` and the pointer `/where/0/1/0`, naming the unknown operator `~`.

**A5. Nested selection.** `"select": {"?p": ["name", {"ex:knows": {"select": ["name"],
"limit": 2}}]}` over a person who knows five people returns one object with two nested
`ex:knows` objects, and the engine fetches two rows for that level, not five.

**A6. Frame.** The frame `{"@context": …, "@type": "ex:Library", "ex:contains":
{"@type": "ex:Book"}}` over the framing recommendation's library example data in the
default graph returns the same JSON-LD as the framing test suite expects for that
example.

**A7. Datalog query.** With F17 A1's 100,000-node parent chain, the program of §6.2
without the `cousin` and `sym` clauses returns the 99,990 ancestors of `ex:n10`, derives
nothing about `ex:n0` to `ex:n9`, and writes no triple to the dataset.

**A8. N-ary relation.** `.decl edge3(a, b, w)` with
`edge3(?a, ?b, ?w) :- (?r ex:from ?a), (?r ex:to ?b), (?r ex:weight ?w).` and
`?- edge3(?a, ?b, ?w), ?w > 5.` returns the same rows as the equivalent SPARQL join, by
unfolding, without running the rule engine.

**A9. Jena rules unchanged.** Every rules file in the reasoner's tests parses as a
program, and a program that is only Jena rules and a `?-` query gives the same answers as
F17's `reasoning=demand` over a SPARQL query of the same body.

**A10. Existential rules.** The `advisor` program of §6.5 is accepted and derives one
advisor node per student. Adding `(?a a ex:PhDStudent) :- advisor(?s, ?a).` as a clause
in triple form makes it not weakly acyclic, and it is refused with the cycle named.

**A11. Views.** A caller whose C12b protection hides `ex:salary` gets no salary values
from a JSON query, a frame that embeds `ex:salary`, or a Datalog query that reads it.

## 11. Testing

- **Round trip with SPARQL.** A generator of SPARQL queries over the features of §3
  (the W3C SPARQL 1.1 query test queries that use only those features, and random
  queries) is translated to JSON by a test-only printer, compiled back, and checked
  for algebra equal to `spargebra`'s parse of the original, after normalization.
- **W3C suites.** The SPARQL 1.1 query evaluation tests whose queries use only §3's
  features run through the JSON form with the expected results. The JSON-LD 1.1 framing
  test suite runs against `/{ds}/frame` with each test's input loaded as the dataset.
  The JSON-LD expansion tests that apply to node objects check the context and expansion
  code.
- **Datalog.** Differential tests against F17's `demand` mode and against full
  materialization of the compiled rules, over random programs from the reasoner's test
  generators extended with n-ary relations and negation; safety, stratification and
  weak acyclicity each with positive and negative cases.
- **Benchmarks (X06).** The BSBM explore queries written in JSON against their SPARQL
  form: the target is no difference beyond the compile time, which must stay under
  0.2 ms for the BSBM queries.

## 12. Rejected alternatives

- **Adopting Fluree's query syntax as is.** Its documentation describes the language
  as it is today, under a different license, and changes with Fluree's releases. A
  syntax of Sparkles' own, built on JSON-LD node objects and SPARQL's operator and
  function names, is stable under Sparkles' control and maps to SPARQL one to one. The
  simple Fluree queries that use `@context`, `select` and node-pattern `where` clauses
  translate mechanically, and a converter can be added if users ask (open question 4).
- **A JSON serialization of the SPARQL algebra.** It would be complete and trivial to
  compile, but it is not what users who want JSON queries write; the patterns would not
  look like their data.
- **Framing only, without a query language.** Frames cannot express joins between
  unrelated nodes, aggregates, negation over paths or ordering, so they answer only
  "these nodes, shaped like this".
- **A new Datalog syntax with Soufflé's typed columns and `.input`/`.output`.** It would
  split users from Jena rule files, which F17 §14 rejects for the same reason, and typed
  columns add nothing over RDF terms.
- **Writing derived relations to a graph.** That is materialization, which C08 and F16
  already provide with freshness tracking; query-local relations need none of it.

## 13. Open questions

1. Should the JSON query media type also be accepted as `application/json` on
   `/{ds}/query`, where no other JSON body is meaningful today?
2. Should JSON updates (`insert`, `delete`, `where`) follow in a later phase, compiled to
   SPARQL Update's algebra, with Fluree-style upserts expressed as one delete/insert?
3. Should nested selections allow `orderBy` per level? It costs a sort per parent batch.
4. Is a converter from Fluree's query JSON wanted, for the queries whose features map?
5. Should Datalog programs be storable as reasoning rule sets (F17's dataset setting)
   with their `.decl` relations, or only sent per query?

## Provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

- **Spec:** [`C24-json-query-languages.md`](C24-json-query-languages.md), written on 2026-10-12 independently from:
  - the W3C JSON-LD 1.1, JSON-LD 1.1 Processing Algorithms and API, JSON-LD 1.1 Framing,
    SPARQL 1.1 Query Language and SPARQL 1.1 Query Results JSON Format recommendations,
    and RFC 6901 (JSON Pointer), cited from working knowledge and not re-fetched;
  - the public documentation of Fluree's JSON-LD query language (Fluree is BUSL-1.1; no
    code was read), of TerminusDB's WOQL, of Apache Jena ARQ's `JSON` query form and rule
    syntax, and of Soufflé, cited from working knowledge;
  - Abiteboul, Hull and Vianu, *Foundations of Databases* (1995); Calì, Gottlob and
    Lukasiewicz, "A general Datalog-based framework for tractable query answering over
    ontologies" (Journal of Web Semantics, 2012); Fagin, Kolaitis, Miller and Popa,
    "Data exchange: semantics and query answering" (Theoretical Computer Science, 2005);
  - the Sparkles code (`crates/sparkles-graphql/src/{algebra.rs,exec.rs}`,
    `crates/sparkles-core/src/sparql/mod.rs`, `crates/sparkles-reasoner/src/{parser.rs,builtins.rs}`,
    `crates/sparkles-server/src/http.rs`, `crates/sparkles-server/src/main.rs`) and specs
    C01, C03, C11, C12, C12b, C16, C20, F01 and F17, FEATURES and COMPARISON.
- **Implementation:** not built. No new dependency is planned; JSON-LD context processing,
  pattern expansion and framing are to be written in `sparkles-core`.
- **Rejected** (spec §12):
  - adopting Fluree's query syntax as is;
  - a JSON serialization of the SPARQL algebra;
  - framing only, without a query language;
  - a new Datalog syntax with typed columns and `.input`/`.output`;
  - writing derived relations to a graph.

## Outcome

Not built. See [ROADMAP](../ROADMAP.md) item 72.
