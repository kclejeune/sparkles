# W3C R2RML test cases

These files come from the W3C Working Group Note "R2RML and Direct Mapping Test Cases"
(https://www.w3.org/2001/sw/rdb2rdf/test-cases/), as packaged in the repository
https://github.com/kg-construct/r2rml-test-cases-support at commit
976098802e9bb0e297d196b26a4c2e0b95fd7644 (2022-01-13), fetched on 2026-10-24. Only the
manifest, the database scripts under `databases/` and the `R2RMLTC*` directories with
their mappings and expected outputs are kept. The Direct Mapping cases, the MySQL and
PostgreSQL compose files and the repository's tooling are left out. The files are
unchanged.

The test cases are W3C documents, available under the W3C Software and Document License
(https://www.w3.org/copyright/software-license-2023/). The packaging repository is
distributed under the Apache License 2.0, which is in `LICENSE-APACHE`.

The harness runs each database script in SQLite after removing `CASCADE` from its
`DROP TABLE` statements, and generates IRIs against the base `http://example.com/base/`,
as the cases specify.
