| query | fluree (ms) |
|---|---:|
| **load** (s) | **16.00** |
| optional-chain | error (1/1 runs failed) |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | fluree |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 271 |
| server RSS after every query ran once (MiB) | 12033 |
| load peak RSS (MiB) | 2768 |
| index size on disk | 398 MiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | fluree |
|---|---:|
| optional-chain | +11808 / 12079 |
