| query | fluree (ms) |
|---|---:|
| **load** (s) | **14.58** |
| q8 | **232.5 ± 13.2** |
| q12b | **223.8 ± 14.6** |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | fluree |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 284 |
| server RSS after every query ran once (MiB) | 503 |
| load peak RSS (MiB) | 1819 |
| index size on disk | 888 MiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | fluree |
|---|---:|
| q8 | +246 / 530 |
| q12b | +168 / 564 |
