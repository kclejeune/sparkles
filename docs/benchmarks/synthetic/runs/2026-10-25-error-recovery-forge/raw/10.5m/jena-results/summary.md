| query | jena-fuseki (ms) |
|---|---:|
| **load** (s) | **46.25** |
| knows-reach | **6287.4 ± 267.0** |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | jena-fuseki |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 258 |
| server RSS after every query ran once (MiB) | 2182 |
| load peak RSS (MiB) | 3655 |
| index size on disk | 1.3 GiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | jena-fuseki |
|---|---:|
| knows-reach | +1917 / 2175 |
