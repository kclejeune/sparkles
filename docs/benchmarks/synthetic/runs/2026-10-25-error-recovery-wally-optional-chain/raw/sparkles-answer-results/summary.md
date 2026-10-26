| query | sparkles (ms) |
|---|---:|
| **load** (s) | **6.50** |
| optional-chain | — |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | sparkles |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 92 |
| server RSS after every query ran once (MiB) | 158 |
| load peak RSS (MiB) | 1919 |
| index size on disk | 298 MiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | sparkles |
|---|---:|
| optional-chain | +285 / 377 |
