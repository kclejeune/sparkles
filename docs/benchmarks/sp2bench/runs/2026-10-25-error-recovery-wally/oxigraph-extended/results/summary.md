| query | oxigraph (ms) |
|---|---:|
| q4 | — |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | oxigraph |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 32 |
| server RSS after every query ran once (MiB) | 4052 |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | oxigraph |
|---|---:|
| q4 | +5619 / 5651 |
