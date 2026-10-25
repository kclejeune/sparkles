# Benchmarks

Sparkles benchmarks cover query latency, loading, durable writes, memory and language
bindings. Start with the [benchmark summary](../BENCHMARKS.md) for representative
results and tradeoffs. Each workload below has a methodology page and a result report.
The summary also records measurement coverage and the full loss table. The
[reference setup](reference/README.md) explains timing and answer rules.

| Workload | Methodology | Results |
|---|---|---|
| Synthetic suite | [Read](synthetic/README.md) | [Read](synthetic/RESULTS.md) |
| WatDiv | [Read](watdiv/README.md) | [Read](watdiv/RESULTS.md) |
| SP²Bench | [Read](sp2bench/README.md) | [Read](sp2bench/RESULTS.md) |
| LUBM and reasoning | [Read](reasoning/README.md) | [Read](reasoning/RESULTS.md) |
| DBpedia | [Read](dbpedia/README.md) | [Read](dbpedia/RESULTS.md) |
| BSBM | [Read](bsbm/README.md) | [Read](bsbm/RESULTS.md) |
| Full-text search | [Read](full-text/README.md) | [Read](full-text/RESULTS.md) |
| Vector search | [Read](vector/README.md) | [Read](vector/RESULTS.md) |
| Spatial queries | [Read](spatial/README.md) | [Read](spatial/RESULTS.md) |
| Paths and graph algorithms | [Read](graph/README.md) | [Read](graph/RESULTS.md) |
| Engine features and overheads | [Read](engine/README.md) | [Read](engine/RESULTS.md) |
| Loading and index builds | [Read](loading/README.md) | [Read](loading/RESULTS.md) |
| Writes and updates | [Read](writes/README.md) | [Read](writes/RESULTS.md) |
| Language bindings | [Read](bindings/README.md) | [Read](bindings/RESULTS.md) |
| Natural-language questions | [Read](natural-language/README.md) | [Read](natural-language/RESULTS.md) |
| Memory and disk | [Read](memory/README.md) | [Read](memory/RESULTS.md) |

## Evidence and generated reports

RESULTS.md files contain imported, manually maintained reports unless they explicitly
say they are generated. Their tables retain the original timing statistics, footnotes
and qualifications. A report can mix multiple runs and timing contracts. It must not be
reconstructed by selecting the newest JSON file alone.

Each workload has a RUNS.json catalog. A run ID is the SHA-256 of the raw record bytes,
so several runs on the same day remain distinct. Existing JSON records retain their
original paths. New runs belong in `<workload>/runs/<run-id>/record.json`, with their
original manifest and raw outputs alongside them. Never edit a retained run to refresh a
result. Add a new run and select it explicitly.

The [synthetic record tables](synthetic/RECORDS.md) and charts are generated from selected
common records. Their source hashes, eligibility rules and carried-over measurements are
visible in the report and its selection manifest. They supplement the manual report and
do not replace its mean timings or establish current performance of a different build.

Run `python3 scripts/bench-docs.py --check` from the repository root to verify source
hashes and generated output. Run it without `--check` to update only the selected
generated tables and charts. A hash mismatch stops both modes.
