# BSBM results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## BSBM

### BSBM Explore at 10M triples

This run used 10,095,035 generated triples and 28,480 products. The configured driver mix
produced three instances of each of eleven query templates. Each latency below is the
pooled median of thirty HTTP samples, after two warm-up requests per instance, except
`q5`'s paired Sparkles, QLever and Fluree cells, which pool ninety samples after ten
warm-ups per instance. DESCRIBE
answers are not compared because SPARQL leaves their contents to the engine. A dagger
marks an unresolved answer disagreement, whose latency is not ranked.
`SPARKLES_ALLOW_GPL_BENCHMARK=1 mise run bench:bsbm 10m` reproduces the run.

| Template, median ms | Sparkles | Jena | QLever | Fluree | Oxigraph |
|---|---:|---:|---:|---:|---:|
| q1 | **3.95** | 8.00 | 9.63 | 4.71 | 4.60 |
| q2 | **4.09** | 9.31 | 14.91 | 4.41 | 4.67 |
| q3 | **4.02** | 8.22 | 12.64 | 4.53 | 4.79 |
| q4 | **4.11** | 8.23 | 25.43 | 5.25 | 5.65 |
| q5 | **4.65** | 629.44 | 20.26 | 5.59 | 42.65 |
| q7 | 5.27 | 7.23 | 90.64 | 5.07 | **4.51** |
| q8 | 5.97 | 7.26 | 50.76 | 4.29 † | **4.13** |
| q9, unranked | 3.46 | 6.61 | 6.24 | 3.61 | 3.67 |
| q10 | **4.12** | 6.98 | 139.03 | 5.55 | 4.35 |
| q11 | **3.41** | 6.63 | 6.13 | 3.56 † | 3.54 |
| q12 | **3.65** | 7.18 | 11.96 † | 3.96 † | 3.90 |

*The paired `q5` cells for Sparkles, QLever and Fluree come from the
[query record](../2026-10-25-competitive-query-pairs-forge.json), with Sparkles
at `2ec8a266`. The [Explore record](../2026-10-23-bsbm-10m-explore-forge.json)
keeps the other cells, measured with Sparkles at `6699f27d`, and the driver status.*

Direct checks against the generated triples independently confirm Sparkles' answers to
all three instances of q8, q11 and q12. The checks reconstruct the selected reviews,
product offers and CONSTRUCT triples without a SPARQL engine. Fluree's retained value
fingerprints differ on those nine instances. QLever differs on q12. The
q9 row is a DESCRIBE whose answers are not compared, so it is not ranked.
Sparkles has the lower pooled q5 median in both paired passes. The closest instance
takes a mean of 4.58 ms against Fluree's 4.76 ms, within 4%. Oxigraph was fastest on
q7, 1.17 times faster, and on q8, 1.45 times.

The driver ran fifty warm-up mixes and five hundred measured mixes with each
client count. The standard mix includes DESCRIBE and CONSTRUCT, which the driver
requests as RDF/XML. QLever refuses that format. The SELECT-only mix omits those
queries and is reported separately. An error means the driver failed or its result
counts failed the comparison. Fluree's throughput is two to three times Sparkles' at
every client count. It is marked with a dagger and not ranked, because its fixed-instance
answers to q8, q11 and q12 disagree with the other engines.

| Mix | Clients | Sparkles | Jena | QLever | Fluree | Oxigraph |
|---|---:|---:|---:|---:|---:|---:|
| Standard QMpH | 1 | 45,077 | 2,047 | error | 95,969 † | 15,622 |
| Standard QMpH | 4 | 165,605 | 3,173 | error | 304,636 † | error |
| Standard QMpH | 16 | 200,083 | 2,012 | error | 579,271 † | error |
| SELECT-only QMpH | 1 | 36,687 | 2,105 | error | 103,489 † | error |
| SELECT-only QMpH | 4 | 169,528 | 3,237 | error | 329,816 † | 74,102 |
| SELECT-only QMpH | 16 | 203,454 | error | error | 662,847 † | error |

The 100M scale has not been measured.

### BSBM BI at 10M triples

This run used the same 10,095,035 triples and 28,480 products. Each template has three
fixed instances. The paired `q2` cells for Sparkles, QLever and Fluree pool ninety HTTP
samples after ten warm-ups per instance. The other cells pool thirty samples after
two warm-ups per instance. A dagger marks an answer that differs from the majority and is
excluded from ranking. Oxigraph timed out on two q7 instances, so its pooled cell is a
timeout. `SPARKLES_ALLOW_GPL_BENCHMARK=1 mise run bench:bsbm 10m --usecase bi` reproduces
the run.

| Template, median ms | Sparkles | Jena | QLever | Fluree | Oxigraph |
|---|---:|---:|---:|---:|---:|
| q1 | **13.59** | 3,241.71 | 52.51 | 155.26 | 2,077.28 |
| q2 | **20.54** | 3,562.21 | 162.30 | 22.50 † | 32,569.39 |
| q3 | 35.18 | 1,733.90 | **28.45** | 287.78 † | 3,287.87 |
| q4 | **152.80** | 8,452.57 | 1,848.57 | 162.22 | 6,868.73 |
| q5 | **27.32** | 198.17 | 98.63 | 693.70 | 138.93 |
| q6 | **16.50** | 23.72 | 67.88 | 338.27 | 28.77 |
| q7 | **52.62** | 74.60 | 59.38 | 210.01 | timeout |
| q8 | **35.04** | 121.75 | 68.09 † | 202.66 † | 184.04 |

*The paired `q2` cells for Sparkles, QLever and Fluree come from the
[query record](../2026-10-25-competitive-query-pairs-forge.json), with Sparkles
at `2ec8a266`. The [BI record](../2026-10-23-bsbm-10m-bi-forge.json) preserves
the other cells, measured with Sparkles at `6699f27d`, including failures.*

The answer checks compare raw answers captured on atlas, where each engine served the same
data alone with the same binaries, because QLever's check on forge left q4 to q8 without
fingerprints. q4 and q5 average `xsd:float` prices, and SPARQL leaves the order of the
additions open, so engines that add in different orders legitimately return different
floats. `scripts/bench-bsbm/bi-float-oracle.py` computes the exact mean of the
float-rounded prices of every q4 and q5 instance from the data and accepts a value
within the error bound of a float summation in any order. All five engines' q4 and q5
answers pass it, with the same rows, counts and IRIs. Sparkles, Jena and Oxigraph return
`xsd:float`, QLever `xsd:decimal` and Fluree `xsd:double`.

`scripts/bench-bsbm/bi-answer-oracle.py` independently computes q6 reviewer scores and
its HAVING threshold from the same data. All five engines' three q6 captures pass the
binary32 error bounds. This establishes numeric equivalence for the comparison, rather
than identical RDF datatypes. The same tool checks every successful retained q7 capture
against the offer counts and country exclusions. The inner LIMIT orders only by offer
count. On the third instance 41 products tie for its last 21 places, and the outer
country filter permits 75 to 85 rows. The 81-row Sparkles, Jena and QLever answers and
the 79-row Fluree and Oxigraph answers are all valid. The checked Fluree captures total
80 rows across the three instances. They replace an unavailable 77-row fingerprint in
the record without changing any timing sample.

The paired q2 answers also pass an independent streamed data oracle for Product
membership, common-feature counts, anchor exclusion and the complete ordered top ten.
Its pooled fingerprint matches the published Sparkles answer pool. Fluree's q2 pool
differs from that independently verified result, and remains unranked.

Fluree's q3 and q8 answers and two QLever q8 answers remain unresolved fingerprint
disagreements. The q3 and q8 exclusions have no independent data oracle. Sparkles has
the lowest median on q1, q2 and q4 to q8. Its q4 lead over Fluree is within 10%.
QLever remains 1.24 times faster on q3, where its answers agree with Sparkles'. Its two
q8 instances whose answers differ from the majority carry a dagger.

The driver uses generated parameters beyond the three fixed instances. It requested
fifty warm-up mixes and five hundred measured mixes at each client count, with a
1,800 second wall budget per cell.

| BI QMpH | Clients | Sparkles | Jena | QLever | Fluree | Oxigraph |
|---|---:|---:|---:|---:|---:|---:|
| Standard mix | 1 | error | timeout | error | timeout | timeout |
| Standard mix | 4 | error | timeout | error | 2,174 † | timeout |
| Standard mix | 16 | error | timeout | error | 3,143 † | timeout |

In this run Sparkles failed all three driver cells. At one and four clients, q4 with
ProductType6 asked the query pool for about 5.8 GiB, more than the default pool's 5.2 GiB.
At sixteen clients, q2 instances of about 450 MB each queued for the pool and some exceeded
the admission wait. The current engine streams that q4 instance and answers it in 15.1 s
with a peak resident size of 282 MB under the default pool, where eager execution takes
17.7 s and 8.3 GB, and a request larger than the whole pool answers `507` without
`Retry-After` ([roadmap](../../ROADMAP.md) item 96). The driver cells have not been rerun with
it. QLever's driver failed with a refused connection at one client and HTTP 500 at four and
sixteen clients. Jena and Oxigraph timed out at every client count. Fluree alone completed
two driver cells, but their result-count check has no successful competing engine to
compare, and its fixed q2, q3 and q8 answers have unresolved disagreements, so its QMpH figures are not
ranked.
