# SP²Bench results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

<a id="sp2bench"></a>
<a id="sp2bench-at-10m-triples"></a>

## SP²Bench

SP²Bench v1.01 generates bibliographic data in the style of DBLP. At 10M triples its
N-Triples file is 1.49 GB. The 17 queries ran with a request limit of 600 s. Times are
mean ± σ in ms. The paired `q12a` cells and Jena's `q12b` cell use 30 runs after
10 warm-ups. The Sparkles and Oxigraph `q12b` cells pool two reversed-order runs of
60 requests after 15 warm-ups per engine. Other cells use 10 runs after 2 warm-ups. In the original answer check, which kept all five
servers up, the servers of QLever and Fluree were killed during `q4`, so their answers
from `q5a` on were not checked (§). `mise run bench:sp2b 10m` reproduces the run.

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| q1 | **3.5 ± 0.1** | 8.3 ± 0.8 | error | error | 29.6 ± 5.7 |
| q2 | **771.6 ± 11.2** | 35233.4 ± 1583.9 | 3415.2 ± 38.5 ‡ | 3275.0 ± 40.1 | 40256.7 ± 93.7 |
| q3a | **45.1 ± 4.9** | 1281.1 ± 497.7 | 252.9 ± 9.6 | 3756.5 ± 16.8 | 2868.4 ± 35.4 |
| q3b | **5.2 ± 0.2** | 515.3 ± 40.1 | 10.0 ± 0.8 | 2548.7 ± 641.6 | 2035.7 ± 15.5 |
| q3c | **4.6 ± 0.4** | 472.2 ± 10.5 | 7.4 ± 0.5 | 2739.6 ± 695.3 | 2030.6 ± 15.0 |
| q4 | error | timeout | error | error | timeout |
| q5a | **3946.0 ± 665.9** | timeout | timeout | timeout | timeout |
| q5b | **234.9 ± 23.4** | 7036.6 ± 80.5 | 653.9 ± 7.1 § | 2588.3 ± 16.9 § | 21273.6 ± 22.8 |
| q6 | error | **108648.1 ± 454.0** | 2160.0 ± 25.8 § | timeout | timeout |
| q7 | **145.0 ± 7.5** | timeout | 304.1 ± 15.5 § | 5091.6 ± 50.5 § | timeout |
| q8 | **8.3 ± 1.0** | 35.2 ± 2.9 | error | 185.1 ± 1.9 § | 1911.6 ± 52.6 |
| q9 | **158.5 ± 33.5** | 4227.3 ± 15.1 | 284.1 ± 8.6 § | 6160.2 ± 46.5 § | 5202.0 ± 22.1 |
| q10 | **3.8 ± 0.2** | 7.3 ± 0.5 | 6.8 ± 0.6 § | 4.2 ± 0.2 § | 7.6 ± 0.9 |
| q11 | **103.1 ± 9.0** | 375.8 ± 21.4 | 42.8 ± 2.8 § | 51.7 ± 0.8 § | 31619.1 ± 97.9 |
| q12a | **3.8 ± 0.1** | 9.0 ± 1.1 | 245.1 ± 7.3 § | 1670.3 ± 5.7 § | 6794.7 ± 61.5 |
| q12b | 4.0 ± 0.1 | 9.7 ± 1.3 | error | 180.5 ± 2.9 § | **3.7 ± 0.1** |
| q12c | **3.6 ± 0.1** | 6.7 ± 0.2 | 4.2 ± 0.1 § | 3.4 ± 0.1 § | 3.7 ± 0.1 |

*The Sparkles and Oxigraph `q12b` cells come from the
[pure ASK prefix record](../2026-10-25-pure-ask-prefix-q12b-forge.json), with
Sparkles at `b26ce6a9`. Oxigraph has the lower mean, and the 7.8% difference is inside
the ten-percent tie band. The `q12a` cells and Jena's `q12b` cell come from the
[paired forge record](../2026-10-25-competitive-query-pairs-forge.json), with
Sparkles at `2ec8a266`. Other Sparkles cells were measured at `6699f27d`, and the
[SP²Bench record](../2026-10-23-sp2b-10m-forge.json) keeps those samples and the
other engine cells.*

Sparkles is the only engine that answers `q5a`, whose 385,208 rows match an independent
oracle built from the data, in 3.9 s. Its answers to `q8`, `q12a` and `q12b` agree with
Jena's and Oxigraph's.

The `q4` and `q6` errors in the table are refusals by the default memory budgets of the
build measured there. The current engine answers both. Its `q6` OPTIONAL shares no variable
with the outer pattern, and a keyed left join pairs the rows through `?author = ?author2`,
testing 7.4 million candidate pairs instead of a product of 1.6 × 10¹² rows. Over two
interleaved rounds of 10 runs under the same 12 GiB cap, `q6` took a mean of 948 ms and
returned 906,313 rows with the exact and value digests of Jena's answer, 115× faster than
Jena's 108.6 s. `q4` runs as a cursor without spill. Its answer has 41,073,618 rows and
1,955,839,207 bytes of TSV, more than the default response budget of 1 GiB, so `q4` stays an
error with the default settings. With `--max-result-mb 4096` on one thread and without the
server cap, a streamed answer takes 30.4 s and an eager one 16.3 s. These figures come from
forge on 2026-10-23.

Sparkles answers `q12a` faster than Jena in both paired passes. On `q12b`, its mean of
4.0 ms is 7.8% above Oxigraph's 3.7 ms, within the ten percent tie band. On `q11` the
unchecked times of QLever and Fluree, 42.8 and 51.7 ms, are lower than Sparkles' 103.1 ms.

Sparkles loads the data in 3.93 s, against 8.90 s for Fluree, 9.70 s for QLever, 20.49 s
for Oxigraph and 44.88 s for TDB2. Its index takes 307 MiB on disk, against QLever's
294 MiB, and its load peaks at 2,318 MiB, against Fluree's 1,835 MiB and QLever's
1,879 MiB.
