# DBpedia results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Comparison summary

At the 50M slice, among the 27 queries with complete, agreeing content
  checks across all five engines, Sparkles has the lowest median on 25 warm and 27 cold.
  Fluree is within 5% ahead on two small warm queries. After the
  [decimal arithmetic change](#decimal-arithmetic-and-inline-dates), `births-by-decade`
  takes 7.1 ms warm and 37.8 ms cold in the comparative tables, against QLever's 13.5 and
  43.8 ms. The complete-HTTP timer of the
  [cursor assessment](#cursor-expression-reuse-controls) gives 3.9 and 27.5 ms against
  17.2 and 29.3 ms, so the cold result is within 10%.

At 1.24 billion triples, Sparkles loads English DBpedia in 480 s, against
  QLever's 1,674 s and Fluree's 2,919 s. The comparative tables report lower times than
  QLever on 29 warm queries with agreeing qualified answer checks and on 29 of 31 cold
  queries. The full-data `births-by-decade` guards are internal A/B comparisons and do
  not establish a peer ranking at that scale.

## DBpedia

### DBpedia at the 50M slice

The subject-hash slice of DBpedia 2022.12.01 contains 50,143,058 input statements.
Sparkles and QLever retain 41,127,700 distinct triples. Jena used the TDB2 6.2.0 bulk
loader and Fuseki 5.1.0 for queries. Warm times are medians of five requests after one
warm-up, including curl. Cold times are one request after a server restart and file-cache
eviction. Both use a 600 second request limit. Answer checks ran separately before warm
timing with a 300 second limit, and their result also qualifies the cold comparison.
`mise run bench:billion` reproduces the run.

Of the 27 queries with complete, agreeing content checks across all five engines,
Sparkles has the lowest median on 25 warm and 27 cold. The `births-by-decade` row
comes from a later paired run of Sparkles and QLever after the
[decimal arithmetic change](#decimal-arithmetic-and-inline-dates), and its footnote
gives the details. Fluree's
warm medians are lower on `inlinks-count-2`, 3.7 against 3.9 ms, and on `place-union-1`,
3.9 against 4.0 ms, both within 10%. This is agreement between engines, rather than an
independent oracle. Several small-query medians and throughput rates are within 10%.
`label-regex` is bimodal in Sparkles, near 0.65 ms or near 1.4 ms of server time in a
given server process.

| query | Sparkles warm (ms) | Fuseki warm (ms) | QLever warm (ms) | Fluree warm (ms) | Oxigraph warm (ms) |
|---|---:|---:|---:|---:|---:|
| abstract-contains | **6.8** | 58.7 | 20.8 | 9.1 | 49.0 |
| births-by-decade ¶ | **7.1** | 225.7 | 13.5 | 166.7 | 433.5 |
| category-people-1 | **3.8** | 6.9 | 12.1 | 4.5 | 4.7 |
| category-people-2 | **3.7** | 7.9 | 13.9 | 4.2 | 4.4 |
| category-tree | **3.6** | 7.5 | 7.2 | 3.7 | 4.0 |
| class-counts | **3.8** | 9639.2 | 24.7 | 297.0 | 1081.3 |
| costar-birthplace | **5.8** | 92.9 | 21.7 | 21.5 | 158.8 |
| count-all | 3.6 | 6585.3 † | 137.6 | 3.8 | 9778.3 † |
| country-population | **3.6** | 7.0 | 7.9 | 5.0 | 5.9 |
| entity-facts-1 | **3.5** | 7.6 | 6.0 | 3.8 | 4.6 |
| entity-facts-2 | **3.7** | 8.6 | 6.0 | 4.0 | 5.6 |
| entity-summary-1 | **3.8** | 8.0 | 6.5 | 3.9 | 4.3 |
| entity-summary-2 | **3.5** | 7.7 | 6.6 | 4.0 | 4.3 |
| export-1m § | 282.7 | 2579.0 | 1597.1 | 579.6 | 5304.6 |
| film-director-optional | **4.1** | 41.5 | 14.9 | 15.2 | 54.1 |
| geo-box | **6.5** | 48.9 | 13.5 | 40.8 | 36.6 |
| inlinks-count-1 | **3.9** | 11.2 | 4.4 | **3.9** | 5.0 |
| inlinks-count-2 | 3.9 | 9.6 | 4.8 | **3.7** | 4.8 |
| label-regex | **4.3** | 12.8 | 6.2 | 5.6 | 15.6 |
| outlink-classes-1 | **3.9** | 7.9 | 12.5 | 5.7 | 5.5 |
| outlink-classes-2 | **3.7** | 7.4 | 10.5 | 5.2 | 4.4 |
| people-no-birthdate | **5.4** | 186.9 | 8.4 | 20.3 | 397.8 |
| place-births-1 | **4.1** | 6.2 | 12.2 | 4.7 | 5.9 |
| place-births-2 | **3.8** | 7.6 | 11.6 | 4.7 | 6.4 |
| place-union-1 | 4.0 | 7.3 | 6.0 | **3.9** | 4.6 |
| place-union-2 | **3.7** | 7.3 | 5.6 | 3.8 | 4.3 |
| predicate-counts | 8.5 | 12505.5 † | 250.7 | 2480.6 | 11289.1 † |
| redirect-target-1 | **3.8** | 6.0 | 17.2 | 4.1 | 4.2 |
| redirect-target-2 | **3.9** | 6.7 | 12.5 | 4.2 | 4.3 |
| sameas-subjects | **3.6** | 9.4 | 5.2 | 4.0 | 5.0 |
| top-linked | **61.0** | 33712.5 | 471.6 | 5369.4 | 282458.5 ‡ |

| query | Sparkles cold (ms) | Fuseki cold (ms) | QLever cold (ms) | Fluree cold (ms) | Oxigraph cold (ms) |
|---|---:|---:|---:|---:|---:|
| abstract-contains | **57.5** | 1544.0 | 259.7 | 146.0 | 1240.8 |
| births-by-decade ¶ | **37.8** | 2883.1 | 43.8 | 246.8 | 4380.9 |
| category-people-1 | **12.8** | 71.7 | 45.8 | 130.2 | 50.6 |
| category-people-2 | **17.8** | 70.9 | 48.5 | 121.1 | 33.0 |
| category-tree | **6.9** | 58.4 | 29.5 | 48.3 | 11.0 |
| class-counts | **7.9** | 17438.0 | 58.7 | 389.7 | 1298.7 |
| costar-birthplace | **22.5** | 1550.2 | 49.3 | 91.9 | 1216.0 |
| count-all | 4.1 | 4857.7 † | 265.8 | 36.0 | 10994.9 † |
| country-population | **11.7** | 115.9 | 36.5 | 222.9 | 73.4 |
| entity-facts-1 | **8.2** | 66.3 | 28.1 | 78.4 | 41.5 |
| entity-facts-2 | **10.8** | 85.5 | 32.8 | 91.1 | 48.5 |
| entity-summary-1 | **9.5** | 49.0 | 27.5 | 43.6 | 17.1 |
| entity-summary-2 | **10.7** | 68.9 | 30.6 | 35.4 | 18.1 |
| export-1m § | 543.0 | 20355.4 | 2159.1 | 1259.5 | 24409.3 |
| film-director-optional | **12.2** | 1504.7 | 42.0 | 750.8 | 935.8 |
| geo-box | **31.4** | 502.3 | 42.8 | 708.1 | 223.2 |
| inlinks-count-1 | **6.5** | 426.1 | 23.3 | 61.2 | 15.5 |
| inlinks-count-2 | **6.8** | 371.7 | 24.2 | 44.2 | 17.8 |
| label-regex | **19.7** | 604.3 | 32.4 | 173.1 | 262.8 |
| outlink-classes-1 | **21.6** | 128.8 | 40.9 | 101.3 | 73.2 |
| outlink-classes-2 | **18.1** | 92.0 | 35.3 | 85.0 | 50.5 |
| people-no-birthdate | **17.3** | 1118.4 | 28.3 | 86.4 | 4383.6 |
| place-births-1 | **15.2** | 109.1 | 60.4 | 180.4 | 73.4 |
| place-births-2 | **19.0** | 150.3 | 65.5 | 186.8 | 88.3 |
| place-union-1 | **12.9** | 80.2 | 43.5 | 97.9 | 47.5 |
| place-union-2 | **12.5** | 71.2 | 39.9 | 103.8 | 38.5 |
| predicate-counts | 19.0 | 11164.3 † | 294.8 | 3475.5 | 14950.4 † |
| redirect-target-1 | **9.3** | 60.8 | 48.1 | 56.3 | 18.1 |
| redirect-target-2 | **12.6** | 63.9 | 41.6 | 58.6 | 21.9 |
| sameas-subjects | **6.3** | 325.7 | 25.0 | 67.7 | 17.5 |
| top-linked | **135.0** | 48716.9 | 576.4 | 6656.7 | 301481.4 ‡ |

*Sparkles was loaded and measured on forge on 2026-10-23 at commit 6699f27d, and the other
engines on 2026-10-20 over the stores of 2026-10-19. The
[result record](../2026-10-23-dbpedia-50m-forge.json) has every timing result,
answer fingerprint and the SHA-256 and size of each of the 38 input files.*

† On `count-all` and `predicate-counts`, Sparkles, QLever and Fluree have one
value fingerprint, while Jena and Oxigraph have another, and the collector marks the
smaller group as `wrong-answer`. Sparkles and QLever count 41,127,700 triples and Jena
and Oxigraph 41,127,697, two fewer for `geo:lat` and one fewer for `geo:long`. The slice
has exactly three such pairs, a subject with two `xsd:float` values of one predicate
whose lexical forms differ but parse to the same number, such as `"37.89527777777778"`
and `"37.895278"`. Jena and Oxigraph store a float by its value and so merge each pair,
while RDF 1.1 identifies a literal by its lexical form and datatype, as Sparkles, QLever
and Fluree do here. These rows stay out of the ranking because the engines answer
different questions, not because one answer is wrong.

‡ Oxigraph's `top-linked` answer check did not complete within 300 seconds. Its five
warm timing requests and its cold request all completed within the separate 600
second limit, but the returned values remain unverified. Its timing is shown
without ranking it against the engines whose answers agreed.

¶ The Sparkles and QLever cells of `births-by-decade` come from a paired run on forge of
`1acdc38a` and QLever 0.5.48 over freshly loaded stores of the same slice. The other
three cells keep the run above. Each engine's warm cell is the median of 50 timed
requests from five passes of ten requests after two warm-ups, and its cold cell is the
median of five restarts with the store evicted from the page cache. Both engines return the
same 80 groups with one value fingerprint. One of the five Sparkles passes ran at about
24 ms for all ten requests, so its 95th percentile is 25.6 ms. The earlier cells were
54.4 and 71.4 ms for Sparkles and 11.1 and 41.0 ms for QLever. The
[run record](runs/2026-10-31-births-by-decade-forge/record.json) holds every sample.

§ `export-1m` checks the number of returned rows only. All five returned one million
rows. It is excluded from the content-verified ranking above.

The throughput runs send 64 separate curl requests through 16 workers, with one
warm-up batch and three measured batches. The rates below use the median batch
time and include client process overhead.

| query | Sparkles (q/s) | Fuseki (q/s) | QLever (q/s) | Fluree (q/s) | Oxigraph (q/s) |
|---|---:|---:|---:|---:|---:|
| entity-facts-1 | 272.5 | 240.9 | 261.3 | 271.6 | 269.7 |
| place-births-1 | 269.6 | 244.3 | 244.5 | 267.3 | 268.7 |

| load measurement | Sparkles | TDB2 | QLever | Fluree | Oxigraph |
|---|---:|---:|---:|---:|---:|
| Time in seconds | 24.55 | 579.35 | 49.46 | 106.14 | 83.27 |
| Peak RSS in MiB | 3229 | 6411 | 7134 | 5057 | 3550 |
| Index size in GiB | 1.41 | 4.25 | 1.57 | 3.48 | 5.96 |

### DBpedia at 1.24 billion triples

This comparison runs English DBpedia, release 2022.12.01, against QLever and Fluree. The 38 files of
`scripts/bench-billion/dbpedia-2022.12.tsv` hold 1,237,418,025 lines of N-Triples, 16 GB
compressed with zstd, which are 1,014,683,275 distinct triples. Sparkles' vocabulary has
205,941,262 terms. The run uses `mise run bench:billion full` with `RUNS=3`,
`TIMEOUT=300` and a query memory budget of 8 GB for both engines. Each step runs alone in
a systemd scope capped at 14 GiB with no swap, so the index of 28.7 GiB does not fit in
memory. Each query runs once to warm up and then three times. Cold runs restart the server
and evict its files from the page cache before each query, then measure it once.

Fluree runs with the settings of the 10.5M runs, an 8 GiB import budget and a 4 GiB cache.
Its import fails on a file larger than its chunk size, because it finds no statement
boundary where it cuts the file, so the harness splits the input into 370 pieces of at
most 512 MB first, which is not part of the load time. Fluree's server reached the 14 GiB
limit during the warm runs, so only four warm queries have Fluree timings, while all 31
cold queries completed. Of the 29 queries Fluree answered warm, its answers agreed with
the other engines on 27. On `births-by-decade` it returned 165 rows where Sparkles and
QLever returned 171, and on `entity-facts-2` it returned the same values as different RDF
terms.

| query | sparkles (ms) | qlever (ms) | fluree (ms) |
|---|---:|---:|---:|
| abstract-contains | **37.2 ± 7.0** | 234.2 ± 1.1 ‡ | 6719.1 ± 317.4 |
| births-by-decade | **51.3 ± 3.5** | 137.9 ± 8.3 | 4300.2 ± 88.3 † |
| category-people-1 | **13.0 ± 0.9** | 53.6 ± 3.5 | 63.2 ± 34.1 |
| category-people-2 | **6.8 ± 1.8** | 36.3 ± 8.2 | 49.1 ± 34.3 |
| category-tree | **269.3 ± 2.3** | 2050.1 ± 10.1 ‡ | — § |
| class-counts | **10.8 ± 2.2** | 99.1 ± 5.8 ‡ | — § |
| costar-birthplace | **50.9 ± 5.8** | 154.4 ± 5.6 ‡ | — § |
| count-all | **8.4 ± 3.4** | 2616.8 ± 11.9 † | — § |
| country-population | **12.2 ± 1.7** | 19.6 ± 6.0 ‡ | — § |
| entity-facts-1 | **10.9 ± 0.5** | 16.1 ± 0.3 ‡ | — § |
| entity-facts-2 | **5.7 ± 2.6** | 12.1 ± 4.0 | — § |
| entity-summary-1 | **10.5 ± 0.9** | 21.6 ± 7.0 | — § |
| entity-summary-2 | **6.4 ± 1.5** | 12.1 ± 3.1 | — § |
| export-1m | **241.7 ± 13.2** | 1440.1 ± 3.3 | — § |
| film-director-optional | **21.2 ± 1.2** | 33.3 ± 8.1 ‡ | — § |
| geo-box | **27.1** | 143.8 ± 1.4 ‡ | — § |
| inlinks-count-1 | **10.4 ± 1.0** | 14.7 ± 1.4 ‡ | — § |
| inlinks-count-2 | **11.5 ± 0.6** | 16.5 ± 1.1 ‡ | — § |
| label-regex | **13.6 ± 3.1** | 19.0 ± 0.0 | — § |
| outlink-classes-1 | **16.4 ± 5.3** | 22.6 ± 5.1 ‡ | — § |
| outlink-classes-2 | **16.3 ± 6.2** | 23.7 ± 4.1 ‡ | — § |
| people-no-birthdate | **50.7 ± 0.8** | 65.6 ± 4.9 ‡ | — § |
| place-births-1 | **16.9 ± 0.7** | 43.9 ± 2.4 | — § |
| place-births-2 | **15.6 ± 2.8** | 41.9 ± 1.0 | — § |
| place-union-1 | **12.8 ± 1.1** | 13.7 ± 4.3 | — § |
| place-union-2 | **10.0 ± 0.7** | 15.0 ± 0.6 | — § |
| predicate-counts | 18.4 ± 2.6 † | 5732.6 ± 45.0 † | — § |
| redirect-target-1 | **13.6 ± 4.2** | 22.8 ± 1.3 | — § |
| redirect-target-2 | **9.1 ± 5.5** | 22.7 ± 1.6 | — § |
| sameas-subjects | **8.8 ± 1.1** | 21.3 ± 2.9 ‡ | — § |
| top-linked | **572.6 ± 14.9** | 5580.6 ± 7.6 | — § |
| **throughput**, entity-facts-1, 16 clients (q/s) | **1517** | 1064 | — § |
| **throughput**, place-births-1, 16 clients (q/s) | **830** | 231 | — § |

‡ Same values as different RDF terms, because QLever returns counts as `xsd:int`.
§ Fluree's server was killed at the 14 GiB limit before it ran these queries warm.
† The engine's answer differs from the majority, so the query is not ranked for it. On `count-all` and `predicate-counts`, Sparkles and QLever disagree for the reason given [below](RESULTS.md#why-count-all-and-predicate-counts-differ).
QLever's `count-all` is one triple short of the distinct input, and `predicate-counts` is
not ranked. The `geo-box` row is the median of 11 warm requests with the
[numeric column](RESULTS.md#numeric-literals-in-the-vocabulary).

*Measured on forge between 2026-10-03 and 2026-10-09.*

Sparkles is faster than QLever on all 29 queries that both rank. `place-union-1` is within
10%. Under 16 clients it serves 1,517 `entity-facts-1` requests per second against
QLever's 1,064, and 830 `place-births-1` requests per second against 231. Fluree is slower
on the four warm queries it completed.

| query (cold) | sparkles (ms) | qlever (ms) | fluree (ms) |
|---|---:|---:|---:|
| abstract-contains | **721.0** | 4296.2 | 4195.1 |
| births-by-decade | **101.8** | 165.2 | 6108.5 |
| category-people-1 | **118.6** | 224.0 | 2886.2 |
| category-people-2 | **154.1** | 166.2 | 2172.0 |
| category-tree | **368.9** | 2098.1 | 4559.3 |
| class-counts | **6.4** | 177.8 | 9581.4 |
| costar-birthplace | **146.6** | 183.0 | 4358.2 |
| count-all | **5.5** | 4630.4 | 1273.3 |
| country-population | **31.6** | 45.6 | 4324.1 |
| entity-facts-1 | **18.7** | 36.4 | 1312.2 |
| entity-facts-2 | **25.0** | 40.4 | 837.6 |
| entity-summary-1 | **8.4** | 29.0 | 1303.7 |
| entity-summary-2 | **9.3** | 29.5 | 799.9 |
| export-1m | **1000.4** | 4261.4 | 6647.1 |
| film-director-optional | **35.3** | 68.6 | 13434.8 |
| geo-box | **108** | 157.9 | 12218.4 |
| inlinks-count-1 | **13.1** | 30.2 | 792.6 |
| inlinks-count-2 | **10.4** | 27.3 | 1275.8 |
| label-regex | **40.1** | 71.3 | 4328.6 |
| outlink-classes-1 | **61.7** | 73.7 | 1711.0 |
| outlink-classes-2 | 43.8 | **34.9** | 1665.2 |
| people-no-birthdate | 97.3 | **92.0** | 2113.6 |
| place-births-1 | **74.3** | 283.9 | 3293.1 |
| place-births-2 | **85.8** | 361.5 | 3956.5 |
| place-union-1 | **34.3** | 220.3 | 1235.7 |
| place-union-2 | **30.9** | 217.4 | 1244.6 |
| predicate-counts | **32.0** | 5880.8 | 81502.3 |
| redirect-target-1 | **21.9** | 45.9 | 1290.5 |
| redirect-target-2 | **20.4** | 48.0 | 1276.7 |
| sameas-subjects | **9.4** | 38.2 | 772.8 |
| top-linked | **938.8** | 6538.4 | 57624.6 |

Cold, Sparkles is faster than QLever on 29 of 31 queries. QLever wins
`outlink-classes-2`, at 34.9 against 43.8 ms, and `people-no-birthdate` by less than 10%.
Fluree is slower than Sparkles on every cold query.

#### Why `count-all` and `predicate-counts` differ

QLever counts 1,014,683,273 triples, and Sparkles and Fluree count 1,014,683,275. QLever folds
`xsd:float` and `xsd:double` literals whose lexical forms parse to the same number into
one term. DBpedia has such pairs, for example distinct lexical forms of the same value in
`geo:lat` and in `dbo:orbitalPeriod`, so QLever merges two pairs of triples that differ
only in that literal. RDF 1.1 identifies a literal by its lexical form and datatype, so
Sparkles keeps them as distinct terms and distinct triples, as Jena does. The
per-predicate counts differ for the same reason, and
[COMPARISON.md](../../COMPARISON.md#general) lists this among the divergences.

#### Numeric literals in the vocabulary

Sparkles stores a number inline in its id only when its lexical form is canonical.
DBpedia writes coordinates as `"48.8566"^^xsd:float` and many counts as `xsd:int` or
`xsd:nonNegativeInteger`, so 2,057,247 of its numeric literals live in the vocabulary. The
numeric column, `vocab.num`, holds their exact values next to the vocabulary, so a range
filter, sort or aggregate over them reads 8 bytes per value instead of decoding each key.
It costs 20.7 MB on disk beside an 8.0 GB `vocab.dat`, and 4.3 MB of it stays in memory
while the database is open. The `geo-box` and `country-population` queries of the suite and five extra queries over
`geo:lat`, `geo:long`, `dbo:populationTotal` and `dbo:elevation` compare the same binary
with the column and with `SPARKLES_NUMERIC_COLUMN=off`. Cold times are medians of 10
alternating rounds with a restart and file-cache eviction before each query, and warm
times are medians of 11 requests. The answers are identical in both modes.

| 1.01B | cold, with (ms) | cold, without (ms) | warm, with (ms) | warm, without (ms) |
|---|---:|---:|---:|---:|
| `geo-box` | **108** | 255 | **27.1** | 139.5 |
| `country-population` | 29.0 | 28.2 | **2.8** | 3.2 |
| `geo-avg` | **553** | 3,001 | **191** | 580 |
| `lat-order` | **72** | 137 | **21.8** | 50.2 |
| `pop-topk` | **41** | 90 | **9.3** | 20.7 |
| `pop-range` | **43** | 65 | **7.8** | 17.2 |
| `elev` | **41** | 54 | **6.7** | 14.0 |

Cold, the column's values are read ahead page by page, so a filter over a few thousand
values reads only the pages that hold them. `country-population` reads few such literals
and is within 10% either way.

#### Prefix filtering and large results

The `label-regex` filter, `REGEX(?l, "^The B")`, fixes the start of the label. Sparkles
uses the sorted vocabulary to narrow the matching ids to ranges before the join, so ids
outside the range fail without reading their keys. Results above 65,536 rows are
serialized in chunks of that size. Within a chunk, vocabulary ids are sorted, their pages
are requested ahead, and front-coded blocks are decoded once, in id order and in parallel.
Requests for the next chunk overlap the writing of the current one. This holds up to about
70 MiB of additional memory while an export is written.

### Cursor expression reuse controls

A dedicated complete-HTTP JSON comparison over the verified 50M slice gave Sparkles
50.2 ms and QLever 16.6 ms warm, and 68.1 and 27.6 ms cold, before the
[decimal arithmetic change](#decimal-arithmetic-and-inline-dates). The paired forge run
after that change is described under
[decimal arithmetic and inline dates](#decimal-arithmetic-and-inline-dates). The [assessment record](../2026-10-25-cursor-bind-assessment-forge.json)
retains the paired samples and settings. Its Python timer and current kernel differ from
the retained comparative tables' hyperfine and curl contract.

Matching pre-cache and cache builds take 1,210.9 and 164.6 ms on the full-data warm
guard, and 1,267.1 and 214.4 ms after file-cache eviction. Automatic and forced streaming
execution keep all 171 groups. Native query-accounted peak memory rises from about
2.8 to 8.3 MiB. Forced eager execution takes 42.9 ms with about 52.4 MiB of query-accounted
peak memory. These counters describe query allocations rather than process RSS.

The 50M automatic guard stays near 50 ms. Conservative cache admission forgoes the
36.7 ms shortcut that slowed cheap near-unique inputs. An independent source stream and
calendar oracle verifies all 80 slice groups and counts. The full guard checks unchanged
answers without independently classifying the historical minority result. Generated
strings, IRIs and dates cover 63 scale and reuse distributions with matching checksums.
Longer pinned checks clear the coarse flags, while small remaining costs and original
all-thread flags stay recorded under [item 293](../../ROADMAP.md).

### Decimal arithmetic and inline dates

`births-by-decade` spends almost all of its time in `FLOOR(YEAR(?d) / 10) * 10`, once
for each of the slice's 40,790 dated people. Products of a decimal and a nonzero
integer and quotients of two integers now take one 128-bit operation, decimal results
pack into inline ids without text, and YEAR and the other date parts read an inline
date's fields. The [X06 Outcome](../../specs/X06-benchmark-coverage.md#decimal-arithmetic-and-inline-date-components)
describes the profile and the tests.

An internal comparison on atlas, an Intel Core i5-13500, ran the two Sparkles builds
alone over the same 50M slice indexes with the complete-HTTP JSON timer of the cursor
assessment. Four rounds gave each build 80 warm requests and 12 cold restarts after
file-cache eviction. `4196fb84` took 48.9 ms warm and 64.5 ms cold, and `249f5f35`
took 4.7 and 29.8 ms, with the same 80 groups and counts. The
[record](../2026-10-30-births-decimal-atlas.json) also keeps a QLever run on the same
host, which is not a published ranking.

A paired run on forge then measured `1acdc38a`, which contains the change, against
QLever 0.5.48 over freshly loaded stores of the same slice. Each engine ran alone, in
alternating order, and no other build or benchmark ran on the host. The complete-HTTP
timer of the cursor assessment ran 20 rounds. In each round every engine took three cold
requests, each after a restart with the store evicted from the page cache, and then one
warm process with three untimed and 20 timed requests. Sparkles took 3.9 ms warm and
27.5 ms cold, against 17.2 and 29.3 ms for QLever, over 400 warm requests and 60 cold
restarts each. The same contract gave Sparkles 50.2 and 68.1 ms before the change. Every
request returned the same 80 groups and counts. The cold medians differ by 6%, which is
inside the ten-percent tie band, and the cold requests of QLever range from 17 to 62 ms.
The hyperfine and curl timer of the comparative tables gives 7.1 ms warm and 37.8 ms
cold for Sparkles, against 13.5 and 43.8 ms for QLever. Its curl process adds about
2 ms to each request.

Sparkles varies more between server processes than QLever does on this query. Most of its
warm processes take 3 to 5 ms, but one of the 20 rounds took 15.4 ms for the whole
process, and one of the five hyperfine passes took about 24 ms for each request. The
cause is open under roadmap item NEW-ITEM, and the medians above include these processes.
The [run record](runs/2026-10-31-births-by-decade-forge/record.json) keeps the samples,
binary hashes and settings of both timers.

On forge, the full 1.24-billion-triple store took 161.7 ms warm and 229.0 ms cold with
`4196fb84`, against 58.9 and 109.5 ms with `249f5f35`, and both kept all 171 groups.
The change adds no state, so inputs with many distinct values pay no new cost. The
record keeps every sample, the binary hashes and the settings.
