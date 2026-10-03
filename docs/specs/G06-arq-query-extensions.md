# G06: Jena ARQ's query language extensions

> **Status:** implemented
>
> **Phases:** Phase 1 shipped on 2026-10-02: `LATERAL`, property path ranges and
> CONSTRUCT templates with `GRAPH`. Phase 2 shipped on 2026-10-02: configurable DESCRIBE
> (§11). Phase 3 shipped on 2026-10-03: `LET`, the composite datatypes with `FOLD` and
> `UNFOLD`, the property function library, the rest of the function library, `SEMIJOIN`,
> `ANTIJOIN` and ARQ's other path forms (§12).
>
> **User docs:** [API: ARQ syntax extensions](../API.md#arq-syntax-extensions) ·
> [API: extension functions](../API.md#extension-functions-and-aggregates) ·
> [API: property functions](../API.md#arqs-property-functions) ·
> [API: DESCRIBE](../API.md#describe) ·
> [Features](../FEATURES.md#sparql-arq-equivalent) ·
> [Comparison with Jena](../COMPARISON.md#vs-apache-jena--fuseki)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This spec draws on the Sparkles code, the grammar and engine sources of Apache Jena's
ARQ (`jena-arq`, Apache-2.0) for syntax and behaviour, ARQ's test suites
(`jena-arq/testing/ARQ` and the unit tests of `org.apache.jena.sparql.path`), Jena
6.2.0's `arq` command run on small graphs, the SPARQL 1.1 and 1.2 Query specifications
and the SPARQL community proposal SEP-0006 for `LATERAL`.

## 1. Summary

Fuseki parses every query with ARQ's syntax, a superset of SPARQL. Queries written for
Fuseki therefore use extensions that Sparkles rejects as syntax errors. Sparkles already
accepts ARQ's statistical aggregates. This spec adds the three syntax extensions that
Jena users rely on most:

| Extension | Example | What it does |
|---|---|---|
| `LATERAL` | `?p :name ?n LATERAL { SELECT ?f { ?p :friend ?f } LIMIT 3 }` | Evaluates the group once per solution of the patterns before it, with that solution's values substituted |
| Path ranges | `?x :parent{2} ?gp`, `?x :next{1,3} ?y`, `?x :p{2,} ?y`, `?x :p{,4} ?y` | A path of a given number of steps, counting each way through the graph |
| CONSTRUCT with `GRAPH` | `CONSTRUCT { GRAPH ?g { ?s :p ?o } } WHERE { … }` | A CONSTRUCT that returns quads, written as TriG or N-Quads |

Goals:
- A query that Jena 6 accepts with one of these extensions parses, and its results equal
  Jena's, apart from the deliberate differences named below.
- Strict SPARQL is unchanged. The W3C SPARQL 1.0, 1.1 and 1.2 suites keep passing in
  full, and a parser setting rejects the extensions for callers that want SPARQL only.
- Queries that use none of the extensions plan and run exactly as before.
- The parser has one place where ARQ-only grammar is switched on or off, which the later
  phases (`LET`, `UNFOLD`, DESCRIBE options) reuse.
- `sparkles fmt`, the editor's highlighting and the query builder know the new syntax.

Non-goals:
- ARQ's other path forms: the binary inverse `:p^:q`, `distinct(…)`, `shortest(…)` and
  `multi(…)`. Jena does not implement `shortest`.
- `SEMIJOIN`, `ANTIJOIN`, `LET`, `UNFOLD`, `FOLD`, the `JSON` query form and the
  `EXISTS { … }` element. They are later work or rarely used.
- Quads in `DESCRIBE`. Jena's DESCRIBE returns a graph.

## 2. Accepting ARQ syntax

The parser is the vendored `spargebra` 0.4.7 ([PATCHED.md](../../vendor/spargebra/PATCHED.md)),
which already parses `LATERAL` behind its `sep-0006` cargo feature. The workspace turns
that feature on, and the new grammar is added to the same rust-peg grammar.

`SparqlParser` gains a setting, `with_arq_syntax(bool)`. It is on by default, because
Fuseki parses queries and updates with ARQ's syntax and Sparkles' endpoints, CLI and
APIs follow Fuseki. Every ARQ-only rule starts with one guard rule, `arq()`, which fails
with the message `ARQ syntax` when the setting is off. ARQ's aggregate keywords move
behind the same guard. A later phase adds its grammar the same way.

The extensions cannot change the meaning of a SPARQL query. `LATERAL` is not a SPARQL
keyword, and a prefixed name such as `lateral:x` still parses as one. A `{` directly after
a path element is never valid SPARQL. A CONSTRUCT template in SPARQL contains triples
only. The W3C negative syntax tests contain none of the three forms, so they keep
failing to parse.

`sparkles qparse` and `uparse` get `--syntax arq|sparql`, Jena's flag name, defaulting to
`arq`.

## 3. `LATERAL`

### 3.1 Syntax and scope

`LATERAL { … }` is an element of a group graph pattern, like `OPTIONAL`. In the algebra,
the patterns before it in the group become the left side `L`, and its group becomes the
right side `R`: `Lateral(L, R)`.

A variable that is in scope in `L` must not be assigned again in `R`. This is ARQ's
check (`SyntaxVarScope.checkLATERAL`), which spargebra's `sep-0006` code already makes:
`BIND (… AS ?v)`, `VALUES ?v` and `SELECT (… AS ?v)` inside `R` are syntax errors when
`?v` is in scope in `L`. A sub-select that does not project `?v` may assign it, because
its `?v` is a different variable. These are the cases of ARQ's `Syntax-Lateral` tests.

### 3.2 Semantics

ARQ evaluates `Lateral(L, R)` as follows. For each solution μ of `L`, every variable of
`R` that μ binds is replaced by its value, the substituted `R` is evaluated, and each of
its solutions is merged with μ. Variables of `R` that μ leaves unbound are not replaced,
so they may be bound by `R` and must then agree when merged.

Substitution respects the scope of sub-selects. A variable inside a sub-select of `R`
that the sub-select does not project is a different variable from the outer one with the
same name, so it is not replaced. ARQ renames such variables apart. In ARQ's test
`lateral-5`, `?s :p ?o LATERAL { SELECT ?z { ?s :p ?z } }` pairs every `?s` with every
`?z`.

Substitution reaches the parts of `R` that a join cannot. A FILTER inside `R` sees the
values of `L` (`lateral-3`), and a sub-select with ORDER BY and LIMIT runs per solution
of `L`, which gives "the top k per group" (`lateral-1`). A `LATERAL` inside an OPTIONAL
has only the OPTIONAL's own group as its left side and sees nothing outside it
(`lateral-in-optional`).

### 3.3 Planning and execution

`Lateral(L, R)` is the join of `L` and `R` whenever substitution cannot change `R`'s
solutions. The planner checks two cases and plans those laterals as ordinary joins, so
the cost-based join order applies to them:

1. `R` mentions none of the variables in scope in `L`, outside sub-selects that hide
   them.
2. `R` is built only from basic graph patterns, paths that cannot match a zero-length
   path, joins, `GRAPH` and deterministic FILTERs, and no variable in scope in `L` is
   *risky* in `R`. A variable is risky when a FILTER of `R` uses it over a pattern that
   does not bind it in every solution. This is the analysis that already decorrelates
   `FILTER EXISTS` (`sparql/exists.rs`), and its argument carries over: under these
   conditions, the solutions of the substituted `R` are exactly the solutions of `R`
   that agree with μ.

Any other lateral becomes a new operator, `Lateral`, with `L`'s plan as its child. It
groups `L`'s rows by their values of the variables that `R` mentions, and for each
distinct group plans `R` with those values as constants and runs it. The merged rows of
the group follow. A variable unbound in a row is not substituted. Planning `R` per
distinct key reuses the per-row machinery of `EXISTS`, which substitutes through the
planner's constant table. That table gains scope: a variable substituted by `LATERAL` is
removed from it inside a sub-select that does not project it. Constants substituted for
other reasons, such as initial bindings, keep their current behaviour.

The operator checks the deadline, cancellation and the row and memory budgets between
groups, and reports the number of groups and evaluations in EXPLAIN. The result cache
keys it by `R`'s text and the constants in force. A `GRAPH ?g` group whose `?g` is
substituted reads that one graph. Today the planner reads every named graph there, which
is also wrong for `EXISTS`, so the fix serves both.

## 4. Property path ranges

### 4.1 Syntax

ARQ's grammar (`PathMod` in `main.jj`) allows a brace form after a path element:

| Form | Meaning |
|---|---|
| `p{n}` | exactly `n` steps |
| `p{n,m}` | between `n` and `m` steps |
| `p{n,}` | `n` or more steps |
| `p{,m}` | between 0 and `m` steps |
| `p{*}` | 0 or more steps, counted |
| `p{+}` | 1 or more steps, counted |

`n` and `m` are unsigned integers. `p{n,m}` with `n > m` is a syntax error. Jena accepts
it and fails at execution with "Bad path". In the algebra all six forms are one new path
expression, `PropertyPathExpression::Range { path, min, max }`, with `max` absent for the
unbounded forms. `{,m}` is `min = 0`, `{*}` is `{0,}` and `{+}` is `{1,}`, as their
semantics in §4.2 are the same.

### 4.2 Semantics

SPARQL's `*`, `+` and `?` are sets: each pair of nodes they connect is one solution. ARQ's
ranges are bags. They count the ways through the graph, as a sequence `p/p/p` does.

* **Bounded ranges.** `p{n,m}` has one solution for each walk of `k` steps of `p`, for
  every `k` from `n` to `m`. A walk may visit a node more than once. `p{n}` is
  `p{n,n}`, the same bag as `p/p/…/p` with `n` copies, and `p{0}` matches the zero-length
  path only.
* **Unbounded ranges.** ARQ evaluates `p{n,}` as `p{n}/p{*}` (`PathCompiler`). `p{*}`
  has one solution for each *simple* path from the start: each path that does not visit
  a node twice, the zero-length path included. So `p{n,}` counts every walk of `n`
  steps followed by a simple path from its end.

On `:n1 :p :n2, :n3 . :n2 :p :n4 . :n3 :p :n4`, Jena 6.2.0 gives `:n1 :p{2} ?o` the rows
`:n4, :n4`, `:n1 :p{0,2} ?o` the rows `:n1, :n2, :n3, :n4, :n4`, and `:n1 :p{*} ?o` the
rows `:n1, :n2, :n3, :n4, :n4`, where `:n1 :p* ?o` has `:n4` once. On the cycle
`:c1 :q :c2 . :c2 :q :c1, :c3`, `:c1 :q{1,3} ?o` gives `:c2, :c1, :c3, :c2`, and
`:c1 :q{2,} ?o` gives `:c1, :c2, :c3, :c3`. These are acceptance examples A6 and A7.

Zero-length matches follow SPARQL's existing rules for `*` and `?`, which Sparkles
already implements. With two variable ends, the zero-length path ranges over the nodes
of the active graph. With a constant end, it matches that constant.

`p{0,}`, written with an explicit 0, is the one deliberate difference. Jena's evaluator
treats `P_Mod(0, unbounded)` as `{+}`, so `:n1 :p{0,} ?o` omits `:n1`, unlike `p{*}`
and unlike ARQ's documentation, which says that "a zero occurrence of a path element
always matches". Sparkles gives `{0,}` the meaning of `{*}`.

### 4.3 Planning and execution

The planner rewrites a range as ARQ's `PathCompiler` does. `p{n,m}` with `n ≥ 1` becomes
`n` steps of `p` through fresh variables, followed by `p{0,m−n}` (or `p{*}` when `m` is
absent), and a range with `n = m` needs no tail. The steps take part in the cost-based
join order of their group like the steps of a sequence path. The rewrite stops at 32
steps, and a larger range runs whole in the executor.

The path operator gains a counting mode for what remains, ranges with `min = 0` and
oversized ones. The ranges reuse its index access, both directions and the graph
handling.
* A bounded range expands level by level, keeping for each node the number of walks
  that reach it. Each node of a level within the range is emitted that many times.
* The unbounded tail enumerates simple paths depth first, with an explicit stack and the
  set of nodes on the current path. It is exponential on dense graphs, as in Jena. The
  deadline, cancellation and the row budget stop it.
* Evaluated from the object end, the walks and simple paths run on reversed edges. The
  counts are the same in both directions.

The number of solutions is the product of path counts and can grow quickly. The existing
row and memory budgets apply to it.

## 5. CONSTRUCT with `GRAPH`

### 5.1 Syntax

ARQ's CONSTRUCT template is a TriG-like block (`ConstructQuads` in `main.jj`). It holds
triples, `GRAPH g { triples }` blocks and bare `{ triples }` blocks in any order, each
block optionally followed by `.`. `g` is an IRI, a variable or a blank node. A bare block
and the graph names `<urn:x-arq:DefaultGraphNode>` and `<urn:x-arq:DefaultGraph>` stand
for the default graph. The short form `CONSTRUCT WHERE { … }` may contain `GRAPH` blocks
too. Its pattern is then the template's triples and blocks, so a blank-node graph name is
an error there, and so is a FILTER (ARQ's `syntax-quad-construct-bad-01`).

`Query::Construct` keeps `template` for the default graph's triples and gains
`graph_templates`, a list of `GraphTemplate { name, triples }`. A CONSTRUCT without
`GRAPH` has an empty list, so code that reads only `template` keeps working.

### 5.2 Results

Each solution instantiates the template as before. A `GRAPH` block whose name is unbound,
or bound to a literal or a triple term, produces nothing for that solution, as a triple
with an unbound subject does. A blank-node graph name is a fresh blank node per solution,
shared by all blocks with that label.

The result holds the default graph's triples, as today, and a new list of quads in named
graphs. Responses follow Fuseki, which builds a dataset from every CONSTRUCT:
* A dataset format (TriG, N-Quads, JSON-LD, RDF Thrift and RDF Protobuf) writes both.
* A graph format (Turtle, N-Triples, RDF/XML and the others) writes the default graph
  only.
* The `application/x-sparkles+json` document gains a `quads` array of
  `[subject, predicate, object, graph]` beside `triples`, present when it is not empty.
* `sparkles query --format trig|nquads` writes the quads. The Rust `QueryResult` gains
  `quads`, and `Dataset::construct` in Rust and Python keeps returning the default graph,
  with a new `construct_quads` that returns both.

## 6. Formatter, editor and query builder

* `sparkles fmt` parses `LATERAL` as a group element and prints it like `OPTIONAL`, path
  ranges tight against their element (`:p{1,3}`), and `GRAPH` and bare blocks inside a
  CONSTRUCT template like the quad blocks of `INSERT DATA`. The formatter's check that the
  output means the same as the input covers the new algebra, because it compares
  spargebra's parses of the two.
* The UI editor highlights `LATERAL` as a keyword and offers it in completion.
* The Rust query builder gets `WhereBuilder::lateral`, and its path syntax accepts the
  brace forms. The Python builder mirrors `lateral`.

## 7. Code layout

* `vendor/spargebra`: the `arq()` guard and `with_arq_syntax`, `Range` in
  `PropertyPathExpression` with its SPARQL and SSE output, the template grammar, and
  `GraphTemplate`. PATCHED.md lists the changes.
* `crates/sparkles/src/sparql/plan.rs`: `Lateral` and `Range` in the planner, the
  rewrite of ranges, the scoped constant table and the `GRAPH ?g` fix.
* `crates/sparkles/src/sparql/lateral.rs`: the decorrelation test and the operator.
* `crates/sparkles/src/sparql/exec.rs`: the counting path mode.
* `crates/sparkles/src/sparql/mod.rs`: CONSTRUCT quads. `results.rs`: their output.
* The pattern walkers (`depth`, `rdfs`, `exists`, scope validation, stored queries, the
  MCP tools, SHACL and ShEx helpers) handle the new algebra.
* `crates/sparkles-fmt`: the CST rules and printing. `ui/src/lib/sparql-lang.ts`: the
  keyword. `querybuilder`: `lateral` and path ranges.

## 8. Acceptance examples

The ARQ tests named here are in Jena's `jena-arq/testing/ARQ` and are ported to
`crates/sparkles/tests/arq_syntax.rs` with their data and expected results.

* **A1.** The positive and negative tests of `Syntax-Lateral` and the
  `syntax-quad-construct-*` tests of `Syntax-ARQ` parse or fail as in ARQ.
* **A2.** `Lateral/lateral-1` to `lateral-5`, `lateral-in-optional` and
  `optional-in-lateral` return ARQ's results.
* **A3.** `SELECT ?p ?f { ?p a :Person LATERAL { SELECT ?f { ?p :knows ?f } ORDER BY ?f
  LIMIT 1 } }` returns at most one friend per person.
* **A4.** `?s :p ?o LATERAL { ?o :q ?x }` is planned as a join, and EXPLAIN shows no
  `Lateral` operator.
* **A5.** The path cases of Jena's `TestPath` (`path_02` to `path_09` and `path_32` to
  `path_34`) and `TestPathQuery` give Jena's bags.
* **A6.** The `:n1` examples of §4.2 give Jena 6.2.0's rows.
* **A7.** The `:c1` examples of §4.2 give Jena 6.2.0's rows. `?s :q{2} ?s` gives `:c1`
  and `:c2` once each.
* **A8.** `CONSTRUCT { GRAPH ?g { ?s ?p ?o } } WHERE { GRAPH ?g { ?s ?p ?o } }` with
  `Accept: application/n-quads` returns the dataset's named-graph quads. With
  `Accept: text/turtle` it returns an empty graph.
* **A9.** With `with_arq_syntax(false)`, and with `qparse --syntax sparql`, each of the
  three extensions is a syntax error.
* **A10.** The W3C SPARQL suites still pass 482/328/157/269 with no failures.

## 9. Rejected alternatives

* **Rewriting ranges in the parser.** Expanding `p{3}` into `p/p/p` at parse time would
  need no executor work for fixed lengths. It would change what `qparse`, the formatter
  check and SERVICE requests see, and it does not cover the other forms.
* **Always evaluating `LATERAL` per row.** It is the definition, but it would make the
  common correlated-free use, a lateral that only adds patterns, far slower than the join
  it equals.
* **Always decorrelating `LATERAL` into a join.** It is wrong for FILTERs that use outer
  variables, for sub-selects with LIMIT and for aggregates, which are the reasons to
  write `LATERAL`.
* **Renaming hidden sub-select variables in the algebra, as ARQ does.** It needs a
  rename pass over every pattern and expression form. Scoping the constant table in the
  planner gives the same result for `LATERAL` with a few lines.
* **Copying Jena's `{0,}`.** It contradicts ARQ's documentation and `{*}`, and a user who
  writes `{0,}` asks for the zero-length match.
* **ARQ syntax off by default.** Fuseki parses with ARQ's syntax, and the queries this
  serves are written for Fuseki.
* **Returning quads from `construct` in the library.** Existing callers expect a graph.
  A separate method keeps them unchanged.

## 10. Sources

* Apache Jena `jena-arq` (Apache-2.0): the ARQ grammar `Grammar/main.jj` (`LateralGraphPattern`,
  `PathMod`, `ConstructQuads`, `ConstructTemplate`), `SyntaxVarScope`, `OpLateral` and its
  executor, `path/P_Mod`, `P_FixedLength`, `PathFactory`, `PathCompiler`, `PathLib`,
  `path/eval/PathEngineSPARQL` and `PathEngineN`, `Template`, `TemplateLib` and Fuseki's
  `SPARQLQueryProcessor` and `Responses`, read for behaviour. No code was copied.
* ARQ's tests: `testing/ARQ/Syntax-Lateral`, `testing/ARQ/Lateral`,
  `testing/ARQ/Syntax-ARQ` (`syntax-quad-construct-*`), and the unit tests `TestPath` and
  `TestPathQuery`.
* Jena 6.2.0's `arq` command, run on the graphs of §4.2.
* SPARQL 1.1 Query (§9 property paths, §18.2 translation) and SPARQL 1.2 Query, and the
  SPARQL 1.2 community proposal SEP-0006 (lateral join).
* The Sparkles code: the planner, the path operator, EXISTS decorrelation, the result
  writers and the formatter.

## 11. Phase 2: configurable DESCRIBE

This section was added for Phase 2. Its sources are Jena's `org.apache.jena.sparql.core.describe`
package (`DescribeHandler`, `DescribeHandlerRegistry`, `DescribeBNodeClosure`), Jena's
`Closure` and `QueryExecDataset.describe`, TDB2's `QueryEngineTDB`, Jena 6.2.0's `arq`
command run on small datasets, and the W3C member submission *CBD - Concise Bounded
Description* (Patrick Stickler, 2005) with its symmetric form.

### 11.1 What Jena does

SPARQL leaves the result of DESCRIBE to the implementation. ARQ collects the resources a
query names and binds, and passes each one to the handlers of its
`DescribeHandlerRegistry`. The default registry holds one handler,
`DescribeBNodeClosure`. For a resource it computes the blank-node closure in the default
graph: the resource's triples, then the triples of each blank-node object, recursively.
It then finds the named graphs in which the resource is a subject, with
`SELECT DISTINCT ?g { GRAPH ?g { ?s ?p ?o } }`, and computes the closure inside each of
them on its own. All the triples go into one result graph. Literals are not described.

Three details follow from the code, and Jena 6.2.0 confirms them on the data of the
acceptance examples below.

* A blank node is followed only inside the graph where it was found. With
  `:m :p _:x` in graph `g3` and `_:x :q "v"` in graph `g4`, `DESCRIBE :m` does not
  contain `_:x :q "v"`.
* The default graph is `DatasetGraph.getDefaultGraph()`. In TDB2 that is the stored
  default graph even when the dataset sets `unionDefaultGraph`, because only
  `QueryEngineTDB` applies the union, when it evaluates the WHERE clause.
* Reifiers are not followed. The annotation in `:a :p :b {| :source :wiki |}` is
  missing from `DESCRIBE :a`.

Sparkles followed the closure in the default graph only. Its descriptions therefore left
out the triples of named graphs that Jena includes.

Fuseki has no configuration for DESCRIBE. A handler is registered from Java code, and
the assembler vocabulary has no term for it.

### 11.2 Goals

* The default answer equals Jena's, including the triples from named graphs.
* Three modes, chosen per dataset and per request:
  * `cbd`, the concise bounded description of the submission, which is the default;
  * `scbd`, its symmetric form;
  * `outgoing`, the resource's own triples.
* Reifiers are handled as the submission's third rule asks.
* Options add the labels of linked IRIs and limit the size and the depth of a
  description.
* The graph view of the caller (C12) and its triple protections (C12b) apply.
* The setting is persisted with the dataset, can be changed over HTTP and from the CLI,
  and is listed in the service description.

Non-goals:
* Handlers written by users. Sparkles has no plugin interface, and the three modes cover
  the handlers that Jena users write most often.
* New query syntax. A query-level option would have to go behind the `arq()` guard of §2,
  but ARQ has no such syntax, and the request parameter covers the need.
* The submission's inverse functional form, which needs OWL's inverse functional
  properties.

### 11.3 The modes

Each mode is defined for one source graph, as the submission defines it.

**`cbd`.** Start from the resource.
1. Include every triple whose subject is the resource.
2. For every included triple with a blank-node object, include the triples whose subject
   is that blank node, recursively.
3. For every included triple, include the description of each of its reifiers,
   recursively.

**`scbd`.** Include the `cbd` of the resource. Then include every triple whose object is
the resource, and, for every included inbound triple with a blank-node subject, the
triples whose object is that blank node, recursively. The reifiers of every included
triple contribute their own symmetric description. Following the submission, a blank
node reached backwards is not described forwards.

**`outgoing`.** Include the triples whose subject is the resource, and nothing else.

A reifier is either kind of statement node.
* RDF 1.2: a node `r` with `r rdf:reifies <<( s p o )>>`. The annotation syntax writes
  these.
* RDF 1.1: a node `r` with `r rdf:subject s`, `r rdf:predicate p` and `r rdf:object o`.
  This is the reification that the submission refers to.

The `reifiers` option, on by default, switches the third rule off. That gives Jena's
`DescribeBNodeClosure` exactly. A store without `rdf:reifies` or `rdf:subject` triples
skips the lookups.

### 11.4 Sources and graphs

The description reads the query's dataset. The source graphs are the default graph and
each named graph, and each source is described on its own, as in Jena. A blank node, an
inbound blank-node subject or a reifier is followed inside the source where it was found.
The triples of all the sources go into one result graph. A resource is looked up in
every source. A blank node or reifier is looked up only in the source that led to it.

The default graph is chosen as follows.
* With `FROM`, `default-graph-uri` or the inference overlay of materialized reasoning,
  the default graph is the merge of the listed graphs, and with
  `FROM <urn:x-arq:UnionGraph>` it is the union of the named graphs.
* Otherwise it is the stored default graph, as in TDB2, even in a store whose queries see
  the union of the named graphs.

The named graphs are those of `FROM NAMED` or `named-graph-uri` when the request gives
them, and otherwise every named graph of the store. The graph of materialized inferences,
`urn:x-sparkles:inferred`, is never a named source. Its triples reach a description
through the default graph when the request reads inferences, so `reasoning=false` keeps
them out.

The access view needs no code of its own. The query context's dataset is already limited
to the graphs the caller may read (C12), and its snapshot is masked by the caller's
triple protections (C12b). In a union store, a view that may not read the stored default
graph gets no default source.

### 11.5 Options and limits

| Option | Default | Meaning |
|---|---|---|
| `mode` | `cbd` | `cbd`, `scbd` or `outgoing` |
| `labels` | `false` | Add the `rdfs:label` and `skos:prefLabel` triples of every IRI in the description, as subject or object, read from the source in which the IRI was found |
| `reifiers` | `true` | Follow reifiers in `cbd` and `scbd` |
| `maxTriples` | none | The most triples one DESCRIBE returns |
| `maxDepth` | none | The most levels a description follows |

The resource's own triples are level 1. Each blank node, inbound blank node or reifier
followed adds a level, so `maxDepth: 1` with `cbd` gives `outgoing`. Labels do not
count as a level. A node reached again at a lower level is described again, so that the
order of the resources does not change what a depth limit includes.

`maxTriples` bounds the whole result of one query. When it is reached, the description
stops. The result keeps the triples found so far, the plan gets the warning
`describe-truncated`, and the HTTP response gets the header
`Sparkles-Describe-Truncated: true`. Stopping is more useful than failing for a hub
resource under `scbd`, whose inbound triples can run into millions. The existing budgets
apply too. The `rows` budget counts the triples of the description, and the timeout and
cancellation are checked once per node.

### 11.6 Configuration

* **Engine.** `QueryOptions::describe` holds a `DescribeOptions`. `QueryResult` gains
  `describe_truncated`.
* **Dataset setting.** The `Store` keeps the dataset's options. A persistent store writes
  them to `describe.json`, which holds the options that differ from the defaults and a
  `format` number, and the defaults remove the file. `Dataset::query` uses the setting.
* **HTTP.** `GET /$/describe/{ds}` returns the setting, `PUT` replaces it and `DELETE`
  restores the defaults, with the read and admin permissions of the other dataset
  settings. A query request takes `describe=cbd|scbd|outgoing`,
  `describe-labels=true|false`, `describe-reifiers=true|false`, `describe-max-triples=N`
  and `describe-max-depth=N`. The two limits of a request can only lower the dataset's,
  like the budget parameters of C01.
* **CLI.** `sparkles describe-settings` prints and changes the setting, with `--loc` or
  `--server`, in the form of `sparkles compaction`. `sparkles query --describe MODE` and
  `--describe-labels` change a local query.
* **Service description.** The query service lists `spk:describeMode`,
  `spk:describeLabels` and `spk:describeReifiers`, and the limits when they are set.
* The MCP tools and stored queries use the dataset's setting.

### 11.7 Acceptance examples

The data is a TriG dataset with nested blank nodes, a cycle of blank nodes, inbound
links from IRIs and blank nodes, an annotated triple, and named graphs. One named graph
holds a triple about the resource, one holds an inbound link, and two share a blank
node.

* **A11.** With `reifiers: false`, ten DESCRIBE queries give Jena 6.2.0's answers. They
  cover a resource with nested blank nodes and a named-graph triple, resources bound by
  a WHERE clause (an IRI and a blank node), a resource found only in a named graph, a
  blank node shared by two named graphs, a cycle of blank nodes, `DESCRIBE *`, several
  resources and an unknown resource.
* **A12.** `cbd` adds the annotation's reifier to the description of `:a`. A reifier of a
  reifier's triple, and an RDF 1.1 reification, are included as well.
* **A13.** `scbd` adds the inbound triples from the default graph and from named graphs,
  and it follows an inbound blank node backwards but not forwards.
* **A14.** `outgoing`, `labels`, `maxDepth` 1 to 3 and `maxTriples` give the triples
  §11.5 defines, and a truncated result says so.
* **A15.** `FROM`, `FROM NAMED` and the protocol's dataset choose the sources as §11.4
  says. A union store gives the same answers as a store without the union.
* **A16.** The differential tests of C12 and C12b run every DESCRIBE query in `cbd` and in
  `scbd` with labels, in stores with and without a union default graph. A view answers
  as a store of the graphs and triples it sees.
* **A17.** The setting survives a restart, a request's mode and limits apply, malformed
  values are a `400`, and the service description lists the setting.

### 11.8 Rejected alternatives

* **A handler registry.** Rust code cannot be registered at run time without a plugin
  interface, and a configuration of handler names would offer no more than the modes do.
* **Reifiers off by default.** That would match Jena exactly, but the CBD would then not
  be the submission's CBD. Annotations are how RDF 1.2 data says where a triple came
  from, and a description without them loses that. The switch keeps Jena's behaviour one
  option away.
* **Failing at `maxTriples`.** An error gives a client nothing to show for a hub
  resource. Truncation with a header and a warning gives it a bounded description that
  it can recognise as partial.
* **Reading the union graph as the default graph in a union store.** It would cross from
  one named graph into another along blank nodes, which Jena never does, and the stored
  default graph would be left out.

## 12. Phase 3: the rest of ARQ's language and library

This section was added for Phase 3. Its sources are ARQ's grammar (`main.jj`:
`Assignment`, `Unfold`, `SemiJoinGraphPattern`, `AntiJoinGraphPattern`, the `FOLD`
aggregate and the `PathPrimary` and `PathSequence` forms), `AlgebraGenerator`,
`SyntaxVarScope`, `QueryIterAssign`, `QueryIterUnfold`, `QueryIterHalfJoin`, the
`org.apache.jena.cdt` package with `NVCompare` and `ValueSpace`, the aggregators
`AggFoldList` and `AggFoldMap`, the function library `CDTLiteralFunctions`, the
property functions of `org.apache.jena.sparql.pfunction.library` with `GraphList`,
`StandardFunctions` and the classes of `org.apache.jena.sparql.function.library`,
`XSDFuncOp` and `NodeValueOps`, and the path engines `PathEngine1` and `PathEngineN`. The
tests are ARQ's `testing/ARQ` (`Syntax-ARQ`, `PropertyFunctions`) and Jena's
`testing/SPARQL-CDTs`, and Jena 6.2.0's `arq` command gave the answers for the rest.

### 12.1 Goals

* `LET`, `UNFOLD`, `FOLD`, `SEMIJOIN`, `ANTIJOIN` and the path forms `distinct(…)`,
  `multi(…)`, `shortest(…)` and `:p^:q` parse behind the `arq()` guard of §2 and give
  ARQ's answers.
* The composite datatypes `cdt:List` and `cdt:Map` have Jena's lexical forms, equality,
  order and 16 functions, and Jena's SPARQL-CDTs suite passes apart from listed cases.
* ARQ's property function library runs as ordinary operators, with ARQ's answers.
* Every function of Jena's `FunctionRegistry` that does not need a JavaScript engine or
  ARQ's execution context is implemented.
* Queries without these forms plan exactly as before.

Non-goals:
* JavaScript functions (`js:`), which need a JavaScript engine.
* `afn:context`, which reads ARQ's execution context, and the Leviathan library
  (`lfn:`), which is not part of ARQ's `afn:` and `fn:` libraries.
* `rdfs:member` as a property function. ARQ registers it, which changes what a query
  over `rdfs:member` means, and the container functions `apf:bag`, `apf:seq`,
  `apf:alt` and `apf:container` with it.
* The `JSON` query form and ARQ's `EXISTS { … }` group element.

### 12.2 Syntax and algebra

The vendored spargebra gains these forms.

| Syntax | Algebra | SSE |
|---|---|---|
| `LET (?v := e)` | `GraphPattern::Assign { inner, variable, expression }` | `(assign ((?v e)) …)` |
| `UNFOLD(e AS ?v, ?w)` | `GraphPattern::Unfold { inner, expression, variable, second }` | `(unfold (e ?v ?w) …)` |
| `SEMIJOIN { … }` | `GraphPattern::SemiJoin { left, right }` | `(semijoin L R)` |
| `ANTIJOIN { … }` | `GraphPattern::AntiJoin { left, right }` | `(antijoin L R)` |
| `FOLD(DISTINCT? e, f ORDER BY …)` | `AggregateExpression::Fold { expr, value, distinct, order }` | `(fold e f (order …))` |
| `distinct(p)`, `multi(p)`, `shortest(p)` | `PropertyPathExpression::Distinct`, `Multi`, `Shortest` | `(distinct p)` … |
| `:p^:q` | `Sequence(:p, Reverse(:q))` | `(seq :p (reverse :q))` |

`LET` may assign a variable that is in scope, as ARQ's `SyntaxVarScope` allows. Neither
variable of `UNFOLD` may be in scope (ARQ's `checkUNFOLD`). The in-scope variables of a
half join are those of its left side. The check against nested aggregates covers every
expression of `FOLD`.

### 12.3 `LET`

ARQ's `QueryIterAssign` evaluates the expression per solution. An error leaves the
solution as it is. An unbound variable takes the value. A bound one keeps the solution
when its value is the same value as the expression's (`Node.sameValueAs`, under which
`1` and `1.0` are the same and `1` and `1e0` are not) and drops it otherwise. The
planner plans a `LET` of a variable that the pattern before it cannot bind as `BIND`, so
those plans are `Extend`. Any other `LET` is a `Let` operator that computes the column
and compares it where the variable is bound.

### 12.4 Composite datatypes, `FOLD` and `UNFOLD`

A `cdt:List` or `cdt:Map` literal keeps its lexical form in the store, and a new module
parses it when an operation needs its value. The grammar is Jena's `cdt_literals.jj`:
IRIs, blank nodes, literals in Turtle's syntax without prefixes, numbers, booleans,
`null` and nested lists and maps, with distinct IRI or literal keys in a map. A literal
that does not parse is ill-formed, and only the same literal equals it.

* `=` is `CompositeDatatypeList.isEqual` and its map form, element by element with
  `Node.sameValueAs`. Two different blank nodes cannot be compared, which is an error.
* `<` is the proposal's list-less-than and map-less-than: element by element, by the
  elements' `<` and `=`, an empty list first and then by length. A `null` against a value
  is an error.
* ORDER BY uses the same walk with ORDER BY's order of the elements, `null` first, and
  breaks ties by lexical form. Lists and maps sort after the other literals.
* The functions are those of `CDTLiteralFunctions`, with Jena's argument checks and
  errors. A function that returns its argument unchanged, such as `cdt:put` of an entry
  that is there, returns the same term.

Values built by functions, `FOLD` and `UNFOLD` are written canonically: `, ` between
elements, terms as Turtle writes them without prefixes, and a map's entries in key order
(`CDTKeySorter`). Jena writes a map in hash order, so lexical forms can differ and values
cannot.

`FOLD` is an aggregate of the group operator. It sorts the group's rows by its ORDER BY
with ORDER BY's order, then folds the values into a list (errors become `null`, and
`DISTINCT` keeps the first of equal terms and one `null`) or into a map (rows whose key is
an error or a blank node are skipped, and a later row replaces a value). An empty group
gives an empty list or map. `UNFOLD` is an operator over its input's rows that parses the
literal of each distinct value once. It gives one row per element (with the position
from 1) or per entry (with the value), leaves a `null` unbound, gives no rows for an empty
list or map, and keeps the row once with both variables unbound for any other value or an
error, as `QueryIterUnfold` does.

### 12.5 Property functions

ARQ's library is `list:member`, `list:index` and `list:length` in `list:`, and
`apf:strSplit`, `apf:concat`, `apf:str`, `apf:splitIRI` (`apf:splitURI`),
`apf:assign`, `apf:bnode` (`apf:blankNode`) and the list functions under their class
names in `apf:`, with the old `jena.hpl.hp.com` namespaces too. Jena's documentation
lists `apf:versionARQ`, which Jena 6 no longer has. Sparkles binds `<urn:x-sparkles:>`
and its own version to it.

ARQ evaluates a property function per solution of the patterns before it in its basic
graph pattern, with their values substituted. The planner takes the calls out of each
basic graph pattern, with their list arguments, and notes which of the variables a call
reads (its list for the list functions, its arguments for `apf:strSplit`, `apf:concat`
and `apf:str`) are bound by the group's earlier elements or by earlier patterns of the
same basic graph pattern. A call that reads none of them is a leaf of the join order,
which for the list functions with an unbound list enumerates the heads of all lists, as
`GraphList.findAllLists` does. A call that reads some is attached to the rest of the
group. It solves each distinct substitution of what it reads once, and keeps each
solution that agrees with the row on every variable the row binds. Reading only the
variables bound before the call gives ARQ's order-dependent answers in a planner that
reorders joins: `?l list:member 4 . ?x :p ?l` finds the heads of the lists that hold 4,
not the cells.

An OPTIONAL whose right side calls a function that reads a variable of the left side is
evaluated per left row, as ARQ substitutes into an OPTIONAL's right side. The `Lateral`
operator of §3.3 gains an optional mode for it, which keeps a left row without solutions
and applies the OPTIONAL's filter. Lists are read per named graph: a basic graph pattern
with calls is never a simple `GRAPH ?g` group.

The list walks follow `GraphList`: a node is a list when it is `rdf:nil` or has an
`rdf:rest`, members are the `rdf:first` objects along `rdf:rest`, `list:index` counts
from 0 and finds a member's first position only, and `list:length` of a constant length
with an unbound list has no solutions. A walk stops at a cycle.

### 12.6 Functions

The registry diff adds `fn:format-number`, `fn:collation-key` and `fn:apply`, and
`afn:sprintf`, `afn:system-timezone`, `afn:nowtz`, `afn:version`, `afn:collation`,
`afn:eval`, `afn:print`, `afn:execTime` and `afn:wait`.

* `afn:sprintf` formats as `String.format`, with ARQ's mapping of terms to Java values
  in `XSDFuncOp.javaSprintf`, including its oddities: a language-tagged string is passed
  as its tag, and an IRI as a quoted string. `%f` and `%e` round the shortest decimal
  digits half up, as Java's `Formatter` does.
* `fn:format-number` is `DecimalFormat` with the picture as its pattern, as ARQ
  implements it, and the third argument as a language tag for the symbols.
* `fn:apply` and `afn:eval` call an extension function or cast by IRI.
* `afn:collation` returns its string. Ordering by a locale's collator would need a
  collation library and a value kind for sort keys.
* `afn:print` and `afn:execTime` return `true` without printing.
* `afn:localname` and `afn:namespace` change to Jena's split of an IRI
  (`SplitIRI.splitXML`), which `apf:splitIRI` uses too.

`NodeValueOps` defines `+`, `-`, `*` and `/` on dates, times and durations beyond
SPARQL's. Sparkles adds a date or time plus or minus a duration, the difference of two
times, sums of durations of different kinds, a duration times or divided by a number and
the ratio of two durations, keeping `xsd:dayTimeDuration` where F&O 3.1 does.

### 12.7 Half joins and path forms

`SemiJoin` and `AntiJoin` are operators over both sides' tables, which hash the right
side on the shared variables and fall back to a compatibility scan for rows with unbound
shared variables. As in `QueryIterHalfJoin`, a right row that shares no variable with a
left row is compatible with it, unlike in MINUS. Filters over a half join are pushed into
its left side, whose rows it keeps unchanged.

`PathEngine1` gives `distinct(p)` a set of end nodes per start node, so the planner plans
it as the distinct pairs of `p`. `PathEngineN` counts walks, so `multi(p)` is `p` with its
`*`, `+` and `?` replaced by the counted ranges `{*}`, `{+}` and `{0,1}` of §4. ARQ throws
"not implemented" for `shortest(p)`, and the planner refuses it.

### 12.8 Acceptance examples

* **A18.** Jena's SPARQL-CDTs suite runs with the W3C harness, with the cases Sparkles
  answers otherwise listed with their reasons.
* **A19.** ARQ's `PropertyFunctions` tests run with the W3C harness, with the container
  tests listed.
* **A20.** ARQ's `syntax-let-*` tests parse or fail as in ARQ, and `LET` over bound,
  unbound and failed values gives Jena 6.2.0's answers.
* **A21.** `FOLD` with ORDER BY and `UNFOLD` of the result give Jena 6.2.0's answers.
* **A22.** `SEMIJOIN`, `ANTIJOIN`, `distinct(…)`, `multi(…)` and `:p^:q` give Jena
  6.2.0's answers, including an antijoin with no shared variables.
* **A23.** `afn:sprintf`, `fn:format-number`, `fn:apply`, `afn:localname` and the
  temporal operators give Jena 6.2.0's strings and values.
* **A24.** The benchmark's queries get the same plans as before.
* **A25.** The W3C SPARQL suites still pass 482/328/157/269.

### 12.9 Rejected alternatives

* **Rewriting `LET` as `BIND` with a filter.** `Extend` cannot rebind a variable, and a
  filter after a fresh variable needs a rename of every later use.
* **A value kind for composite literals.** Parsing the lexical form when an operation
  needs it keeps the store, the dictionary and the result writers unchanged. Queries
  that touch lists parse them, which costs a few microseconds per literal.
* **Property functions as row-at-a-time callbacks.** ARQ calls a function per binding.
  Solving each distinct substitution once in an operator gives the same answers and lets
  a call that reads nothing take part in the join order.
* **Reading every variable the group binds into a call.** It would be simpler, but it
  changes ARQ's answers wherever a call comes before the pattern that binds its list.
* **`rdfs:member` as ARQ's container function.** It is ARQ's default, but it changes the
  answers and plans of queries that use `rdfs:member` as a property.

## Outcome

**Delivered.** Phase 1 landed on 2026-10-02 as designed in §2 to §6:
- `SparqlParser::with_arq_syntax` in the vendored spargebra, on by default, with
  `LATERAL` (spargebra's `sep-0006` code, now switched on), path ranges as
  `PropertyPathExpression::Range` and CONSTRUCT templates with `GRAPH` as
  `Query::Construct::graph_templates`. ARQ's aggregate keywords moved behind the same
  switch;
- the planner's join test for `LATERAL` and the `Lateral` operator
  (`sparql/lateral.rs`), the scoped constant table and the `GRAPH ?g` fix, which also
  corrects per-row `EXISTS` over a non-trivial `GRAPH ?g` group;
- the range rewrite in the planner and the counting mode of the path operator;
- CONSTRUCT quads in `QueryResult::quads`, in every dataset format of the server and the
  CLI, and in the `x-sparkles+json` document;
- the formatter, the editor keyword, `WhereBuilder::lateral` and range paths in the
  query builder, and `qparse --syntax` and `uparse --syntax`.

**Deviations and decisions.**
- A guard rule cannot choose the parser's message. peg reports the alternatives that
  failed furthest into the text, so strict mode gives an ordinary syntax error at the
  extension rather than the words `ARQ syntax`.
- One W3C negative syntax test, `constructwhere06` (`CONSTRUCT WHERE { GRAPH … }`), is
  valid ARQ syntax, as ARQ's own `syntax-quad-construct-11` says. The W3C harness now
  parses positive syntax tests in both modes and negative ones as strict SPARQL, which
  is how Jena's runner reads `.rq` files.
- `qparse --syntax` also takes Jena's names `SPARQL_11` and `SPARQL_12`.
- In Python, `construct()` keeps returning `QueryTriples`, which gained a `quads`
  attribute, and its `serialize` writes them in dataset formats. That replaced the
  planned `construct_quads` method. Rust has `Dataset::construct_quads` as planned.
- RDFS on read rewrites a range of at most 8 steps as the union of its sequences, so
  that each entailed link counts once. Inside a longer or unbounded range, a link that
  holds through a property and also through its subproperty counts twice.
- A SERVICE inside a `LATERAL` that runs per row is sent as written, without the outer
  values.
- The `Lateral` operator evaluates its groups one after another. Running them in
  parallel was left for later.

**Tests.** `crates/sparkles/tests/arq_syntax.rs` ports ARQ's `Syntax-Lateral` (13
cases), `Lateral` (7) and `syntax-quad-construct-*` (13) tests, the range cases of
`TestPath` and `TestPathQuery`, and Jena 6.2.0's answers to 35 queries on small graphs for
ranges and `LATERAL` (A1 to A9). It found that a sub-select that does not project an
outer variable hides it, as in ARQ, which two of the first expectations had wrong. The
formatter has golden files for the three forms, and its check that the output means the
same as the input passes on them. The query builder, the Python bindings and `qparse`
have tests of their parts. The W3C SPARQL suites still pass 482/328/157/269 (A10).

**Measurements.** On the 1.05M-triple benchmark data, with a release build on a machine
that other builds kept at a load of about 55:
- `?p a ex:Researcher LATERAL { SELECT ?p ?f { ?p foaf:knows ?f } ORDER BY ?f LIMIT 1 }`
  ran 30,279 groups in 666 ms, about 22 µs per group;
- `?p a ex:Researcher LATERAL { ?p foaf:knows ?f }` and `?a foaf:knows{2} ?c` get the
  same plans as the plain join and the sequence `foaf:knows/foaf:knows`;
- eight of the benchmark's ordinary queries get the same plans from main and from this
  branch, and their timings, interleaved over 11 rounds, differ only by noise.

**Not built.** ARQ's other path forms (`:p^:q`, `distinct(…)`, `shortest(…)`,
`multi(…)`), `SEMIJOIN`, `ANTIJOIN`, `LET`, `UNFOLD` and the `JSON` query form (§1
non-goals), and parallel evaluation of `LATERAL` groups.

**Phase 2, delivered.** Configurable DESCRIBE landed on 2026-10-02 as §11 designs it:
- `sparql/describe.rs` with `DescribeMode` (`cbd`, `scbd`, `outgoing`) and
  `DescribeOptions` (`labels`, `reifiers`, `maxTriples`, `maxDepth`), read from the
  default graph and each named graph as separate sources, with RDF 1.2 reifiers and
  RDF 1.1 reification;
- `QueryOptions::describe`, `QueryResult::describe_truncated` and the plan warning
  `describe-truncated`;
- the dataset's setting in the `Store`, kept in `describe.json`, and used by
  `Dataset::query`, the server's queries, stored queries and the MCP tools;
- `GET`, `PUT` and `DELETE /$/describe/{ds}`, the request parameters, the response
  header `Sparkles-Describe-Truncated`, the service description's `spk:describeMode`
  and its options, and the OpenAPI description of all of them;
- `sparkles describe-settings`, and `--describe` and `--describe-labels` on
  `sparkles query`;
- the Python bindings' dataset queries, which use the dataset's setting. Queries inside
  a Python transaction use the defaults. Since 2026-10-03 a transaction uses the
  dataset's setting as it was when the transaction began, and every Python query method
  takes a `describe` argument, a mode or a dict of options over that setting.

**Phase 2, deviations and decisions.**
- The default answer changed in two ways. Descriptions now include the triples of named
  graphs, which Jena includes and Sparkles had left out. They also include the
  descriptions of reifiers, which Jena leaves out. On data without reifiers the default
  equals Jena's answer, and `reifiers: false` equals it on any data.
- In a store with a union default graph, a description without a dataset used to read
  every graph as one. It now reads the stored default graph and each named graph on its
  own, as Jena does. The C12 and C12b differential tests had skipped DESCRIBE in union
  stores, because the old reading included the stored default graph that a view's union
  leaves out. They now run it.
- `--describe` and `--describe-labels` on `sparkles query` apply to `--loc` and `--data`.
  At first `--server` refused them. Since 2026-10-03 the remote client sends them as the
  request parameters, along with the newer `--describe-reifiers`,
  `--describe-max-triples` and `--describe-max-depth`.
- Jena 6.2.0's `arq` command fails with a `NullPointerException` when a DESCRIBE query
  has `FROM` or `FROM NAMED` (`DescribeBNodeClosure` gets no dataset). The rules of §11.4
  for `FROM` are therefore checked against the design only, not against Jena.
- A description's index scans check the `rows` budget, the deadline and cancellation
  while they read, so a hub resource under `scbd` cannot hold unbounded memory before
  `maxTriples` applies.
- The setting's `GET` is not limited to callers who read the whole dataset, unlike the
  RDFS and compaction settings, because it reveals nothing about the data.

**Phase 2, tests.** `crates/sparkles/tests/describe.rs` has 12 tests for A11 to A15:
Jena 6.2.0's answers to ten queries, the reifier cases, `scbd`, `outgoing`, labels, the
depth and size limits, the query's dataset, a union store, the inference overlay, the
options' parsing and the persisted setting. The differential tests of
`graph_access.rs` and `triple_access.rs` run each DESCRIBE query in `cbd` and in `scbd`
with labels, with and without a union default graph (A16). The server's tests cover the
route, the request parameters, the header, the service description and a restart (A17),
and `sparkles describe-settings` on a local database. The W3C SPARQL suites still pass
482/328/157/269, and Jena's HTTP clients pass 138 of 138 checks, `QueryExecHTTP`'s
DESCRIBE included.

**Phase 2, cost.** The default mode reads the same index ranges as before. The scan of a
resource's subject prefix returns its quads in every graph, and the named-graph sources
filter them, so no extra scans are made. Reifier lookups run only when the store has
`rdf:reifies` or `rdf:subject` triples, and they cost a dictionary lookup and a prefix
scan per described triple. `scbd` adds one object-prefix scan per node.

**Phase 2, not built.** Handlers registered by users, the inverse functional form of the
submission, DESCRIBE options in Python, in Python transactions and in the remote
client, and a UI control for the setting.

**Phase 3, delivered.** The rest of ARQ's language and library landed on 2026-10-03 as
§12 designs it:
- `LET` (`GraphPattern::Assign`, planned as `BIND` or a `Let` operator), `UNFOLD`
  (`GraphPattern::Unfold` and an `Unfold` operator), `FOLD`
  (`AggregateExpression::Fold`), `SEMIJOIN` and `ANTIJOIN` (`SemiJoin` and `AntiJoin`
  operators), and the paths `distinct(…)`, `multi(…)`, `shortest(…)` (refused) and
  `:p^:q`, all behind the `arq()` guard;
- `sparql/cdt.rs` with the lexical forms, equality, order and the 16 `cdt:` functions;
- `sparql/arqpf.rs` with the `list:` and `apf:` property functions as a
  `PropertyFunction` operator, and the optional mode of `Lateral` for OPTIONALs whose
  calls read the left side;
- `sparql/fnformat.rs` with `afn:sprintf` and `fn:format-number`, and the other new
  functions and temporal operators in `expr.rs` and `fnlib.rs`;
- the service description lists the `cdt:` functions and the property functions, the
  formatter parses and prints every new form, and the editor knows the keywords and
  offers the `cdt:`, `list:` and `apf:` names after their prefixes.

**Phase 3, deviations and decisions.**
- A blank node label in a CDT literal names a blank node of the query that reads it.
  Jena's loader also relates it to the data file's blank nodes, which Sparkles does not,
  so 11 of the suite's blank node tests fail. Two more fail because Sparkles stores
  `"01"^^xsd:integer` as `1`.
- The property functions read only the variables bound before the call. A call with an
  unbound list therefore lists heads even when a later pattern binds the list, as in ARQ.
- `apf:concat` with a bound subject compares it with the concatenation, where ARQ fails.
  `apf:versionARQ`, missing from Jena 6, binds `<urn:x-sparkles:>` and the Sparkles
  version, and `afn:version` returns the Sparkles version.
- `afn:sprintf` conversions that Java refuses are expression errors, where ARQ fails the
  whole query. `%c` is always an error, as Java takes no `BigInteger` for it.
- `fn:format-number` knows the separators of the common European languages and uses the
  root locale's for other tags.
- `afn:collation` orders by code point, and `afn:print` and `afn:execTime` print nothing.
- A day-time duration times or divided by a number, and the difference of two times,
  stay `xsd:dayTimeDuration`, where ARQ gives `xsd:duration`. A number times a duration
  and a year-month duration times a number are defined as in F&O 3.1.
- `afn:localname` and `afn:namespace` now split as Jena does, which changes their
  answers for IRIs whose last segment starts with a digit or holds a character that an
  XML name cannot.
- The query builder has no methods for the Phase 3 forms.

**Phase 3, tests.** Jena's `SPARQL-CDTs` (655 tests) and ARQ's `PropertyFunctions` (48)
are vendored in `testsuite/jena-arq` with Jena's license and run from `tests/w3c.rs`:
642 and 39 pass, and the 13 and 9 others are listed with their reasons in
`expected-failures.txt` (A18, A19). `tests/arq_library.rs` checks `LET`, `FOLD`,
`UNFOLD`, the property functions, the half joins and the path forms against Jena 6.2.0's
answers, and the plans of calls and OPTIONALs (A20 to A22). `tests/arq_functions.rs`
checks the new functions and operators (A23), and unit tests cover the CDT parser,
`sprintf`, `DecimalFormat` patterns and the IRI split. The formatter has golden files
for every new form. The 28 benchmark queries get the same EXPLAIN from main and from this
branch on a 210,509-triple dataset, apart from the generated names of aggregates (A24).
The W3C SPARQL suites still pass 482/328/157/269 (A25).

**Phase 3, cost.** Queries without the new forms run the code they ran before. The
planner checks each basic graph pattern for a property function, and value comparisons
check for the two composite datatypes only when both values are literals of an unknown
datatype. A composite literal is parsed each time an operation reads it.

**Container functions.** A later change added `apf:container`, `apf:bag`, `apf:seq` and
`apf:alt`, and `rdfs:member` as ARQ's container function, to `sparql/arqpf.rs`. They
follow Jena's `container` class and `GraphContainerUtils`. A container is a resource
typed `rdf:Bag`, `rdf:Seq` or `rdf:Alt`. A bound container gives its members in the
order of their `rdf:_n` numbers, a bound member is counted once per numbered triple that
holds it, and an unbound container is each typed resource, or each subject of a triple
whose object is the bound member. `rdfs:member` concatenates the stored `rdfs:member`
triples with the members of every container, as ARQ does.

The non-goal of §12.1 was that ARQ's reading of `rdfs:member` changes the answers and
plans of queries that use it as a property. Jena's PropertyFunctions suite writes the
function as a plain triple pattern, so passing it needs ARQ's reading there. The
planner therefore reads `rdfs:member` as the function only while the store holds a
resource typed as a container, checked with one index lookup per type when a basic
graph pattern uses `rdfs:member`. Without a container, the function's solutions are
exactly the stored triples, so the pattern stays an ordinary triple pattern with its
plans. An OPTIONAL whose right side reads a variable through `rdfs:member` is not run
per left row, unlike the library's other functions. ARQ's PropertyFunctions suite now
passes in full (48 of 48), and `tests/arq_library.rs` checks the functions against
Jena 6.2.0's `arq` on a dataset with each container type, an untyped resource with
numbered triples and a stored `rdfs:member` triple, and that the plan changes only with
a container in the store.

**Phase 3, not built.** JavaScript functions, `afn:context`, the Leviathan library,
locale collation, the `JSON` query form, ARQ's `EXISTS { … }` element, and Phase 3
methods in the query builder.
