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
  * Every query goes over HTTP as a SPARQL protocol POST with TSV results.
  * hyperfine does 2 warmup runs, then 10 measured runs. It reports the mean ± σ in
    milliseconds, including the `curl` process (about 10 ms).
  * QLever's result cache is cleared in `--prepare`, outside the timed region. Sparkles
    and Fuseki have no result cache.
  * Before timing, the script checks that all three engines return the same number of
    rows for every query.
* **Machine:** Intel Core Ultra X7 358H (16 threads), 30 GB RAM, Linux 6.18, 2026-09-30.

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) |
|---|---:|---:|---:|
| **load** (s) | 0.88 | 4.39 | 1.41 |
| count-all | **11.7 ± 3.4** | 177.8 ± 25.7 | 30.2 ± 1.2 |
| types-grouped | 24.9 ± 1.8 | 75.8 ± 15.3 | **17.9 ± 1.1** |
| star-join | **33.4 ± 1.7** | 40.4 ± 13.4 | 58.5 ± 4.8 |
| two-hop-count | **42.2 ± 13.6** | 317.1 ± 22.4 | 46.5 ± 2.1 |
| range-topk | 22.3 ± 0.6 | 80.6 ± 11.8 | **10.9 ± 4.6** |
| optional-count | **23.4 ± 1.2** | 96.6 ± 13.1 | 26.7 ± 1.9 |
| contains | 64.6 ± 16.2 | **61.7 ± 16.1** | 98.2 ± 9.0 |
| group-avg | 49.2 ± 2.7 | 195.5 ± 14.0 | **39.1 ± 1.7** |
| path-plus | **12.5 ± 0.8** | 20.6 ± 3.2 | 19.3 ± 1.2 |
| distinct-obj | **31.3 ± 1.0** | 105.4 ± 24.0 | 47.4 ± 3.7 |
| export-500k | **175.6 ± 1.7** | 440.4 ± 7.0 | 757.6 ± 13.2 |

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

* **Bulk load.** Sparkles is about 1.6× faster than QLever and 5× faster than TDB2.
  The pipeline is the same shape as QLever's; LZ4 per column and a fully in-memory sort
  at this size are the difference.
* **Where QLever is ahead.** It wins `types-grouped` and `group-avg` through its
  specialised GROUP BY paths (pattern trick, counts straight from index metadata), and
  `range-topk` through block-level filter prefiltering on sorted ids.
* **Tuning with this suite.** Inlining `xsd:decimal` took `range-topk` from 265 ms to
  22 ms, and sharding the decoded-value cache helped string filters.
* **Reasoning.** RDFS materialization over 1.0M triples (2.75M inferred) takes 3.9 s end
  to end. The derived triples are merged into a freshly built index generation rather
  than inserted into the delta one by one, which took 8.9 s.
