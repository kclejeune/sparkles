# Apache Jena's Fuseki configurations

These files are copied unchanged from Apache Jena:

- `examples/` from `jena-fuseki2/examples`, Fuseki's example configurations;
- `testing/Access/` from `jena-fuseki2/jena-fuseki-main/testing/Access`, the access
  control tests' configurations and password file;
- `testing/Shiro/` from `jena-fuseki2/jena-fuseki-main/testing/Shiro`;
- `testing/FusekiBuild/` from `jena-fuseki2/jena-fuseki-main/testing/FusekiBuild`;
- `testing/GeoAssembler/` from `jena-integration-tests/src/test/files/GeoAssembler`.

The unit tests in `crates/sparkles-server/src/fuseki_config/tests.rs` and
`crates/sparkles-server/tests/cli_config.rs` convert them with
`sparkles config import fuseki` (spec G08).

Apache Jena is distributed under the Apache License 2.0, which is in `LICENSE-APACHE`.
Jena's `NOTICE` file is included as the license asks.
