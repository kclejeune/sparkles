| query | sparkles (ms) | qlever (ms) |
|---|---:|---:|
| **load** (s) | **6.56** | 10.99 |
| bi-q1-1 | **12.5 ± 0.8** | 55.2 ± 9.6 |
| bi-q1-2 | **9.4 ± 0.4** | 33.5 ± 4.0 |
| bi-q1-3 | 88.6 ± 13.8 | **58.6 ± 6.4** |
| bi-q2-1 | **22.3 ± 2.0** | 172.8 ± 17.1 |
| bi-q2-2 | **21.7 ± 2.3** | 131.3 ± 9.7 |
| bi-q2-3 | **22.2 ± 2.2** | 165.6 ± 13.3 |
| bi-q3-1 | **13.1 ± 0.7** | 17.9 ± 2.1 |
| bi-q3-2 | 33.7 ± 6.4 | **29.8 ± 4.1** |
| bi-q3-3 | **48.0 ± 9.5** | 81.0 ± 5.0 |
| bi-q4-1 | 272.1 ± 28.8 † | 2819.2 ± 368.1 † |
| bi-q4-2 | 53.5 ± 11.6 † | 173.9 ± 6.9 † |
| bi-q4-3 | 170.6 ± 11.9 † | 1757.5 ± 11.5 † |
| bi-q5-1 | 6.4 ± 0.3 † | 83.0 ± 7.0 † |
| bi-q5-2 | 194.0 ± 4.4 † | 197.9 ± 7.9 † |
| bi-q5-3 | 29.9 ± 2.0 † | 94.5 ± 5.4 † |
| bi-q6-1 | **16.8 ± 1.0** | 76.2 ± 10.7 |
| bi-q6-2 | **17.1 ± 2.1** | 77.6 ± 12.1 |
| bi-q6-3 | **14.5 ± 0.8** | 81.7 ± 12.7 |
| bi-q7-1 | 82.7 ± 1.9 | **70.9 ± 8.1** |
| bi-q7-2 | **8.3 ± 0.3** | 49.9 ± 5.6 |
| bi-q7-3 | 50.3 ± 0.3 | **37.2 ± 5.3** |
| bi-q8-1 | 263.9 ± 11.7 † | 313.5 ± 12.7 † |
| bi-q8-2 | 32.3 ± 2.3 † | 70.9 ± 11.4 † |
| bi-q8-3 | **6.6 ± 0.1** | 40.6 ± 6.0 |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | sparkles | qlever |
|---|---:|---:|
| server RSS 2 s after start, before any query (MiB) | 89 | **41** |
| server RSS after every query ran once (MiB) | 468 | **405** |
| load peak RSS (MiB) | 3802 | **2016** |
| index size on disk | 623 MiB | **384 MiB** |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | sparkles | qlever |
|---|---:|---:|
| bi-q1-1 | +123 / 212 | **+109** / 150 |
| bi-q1-2 | **+20** / 232 | +32 / 171 |
| bi-q1-3 | +218 / 450 | **+102** / 267 |
| bi-q2-1 | **+11** / 461 | +52 / 307 |
| bi-q2-2 | **+0** / 459 | +18 / 305 |
| bi-q2-3 | **+9** / 428 | +72 / 374 |
| bi-q3-1 | **+10** / 438 | +11 / 385 |
| bi-q3-2 | +21 / 459 | **+6** / 391 |
| bi-q3-3 | **+2** / 461 | +37 / 428 |
| bi-q4-1 | +210 / 671 | **+153** / 557 |
| bi-q4-2 | **+1** / 672 | +19 / 440 |
| bi-q4-3 | **+12** / 684 | +76 / 506 |
| bi-q5-1 | **+0** / 681 | +19 / 456 |
| bi-q5-2 | +70 / 562 | **+18** / 467 |
| bi-q5-3 | **+1** / 563 | +3 / 456 |
| bi-q6-1 | +26 / 589 | **+0** / 452 |
| bi-q6-2 | +2 / 591 | **+1** / 348 |
| bi-q6-3 | **+0** / 588 | +0 / 347 |
| bi-q7-1 | **+25** / 579 | +33 / 381 |
| bi-q7-2 | **+0** / 579 | +34 / 409 |
| bi-q7-3 | +2 / 581 | **+0** / 398 |
| bi-q8-1 | +66 / 647 | **+21** / 419 |
| bi-q8-2 | **+1** / 645 | +2 / 404 |
| bi-q8-3 | **+3** / 648 | +8 / 412 |

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
