| query | sparkles (ms) | qlever (ms) |
|---|---:|---:|
| **load** (s) | **4.05** | 9.66 |
| q11 | **3.5 ± 0.1** | 43.9 ± 5.2 |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | sparkles | qlever |
|---|---:|---:|
| server RSS 2 s after start, before any query (MiB) | 79 | **45** |
| server RSS after every query ran once (MiB) | **105** | 117 |
| load peak RSS (MiB) | 2308 | **1887** |
| index size on disk | 317 MiB | **294 MiB** |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | sparkles | qlever |
|---|---:|---:|
| q11 | **+23** / 102 | +72 / 117 |
