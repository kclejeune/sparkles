# TopBraid SHACL rule tests

These files are the rule tests of TopQuadrant's TopBraid SHACL API, copied without change
from `src/test/resources/sh/tests/rules` of https://github.com/TopQuadrant/shacl at commit
`6687b48bd2c81eda369f224598061f87dce0d425`. They are licensed under the Apache License 2.0,
whose text is in `LICENSE` here.

Each `*.test.ttl` file is a `dash:InferencingTestCase`: its shapes and data are one graph, and
its `dash:expectedResult` nodes list the triples that the rules infer. `person.ttl` is the data
that `person2schema` and `schema2person` import. The test `tests/shacl_af.rs` loads each file
into the default graph, with `person.ttl` where a file imports it, runs the `shacl-af` profile
and compares the inferred graph with the expected triples exactly.
