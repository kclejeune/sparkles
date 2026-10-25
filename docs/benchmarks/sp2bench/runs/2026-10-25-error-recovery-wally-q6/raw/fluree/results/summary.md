| query | fluree (ms) |
|---|---:|
| q6 | — |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | fluree |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 287 |
| server RSS after every query ran once (MiB) | 1081 |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | fluree |
|---|---:|
| q6 | +783 / 1070 |
