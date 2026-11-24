# Benchmarks

This directory holds the methodology pages, result reports, charts and immutable run
catalogs for Sparkles. Start with the [benchmark summary](../BENCHMARKS.md) for headline
results, measurement coverage and the full list of
[where Sparkles loses](../BENCHMARKS.md#where-sparkles-loses). The
[reference setup](reference/README.md) defines the shared hardware, engine versions,
timing rules and answer checks.

## Workloads

| Workload | Scope | Methodology | Results |
|---|---|---|---|
| Synthetic suite | 28 HTTP SPARQL queries at 1.05M and 10.5M triples, warm, cold, changed-store and mixed-load | [Methodology](synthetic/README.md) | [Results](synthetic/RESULTS.md) |
| WatDiv | 20 basic templates (88 instances) at 10.97M triples | [Methodology](watdiv/README.md) | [Results](watdiv/RESULTS.md) |
| SP²Bench | Bibliographic queries at 10M triples under a 12 GiB server cap | [Methodology](sp2bench/README.md) | [Results](sp2bench/RESULTS.md) |
| LUBM and reasoning | Materialized OWL 2 RL query times, Nemo materialization comparisons and query-time reasoning | [Methodology](reasoning/README.md) | [Results](reasoning/RESULTS.md) |
| OWL 2 EL classification | GO, ChEBI lite and ORE 2015 against ELK, Konclude, Whelk, HermiT and ROBOT | [Methodology](el-classification/README.md) | [All results](el-classification/RESULTS.md) and [Horn rules with individuals](el-classification/RESULTS.md#guarded-horn-rules-with-individuals) |
| DBpedia | 50M slice and full 1.24B-triple English DBpedia loads and queries | [Methodology](dbpedia/README.md) | [Results](dbpedia/RESULTS.md) |
| BSBM | Explore and Business Intelligence queries and driver mixes at 10M triples | [Methodology](bsbm/README.md) | [Results](bsbm/RESULTS.md) |
| Full-text search | BM25 `text:query` comparisons against Fuseki jena-text and QLever | [Methodology](full-text/README.md) | [Results](full-text/RESULTS.md) |
| Ontology entity search | Typeahead prefix and typo-tolerant search over GO and ChEBI lite | [Methodology](entity-search/README.md) | [Results](entity-search/RESULTS.md) |
| Vector search | Exact and HNSW approximate recall and latency | [Methodology](vector/README.md) | [Results](vector/RESULTS.md) |
| Spatial queries | Indexed GeoSPARQL queries and GeoSPARQL compliance coverage | [Methodology](spatial/README.md) | [Results](spatial/RESULTS.md) |
| Paths and graph algorithms | Shortest-path, weighted-pair and graph algorithm timings | [Methodology](graph/README.md) | [Results](graph/RESULTS.md) |
| Engine features and overheads | Focused execution, planning, serialization and rewrite-gate baselines | [Methodology](engine/README.md) | [Results](engine/RESULTS.md) |
| Loading and index builds | Bulk loads across suites, compressed inputs and compaction | [Methodology](loading/README.md) | [Results](loading/RESULTS.md) |
| Writes and updates | Durable commits, group commit, mixed read/write load and incremental views | [Methodology](writes/README.md) | [Results](writes/RESULTS.md) |
| Language bindings | Embedded JVM (Jena), Python and Node.js API comparisons | [Methodology](bindings/README.md) | [Results](bindings/RESULTS.md) |
| Natural-language questions | Offline and model-assisted SPARQL generation accuracy and latency | [Methodology](natural-language/README.md) | [Results](natural-language/RESULTS.md) |
| Memory and disk | Server RSS, index sizes, block-cache tradeoffs and spill records | [Methodology](memory/README.md) | [Results](memory/RESULTS.md) |
| Web UI at scale | Browser startup, tree scrolling, Explore graph growth and individuals paging | [Methodology](web-ui-scale/README.md) | [Results](web-ui-scale/RESULTS.md) |

## Evidence and generated reports

`RESULTS.md` files are manually maintained reports unless they explicitly state that they
are generated. Their tables retain original timing statistics, footnotes and
qualifications, and a single report can combine multiple runs under distinct timing
contracts. A raw record supports only the measurements and metadata it contains, so a
claim that needs a missing build revision, host detail, durability setting or answer check
requires a new run.

Each workload keeps a `RUNS.json` catalog. Every run ID is the SHA-256 of the raw record
bytes, keeping multiple runs from the same day distinct. Existing JSON records remain at
their original paths, while new runs belong under `<workload>/runs/<run-id>/record.json`
beside their manifest and raw outputs. Retained runs are immutable; refresh a result by
adding a new run and selecting it explicitly.

The [synthetic record tables](synthetic/RECORDS.md) and charts are generated from selected
common records. Their source hashes, eligibility rules and carried-over measurements are
visible in the report and in [report.json](synthetic/report.json). They supplement the
manual report rather than replacing its mean timings or establishing performance for a
different build.

Run `python3 scripts/bench-docs.py --check` from the repository root to verify source
hashes and generated output. Run it without `--check` to regenerate only the selected
tables and charts. A hash mismatch stops both modes.
