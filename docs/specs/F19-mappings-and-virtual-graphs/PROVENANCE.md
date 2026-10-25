# Mapping languages and virtual graphs

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

See the [spec landing page](README.md), [design](SPEC.md) and
[delivery record](OUTCOME.md) for their respective contracts.

- **Spec:** [`F19-mappings-and-virtual-graphs.md`](README.md),
  written on 2026-10-12 independently from:
  - the W3C Recommendation "R2RML: RDB to RDF Mapping Language" (2012) and the W3C note
    "R2RML and Direct Mapping Test Cases" (2012), cited from working knowledge;
  - the RML specification at rml.io and the RML-Core module drafts of the W3C Knowledge
    Graph Construction Community Group, at the level of their vocabularies, and Dimou et
    al., "RML" (LDOW 2014);
  - the YARRRML specification and Heyvaert et al., "Declarative Rules for Linked Data
    Generation at your Fingertips!" (ESWC 2018 Satellite Events);
  - the W3C CSVW Recommendations (2015), for standard mode and key checks;
  - Calvanese et al., "Ontop: Answering SPARQL queries over relational databases"
    (Semantic Web Journal, 2017), and Rodríguez-Muro and Rezk, "Efficient SPARQL-to-SQL
    with R2RML mappings" (Journal of Web Semantics, 2015), for unfolding. Ontop
    (Apache-2.0) is described from its papers and documentation; its code was not read;
  - Lanti et al., "The NPD Benchmark" (EDBT 2015) and Bizer and Schultz, "The Berlin
    SPARQL Benchmark" (IJSWIS, 2009), for the measurements;
  - RFC 9535 (JSONPath), the Apache Arrow, Apache Parquet and Apache Iceberg
    specifications, and Tarql's public documentation (Apache-2.0);
  - the Sparkles code and specs C01, C05, C12, C12b, C16, C19, C20, C21, F05, F06 and G10.
- **Implementation:** not built. Planned dependencies, each behind a cargo feature and
  none in the library's default build:
  - `tokio-postgres` (MIT OR Apache-2.0), with rustls, for PostgreSQL (`mapping-postgres`);
  - `rusqlite` (MIT) with the bundled SQLite (public domain) (`mapping-sqlite`);
  - `duckdb` (MIT) with the bundled DuckDB (MIT), a C++ build, off in the server
    (`mapping-duckdb`);
  - `parquet` and `arrow` (Apache-2.0) (`mapping-parquet`), and `object_store`
    (Apache-2.0, already used by `sparkles-backup`) for object stores;
  - `serde_json_path` (MIT) or another RFC 9535 implementation, after review;
  - a YAML 1.2 parser such as `yaml-rust2` (MIT OR Apache-2.0), after review;
  - Phase 2: `datafusion` (Apache-2.0) and `iceberg` with its DataFusion integration
    (iceberg-rust, Apache-2.0) (`virtual`);
  - test data: the W3C R2RML and CSVW test suites (W3C test suite licence) and the RML
    test cases, read from pinned copies and never linked into a binary; Ontop as an
    external benchmark baseline, run as a separate process.
- **Rejected** (spec §14):
  - translating R2RML and RML into CSVW or CONSTRUCT templates;
  - embedding an existing Java or Python mapping engine;
  - Ontop behind SERVICE as the virtual graph engine;
  - virtual graphs in the default graph;
  - joins on concatenated IRI strings;
  - rewriting `rr:sqlQuery` text with an SQL parser;
  - DuckDB for Parquet and Iceberg;
  - writable virtual graphs.
