# G06: Jena ARQ's query language extensions

> **Status:** implemented in part
>
> **Phases:** Phase 1 shipped on 2026-10-02: `LATERAL`, property path ranges and
> CONSTRUCT templates with `GRAPH`. Configurable DESCRIBE and the rest of ARQ's function
> library are later phases and will extend this spec.
>
> **User docs:** [API: ARQ syntax extensions](../API.md#arq-syntax-extensions) ·
> [Features](../FEATURES.md#sparql-arq-equivalent) ·
> [Comparison with Jena](../COMPARISON.md#vs-apache-jena--fuseki)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This spec draws on the Sparkles code, the grammar and engine sources of Apache Jena's
ARQ (`jena-arq`, Apache-2.0) for syntax and behaviour, ARQ's test suites
(`jena-arq/testing/ARQ` and the unit tests of `org.apache.jena.sparql.path`), Jena
6.2.0's `arq` command run on small graphs, the SPARQL 1.1 and 1.2 Query specifications
and the SPARQL community proposal SEP-0006 for `LATERAL`. Fluree was not consulted.

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
