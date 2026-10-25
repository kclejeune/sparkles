| query | qlever (ms) |
|---|---:|
| q8 | error (1/1 runs failed) |
| q12b | error (1/1 runs failed) |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | qlever |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 44 |
| server RSS after every query ran once (MiB) | 1392 |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | qlever |
|---|---:|
| q8 | +7192 / 7236 |
| q12b | +6932 / 8117 |
