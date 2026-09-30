# Benchmarks

Reproduce with `scripts/bench.sh [N_PEOPLE] [WORKDIR]` (needs `hyperfine`; Jena, Fuseki and
QLever are pulled from nixpkgs if not on `PATH`).

* **Data:** `scripts/gen-data.py 100000` gives 1,052,807 triples: people, organisations
  and documents, typed literals (integers, decimals, dates), `foaf:knows` and
  citation graphs, and a small OWL class hierarchy.
* **Engines:**
  * Sparkles, `target/release/sparkles serve`.
  * Apache Jena: `tdb2.tdbloader` 6.2.0 for the load, served by Fuseki 5.1.0 (the
    version nixpkgs packages) with `-Xmx8G`.
  * QLever 0.5.48 (`qlever-index` / `qlever-server`, 8 GB memory limit).
* **Method:**
  * Sparkles runs with its subtree result cache disabled (`--result-cache-mb 0`), and
    QLever's cache is cleared before every run, so both measure execution.
  * Every query goes over HTTP as a SPARQL protocol POST with TSV results.
  * hyperfine does 2 warmup runs, then 10 measured runs. It reports the mean ± σ in
    milliseconds, including the `curl` process (about 10 ms).
  * QLever's result cache is cleared in `--prepare`, outside the timed region. Sparkles
    and Fuseki have no result cache.
  * Before timing, the script checks that all three engines return the same number of
    rows for every query.
* **Machine:** Intel Core Ultra X7 358H (16 threads), 30 GB RAM, Linux 6.18, 2026-09-30
  (re-run after the early-termination, GROUP BY/COUNT and expression-evaluation work).

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) |
|---|---:|---:|---:|
| **load** (s) | 0.81 | 4.34 | 1.54 |
| count-all | **15.0 ± 1.1** | 169.8 ± 37.8 | 27.1 ± 6.1 |
| types-grouped | **9.5 ± 4.2** | 79.9 ± 16.1 | 17.2 ± 4.1 |
| star-join | **37.9 ± 1.6** | 47.9 ± 13.0 | 61.0 ± 1.4 |
| two-hop-count | **36.4 ± 1.4** | 322.8 ± 24.3 | 46.1 ± 2.0 |
| range-topk | 26.5 ± 1.5 | 82.4 ± 12.6 | **16.5 ± 4.4** |
| optional-count | **22.0 ± 0.7** | 88.1 ± 20.1 | 27.6 ± 2.0 |
| contains | **44.8 ± 5.1** | 63.7 ± 8.1 | 85.4 ± 16.9 |
| group-avg | 46.8 ± 2.3 | 193.3 ± 9.2 | **36.4 ± 3.5** |
| path-plus | **13.0 ± 1.1** | 20.1 ± 1.8 | 19.0 ± 0.9 |
| distinct-obj | **35.4 ± 1.3** | 110.4 ± 22.9 | 50.2 ± 7.5 |
| export-500k | **178.2 ± 3.0** | 443.4 ± 17.4 | 751.1 ± 12.2 |

Queries (prefixes omitted):

| name | query |
|---|---|
| count-all | `SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }` |
| types-grouped | `SELECT ?t (COUNT(?s) AS ?c) WHERE { ?s a ?t } GROUP BY ?t ORDER BY DESC(?c)` |
| star-join | `SELECT ?p ?n ?a ?o WHERE { ?p a ex:Researcher ; foaf:name ?n ; foaf:age ?a ; ex:worksFor ?o . ?o ex:city "Kyoto" }` |
| two-hop-count | `SELECT (COUNT(*) AS ?n) WHERE { ?a foaf:knows ?b . ?b foaf:knows ?c }` |
| range-topk | `SELECT ?p ?s WHERE { ?p ex:salary ?s FILTER(?s > 150000) } ORDER BY DESC(?s) LIMIT 10` |
| optional-count | `SELECT (COUNT(*) AS ?c) WHERE { ?p a ex:Student OPTIONAL { ?p ex:worksFor ?o } }` |
| contains | `SELECT (COUNT(*) AS ?c) WHERE { ?p foaf:name ?n FILTER(CONTAINS(?n, "Ada")) }` |
| group-avg | `SELECT ?o (AVG(?a) AS ?avg) (COUNT(?p) AS ?n) WHERE { ?p ex:worksFor ?o ; foaf:age ?a } GROUP BY ?o ORDER BY DESC(?n) ?o LIMIT 10` |
| path-plus | `SELECT (COUNT(*) AS ?c) WHERE { <http://example.org/doc/1> ex:cites+ ?d }` |
| distinct-obj | `SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s foaf:knows ?o }` |
| export-500k | `SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 500000` |

## Observations

* **Bulk load.** Sparkles is about 1.9× faster than QLever and 5× faster than TDB2.
* **Query wins.** Sparkles is fastest on 9 of the 11 queries.
  * `types-grouped` counts groups straight from index runs.
  * `contains` batch-decodes the column once and evaluates without contention.
  * `two-hop-count` counts join pairs without materializing them.
* **Where QLever is ahead.** It still wins `range-topk` and `group-avg`: its filters
  prefilter blocks on sorted ids, and its GROUP BY paths are more specialised.
* **Earlier tuning.** Inlining `xsd:decimal` took `range-topk` from 265 ms to 22 ms.
* **Reasoning.** RDFS materialization over 1.0M triples (2.75M inferred) takes 3.9 s end
  to end, because derived triples go into a freshly built index generation.

## Scale check: 10.5M triples

`scripts/gen-data.py 1000000` gives 10,527,323 triples (1.12 GB of N-Triples).

| bulk load (hyperfine, 1 run) | wall time | CPU (user) | index size |
|---|---:|---:|---:|
| **Sparkles** | **6.1 s** | 26.7 s | 286 MB |
| QLever 0.5.48 | 9.1 s | 73.8 s | 277 MB |
| Jena TDB2 6.2.0 (`tdb2.tdbloader`) | 42.6 s | 89.6 s | 1.4 GB |

Sparkles' peak RSS during the load was 1.9 GB. The size includes all 7 permutations
plus the vocabulary. Sample query times at this scale (CLI, in-process, no result cache):

| query | exec |
|---|---:|
| two-hop `foaf:knows` join count (6.2M results) | 410 ms |
| `GROUP BY ?o` + AVG/COUNT over `ex:worksFor` / `foaf:age` | 227 ms |
| `FILTER(CONTAINS(?n, "Ada"))` over 1M names | 208 ms |
| salary range filter + top-3 | 61 ms |
| 4-pattern star join + date filter + city constant | 84 ms |
| `ASK { ?s ?p ?o }` / `SELECT * … LIMIT 100` (early termination) | 1.9 ms / 1.8 ms |
