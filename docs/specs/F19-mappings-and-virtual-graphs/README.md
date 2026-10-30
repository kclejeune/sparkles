# F19: Mapping languages and virtual graphs

> **Status:** implemented in part (most of Phase 1)

Materialized mappings over the implemented CSV, TSV, JSON, local Parquet and SQLite sources are available. Remaining Phase 1 source and administration gaps are tracked under items 230 to 239 and 404, and virtual graphs remain under item 43.

Native Rust direct mapping runs accept structured write controls through
`Dataset::run_mapping_with`, including conversion and the final atomic load. The
existing direct method and conversion option fields remain compatible. Stored runs
apply the cancellation sources and deadline of their write options to conversion too.

## Documents

* [Design and acceptance](SPEC.md).
* [Delivery record](OUTCOME.md).
* [Provenance](PROVENANCE.md).

## User guides

[Mappings](../../API.md#stored-mappings),
[usage](../../USAGE.md#mappings-r2rml-rml-and-yarrrml) and
[features](../../storage/README.md#storage-and-indexes).

The [roadmap](../../ROADMAP.md#completion-paths-for-partially-complete-specs)
tracks remaining work. The delivery record preserves the detailed phase history.
