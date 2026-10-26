| query | oxigraph (ms) |
|---|---:|
| **load** (s) | **26.03** |
| optional-chain | **5448.8 ± 30.7** |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | oxigraph |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 31 |
| server RSS after every query ran once (MiB) | 195 |
| load peak RSS (MiB) | 1913 |
| index size on disk | 1.4 GiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | oxigraph |
|---|---:|
| optional-chain | +164 / 195 |
