# U09: A path finder between two entities

> **Status:** implemented
>
> **Phases:** Phases 1 and 2 shipped on 2026-10-23 and Phase 3 on 2026-10-24. Phase 1 adds
> two parameters to F07's path search, excluded predicates and a degree limit for the nodes
> a search passes through. Phase 2 is the path finder in Explore, which draws the shortest
> path or the k shortest paths between two entities and animates them, with predicate,
> direction and length filters. Phase 3 adds weights from reifiers and draws the paths on
> the query page's graph results, in the canvas and in the WebGL view.
>
> **User docs:** [API: Path search](../API.md#path-search) ·
> [Usage: Finding paths](../USAGE.md#finding-paths) · [Features](../visualization/README.md#web-ui)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This design was written from the Sparkles code and specs: path search
([F07](F07-path-search.md)), the Explore graph and its time slider
([U01](U01-time-slider.md)), large-graph rendering ([U06](U06-large-graph-rendering.md)),
schema discovery ([C02](C02-schema-discovery.md)) and graph and triple access control
([C12](C12-graph-access-control.md), [C12b](C12b-triple-access-control.md)). It also used
J. Y. Yen's "Finding the k Shortest Loopless Paths in a Network" (Management Science,
1971), which F07 already implements, and the WAI-ARIA Authoring Practices for the
combobox and listbox patterns.

## 1. Summary, goals, non-goals

F07 finds paths through `SERVICE path:search`, in four modes and over chosen predicates and
directions, and the MCP tool `find_paths` offers it to agents. A person in the UI can use it
only by writing the service call by hand. This spec gives Explore a path finder. The person
picks two entities, and the shortest path or the k shortest paths between them are drawn on
the graph, one after another, with the predicates and lengths the person allowed.

Real graphs make a naive path search useless, because almost every pair of entities is two
steps apart through `rdf:type` and a class, or through a hub such as a country. F07 can
list the predicates to follow, but a person rarely knows the full list, and it cannot skip
hubs. Phase 1 therefore adds two small parameters to F07 before the UI is built.

**Goals**

1. Paths between two entities in at most three actions from Explore.
2. Shortest, all shortest and k shortest paths, with the paths drawn in order and
   animated edge by edge.
3. Filters that make results meaningful: predicates to follow or to skip, direction, a
   length range, and a limit on the degree of the nodes a path passes through.
4. Bounded cost. Every search runs under a visit limit and a timeout, and the browser never
   receives more than the paths it draws.
5. The search sees only what the caller may read, at the state Explore shows.

**Non-goals**

- Paths over the whole result of a query, between sets of sources and targets. F07 does
  that in SPARQL, and the editor is one click away.
- Weighted routing over geometry, such as road networks. G01 and the map view handle
  spatial questions.
- Explaining why a path is interesting. The path finder shows paths, not rankings
  beyond length and cost.

## 2. User-visible behaviour

### 2.1 Starting a search

Explore's graph toolbar gets **Paths**, with the shortcut `Shift+P`. It opens a panel in
place of the properties panel.

```
┌─ Paths ───────────────────────────────────────────────────────────────────────┐
│ From  [ex:alice "Alice"         ▾]   To  [ex:lyon "Lyon"          ▾]  [⇄]      │
│ Mode  (•) shortest  ( ) all shortest  ( ) k shortest  k [3 ▾]                  │
│ Length 1 ━━━●━━━━━ 6     Direction  [both ▾]     Skip hubs over [1,000 ▾]      │
│ Follow  (•) every predicate except: rdf:type ✕  owl:sameAs ✕  [+]              │
│         ( ) only: [+ predicate]                                                │
│ [Find paths]                                        at commit 318 · main        │
├───────────────────────────────────────────────────────────────────────────────┤
│ 3 paths · 0.4 s · 12,840 nodes visited                                         │
│ ▶ 1  length 2   alice ─worksFor→ acme ─locatedIn→ lyon                         │
│   2  length 3   alice ─knows→ bob ─worksFor→ beta ─locatedIn→ lyon             │
│   3  length 4   …                                                              │
│ [Open in editor]   [Add to graph]   [Clear]                                    │
└───────────────────────────────────────────────────────────────────────────────┘
```

- **From and To.** Each is a combobox over Explore's label search, which also accepts a
  pasted IRI. With two nodes selected in the graph, **Paths** fills both. A node's context
  menu has **Paths from here** and **Paths to here**. `⇄` swaps them.
- **Mode.** Shortest returns one path. All shortest returns every path of the shortest
  length, up to 50. K shortest returns up to `k` loopless paths in order of length, with
  `k` from 2 to 20 and 3 by default.
- **Length.** A range from 1 to 8 edges, 6 by default as the upper bound. All shortest
  and shortest use only the upper bound.
- **Direction.** Both by default, so a path can follow a triple against its direction.
  Forward and backward follow the stored direction.
- **Follow.** Either every predicate except a list, which starts with `rdf:type` and
  `owl:sameAs`, or only a list. The predicate picker is the schema picker of U08 §2.2, under
  the caller's view.
- **Skip hubs.** Paths do not pass through a node with more edges than the limit, which
  is 1,000 by default and can be off. The ends are never skipped.

### 2.2 Drawing the paths

**Find paths** draws the paths into the Explore graph. The path nodes that are already
drawn keep their positions, and new nodes are placed along a line between their
neighbours on the path, then relaxed by the incremental layout.

- **Animation.** Path 1 is drawn first, edge by edge at 250 ms per edge, then each next
  path in order. A path's edges carry its number. Under reduced motion all paths appear at
  once with their numbers.
- **Focus.** Choosing a path in the list, or pressing its number key, highlights its nodes
  and edges and dims the others to 30% opacity. **All** removes the highlight.
- **Shared parts.** An edge on several paths is drawn once with all their numbers.
- **Add to graph.** The paths become ordinary expanded elements of the Explore graph and
  the panel closes. Until then they are an overlay that **Clear** removes.
- **Open in editor.** The query page opens with the SPARQL that the panel ran, as §3.2
  shows, so the person can change it.

### 2.3 Keyboard

| Key | Action |
|---|---|
| `Shift+P` | Open the panel, with focus in **From**. |
| `Enter` in the panel | Find paths. |
| `1` to `9` | Highlight that path while focus is in the list or the graph. |
| `A` | Highlight all paths. |
| `↑`, `↓` in the list | Move between paths, and the graph follows. |
| `Escape` | Clear the highlight, then close the panel. |

The list is a listbox, each option's text names the path in words, such as "path 1, length
2, Alice works for Acme, Acme located in Lyon", and a polite live region announces "3 paths
found, the shortest of length 2". The graph's elements of a path are also listed in the
path's row, so the graph is never the only place the path can be read.

### 2.4 The 390 px layout

The panel is a full-height sheet above the graph with **From**, **To**, **Mode** and
**Find paths** visible and the filters folded under **Filters**. After a search, the sheet
shrinks to the path list's first row, and swiping it up shows the rest. Tapping a row
highlights the path and fits it to the screen. Long-pressing a node offers **Paths from
here** and **Paths to here**.

### 2.5 Empty and error states

| Case | What the panel shows |
|---|---|
| No path within the length | "No path of up to 6 edges", with **Allow 8** when the bound is lower than 8. |
| The visit limit was reached | "The search stopped after visiting 1,000,000 nodes", with the advice to skip more predicates or lower the hub limit. |
| The timeout | "The search took longer than 10 s", with the same advice. |
| The same entity twice | **Find paths** is disabled with "Choose two different entities". |
| A literal as the target | Allowed, since F07 lets a literal end a path. A literal cannot be the source, and the combobox does not offer one. |
| A server without Phase 1 | Excluded predicates and the hub limit are disabled with the reason, and the panel searches with an explicit predicate list only. |

## 3. Server and API changes

### 3.1 Phase 1, two new path search parameters

F07's `SERVICE path:search` gains two parameters.

| Parameter | Value | Default | Meaning |
|---|---|---|---|
| `path:excludePredicate` | an IRI, repeatable | none | Triples with this predicate are not edges. It cannot be combined with `path:predicate`. |
| `path:maxDegree` | a positive integer | none | A search does not expand a node whose degree in the projection is above the value. The source and the target are always expanded. |

The degree counts the edges the search could follow from the node in the call's direction,
after predicate selection, so it matches what the search would otherwise visit. It is read
from the index with one count per node, which the search already performs when it lists a
node's edges, so the check adds no extra scan. A node it skips counts as visited. EXPLAIN's
`PathSearch` counters gain `skippedHubs`. Both parameters work in every mode and with
`GRAPH ?g`, and `find_paths` in MCP gains them as `excludePredicates` and `maxDegree`.

The two parameters change the meaning of a path in the call. A path through a skipped hub
is not found, so the shortest path the call reports is the shortest path that avoids hubs.
F07 §4.2's mode definitions apply to the graph without the skipped nodes, and the F07 spec
gains a section saying so.

### 3.2 What the UI runs

The panel runs one query through `/{ds}/sparql`, with Explore's `at` and branch:

```sparql
PREFIX path: <urn:x-sparkles:path#>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
SELECT ?path ?i ?s ?p ?o ?len WHERE {
  SERVICE path:search {
    [] path:source <http://example.org/alice> ; path:target <http://example.org/lyon> ;
       path:algorithm path:kShortest ; path:k 3 ; path:maxLength 6 ;
       path:direction path:both ; path:maxDegree 1000 ;
       path:excludePredicate rdf:type , owl:sameAs ;
       path:maxVisited 1000000 ; path:limit 20 ;
       path:pathIndex ?path ; path:edgeIndex ?i ;
       path:edgeSubject ?s ; path:edgePredicate ?p ; path:edgeObject ?o ; path:length ?len .
  }
}
ORDER BY ?path ?i
```

Labels for the nodes come from the label lookup Explore already batches. No new route is
needed in Phase 2. Phase 3 adds `path:weight` from a picker of numeric reifier properties,
shown as a cost next to the length.

## 4. Limits and budgets

- **Search.** `path:maxVisited` is 1,000,000 from the UI, a tenth of F07's default, and the
  request timeout is 10 s. Both appear in the panel's footer, and a server administrator's
  lower limits win.
- **Rows.** The query returns at most `path:limit` paths of at most 8 edges, so at most 400
  rows for all shortest at 50 paths, and at most 160 for k shortest at 20.
- **Drawing.** Path overlays add at most 400 nodes to the Explore graph, inside its existing
  cap of 1,500 nodes. When the graph would pass that cap, the panel draws the paths alone in
  a fresh graph and says so.
- **One search at a time.** A new search cancels the previous request.

## 5. Access, branches and past states

The search runs as the caller under F07 §4.6, on the masked view of C12 and C12b, so a path
never crosses a triple the caller cannot read and its length reveals nothing about hidden
triples. The degree of `path:maxDegree` is counted in the same view, so a hidden triple does
not make a node a hub. The panel's header names the commit and branch of the search. When
U01's slider moves, the panel offers **Search again at commit N** instead of re-running on
its own, because a path search can cost seconds and the slider steps quickly. Compare mode
of U01 marks path edges that exist at only one of the two states with the slider's added and
removed marks.

## 6. Architecture

| Part | Where |
|---|---|
| New parameters | `crates/sparkles-core/src/sparql/pathsearch.rs`, the plan counters, and the MCP `find_paths` tool |
| Query building, row parsing, path grouping | `ui/src/lib/paths.ts` (new) |
| Panel | `ui/src/lib/components/PathPanel.svelte` (new) |
| Overlay and animation | `GraphView.svelte` gains `overlay` elements with a path number and an `order` for the animation |
| Explore wiring | `routes/explore/+page.svelte` |
| Mock server | `ui/mock/` answers `path:search` queries from a fixture graph with a small BFS and Yen's search, because the mock's SPARQL engine does not know the service |

## 7. Phases

1. **Phase 1.** `path:excludePredicate` and `path:maxDegree` in the engine, EXPLAIN and
   MCP, with the F07 section that defines them.
2. **Phase 2.** The panel, the overlay with animation, the keyboard and list alternative,
   the phone sheet, **Open in editor** and **Add to graph**.
3. **Phase 3.** Weights from reifiers, the panel on the query page's graph results, and
   path overlays in the WebGL view of U06.

## 8. Acceptance examples

The data is U03 §7's, with `ex:alice ex:knows ex:bob` added and every person, organization
and city typed.

- **A1. Exclusion (Phase 1).** `path:excludePredicate rdf:type` between `ex:alice` and
  `ex:bob` with direction both returns the path through `ex:knows`, of length 1, and no path
  through `ex:Person`.
- **A2. Hubs (Phase 1).** With 2,000 people located in `ex:lyon` and `path:maxDegree 1000`,
  no path between two of them passes through `ex:lyon`, and EXPLAIN reports `skippedHubs`
  of at least 1.
- **A3. Exclusive parameters (Phase 1).** `path:excludePredicate` with `path:predicate` in
  one call is a `400` that starts with `path:search:`.
- **A4. K shortest.** From `ex:alice` to `ex:lyon` with k 3, the panel lists paths in order of
  length, the first of length 2 through `ex:acme`, and the graph draws path 1 before path 2.
- **A5. Reduced motion.** With reduced motion emulated, A4's paths appear without animation
  and the wrapper's `data-animations` is 0.
- **A6. Limited reader.** For a reader whose grants hide the graph that holds
  `ex:alice ex:worksFor ex:acme`, A4 returns no path through `ex:acme`.
- **A7. Past state.** At a commit before `ex:alice ex:knows ex:bob` existed, the search
  returns no path through `ex:knows`, and the header names that commit.
- **A8. Bounded.** A search that reaches the visit limit shows the message of §2.5 within
  the timeout, and no more than 400 rows reach the browser.
- **A9. Keyboard.** A4 can be done with the keyboard alone, and the live region announces
  the result.
- **A10. Phone.** At 390 px, A4 can be done with taps alone, and the page does not scroll
  sideways.

## 9. Testing

- **Rust.** In the path search tests, both parameters in every mode, the ends exempt from
  the degree limit, the degree counted after predicate selection and direction, the
  degree under a masked view, `GRAPH ?g`, the `400` of A3, and the counter.
- **Router.** `find_paths` with the new arguments, and a path search through `/{ds}/sparql`
  under a restricted principal.
- **Vitest.** `lib/paths.ts`, for query text from every panel setting, grouping rows into
  paths, shared edges, the overlay's order, and the fallback when the server lacks Phase 1.
- **Playwright against the mock server.** A4, A5, A8 to A10 and the empty states.
- **Playwright against a real server.** A1, A4, A6 and A7.

## 10. Rejected alternatives

- **Nested patterns as the exclusion mechanism.** F07 Phase 2 accepts a nested pattern for
  edges, which could filter out `rdf:type`. The pattern would be evaluated over every triple
  of the dataset before the search starts, which is slow on large data. A parameter lets the
  search skip triples while it reads them.
- **Computing paths in the browser** from the drawn neighbourhood. The drawn graph is a small
  part of the data, so the paths would be wrong.
- **Excluding literals by default.** F07 already ends paths at literals, so they cannot be
  hubs.

## 11. Open questions

Both questions were decided before implementation, and the [Outcome](#outcome) records
how.

1. Whether the default excluded predicates should be a dataset setting under C19, so a
   dataset with a hub predicate of its own, such as `dct:subject`, can exclude it for every
   reader.
2. Whether `path:maxDegree` should count degree in both directions even when the search is
   forward only, which would make the limit easier to explain but would skip nodes that a
   forward search would pass cheaply.

## Provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

- **Spec:** [`U09-path-finder.md`](U09-path-finder.md), written on 2026-10-23 independently from:
  - J. Y. Yen, "Finding the k Shortest Loopless Paths in a Network", Management Science, 1971;
  - the WAI-ARIA combobox and listbox patterns;
  - the Sparkles code (`pathsearch.rs`, `GraphView`, Explore) and specs C02, C12, C12b, F07,
    U01 and U06.
- **Implementation:** not built. No new dependency is planned.
- **Rejected** (spec §10):
  - nested patterns as the exclusion mechanism;
  - paths computed in the browser;
  - literals excluded by default.

## Outcome

Phases 1 and 2 shipped on 2026-10-23 and Phase 3 on 2026-10-24, which completes
[ROADMAP](../ROADMAP.md) item 103.

**Decisions.** The default excluded predicates are a dataset setting, as question 1
proposed. C19 gained the settings kind `paths`, kept in `paths.json` at version 1, whose
one field `excludedPredicates` defaults to `rdf:type` and `owl:sameAs` and holds at most 50
absolute IRIs without duplicates. The panel reads it when it opens and uses it until the
person edits the list. MCP's `find_paths` does not read it, because an agent names its
own filters. For question 2, the degree counts the edges in the call's direction, as §3.1
says, so a forward search does not skip a node that has many incoming edges.

**Phase 1.** F07 §4.9 defines `path:excludePredicate` and `path:maxDegree`, and
[F07's Outcome](F07-path-search.md#skipped-predicates-and-hubs-2026-10-23) describes
how the engine reads them. The degree comes from the index's counts with no extra scan
when every quad is in the active graph, and from at most `maxDegree + 1` edges of the
node otherwise, in both cases in the caller's view. While testing the hub limit, a probe
found that searches backward and both ways passed through literals, against F07 §4.1 and
§10 of this spec. That is fixed in the same change, so a literal now ends a path in every
direction.

**Phase 2.** The **Paths** button of Explore's toolbar, `Shift+P`, the node menu on a
right click or long press, and the **Paths from here** and **Paths to here** buttons of the
side panel open the panel. The node menu stands in for the selection of two nodes, and the
panel takes the previous and the current selection as its ends. The ends are comboboxes
over Explore's search that also take a pasted IRI, prefixed name or quoted literal. The
panel runs the query of §3.2 at Explore's commit of `main`, since Explore reads no other
branch, lists the paths as a listbox,
and overlays them on the graph with an order on each node and edge, animated one step per
250 ms unless reduced motion is set. **Open in editor** opens the query in the query page,
and **Add to graph** keeps the paths' nodes in the drawn graph. Above 1,500 drawn nodes the
paths are drawn alone. At 390 px the panel is a bottom sheet that folds to a handle.

**Tests.** The Rust tests are listed in F07's Outcome. The router tests
`paths_stay_inside_the_readers_view`, `degrees_are_counted_in_the_readers_view` and
`malformed_filters_are_a_400` cover A3 and A6 on the server, and
`find_paths_skips_predicates_and_hubs` covers MCP. `ui/src/lib/paths.test.ts` has 16
Vitest tests, and `ui/tests/mock/paths.spec.ts` runs A4, A5 and A7 to A10, the empty states
and **Open in editor** against the mock server, whose path search is a breadth-first
search with Yen's algorithm for k shortest. The Playwright tests against a real server
that §9 lists for A1, A4, A6 and A7 came with Phase 3, which the Phase 3 tests below
describe. The Rust and router tests cover A1, A2, A3 and A6 on the engine.
`mise run docs:screenshots` writes `docs/images/path-finder.png`, the three shortest paths
between two people of the demo dataset, which the README shows.

**Measurements.** A Namespace screen of the path searches that use neither new parameter
found no change above the noise, with a geometric mean of 0.97 for the new build over the
old one. [BENCHMARKS](../BENCHMARKS.md#path-search) has the
result. A forge A/B is a follow-up after the merge.

**Phase 3.** The engine needed no change, because F07 already weighs edges by a reifier
property with `path:weight` and binds a path's sum with `path:cost`, and its Dijkstra and
Yen searches already honour the Phase 1 filters. The Rust test
`weights_with_skipped_predicates_and_hubs` checks that they combine. The panel gained a
**Weight** select whose options come from one query for the properties that reifiers carry
with numeric values of zero or more, grouped and counted, at most 20. That query reads every
reifier, so the list loads when the select is first focused rather than when the filters
open, and again at a new dataset or state. With a weight chosen the panel's query adds
`path:weight` and `path:cost`, the rows show each path's cost, and the announcement names
the cheapest. On the query page the **Paths** button of the graph toolbar and `Shift+P`
open the same panel beside the result graph, at the result's state and branch. A node menu
on the canvas offers **Paths from here** and **Paths to here**, and in the WebGL view the
selected node fills **From**. The paths are matched to the result's edges by their triples,
and the nodes and edges a path needs that the result lacks are added while the paths are
shown, placed between their neighbours on the path. A literal gets a node per triple, as
the result graph draws literals, so two triples that end in the same literal are marked
apart. The WebGL view draws path edges thicker in the path colour, forces the labels of
path nodes, dims the rest when one path is focused and draws the path numbers at the
edges' midpoints, at most 400 of them. Showing or clearing paths updates the WebGL picture
in place, so its layout and camera stay where they were. The keys of §2.3 work with the
focus on the graph as well as in the list, and **Open in editor** opens the query in a new
tab of the same page, since the query page is already open.

**Phase 3 measurements.** A Namespace screen found the path searches unchanged, with a
geometric mean of 0.93 for the new build over the old one and no query above the noise,
since the engine did not change. It also timed the weighted searches the panel now sends.
A weighted pair search takes 0.68 s on the 10.5M-triple data against 1.2 ms unweighted,
and the five cheapest paths stop at the default limit of 10 million visited nodes.
[BENCHMARKS](../BENCHMARKS.md#weighted-pair-searches) has the figures.

**Weighted searches from both ends.** ROADMAP item 150 made the weighted search of one
pair and Yen's weighted spur searches run from both ends, as
[F07](F07-path-search.md#weighted-searches-from-both-ends-2026-10-24) describes. On forge
the weighted pair search now takes 3.5 ms on the 10.5M-triple data and the five cheapest
paths 69 ms. The panel always sends `path:maxLength`, so its weighted shortest search lists
paths by cost until one is short enough. It answers quickly when the cheapest path is
within the limit, and a pair whose short paths are all dear still runs to the panel's
limit of a million visits, which ROADMAP item 275 addresses.

**Phase 3 decisions.** The spec left these open, and each was decided here. The weight
picker offers reifier properties only, with no free IRI field, since a weight that no
reifier carries makes every edge weigh 1. The WebGL view does not animate the paths,
because its layout runs in a worker and the overlay is redrawn by sigma's reducers, so the
paths appear at once as with reduced motion. The query page offers no **Add to graph**,
because a result graph is the query's answer, and a new result or closing the panel
removes the paths and the nodes they added. On the real server the default graph is not the
union of the named graphs, so A6 hides `ex:alice ex:worksFor ex:acme` from the reader with
a C12b protection on `ex:worksFor` for members of `ex:Founder` rather than with a graph
grant.

**Phase 3 tests.** `ui/src/lib/paths.test.ts` has 20 Vitest tests, four of them for the
weight, and `ui/src/lib/path-overlay.test.ts` has 9 for merging paths into a drawn result and placing the nodes they add.
`ui/tests/mock/paths-phase3.spec.ts` checks the weighted search in Explore, a weight list
that follows the state and drops a weight the state lacks, the panel on the query page's
canvas with the added nodes, the node menu, **Open in editor** and the reset on a new
result, the WebGL overlay's edge count with and without a focused path, the keys with the
focus on the graph, a layout that does not restart, and the phone sheet at 390 px with no
sideways scroll. The mock's path search enumerates simple paths by cost
when a weight is set. `ui/tests/e2e/paths.spec.ts` runs A1, A4, A6 and A7 against the real
server on a dataset loaded in two commits. `mise run docs:screenshots` writes
`docs/images/query-paths.png`, the three shortest flight routes from Vancouver to Auckland
by distance over a query's graph, which leads the README's path entry, and
`path-finder.png` moved to the README's further screenshots.
