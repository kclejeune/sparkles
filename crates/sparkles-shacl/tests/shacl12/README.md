# SHACL 1.2 list constraint tests

The files under `core/` are copied unchanged from the SHACL 1.2 test suite of the W3C
Data Shapes Working Group, `shacl12-test-suite/tests/core/node/` and
`shacl12-test-suite/tests/core/property/` in the `w3c/data-shapes` repository, fetched on
2026-10-02. They test `sh:memberShape`, `sh:minListLength`, `sh:maxListLength` and
`sh:uniqueMembers`. They are distributed under the W3C Software and Document License
(https://www.w3.org/copyright/software-license-2023/). `manifest.ttl` was written for
Sparkles and includes them.

`tests/w3c_shacl.rs` runs them as the `w3c_shacl12_lists` test. It compares results
without `sh:detail`, which a unit test in `tests/components.rs` checks.

## SHACL 1.2 node expression tests

The files under `node-expr/` are copied unchanged from `shacl12-test-suite/tests/node-expr/`
of the same repository, at commit 056ab61a (2026-10-13). They are the `sht:EvalNodeExpr`
tests of the node expression library (`shnex/`, `shnex-sparql/`) and the `sht:Validate`
tests of `sh:expression` and `sh:nodeByExpression` (`constraints/`). `tests/w3c_shacl.rs`
runs them as `w3c_shacl12_node_expr`. An `sht:EvalNodeExpr` test evaluates the expression
in its file, which is both the data graph and the shapes graph, and compares the output
nodes with `mf:result`, in order unless `sht:ignoreOrder` is true. A numeric literal
matches another of the same datatype with the same value, because the suite writes
`"00"^^xsd:decimal` for the seconds of a time.
