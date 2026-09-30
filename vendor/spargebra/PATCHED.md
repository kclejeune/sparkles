# spargebra 0.4.7, patched

This is [spargebra](https://crates.io/crates/spargebra) 0.4.7 from the Oxigraph project
(<https://github.com/oxigraph/oxigraph>, `lib/spargebra`), copyright the Oxigraph
developers, dual-licensed under MIT or Apache-2.0 (as stated in its `Cargo.toml`). It is
used through `[patch.crates-io]` in the workspace `Cargo.toml`.

The one change, in `src/parser.rs`: `AdditiveExpression` and `MultiplicativeExpression`
parse left-associatively, as SPARQL 1.1 §17.3 and the grammar's `( '+' … | '-' … )*`
require. 0.4.7 parsed them right-associatively, so `1 - 2 - 3` evaluated to `2` instead
of `-4`, `10 - 2 + 3` to `5` instead of `11` and `12 / 2 * 3` to `2` instead of `18`.
The multiplicative operands are also `UnaryExpression`s, as in the grammar.

Drop this copy once a spargebra release parses these left-associatively.
