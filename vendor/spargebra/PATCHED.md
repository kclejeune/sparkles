# spargebra 0.4.7, patched

This is [spargebra](https://crates.io/crates/spargebra) 0.4.7 from the Oxigraph project
(<https://github.com/oxigraph/oxigraph>, `lib/spargebra`), copyright the Oxigraph
developers, dual-licensed under MIT or Apache-2.0 (as stated in its `Cargo.toml`). It is
used through `[patch.crates-io]` in the workspace `Cargo.toml`.

The changes, all in `src/parser.rs` unless noted:

- **Left-associative arithmetic.** `AdditiveExpression` and `MultiplicativeExpression`
  parse left-associatively, as SPARQL 1.1 §17.3 and the grammar's `( '+' … | '-' … )*`
  require. 0.4.7 parsed them right-associatively, so `1 - 2 - 3` evaluated to `2` instead
  of `-4`, `10 - 2 + 3` to `5` instead of `11` and `12 / 2 * 3` to `2` instead of `18`.
  The multiplicative operands are also `UnaryExpression`s, as in the grammar.

- **Case-insensitive boolean keywords.** `BooleanLiteral` matches `true` / `false`
  case-insensitively, like every other keyword except `a` (SPARQL 1.1 §19.8, grammar
  notes). 0.4.7 rejected `TRUE` and `False`. A prefixed name such as `TRUE:x` is still a
  prefixed name.

- **Longest match for `<`.** In `RelationalExpression`, `<` and `<=` are operators only
  where the input from the `<` on does not form an `IRIREF` token (SPARQL 1.1 §19.8: "When
  tokenizing the input and choosing grammar rules, the longest match is chosen"; `IRIREF`
  is rule [139]). 0.4.7 accepted `FILTER(?x<?a&&?b>?y)` as `?x < ?a && ?b > ?y`; it is
  `?x <?a&&?b> ?y`, a syntax error. With whitespace (`?x < ?a && ?b > ?y`), or without a
  closing `>`, nothing changes. The lookahead uses the grammar's `IRIREF` character class
  (plus `\u` / `\U` escapes, §19.2) rather than the parser's more lenient `IRIREF` rule.

- **FILTER scope in OPTIONAL.** Only the FILTERs written directly in an `OPTIONAL` group
  become the `LeftJoin` expression (SPARQL 1.1 §18.2.2.6, with §18.2.2.7 "Filters of
  Group"; SPARQL 1.2 §18.3.2.7–8). A group now yields its pattern and its own filter
  conjunction separately (`GroupGraphPattern_parts`). 0.4.7 looked at the already
  simplified pattern, so `OPTIONAL { { P FILTER(e) } }` (a group whose single element is a
  group with a filter) became `LeftJoin(G, P, e)`, letting `e` see the left-hand side's
  bindings. The simplification of `Join(Z, A)` to `A` (§18.2.2.8) happens after the
  translation, so this must be `LeftJoin(G, Filter(e, P), true)`: the filter only sees
  `P`'s bindings (the W3C test `dawg-optional-filter-005-not-simplified`). In
  `src/algebra.rs`, the SPARQL serialization of such a `LeftJoin` writes the right-hand
  side in its own group, so it parses back to the same algebra.

- **SELECT expressions see earlier aliases.** In `build_select`, a variable bound by
  `(expr AS ?v)` is in scope for the SELECT expressions after it (SPARQL 1.2 §16.1.2: "The
  scoping for (expr AS v) applies immediately"; §18.3.4.4 builds the chain of `Extend`s in
  order). 0.4.7 checked every expression of an aggregating query against the variables
  of the grouped pattern only, so `SELECT (COUNT(?v) AS ?c) (?c + 1 AS ?d)` was rejected
  as using an unbound variable. Using an alias before it is defined, or assigning it
  twice, is still an error.

- **No nested aggregates.** An aggregate whose argument contains an aggregate is a syntax
  error (SPARQL 1.2 §19.7, grammar notes: "The expression argument of an aggregate
  function cannot contain an aggregate function"). 0.4.7 accepted
  `COUNT(COUNT(*))`. The check is in `ParserState::new_aggregation`: an aggregate parsed
  inside the argument has already been replaced by the fresh variable registered for it
  at the current query level, so the argument mentions one of those variables. EXISTS
  patterns inside the argument are not searched, and subqueries have their own level.

- **Triple term subjects.** In expressions, the subject of `<<( … )>>` is an IRI or a
  variable (SPARQL 1.2 §19.7, rule [138] `ExprTripleTermSubject ::= iri | Var`), and in
  VALUES data an IRI (rule [123] `TripleTermDataSubject ::= iri`). 0.4.7 accepted a
  literal or a nested triple term as the subject in an expression
  (`BIND(<<( "l" :q :z )>> AS ?x)`), and enforced the VALUES restriction in a semantic
  action instead of the grammar. Triple terms in graph patterns (rule [120]) are
  unchanged.

- **Custom aggregates with DISTINCT.** In `iriOrFunction`, an IRI is a plain IRI only
  when no `(` follows it. 0.4.7 read the IRI of `ex:agg(DISTINCT ?x)` as a complete
  primary expression once `(DISTINCT ?x)` failed to parse as an argument list, so the
  `Aggregate` rule's custom `DISTINCT` form (registered with
  `with_custom_aggregate_function`) could never match and the query was a syntax error.

Upstream Oxigraph (the development version after 0.4.7) has a rewritten parser that
follows the SPARQL 1.2 grammar rules [123] and [138] and moves an OPTIONAL group's own
FILTERs into the `LeftJoin`; the changes here are written against 0.4.7's rust-peg
grammar rather than ported.

Drop this copy once a spargebra release has these fixes.
