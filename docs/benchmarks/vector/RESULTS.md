# Vector search results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Vector search

This Sparkles-only run searches 100,000 clustered 384-dimensional vectors by cosine
distance. Each configuration answered 1,000 top-10 queries after 20 warm-ups over a
persistent HTTP connection with full TSV responses. Recall is measured against Sparkles'
exact search on the same vectors, and no other vector engine was measured. The HNSW graph
is built over a half-precision copy of the vectors.

| Search | Recall@10 against exact | Mean (ms) | p50 (ms) | p99 (ms) |
|---|---:|---:|---:|---:|
| Exact | 1.0000 | 5.39 | 5.44 | 5.99 |
| HNSW `ef=16` | 0.9903 | 0.45 | 0.52 | 0.60 |
| HNSW `ef=32` | 0.9994 | 0.72 | 0.72 | 0.86 |
| HNSW `ef=64` | 0.9994 | 0.46 | 0.35 | 0.94 |
| HNSW `ef=128` | 0.9994 | 0.54 | 0.54 | 0.65 |
| HNSW `ef=256` | 0.9994 | 0.92 | 0.91 | 1.37 |

*Measured on forge with an index built on 2026-10-13.*

Means below about 0.7 ms are not comparable between runs on this host. The same binary
answered the 1,000 `ef=16` queries in 0.29 ms at the median in one pass and 0.60 ms in the
next, and cheap settings were often slower than `ef=64`. This comes from the client and the
processor's idle states rather than from the search. These figures describe this synthetic
distribution, not all vectors.
