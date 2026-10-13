# ShEx 2.2 tests

The files here are the ShEx 2.2 tests of the upstream `shexSpec/shexTest` repository,
fetched on 2026-10-10 and copied unchanged in upstream's layout. They are the schemas
that use `EXTENDS` or `ABSTRACT` (`schemas/`), the validation tests on those schemas
(`validation/`, 100 tests, with their data), the negative structure tests for extension
cycles (`negativeStructure/`), and `negativeSyntax/1dotExtends3`. The manifests list only
these entries, with upstream's `@base`, which `tests/shextest.rs` maps to the directory
that holds the manifest. The tests are distributed under the W3C Software and Document
License in `LICENSE`.

`tests/shextest.rs` runs them in its usual groups with the ids prefixed `shex22/`, next to
the suite that `SPARKLES_SHEX_TESTS` names. The `shexr-write` group compares the written
ShExR with the `.ttl` file after unwrapping the `sx:ShapeDecl` nodes that wrap a shape
expression, because upstream writes every declaration wrapped and Sparkles writes the
wrapper only for an abstract declaration or a declaration that is a reference.
