| query | sparkles (ms) |
|---|---:|
| **load** (s) | **6.63** |
| q4 | — |
| q8 | — |
| q12b | — |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | sparkles |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 76 |
| server RSS after every query ran once (MiB) | 260 |
| load peak RSS (MiB) | 2263 |
| index size on disk | 318 MiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | sparkles |
|---|---:|
| q4 | +6093 / 6169 |
| q8 | +31 / 259 |
| q12b | +0 / 260 |
