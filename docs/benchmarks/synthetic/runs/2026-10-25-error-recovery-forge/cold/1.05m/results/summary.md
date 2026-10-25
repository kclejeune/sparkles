| query | jena-fuseki (ms) |
|---|---:|
| knows-reach | **355.7 ± 23.4** |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | jena-fuseki |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 268 |
| server RSS after every query ran once (MiB) | 538 |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | jena-fuseki |
|---|---:|
| knows-reach | +275 / 543 |

Cold runs: the median of 3 runs of each query after a server restart with the engine's files evicted from the page cache.

| query (cold) | jena-fuseki (ms) |
|---|---:|
| knows-reach | **1417.9** |
| **time to ready** after the restart (median, s) | 1.22 |
