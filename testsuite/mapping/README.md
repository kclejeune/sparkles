# Mapping conformance tests

These are the test cases that `crates/sparkles-core/tests/mapping_conformance.rs` runs
against the mapping engine of spec F19. The harness compares each case's output with the
expected N-Quads by dataset isomorphism, checks that the cases that expect an error fail,
and lists the excluded cases with their reasons.

- `r2rml/` holds the W3C R2RML test cases, run on SQLite when the `mapping-sqlite`
  feature is on. See its README.
- `rml-core/` holds the RML-Core test cases of the W3C Knowledge Graph Construction
  Community Group. See its README.
