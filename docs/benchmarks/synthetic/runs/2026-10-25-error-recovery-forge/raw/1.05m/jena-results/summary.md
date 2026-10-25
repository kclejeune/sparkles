| query | jena-fuseki (ms) |
|---|---:|
| **load** (s) | **4.60** |
| knows-reach | **364.1 ± 35.9** |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | jena-fuseki |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 264 |
| server RSS after every query ran once (MiB) | 496 |
| load peak RSS (MiB) | 2109 |
| index size on disk | 132 MiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | jena-fuseki |
|---|---:|
| knows-reach | +226 / 490 |
