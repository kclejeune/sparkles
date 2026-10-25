| query | jena-fuseki (ms) |
|---|---:|
| **load** (s) | **56.85** |
| q8 | **53.2 ± 17.4** |
| q12b | **12.0 ± 1.1** |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | jena-fuseki |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 290 |
| server RSS after every query ran once (MiB) | 413 |
| load peak RSS (MiB) | 4780 |
| index size on disk | 1.7 GiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | jena-fuseki |
|---|---:|
| q8 | +120 / 410 |
| q12b | +5 / 430 |
