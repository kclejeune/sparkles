| query | jena-fuseki (ms) | fluree (ms) |
|---|---:|---:|
| **load** (s) | 46.25 | **10.61** |
| count-all | — | **3.5 ± 0.1** |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | jena-fuseki | fluree |
|---|---:|---:|
| server RSS 2 s after start, before any query (MiB) | **258** | 271 |
| server RSS after every query ran once (MiB) | 2182 | **292** |
| load peak RSS (MiB) | 3655 | **3129** |
| index size on disk | 1.3 GiB | **398 MiB** |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | jena-fuseki | fluree |
|---|---:|---:|
| count-all | — | +9 / 280 |
