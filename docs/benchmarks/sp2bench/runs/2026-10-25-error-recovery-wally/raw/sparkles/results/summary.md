| query | sparkles (ms) |
|---|---:|
| **load** (s) | **6.63** |
| q8 | **9.8 ± 0.5** |
| q12b | **5.7 ± 0.3** |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | sparkles |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 74 |
| server RSS after every query ran once (MiB) | 170 |
| load peak RSS (MiB) | 2263 |
| index size on disk | 318 MiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | sparkles |
|---|---:|
| q8 | +89 / 163 |
| q12b | +3 / 170 |
