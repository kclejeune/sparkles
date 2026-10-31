# CSVW test suite

These files come from the `tests` directory of the W3C CSV on the Web repository,
https://github.com/w3c/csvw, at commit b3f461db0e86a68c019bc1f912e86f3555907e34
(2026-05-22). The directory holds `manifest-rdf.ttl`, which lists the 270 tests that
convert tabular data to RDF, and every CSV, TSV, JSON metadata and Turtle file that
those tests name. The files are unchanged. The JSON and nonnormative manifests, the
tests' index page and the server configuration of the suite are left out.

`crates/sparkles-core/tests/csvw_conformance.rs` runs the tests with `mise run test:csvw`
and as part of `mbx test`. It plays the part of the processor that locates metadata, so
it applies the test's user metadata, then the `Link` header the test names, then
`{url}-metadata.json`, `csv-metadata.json`, `{url}.json` and `csvm.json`, and takes the
first document that exists and describes the file. It reads the metadata and the cells
with the lenient rules of CSVW, which warn about a problem and use the default. It
converts in minimal mode, or in standard mode unless the test says `csvt:minimal`,
compares the output with the expected Turtle by dataset isomorphism, and requires the
negative tests to fail. `CSVW_ONLY=test001,test002` runs some tests and prints their
output. Tests that are excluded are listed in the harness with their reasons, and the
harness fails when an excluded test passes.

The repository's documents are licensed under the W3C Document License, which is
described in `LICENSE.md`. The test files themselves are published under the W3C Test
Suite License and the W3C 3-clause BSD License, as the suite's index page says.
