| query | sparkles (ms) | qlever (ms) |
|---|---:|---:|
| **load** (s) | **6.56** | 10.99 |
| bi-q1-1 | **12.1 ± 0.5** | 51.2 ± 7.2 |
| bi-q1-2 | **8.9 ± 0.2** | 33.6 ± 5.9 |
| bi-q1-3 | 86.5 ± 15.5 | **57.3 ± 4.2** |
| bi-q2-1 | **18.5 ± 1.5** | 174.3 ± 12.0 |
| bi-q2-2 | **15.8 ± 0.5** | 132.4 ± 7.8 |
| bi-q2-3 | **24.4 ± 3.0** | 173.6 ± 10.3 |
| bi-q3-1 | **12.9 ± 0.5** | 18.7 ± 1.8 |
| bi-q3-2 | **25.8 ± 3.6** | 29.1 ± 4.0 |
| bi-q3-3 | **30.0 ± 3.3** | 80.5 ± 4.9 |
| bi-q4-1 | 267.5 ± 27.3 † | 2756.3 ± 17.5 † |
| bi-q4-2 | 52.3 ± 9.4 † | 172.6 ± 7.3 † |
| bi-q4-3 | 173.5 ± 18.1 † | 1749.9 ± 9.3 † |
| bi-q5-1 | 6.3 ± 0.2 † | 83.8 ± 6.4 † |
| bi-q5-2 | 198.8 ± 11.1 † | 195.7 ± 6.0 † |
| bi-q5-3 | 29.3 ± 2.0 † | 96.6 ± 6.8 † |
| bi-q6-1 | **18.4 ± 2.9** | 75.5 ± 13.0 |
| bi-q6-2 | **17.3 ± 5.9** | 80.7 ± 14.1 |
| bi-q6-3 | **17.2 ± 4.3** | 79.2 ± 13.6 |
| bi-q7-1 | 82.6 ± 1.0 | **69.3 ± 5.1** |
| bi-q7-2 | **8.3 ± 0.2** | 53.6 ± 7.2 |
| bi-q7-3 | 49.7 ± 1.3 | **38.8 ± 6.7** |
| bi-q8-1 | 262.8 ± 10.8 † | 308.0 ± 10.9 † |
| bi-q8-2 | 32.2 ± 2.0 † | 67.5 ± 6.8 † |
| bi-q8-3 | **6.5 ± 0.1** | 40.4 ± 5.0 |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | sparkles | qlever |
|---|---:|---:|
| server RSS 2 s after start, before any query (MiB) | 83 | **41** |
| server RSS after every query ran once (MiB) | 470 | **431** |
| load peak RSS (MiB) | 3802 | **2016** |
| index size on disk | 623 MiB | **384 MiB** |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | sparkles | qlever |
|---|---:|---:|
| bi-q1-1 | +123 / 206 | **+108** / 149 |
| bi-q1-2 | +45 / 251 | **+34** / 172 |
| bi-q1-3 | +207 / 458 | **+22** / 191 |
| bi-q2-1 | **+3** / 461 | +89 / 266 |
| bi-q2-2 | **+10** / 471 | +34 / 299 |
| bi-q2-3 | **+0** / 470 | +66 / 350 |
| bi-q3-1 | +12 / 432 | **+11** / 350 |
| bi-q3-2 | +38 / 470 | **+15** / 364 |
| bi-q3-3 | **+13** / 483 | +35 / 388 |
| bi-q4-1 | +187 / 670 | **+142** / 507 |
| bi-q4-2 | **+1** / 651 | +29 / 397 |
| bi-q4-3 | **+19** / 670 | +119 / 517 |
| bi-q5-1 | **+0** / 670 | +2 / 453 |
| bi-q5-2 | +4 / 674 | **+0** / 444 |
| bi-q5-3 | **+0** / 580 | +7 / 428 |
| bi-q6-1 | +18 / 598 | **+4** / 422 |
| bi-q6-2 | **+0** / 598 | +0 / 391 |
| bi-q6-3 | **+0** / 598 | +8 / 383 |
| bi-q7-1 | **+0** / 597 | +41 / 415 |
| bi-q7-2 | **+1** / 572 | +35 / 415 |
| bi-q7-3 | **+0** / 572 | +18 / 433 |
| bi-q8-1 | +85 / 657 | **+16** / 447 |
| bi-q8-2 | **+0** / 655 | +1 / 432 |
| bi-q8-3 | **+3** / 598 | +13 / 441 |

† `bi-q4-1`: the answers differ and no answer has a majority (qlever 10 rows, sparkles 10 rows); not ranked
† `bi-q4-2`: the answers differ and no answer has a majority (qlever 10 rows, sparkles 10 rows); not ranked
† `bi-q4-3`: the answers differ and no answer has a majority (qlever 10 rows, sparkles 10 rows); not ranked
† `bi-q5-1`: the answers differ and no answer has a majority (qlever 13 rows, sparkles 13 rows); not ranked
† `bi-q5-2`: the answers differ and no answer has a majority (qlever 30 rows, sparkles 30 rows); not ranked
† `bi-q5-3`: the answers differ and no answer has a majority (qlever 30 rows, sparkles 30 rows); not ranked
`bi-q6-1`: sparkles passed the independent data oracle
`bi-q6-1`: qlever passed the independent data oracle
`bi-q6-2`: sparkles passed the independent data oracle
`bi-q6-2`: qlever passed the independent data oracle
`bi-q6-3`: sparkles passed the independent data oracle
`bi-q6-3`: qlever passed the independent data oracle
`bi-q7-1`: sparkles passed the independent data oracle
`bi-q7-1`: qlever passed the independent data oracle
`bi-q7-2`: sparkles passed the independent data oracle
`bi-q7-2`: qlever passed the independent data oracle
`bi-q7-3`: sparkles passed the independent data oracle
`bi-q7-3`: qlever passed the independent data oracle
† `bi-q8-1`: the answers differ and no answer has a majority (qlever 10 rows, sparkles 10 rows); not ranked
† `bi-q8-2`: the answers differ and no answer has a majority (qlever 10 rows, sparkles 10 rows); not ranked
