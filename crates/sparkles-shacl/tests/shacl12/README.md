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
