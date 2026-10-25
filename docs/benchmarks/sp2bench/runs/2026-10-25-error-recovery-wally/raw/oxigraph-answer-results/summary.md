| query | oxigraph (ms) |
|---|---:|
| **load** (s) | **27.45** |
| q4 | — |
| q8 | — |
| q12b | — |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | oxigraph |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 32 |
| server RSS after every query ran once (MiB) | 1917 |
| load peak RSS (MiB) | 1689 |
| index size on disk | 1.6 GiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | oxigraph |
|---|---:|
| q4 | +2740 / 2772 |
| q8 | +32 / 2714 |
| q12b | +0 / 1917 |
