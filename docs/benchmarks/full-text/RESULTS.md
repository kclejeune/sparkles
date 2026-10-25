# Full-text search results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Comparison summary

Sparkles is fastest on all seven queries at 1.05M and on six of
  seven at 10.5M, against Fuseki with jena-text and QLever.

## Full-text search

The comparison uses Sparkles' Tantivy-backed `text:query`, Fuseki's Lucene-backed
jena-text and QLever's `ql:contains-word`. Sparkles and Jena index `foaf:name`,
`ex:title` and `rdfs:label`, with English stemming of titles. QLever indexes all literals
and joins on predicates, searches the indexed form for the stemmed case, and does not
highlight. Hit sets and counts agree, except on `common-top10` and `highlight`, where
the record compares only result counts. Scores and the order of tied hits are not
compared. Times are TSV HTTP means ± standard deviation in ms, over ten runs at 1.05M and
five runs after one warm-up at 10.5M. `scripts/bench-text.sh` reproduces the run.

| query | sparkles 1.05M | jena-fuseki 1.05M | qlever 1.05M | sparkles 10.5M | jena-fuseki 10.5M | qlever 10.5M |
|---|---:|---:|---:|---:|---:|---:|
| rare-top10 | **4.0 ± 0.2** | 13.1 ± 1.0 | 6.8 ± 0.4 | **5.8 ± 0.4** | 15.0 ± 1.9 | 7.8 ± 0.5 |
| common-top10 | **4.1 ± 0.2** | 11.1 ± 1.2 | 7.4 ± 0.6 | **8.1 ± 0.3** | 14.9 ± 2.8 | 17.5 ± 2.7 |
| common-count | **4.8 ± 0.2** | 54.7 ± 3.2 | 7.2 ± 0.5 | 14.8 ± 0.6 | 406.9 ± 4.3 | **13.5 ± 1.1** |
| text-join | **5.6 ± 0.2** | 70.7 ± 5.2 | 11.9 ± 1.2 | **21.4 ± 0.6** | 585.4 ± 5.1 | 46.6 ± 7.6 |
| conjunction | **4.1 ± 0.2** | 15.0 ± 1.8 | 7.2 ± 0.3 | **8.2 ± 0.2** | 80.7 ± 0.8 | 14.3 ± 1.0 |
| highlight | **11.4 ± 0.3** | 103.7 ± 10.1 | 15.0 ± 1.9 | **70.6 ± 0.5** | 841.8 ± 46.2 | 94.8 ± 6.5 |
| stemmed | **3.9 ± 0.2** | 11.1 ± 1.3 | 6.2 ± 0.3 | **5.4 ± 0.3** | 53.5 ± 0.9 | 18.6 ± 2.6 |

*At 1.05M, Sparkles was measured on forge on 2026-10-23 at commit 6699f27d and the other
engines on 2026-10-20, each under a 12 GiB limit with clients pinned to CPU 11. The
[full-text record](../2026-10-23-text-1.05m-forge.json) has the samples and answer
fingerprints. The 10.5M figures come from the runs of 2026-10-03 to 2026-10-09 without that
isolation.*

QLever's lead on `common-count` at 10.5M, which counts 49,837 hits, is within 10%. The
highlight form returns plain literals in QLever and marked words in Sparkles and Jena.
Index build times and sizes are under [Index builds](../loading/RESULTS.md#index-builds).

<a id="full-text-index-and-observability-105m-triples"></a>

### Full-text index and observability

The full-text index commits staged documents when a text query needs them, on a tick of
about one second, or when about 16,000 changes are staged. It syncs to disk at most once a
second and replays the WAL after a crash, so a write does not wait for the index.

Access logging and Prometheus metrics are on by default. With both on, the canonical 28
queries at 10.5M summed to 1,027 ms of means, against 1,031 ms with
`--no-access-log --no-metrics`. Under 16 clients the server answered 225 star joins per
second with them on and 239 with them off, in a single pair of runs.
