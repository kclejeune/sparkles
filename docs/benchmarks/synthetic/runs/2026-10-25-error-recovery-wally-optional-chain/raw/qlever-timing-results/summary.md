| query | qlever (ms) |
|---|---:|
| **load** (s) | **19.18** |
| optional-chain | **583.2 ± 14.5** |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | qlever |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 52 |
| server RSS after every query ran once (MiB) | 101 |
| load peak RSS (MiB) | 1864 |
| index size on disk | 276 MiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | qlever |
|---|---:|
| optional-chain | +62 / 114 |
