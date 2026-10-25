| query | qlever (ms) |
|---|---:|
| **load** (s) | **18.48** |
| q4 | — |
| q8 | — |
| q12b | — |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | qlever |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 41 |
| server RSS after every query ran once (MiB) | 5111 |
| load peak RSS (MiB) | 1850 |
| index size on disk | 294 MiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | qlever |
|---|---:|
| q4 | +8508 / 8549 |
| q8 | +7179 / 9973 |
| q12b | +7164 / 11123 |
