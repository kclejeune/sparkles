| query | jena-fuseki (ms) |
|---|---:|
| q6 | **122587.3 ± 454.0** |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | jena-fuseki |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 288 |
| server RSS after every query ran once (MiB) | 4139 |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | jena-fuseki |
|---|---:|
| q6 | +3846 / 4134 |
