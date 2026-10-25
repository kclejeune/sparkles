# Paths and graph algorithms results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Paths and graph algorithms

### Path search

These Sparkles-only runs compare `SERVICE path:search` with property paths on the
benchmark's `foaf:knows` graph, with a warm server and the result cache off. Each query
had one warm-up and 31 measured requests, interleaved with the other queries, and times
include a fresh HTTP connection and the complete TSV response. Answers were checked
against an independent traversal of the input: reachability, distances, shortest-path
counts, five simple-path lengths and every returned edge. The `+` path from person 0
reaches 89,090 nodes at 1.05M and 892,556 at 10.5M. The last person is 99,999 or 999,999,
with shortest chains of 12 and 17 edges.

| query | what it does | 1.05M min | 1.05M median | 10.5M min | 10.5M median |
|---|---|---:|---:|---:|---:|
| `reach-plus` | `COUNT` of `ex:0 foaf:knows+ ?x` | 12.25 | 12.77 | 133.75 | 139.06 |
| `ps-any` | `COUNT` of one shortest path to every reachable person | 17.03 | 17.38 | 243.84 | 257.02 |
| `ask-pair` | `ASK { ex:0 foaf:knows+ ex:last }` | 10.98 | 11.56 | 129.86 | 131.24 |
| `ps-pair` | the shortest path between the same pair, one row per edge | 0.92 | 1.03 | 3.03 | 3.24 |
| `ps-pair-all` | every shortest path between them (1 at 1.05M, 3 at 10.5M) | 0.77 | 0.86 | 2.95 | 3.08 |
| `ps-pair-both` | shortest path with `path:direction path:both` (7 or 8 edges) | 0.80 | 0.87 | 1.54 | 1.69 |
| `ps-pair-k5` | five shortest simple paths, by Yen's algorithm | 11.50 | 12.10 | 73.22 | 79.08 |
| `ps-values10` | distances from 10 people bound by `VALUES` to the last person | 2.41 | 2.81 | 14.84 | 15.25 |
| `ps-all4` | `COUNT` of every path of up to 4 edges from person 0 | 0.36 | 0.43 | 0.65 | 0.78 |
| `pp-len4` | the same count as a `UNION` of four fixed-length property paths | 0.48 | 0.59 | 0.70 | 0.83 |
| `ps-any-weighted` | `ps-any` with `path:weight`, by Dijkstra's algorithm | 94.54 | 97.52 | 1,601.75 | 1,615.50 |

Times are in milliseconds. A pair search is 11–41× faster than the property-path ASK,
because the bidirectional search stops where its frontiers meet. Searching from one
person to everyone costs 1.4–1.8× the `+` count, since it also keeps path parents and
levels, and weighted search costs 5.6–6.3× the unweighted search.

`path:excludePredicate` and `path:maxDegree` add no measurable cost to searches that use
neither parameter. In a screen of the path searches above at 10.5M, on an 8-vCPU Namespace
instance on 2026-10-23 with four A, B, B, A rounds, the geometric mean of server time with
the parameters' code against without it was 0.97, and medians of one build varied by up to
77% between server processes, so no change was above the noise. Searches backward and both
ways do not pass through literals.

### Weighted pair searches

A weighted search for the cheapest path of one pair runs Dijkstra's algorithm from both
ends, and so do the spur searches of Yen's algorithm with weights
([F07](../../specs/F07-path-search.md#weighted-searches-from-both-ends-2026-10-24)). The table
compares the build before this change with the current one on the 10.5M-triple data, once
with every edge weighing 1 and once with an `ex:km` weight from 1 to 100 on the reifier of
every `foaf:knows` triple. The pair is person 0 and the last person, and every query binds
`path:weight` and `path:cost`.

| query | weights | one end (ms) | both ends (ms) |
|---|---|---:|---:|
| `ps-pair` | all 1 | 820.2 | 3.5 |
| `ps-pair` with `path:direction path:both` | all 1 | 730.6 | 1.4 |
| `ps-pair-k5`, five cheapest paths | all 1 | fails at 10M visits | 68.9 |
| `ps-pair` | 1 to 100 | 4,937.9 | 29.0 |
| `ps-pair` with `path:direction path:both` | 1 to 100 | 11,904.4 | 51.6 |
| `ps-pair-k5`, five cheapest paths | 1 to 100 | fails at 10M visits | 656.0 |
| `ps-pair-all`, every cheapest path | all 1 | 876.2 | 896.8 |

Times are medians of the server's execution time. With every edge weighing 1 the weighted
pair search costs about 1.5 times the unweighted `ps-pair`, which took 2.3 ms in the same
runs. Weighted `path:allShortest` without `path:maxLength` still runs from the source
alone and measured 2.3% slower, although its search did not change, which item 276 covers.
[Roadmap](../../ROADMAP.md) item 277 would run it from both ends. Between equally cheap paths the two builds returned different ones in one case, which
the spec allows, and every other answer matched.

A weighted shortest search with `path:maxLength 6`, as the UI's path finder sends, failed
in both builds at its limit of a million visits, because no path of this pair has fewer
than 17 edges and the search lists paths by cost until one is short enough. Item 275
addresses it. The unweighted `ps-pair`, `ps-pair-all`, `ps-pair-k5` and `ps-values10`
measured 3 to 4% slower in every round although the change does not touch their search,
and item 276 looks for the cause.

*Measured on forge, an Intel Core i5-13500, on 2026-10-24, with the server on one core and
the client on another, in interleaved A, B, B, A rounds. With all weights 1 there were four
rounds of 20 requests per query, or one round of three for the five cheapest paths. With
weights from 1 to 100 there were two rounds of five, or one round of three.*

<a id="graph-algorithm-polling"></a>

### Graph algorithms

The ignored `polling_cost` test of `tests/graph_algorithms_scale.rs` runs each algorithm
on a random graph with 1,000,000 nodes and 5,000,000 edges, pinned to four cores, under a
far deadline so that every cancellation check reads the clock. Each run is the fastest of
three calls, and the table gives the median of eight runs with the slowest and fastest.

| Algorithm | Threads | Time (ms) | Range |
|---|---:|---:|---|
| PageRank, 20 iterations | 1 | 505.5 | 488.0–523.7 |
| Weakly connected components | 1 | 142.8 | 140.8–145.2 |
| Strongly connected components | 1 | 219.9 | 211.6–265.1 |
| Breadth-first search | 1 | 117.5 | 111.1–128.0 |
| Label propagation | 1 | 704.4 | 700.1–711.5 |
| Triangle count | 1 | 494.0 | 469.4–500.0 |
| K-core | 1 | 251.8 | 224.5–252.9 |
| FastRP | 1 | 2310.0 | 2123.9–2313.3 |
| PageRank, 20 iterations | 4 | 161.6 | 153.0–167.3 |
| Weakly connected components | 4 | 40.2 | 38.9–40.5 |
| Strongly connected components | 4 | 221.6 | 189.3–245.9 |
| Breadth-first search | 4 | 35.3 | 33.9–36.8 |
| Label propagation | 4 | 183.4 | 182.5–184.8 |
| Triangle count | 4 | 241.5 | 237.0–250.1 |
| K-core | 4 | 252.3 | 251.4–254.5 |
| FastRP | 4 | 1009.4 | 1003.0–1015.1 |

*Measured on forge.*

Every long loop checks for cancellation and its deadline within bounded work, at most once
per 64K units, and charges its workspace to the call. Against a build without these checks,
label propagation, triangle counting, K-core and FastRP moved by −0.4% to +1.4%, within the
noise of the measurement. Breadth-first search charges at most 9.1 MB at its peak on this
graph, and a unit test keeps it under 16 bytes a node on a graph of 100,000 nodes of degree
50. The graph is uniform random, so it has no node of very high degree.
