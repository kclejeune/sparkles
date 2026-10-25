| query | jena-fuseki (ms) |
|---|---:|
| **load** (s) | **56.85** |
| q4 | — |
| q8 | — |
| q12b | — |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | jena-fuseki |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 277 |
| server RSS after every query ran once (MiB) | 3027 |
| load peak RSS (MiB) | 4780 |
| index size on disk | 1.7 GiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | jena-fuseki |
|---|---:|
| q4 | +2733 / 3010 |
| q8 | +19 / 3025 |
| q12b | +6 / 3034 |
