| query | sparkles (ms) | qlever (ms) |
|---|---:|---:|
| **load** (s) | **6.56** | 10.99 |
| bi-q1-1 | **12.3 ± 0.6** | 51.8 ± 7.8 |
| bi-q1-2 | **9.0 ± 0.2** | 33.3 ± 4.1 |
| bi-q1-3 | 90.7 ± 20.4 | **57.9 ± 6.0** |
| bi-q2-1 | **22.9 ± 3.5** | 172.8 ± 15.9 |
| bi-q2-2 | **16.1 ± 0.5** | 131.4 ± 10.1 |
| bi-q2-3 | **23.1 ± 2.2** | 168.2 ± 13.3 |
| bi-q3-1 | **12.6 ± 0.4** | 17.4 ± 2.0 |
| bi-q3-2 | **27.2 ± 4.1** | 28.9 ± 3.6 |
| bi-q3-3 | **29.4 ± 3.6** | 81.0 ± 4.2 |
| bi-q4-1 | 290.8 ± 44.3 † | 2760.6 ± 32.3 † |
| bi-q4-2 | 52.7 ± 13.0 † | 171.3 ± 6.8 † |
| bi-q4-3 | 175.7 ± 17.8 † | 1759.2 ± 11.2 † |
| bi-q5-1 | 6.3 ± 0.2 † | 84.5 ± 7.2 † |
| bi-q5-2 | 195.7 ± 6.6 † | 196.5 ± 7.7 † |
| bi-q5-3 | 26.4 ± 2.0 † | 94.2 ± 6.6 † |
| bi-q6-1 | **16.6 ± 1.2** | 82.9 ± 10.6 |
| bi-q6-2 | **16.6 ± 1.1** | 81.4 ± 11.7 |
| bi-q6-3 | **14.4 ± 0.8** | 78.9 ± 12.8 |
| bi-q7-1 | 81.7 ± 0.6 | **75.1 ± 9.2** |
| bi-q7-2 | **8.3 ± 0.2** | 53.4 ± 7.2 |
| bi-q7-3 | 51.0 ± 0.8 | **37.3 ± 6.6** |
| bi-q8-1 | 270.3 ± 17.2 † | 311.2 ± 9.7 † |
| bi-q8-2 | 29.6 ± 1.2 † | 72.2 ± 8.7 † |
| bi-q8-3 | **6.5 ± 0.2** | 41.0 ± 4.1 |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | sparkles | qlever |
|---|---:|---:|
| server RSS 2 s after start, before any query (MiB) | 81 | **41** |
| server RSS after every query ran once (MiB) | 446 | **426** |
| load peak RSS (MiB) | 3802 | **2016** |
| index size on disk | 623 MiB | **384 MiB** |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | sparkles | qlever |
|---|---:|---:|
| bi-q1-1 | +123 / 204 | **+107** / 148 |
| bi-q1-2 | +42 / 246 | **+34** / 171 |
| bi-q1-3 | +214 / 460 | **+64** / 232 |
| bi-q2-1 | **+2** / 462 | +50 / 269 |
| bi-q2-2 | **+1** / 463 | +53 / 302 |
| bi-q2-3 | **+0** / 462 | +74 / 365 |
| bi-q3-1 | +30 / 418 | **+12** / 358 |
| bi-q3-2 | +30 / 448 | **+6** / 361 |
| bi-q3-3 | **+1** / 449 | +18 / 371 |
| bi-q4-1 | +211 / 660 | **+148** / 497 |
| bi-q4-2 | **+6** / 643 | +13 / 380 |
| bi-q4-3 | **+32** / 675 | +120 / 498 |
| bi-q5-1 | **+0** / 675 | +8 / 446 |
| bi-q5-2 | **+2** / 677 | +15 / 457 |
| bi-q5-3 | **+0** / 573 | +25 / 476 |
| bi-q6-1 | +19 / 592 | **+0** / 444 |
| bi-q6-2 | **+0** / 592 | +8 / 382 |
| bi-q6-3 | **+0** / 592 | +2 / 383 |
| bi-q7-1 | **+4** / 596 | +12 / 391 |
| bi-q7-2 | **+0** / 549 | +11 / 401 |
| bi-q7-3 | **+3** / 552 | +17 / 419 |
| bi-q8-1 | +71 / 623 | **+18** / 435 |
| bi-q8-2 | **+1** / 624 | +3 / 426 |
| bi-q8-3 | **+2** / 565 | +7 / 431 |

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
