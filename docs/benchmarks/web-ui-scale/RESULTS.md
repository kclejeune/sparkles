# Web UI at scale results

This report compares the web UI before and after the work of ROADMAP item 725. The older
build is the UI of 4ab93ecb and the newer is the UI of a199a8ae. Both ran in turn against the
same release server, built from 4ab93ecb, with the same data and the same Chromium, three
repetitions each, and every number is the median of the three. The
[methodology](README.md) describes the harness, the synthetic taxonomy and the limits of the
runs. The raw records, the generated tables of the harness and the commands are in the
[retained run](runs/2026-11-04-stanley-4ab93ecb-a199a8ae) and the
[run catalog](RUNS.json).

The host was a laptop that ran other work, with a one-minute load average between 2 and 9
at the start of the older build's runs and between 3 and 12 at the start of the newer build's.
WebGL ran in software. The medians of cases that the same state gives twice, such as the
older build's Explore frames at 1,543 nodes, differ by more than the load explains, so a
ratio under about 1.5 in a frame time is not a finding.

## Opening the editor

The older build read every label, every axiom of the subclass kind and the entity lists
before it drew a row, and the tree waited for the server's ontology summary. The newer build
draws the first level of the tree from the hierarchy route and reads names for the rows in
view.

| Case | First row | Requests | Decoded bytes | JS heap | DOM nodes |
|---|---|---|---|---|---|
| ChEBI lite, older | 9,892 ms | 23 | 68.3 MB | 164 MB | 19,589 |
| ChEBI lite, newer | 902 ms | 9 | 0.17 MB | 5.9 MB | 2,033 |
| Taxonomy of 300,000 classes, older | 6,117 ms | 24 | 74.2 MB | 162 MB | 1,288 |
| Taxonomy of 300,000 classes, newer | 415 ms | 10 | 0.01 MB | 4.8 MB | 1,287 |
| UNESCO Thesaurus, older | 432 ms | 14 | 0.8 MB | 11.6 MB | 16,550 |
| UNESCO Thesaurus, newer | 355 ms | 14 | 0.8 MB | 4.9 MB | 1,718 |

The newer build does not wait for the summary, which takes 7.5 s of the 8.5 s until the
network goes quiet on ChEBI lite, so the page head shows its counts as loading until they
arrive. The older build waited for the same route for 7.8 s. The reads that the newer build
no longer makes at open are in the next table.

| ChEBI lite, route | Older requests | Older decoded | Newer requests | Newer decoded |
|---|---|---|---|---|
| Axioms | 6 | 14.6 MB | 0 | 0 |
| Entities | 5 | 3.3 MB | 0 | 0 |
| SPARQL label reads | 3 | 50.2 MB | 3 | under 0.01 MB |
| Hierarchy | 4 | 0.2 MB | 1 | 0.2 MB |

On the taxonomy of 2.6 million classes the older build's server was killed at its 11 GiB
memory cap while the page made these reads, so it has no number. The older build on the
taxonomy of 1 million classes showed no row within 240 to 300 s.

## Selecting a class

A link to a class opens the path to it through the ancestors that the hierarchy route gives.

| Case | First row | Requests | Decoded bytes | JS heap | DOM nodes |
|---|---|---|---|---|---|
| Water in ChEBI lite, older | 2,661 ms | 63 | 68.6 MB | 197 MB | 226,955 |
| Water in ChEBI lite, newer | 363 ms | 34 | 0.25 MB | 7.3 MB | 3,421 |
| Deepest class of 300,000, older | 2,494 ms | 108 | 74.3 MB | 196 MB | 203,665 |
| Deepest class of 300,000, newer | 401 ms | 57 | 0.06 MB | 6.2 MB | 2,514 |
| Class at row 11,000 of a level, older | 4,837 ms | 56 | 78.1 MB | 303 MB | 432,437 |
| Class at row 11,000 of a level, newer | 917 ms | 31 | 2.0 MB | 16 MB | 2,388 |

The newer build opens a class at row 47,000 of a level of 100,000 children in the taxonomy of
1 million classes with a first row at 7.4 s and an interactive page at 15.7 s, 67 requests,
8.4 MB and a heap of 38 MB. It reads the 47 pages in between, one request each, since the
hierarchy route pages from the start of a level. The older build cannot open that taxonomy.

## Scrolling through 10,000 rows

The case reads one family to 10,038 rows by **Show more** and scrolls down and back.

| Taxonomy of 300,000 classes | Older | Newer |
|---|---|---|
| Rows in the page | 10,038 | 30 |
| DOM nodes | 411,869 | 2,557 |
| JS heap | 293 MB | 16 MB |
| Frame time, median | 33.3 ms | 16.7 ms |
| Frame time, 95th percentile | 50 ms | 16.7 ms |
| Longest frame going down | 66.7 ms | 16.8 ms |

The newer build gives the same 16.7 ms median and 95th percentile on the taxonomy of 1
million classes, with a longest frame of 50 ms going up.

## Explore

The case expands the nodes that the page draws, in turn, in a 90-ary tree. The older build
stops at 1,500 nodes and asks the person to clear some. The newer build holds up to 100,000
nodes and draws at most 5,000 of them, in WebGL past 1,000 visible nodes.

| Case | Held and drawn nodes | Expansion, median | Pan frame, median | Pan frame, 95th | Requests | JS heap |
|---|---|---|---|---|---|---|
| Target 1,000, older | 1,000 and 1,000 | 138 ms | 83 ms | 150 ms | 34 | 23 MB |
| Target 1,000, newer | 1,000 and 1,000 | 64 ms | 67 ms | 67 ms | 56 | 23 MB |
| Target 5,000, older | 1,543 and 1,543 | 175 ms | 17 ms | 100 ms | 49 | 32 MB |
| Target 5,000, newer | 5,053 and 5,000 | 164 ms | 350 ms | 450 ms | 236 | 84 MB |
| Target 20,000, older | 1,543 and 1,543 | 177 ms | 100 ms | 100 ms | 49 | 31 MB |
| Target 20,000, newer | 16,303 and 5,000 | 390 ms | 150 ms | 250 ms | 752 | 85 MB |

The newer build stopped the 20,000 case at 16,303 held nodes, because the tree had no
expandable node left. The main thread spent 208 s in long tasks over the 183 expansions of
that case, which is about 1.1 s for each, against 7.8 s over the 19 expansions of the older
build. Each expansion rebuilds the drawn graph from the held graph, and that work grows with
the held graph.

The frame times of the newer build at 5,000 drawn nodes were measured with software WebGL in
headless Chromium and say little about a graphics card. They need a measurement on a graphics
card before they are treated as real. They show that the layout and the drawing compete for the
processor when no graphics card is present.

A resource with 20,000 neighbours is one group node in the newer build, so the first graph
holds 4 nodes where the older build drew 153 of the neighbours, and it appears in 320 ms
against 566 ms.

## The ontology summary route

The summary route maps the whole graph to axioms before it answers, and the first call after
the server starts pays for it. The times are of `GET /$/ontology/{ds}` and include the
server's resident memory after the first call.

| Dataset | Cold | Warm | Server memory |
|---|---|---|---|
| ChEBI lite | 7.25 s | 0.05 s | 2.5 GB |
| Taxonomy of 300,000 classes | 3.09 s | 0.02 s | 1.3 GB |
| Taxonomy of 1 million classes | 13.9 s | 10.6 to 11.7 s | 4.1 GB |

The result is not kept at the size of 1 million classes, so every call pays the mapping. Items
NEW-ITEM-1 and NEW-ITEM-2 of the [roadmap](../../ROADMAP.md) answer the head's counts without
the mapping and speed up the mapping itself.

## Tradeoffs

The newer build keeps a list of rows for each open tree, about the size of the open nodes'
ids, and a map of the labels it has read, in exchange for a page whose size does not grow
with the tree. The heap stays between 5 and 38 MB in the cases above. The held graph of
Explore costs about 5 KB of heap for each node, which is 85 MB at 16,303 nodes, and it buys
filters and budgets that change the picture at once without a read. Each expansion also
asks the store for the size of each predicate's fan-out, which raises the requests of the
1,000-node case from 34 to 56.

Explore follows two accepted slowdowns that have roadmap items. The cost of an expansion as
the held graph grows is item NEW-ITEM-5, and the WebGL frame time without a graphics card is
item NEW-ITEM-6. The page-by-page read of a class far into a wide level is item NEW-ITEM-7.
