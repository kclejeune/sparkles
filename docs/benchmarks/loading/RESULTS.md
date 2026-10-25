# Loading and index builds results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Loading and index builds

Sparkles builds its index in one parallel pass. Parsing and vocabulary merging run on all
cores, sampled keys divide the partial vocabularies into ranges that merge in parallel,
and each 64-million-quad chunk is remapped once, sorted by SPO, OSP and PSO and written as
compressed runs. SOP, OPS and POS derive from those streams by sorting runs of the first
column, and without named graphs GSPO derives from SPO. Each permutation has its own
writer, so the input is read once.

### Bulk loading

Load times include each engine's own preparation, such as `oxigraph optimize`, and peak
RSS comes from GNU time. Each load ran once. Oxigraph reads N-Triples from standard input,
which keeps blank node labels whole and takes about three times as long as its parallel
`--file` loader.

| Load | sparkles | jena (tdb2.tdbloader) | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M triples, time | **0.47 s** | 4.80 s | 1.36 s | 1.38 s | 4.42 s |
| 1.05M triples, peak RSS | **444 MiB** | 2,166 MiB | 629 MiB | 479 MiB | 826 MiB |
| 10.5M triples, time | **3.67 s** | 45.70 s | 9.35 s | 10.20 s | 18.22 s |
| 10.5M triples, peak RSS | 2,024 MiB | 4,433 MiB | **1,929 MiB** | 3,007 MiB | 2,184 MiB |
| SP²Bench, 10M triples, time | **3.93 s** | 44.88 s | 9.70 s | 8.90 s | 20.49 s |
| LUBM(100) closure, 24.3M triples, time | **7.57 s** | 60.38 s | 18.96 s | 20.03 s | 48.23 s |
| WatDiv, 10.97M triples, time | **3.51 s** | 33.77 s | 9.56 s | 8.92 s | 19.41 s |
| DBpedia, 1.24B lines, time | **480 s** | — | 1,674 s | 2,919 s | — |
| DBpedia, peak RSS | **6,059 MiB** | — | 11,401 MiB | 12,366 MiB | — |

*The synthetic, SP²Bench, LUBM and WatDiv loads of Sparkles were measured on forge on
2026-10-23 at commit 6699f27d, and those of the other engines from 2026-10-18 to
2026-10-20. The DBpedia loads come from the runs of 2026-10-03 to 2026-10-09.*

The DBpedia load is one load of the complete input with the CPUs pinned to the
performance cores. Its peak includes file-cache pages counted in the 14 GiB scope, and
writing the numeric column during the load had no measurable cost. QLever indexed with
8 GB of sort memory and 5 million triples per batch. Jena and Oxigraph were not measured
at this scale.

### Compressed inputs

`sparkles load` read the 1.1 GB N-Triples file of the 10.5M dataset raw and compressed.
Times are medians of 3 runs, CPU is the load's user and system time, and parallelism is
CPU divided by wall time.

| Input | File size | Seconds | CPU (s) | Parallelism |
|---|---:|---:|---:|---:|
| raw | 1,121.3 MB | 3.85 | 28.5 | 7.4 |
| gzip-6 | 81.9 MB | 4.27 | 36.8 | 8.7 |
| zstd-3 | 78.6 MB | 4.08 | 36.2 | 8.7 |
| brotli-5 | 69.0 MB | 4.37 | 36.9 | 8.2 |
| lz4 | 160.0 MB | 3.99 | 36.2 | 9.0 |

Decompression runs as one stream in front of the parallel parser and adds 0.1–0.5 s.

### Index builds

| Build | Time | Result |
|---|---:|---|
| Full-text index, 1.05M triples | **0.39 s** (Fuseki 4.53, QLever 1.88) | 13.2 MB (QLever **11.1**, Fuseki 24.9) |
| Full-text index, 10.5M triples | **6.48 s** (Fuseki 25.98, QLever 19.35) | 122.5 MB (QLever **99.9**, Fuseki 227.8) |
| Full-text document store, every string literal of 10.5M (1,540,012 documents) | 6.17 s with zstd, 6.07 s with LZ4 | 114.1 MB with zstd (the default), 117.3 MB with LZ4 |
| HNSW vector index, 100,000 × 384 dimensions, `M=16`, `efConstruction=128` | 6.43 s | 163 MB of mapped files, 1,044.4 MB peak RSS |
| HNSW vector index, 1,000,000 × 384 dimensions, in-process example | 63.5 s. USearch 2.26.2 in half precision takes 50.0 s. | Recall@10 is 0.963 at `ef=128`. Peak RSS is 12.6 GB. |
| Spatial index, 180,640 geometries | 0.54 s | 13.8 MiB |
| RDFS materialization, 1,001,418 triples to 2,751,996 with inferences | 3.974 s, of which inference takes 2.323 s | — |
| Numeric column added to the DBpedia index by `sparkles vocab-index` | 4.8 s | 20.7 MB |
| Predicate patterns, DBpedia 50M slice, 41.1M quads | 0.36 s of a 26.0 s load | 24.1 MB, 1.6% of the index |

The half-precision graph build takes the vector index build from 14.02 s to 6.43 s at
100,000 vectors and from 139.5 s to 63.5 s at a million, and raises the peak RSS of the
million-vector build from 10.2 GB to 12.6 GB.

### Validation

| Validation of 1.05M triples, medians of 5 | Parallel | Sequential |
|---|---:|---:|
| SHACL, 20 shapes, 48,428 results | 449 ms | 933 ms |
| ShEx, 152,000 shape associations | 1.532 s | 2.192 s |

<a id="partial-compaction-and-the-spatial-index"></a>

### Compaction

Compaction writes a new generation while writes go on. A partial compaction rewrites only
the index blocks a delta touches. This test loads the 10.5M data, copies the store for each
delta and compacts once with partial compaction forced and once without. Each delta is 90%
new `foaf:knows` links and 10% deleted `foaf:age` quads between existing terms.
Concentrated deltas touch a narrow range of people, and spread deltas choose people at
random. The generation has 2,254 blocks over seven permutations.

| Delta | Blocks rewritten | Partial build | Full build |
|---|---:|---:|---:|
| 1,000 concentrated | 102 | 0.112 s | 7.343 s |
| 10,000 concentrated | 107 | 0.584 s | 7.168 s |
| 100,000 concentrated | 117 | 0.234 s | 7.317 s |
| 1,000 spread | 1,201 | 0.572 s | 7.313 s |
| 10,000 spread | 1,223 | 0.597 s | 7.245 s |
| 100,000 spread | 1,223 | 0.669 s | 7.752 s |

Partial compaction is 12–66× faster in these samples, and the writer lock is held for 3.0
to 12.1 ms. Automatic selection chooses partial compaction for every delta here.

With a spatial index, the index's base is built alongside the new generation rather than
at the switch. On 100,000 point features with 500 commits before each compaction and 100
more during its build, the median writer lock at the switch is 4.9 ms without a spatial
index, 64.5 ms when the spatial base is built at the switch, and 25.3 ms when it is built
with the generation, which is the default.

### Document conversion in a child process

The server converts each PDF and Office document in a child process with resource limits
([C18](../../specs/C18-natural-language-questions-and-ingest.md) §7.1.1). Each figure is over 200 conversions of the
5 KB DOCX fixture with the release binary, made by the ignored test
`ingest::isolate::tests::overhead`.

| 5 KB DOCX | Median | 90th percentile |
|---|---:|---:|
| In process | 41 µs | 54 µs |
| On a thread, the fallback path | 85 µs | 184 µs |
| In a child process | 8.5 ms | 9.5 ms |

*Measured on a shared 16-core development laptop, not a quiet machine.*

Starting `/bin/true` took 2 to 4 ms on the same machine, so most of the child's cost is
starting a process. The child adds about 10 ms to each document, which is small next to
the model calls of an ingestion. Each running conversion may also use up to
`--convert-memory`, 2 GiB by default, of address space beside the server.

### Tables through mappings

A YARRRML mapping of a CSV table runs faster than C05's default mapping of the same
table ([spec F19](../../specs/F19-mappings-and-virtual-graphs.md#outcome)). The table has
1,000,000 rows of four columns, and both mappings write the same 4,000,000 triples, one
subject per row and one predicate per column. The figures are medians of three rounds of
the release binary at commit 9bd9feb6, run on atlas on 2026-10-24. Atlas is an Intel
i5-13500 with 20 threads, and other work shared it, so these are not quiet-machine
figures.

| 1,000,000 rows | Wall time | Peak memory |
|---|---:|---:|
| `sparkles csv convert`, the C05 default mapping | 2.81 s | 39 MB |
| `sparkles map run`, the YARRRML mapping | 1.61 s | 41 MB |
| `sparkles load`, the C05 default mapping | 4.12 s | 830 MB |
| `sparkles load --mapping`, the YARRRML mapping | 2.88 s | 834 MB |

C05's default mapping passes each cell through CSVW's datatype and URL template steps,
which the mapping engine does not have. A Parquet file of 10,000,000 rows
and two columns, 227 MB on disk, maps to 20,000,000 triples in 11.6 s with a peak of
46 MB, since the file is read one row group at a time.
