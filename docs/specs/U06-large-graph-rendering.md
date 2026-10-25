# U06: Rendering large graphs

> **Status:** partly implemented
>
> **Phases:** Phases 1 and 2 shipped on 2026-10-23. Phase 1 is a WebGL graph view with
> layout in a web worker, used for graph results above the current limits, and a
> class-level overview drawn from the existing class profiles. Phase 2 adds server-side
> aggregation by community and by property with semantic zoom into groups. Phase 3, which
> adds layouts computed and cached on the server and a compact binary transfer format, is
> not built.
>
> **User docs:** [API: Graph overview](../API.md#graph-overview) ·
> [Usage: The Explore overview](../USAGE.md#the-explore-overview) ·
> [Features](../visualization/README.md#web-ui)
>
> This is the design as written before implementation, with the amendment of §2.3 and the
> answers of §11. The [Outcome](#outcome) section at the end records how it landed.

This design was written from the public documentation of sigma.js (version 3) and
graphology, the ForceAtlas2 paper (Jacomy et al., "ForceAtlas2, a Continuous Graph Layout
Algorithm for Handy Network Visualization Designed for the Gephi Software", PLOS ONE,
2014), the public documentation of Cytoscape.js and cytoscape-fcose, which the UI uses
today, the public documentation of cosmos.gl (GPU force layout) and Cosmograph, the
WebGL 2 specification, and the Sparkles code and specs: class profiles in schema
discovery ([C02](C02-schema-discovery.md)), the graph-algorithm library
([F14](F14-graph-algorithms.md)), budgets ([C01](C01-observability-and-budgets.md)),
graph access control ([C12](C12-graph-access-control.md)) and streamed execution
([X05](X05-streaming-query-execution.md)).

## 1. Summary, goals, non-goals

The UI draws graphs with Cytoscape.js on a canvas and lays them out with fcose
(`GraphView.svelte`). That suits the sizes it is used for: Explore expands up to 100
outgoing and 100 incoming edges per node (`lib/explore.ts`), and the query page's graph
view caps a result at a chosen 100 to 2,500 nodes (`maxNodes`, 400 by default) and says
"Showing N of M nodes" beyond it. A canvas renderer with a force layout on the main
thread becomes slow at a few thousand elements, so a result with 50,000 nodes, or a
whole dataset, cannot be looked at.

This spec adds a second renderer for large graphs and a way to look at a graph too large
to send: the server groups nodes by class or by community, the view draws the groups, and
zooming into a group fetches its members. The existing view stays for small graphs.

**Goals**

1. Draw at least 100,000 nodes and 300,000 edges at interactive frame rates (30 frames
   per second while panning) on an integrated laptop GPU.
2. Keep the main thread free: layout runs in a web worker, or on the server.
3. A first picture of any dataset, at any size, within two seconds: its classes and the
   predicates between them.
4. Semantic zoom: a group opens into its members or its sub-groups, and closes again,
   with the rest of the picture kept in place.
5. Use the same selection, focus and **Open in Explore** actions as the small view.
6. Respect budgets and grants: aggregates are computed under the caller's view.

**Non-goals**

- Replacing Cytoscape for small graphs. Its styling, labels and incremental layout are
  better at Explore's sizes.
- Edge bundling, 3D views and animation of layouts over time. U01's slider works on the
  small view.
- Computing communities. F14 computes them, and this spec reads its written-back output.
  F14 also lets the small view colour nodes by community, which this spec does not
  change.
- Graph editing.

## 2. User-visible behaviour

### 2.1 Choosing the renderer

The query page's graph view and Explore choose the renderer from the element count:
Cytoscape up to 2,000 nodes, the WebGL view above. A switch in the graph toolbar
overrides the choice, and the `maxNodes` select gains 10,000, 50,000 and 100,000, which
offer the WebGL view only. Without WebGL 2 the view falls back to Cytoscape with the
current truncation notice and the reason.

### 2.2 The large view

```
┌─ Graph · 84,213 nodes · 251,904 edges ──────── layout ▶ running 62% ▣ [Pause] ──┐
│ Colour by: class ▾   Size by: degree ▾   Labels: auto ▾   [Fit]  [Search…]       │
│                                                                                   │
│        ·  ·::·.        ●Person (12,402)         ...                               │
│     .:::::::::.  ─────────────●Org (840)                                          │
│       ':::::'                                                                     │
├───────────────────────────────────────────────────────────────────────────────────┤
│ Selected: ex:acme "Acme Corp" · Org · degree 312   [Open in Explore] [Neighbours] │
└───────────────────────────────────────────────────────────────────────────────────┘
```

- **Layout.** ForceAtlas2 runs in a worker and the picture settles while the person
  looks. **Pause** stops it. A layout already computed for the same result (§4.4) is
  shown at once.
- **Level of detail.** Labels are drawn for the largest nodes on screen and for the
  hovered and selected ones, never overlapping. Edges are hidden while panning above
  50,000 visible edges and drawn again when the view stops.
- **Colour and size.** Colour by class, by community (when F14 results exist) or by
  graph. Size by degree, or by a numeric property such as F14's PageRank score.
- **Search.** Finds nodes in the drawn graph by label or IRI, centres and selects the
  match.
- **Selection.** Clicking a node selects it and highlights its neighbours.
  **Neighbours** draws them in the small view in Explore.

### 2.3 The overview and semantic zoom (Phase 1 and 2)

Explore gets an **Overview** tab next to Graph, Schema and Search. It opens on the class
level:

- **Class level (Phase 1).** One node per class with its instance count, sized by it,
  and one edge per predicate between two classes, weighted by triples. It is built from
  `GET /$/schema/{ds}/profiles`, which already reports for each class the predicates its
  instances use and the classes of their values. No new server route is needed.
- **Groups (Phase 2).** Double-clicking a class, or pressing `Enter` on it, opens it.
  With fewer than 5,000 instances, its members are drawn in place with their links. With
  more, it opens into communities (when F14 results exist for the class) or into buckets
  by a chosen property's values, each a group node that opens the same way.
- **Breadcrumb.** A breadcrumb shows the path (`Person › community 12 › …`). `Backspace`
  or clicking a crumb closes the groups below it.
- **Mixed levels.** Open groups and closed groups are drawn together. Edges between an
  open member and a closed group are aggregated to the group with their count.

**Amendment (user request, 2026-10-23).** The overview is what Explore shows first when no
resource is selected, in place of the former "Start from a resource" state, rather than
in a tab of its own. Search and paste-an-IRI stay at the top. A resource chosen there, or a
member clicked in the overview, opens the existing resource graph. Because a phone has no
double-click, a click or tap on a class or a group opens it, and `Enter` does the same
from the keyboard. The open path is a single chain from one class down, which the
breadcrumb shows, so opening a node of an earlier level closes the levels below it.

**Amendment (user request, 2026-10-23).** The overview can also start from the GraphRAG
communities of [C22](C22-graph-rag-community-summaries.md) instead of the classes. The
group-by picker above the overview has a **GraphRAG communities** choice, which draws the
communities of one level of the hierarchy over the whole dataset. Level 0 is the coarsest
and the default, and a level picker chooses another. A node is a community. Its size is
its member count, and its colour is its report's status by default, with the community
colouring as the other choice. Edges count the links between members of different
communities, and a node's links among its own members are its internal count, as for the
other groups. Opening a community draws its children at the next level, and a community
of the finest level opens into its members. Members of a community that no child holds
are drawn as one group, labelled as the members of no community below. The breadcrumb
reads `Communities › Engineering › The payments team`.

Selecting or opening a community shows its report beside the canvas, with its title,
status, summary, findings and the review actions of the Communities tab, and the panel is
the same component that the tab uses. Choosing a finding rings the entities it is about,
in the canvas and in the list view. When those entities are inside a community that is
drawn closed, a note says how many are drawn and offers to draw the community's members.
**Open report** goes to the report in the Communities tab, and that tab's **Show in
Explore** comes back to the community in the overview.

The grouping reads the reports of main, where they are built. On another branch the
overview shows the notice that the Communities tab shows and reads nothing. A caller whose
access does not cover the reports graph, by the rule of C22 §6.3, sees only that the
communities are not available to them, and a dataset without reports says that none were
built. Both notices offer to go back to the classes.

### 2.4 Keyboard and accessibility

A WebGL canvas has no accessible content, so the view has a companion: **List view**
(`L`) shows the drawn nodes as a table sorted by size, with group, class, degree and
actions, and stays in sync with the selection. `Tab` moves through the largest visible
nodes in size order, `Enter` opens a group, arrow keys pan and `+`/`−` zoom. A polite
live region describes the level: "Class overview: 42 classes, 118 predicates. Person
opened into 17 communities." Colours come from a palette with text labels in the legend,
and selection is shown by a ring as well as colour. Under reduced motion the layout is
run to convergence before the first frame instead of animating.

### 2.5 Empty and error states

| Case | What the view shows |
|---|---|
| WebGL 2 unavailable or the context is lost | The Cytoscape view with the truncation notice and "WebGL is not available in this browser". A lost context is restored once before falling back. |
| A result over 100,000 nodes | "This result has N nodes. Open the overview or narrow the query", with **Overview** and the query's row count. |
| An aggregate that exceeds the caller's budgets | The `507` budget message, with the group left closed. |
| No F14 communities | Groups open by property buckets, and the community colouring is disabled with the reason. |
| A caller limited to some graphs | Counts reflect the visible graphs only, as schema reports already do for such callers. |

## 3. Server and API changes

Phase 1 needs none: the class level reads `/$/schema/{ds}/profiles`, and large results
come from `/{ds}/sparql` as today, within the caller's row and memory budgets.

Phase 2 adds **`POST /$/graph/{ds}/groups`**, which groups the instances of one group and
returns the groups and the edges between them:

```ts
type GroupsRequest = {
  within: { class: string } | { group: string };     // a class, or a group id from an earlier answer
  by: { community: string }                          // the IRI of the predicate F14 wrote community ids to
    | { property: string; buckets?: number }         // values of a property, bucketed
    | "members";                                     // the members themselves
  limit?: number;                                    // groups or members, at most 5,000
  at?: string; branch?: string;
};
type GroupsAnswer = {
  commit: number;
  groups: { id: string; label: string; size: number; top: { iri: string; label?: string; score?: number }[] }[];
  members?: { iri: string; label?: string; class?: string; score?: number }[];
  edges: { source: string; target: string; predicate: string | null; count: number }[];
  truncated: boolean;
};
```

- **By community.** F14's write-back ([F14 §2.5](F14-graph-algorithms.md)) writes, for
  each node, a predicate the caller names (such as `ex:community`) to a community
  resource per level, with `skos:broader` between levels. The route groups the `within`
  set by that predicate's values at the coarsest level, and opening a community group
  groups its members by the next finer level, following `skos:broader` down. It counts
  the edges between groups, for all predicates or the one the request names. Community
  ids that exist only as `SERVICE` output are not read; the person writes them back
  first.
- **By property.** Values of a literal property are bucketed into at most `buckets`
  ranges (numbers and dates) or most frequent values (strings and IRIs, the rest in one
  group).
- **Members.** The members with their labels, and the edges among them and to the
  sibling groups.
- **Cost.** One pass over the `within` set's type triples and the grouping predicate,
  and one pass over the edges of the set, through the same index scans as class profiles.
  It runs under the caller's view, timeout and memory budget, and fails with `507` like a
  query. Group ids are stable for a commit and a request, so a later request can name
  them.
- **By GraphRAG reports (amendment, 2026-10-23).** `by: { reports: { level?, graph? } }`
  groups the whole dataset by the community hierarchy that C22 stores in its reports
  graph, so the request leaves `within` out. `level` runs from 0, the coarsest and the
  default, to 15, and `graph` defaults to the dataset's reports graph and must lie in the
  `urn:x-sparkles:graphrag:` namespace. A group is a community, and its answer adds the
  community IRI, its level, its number of children and its report as an IRI, a title and a
  status, or null when it has none. Opening a group answers its children at the next
  level, with one more group for the members that no child holds, and a group with no
  children opens into its members. Edges count the links between members in the default
  graph and the named graphs outside the GraphRAG namespace, so report triples are never
  counted as links. A group id is `r1.` and the base64url of the reports graph, the start
  level and the path of communities. The server checks that the path is a chain of
  `skos:broader` in the reports and refuses an id that names any other set.
- **Access to the reports.** The route reads the reports graph only when the caller's
  view proves that graph by the coverage rule of C22 §6.3, which is the check of the
  GraphRAG routes. Otherwise the answer has `available: false`, `unavailable: "access"`
  and a reason, and holds nothing from the reports, whether or not they exist. A reader
  who may read them gets `unavailable: "none"` when the graph holds no communities. The
  grouping reads main, where the reports are built, and refuses another branch with a
  `400`. A request with `at` reads the reports as they were at that point of main.

Phase 3 adds **`GET /$/graph/{ds}/layout`**, which returns positions for an answer of
`groups` or for a result's node list, computed on the server with the same ForceAtlas2
parameters as the worker and cached by commit and request. It also adds
`application/x-sparkles-graph`, a binary form of the node and edge lists (dictionary of
IRIs, then `Uint32` edge pairs and `Float32` positions), for answers above 100,000
elements.

All routes are added to `docs/API.md`, the route table and the OpenAPI description.

## 4. Design

### 4.1 The renderer

`LargeGraphView.svelte` wraps sigma.js on a graphology graph. It takes the same `GNode`
and `GEdge` arrays as `GraphView.svelte`, plus optional `x`, `y`, `size` and `group`, so
callers switch renderers without converting data. Node and edge colours come from the
same CSS variables as the small view, read once and on theme change, as `GraphView`
already does with its `MutationObserver`.

### 4.2 Layout in a worker

`lib/layout.worker.ts` runs ForceAtlas2 (graphology-layout-forceatlas2) with
Barnes-Hut approximation above 2,000 nodes, sending positions as transferable
`Float32Array`s at most 15 times a second. Initial positions come from the server's
layout when there is one, from a group's position for members of an opened group, and
from a seeded random spread otherwise, so the same input gives the same picture.

### 4.3 Semantic zoom

`lib/overview.ts` keeps the tree of open groups: each open group replaces its node with
its children, placed around the group's former position, and the worker relaxes only
the new nodes and their neighbours for a few hundred iterations. Closing a group removes
its descendants and restores the group node at its position. Edge counts between open
and closed parts are recomputed in the browser from the answers already received.

### 4.4 Caching

A layout is cached in memory per result (query text, commit and settings) for the
session, so switching tabs does not lay out again. Phase 3's server layouts are cached by
the server and invalidated by commit.

## 5. Architecture

| Part | Where |
|---|---|
| WebGL view | `ui/src/lib/components/LargeGraphView.svelte` (new) |
| Layout worker | `ui/src/lib/layout.worker.ts` (new) |
| Overview state, aggregation of open and closed groups | `ui/src/lib/overview.ts` (new) |
| Renderer choice | `routes/query/+page.svelte` graph view, `routes/explore/+page.svelte` |
| Overview tab | `routes/explore/+page.svelte`, reading `api.schemaProfiles` |
| List view | `ui/src/lib/components/GraphListView.svelte` (new) |
| Groups route | `sparkles-server` `http` (new `graph` module), with the scans of `sparkles-core`'s schema profiles |
| Server layout (Phase 3) | a Barnes-Hut ForceAtlas2 in `sparkles-core` or a new small crate, run on the blocking pool |

## 6. Phases

1. **Phase 1.** `LargeGraphView`, the worker layout, the renderer switch on the query
   page and in Explore, the class-level overview from class profiles, and the list view.
2. **Phase 2.** `POST /$/graph/{ds}/groups` by community, by property and members, and
   semantic zoom in the overview. Community grouping needs F14.
3. **Phase 3.** Server layouts and the binary transfer format.

## 7. Acceptance examples

- **A1. Switch.** A `CONSTRUCT` that returns 20,000 nodes opens in the WebGL view with
  "20,000 nodes", and the toolbar switch can move it to the Cytoscape view, which then
  shows the truncation notice.
- **A2. Frame rate.** On the reference machine in the Playwright performance test, panning
  a 100,000-node, 300,000-edge synthetic graph runs at 30 frames per second or more after
  layout, measured with `requestAnimationFrame` timestamps.
- **A3. Main thread.** While the worker lays out A2's graph, the page answers a click on
  **Pause** within 100 ms.
- **A4. Class overview.** For a dataset with classes `ex:Person` (3 instances) and `ex:Org`
  (1 instance) and 3 `ex:worksFor` triples from people to the organization, the overview
  shows two nodes with those counts and one edge `ex:worksFor` with count 3. It makes one
  request, to `/$/schema/{ds}/profiles`.
- **A5. Open a group (Phase 2).** With F14's community ids on `ex:community` for 10,000
  people in 4 communities, opening `ex:Person` shows 4 group nodes whose sizes sum to
  10,000. Opening one with fewer than 5,000 members draws its members, and the edges to
  the other three groups carry counts.
- **A6. Close.** `Backspace` after A5 returns to the four groups at their former positions.
- **A7. Grants.** For a caller whose grants exclude the graph holding half of the people,
  A5's group sizes sum to the visible half.
- **A8. Accessibility.** With the keyboard alone, a person can open the overview, move to
  `ex:Person`, open it, and read the groups in the list view. The live region announces
  each level.
- **A9. No WebGL.** With WebGL disabled in the browser, A1's result opens in the
  Cytoscape view with the reason.

## 8. Testing

- **Vitest.** `lib/overview.ts`: opening and closing groups, edge aggregation between
  open and closed parts, stable positions. Renderer choice by count and override.
- **Rust.** The groups route against SPARQL `GROUP BY` queries over the same data, for
  communities, property buckets and members, with limited views and budgets.
- **Playwright.** A1, A3, A4, A6, A8 and A9 against the mock server and a real server.
  A2 runs in a separate performance project with a fixed GPU-capable runner, and its
  numbers are recorded rather than gating ordinary CI.

## 9. Dependencies

sigma (MIT), graphology (MIT), graphology-layout-forceatlas2 (MIT) and the graphology
utilities it needs (MIT). The UI loads them only when the large view or the overview
opens, so the size of the query page's first load does not change.

## 10. Rejected alternatives

- **Raising Cytoscape's limits.** Its canvas renderer and the main-thread fcose layout
  are the bottleneck, and its WebGL mode is newer and covers fewer of the styles the
  small view uses. Keeping Cytoscape for small graphs and sigma for large ones gives each
  its strength.
- **cosmos.gl or Cosmograph.** cosmos.gl lays out on the GPU and is faster at millions of
  nodes. The Cosmograph library around it has its own license terms, which would need a
  review, and cosmos.gl alone lacks the label and interaction layer sigma provides. It
  remains an option for a future level above 100,000 nodes.
- **Server-rendered images (tiles).** They would scale furthest but lose selection,
  search and semantic zoom, and would put rendering in the server.
- **Aggregation in the browser.** It needs every node in the browser, which is what the
  large cases cannot afford.

## 11. Open questions

These were open when the spec was written. The answers below were chosen on 2026-10-23,
taking the conservative option in each case.

1. Whether group answers should be cached on the server per commit, or recomputed, given
   that the schema reports' incremental updates (C02) do not cover them.

   **Answer: cached per commit, bounded.** A commit's data never changes, so an answer
   is right for as long as its commit can be asked for, and no invalidation is needed.
   The key is the branch's id, the commit, the caller's read view
   (`GraphAccess::read_key`), the dataset's inferred overlay and the whole request, so a
   caller never reads an answer computed for a wider view. The cache holds at most 64
   answers and 16 MiB of their JSON and drops the oldest first. It costs a few lines and
   makes reopening a group, which the breadcrumb invites, immediate.

2. Whether the groups route should run an F14 community detection itself when no
   written-back result exists, as a task, instead of asking the person to write one back.

   **Answer: no.** A community run needs a projection, F14's thread admission and, to
   give group ids that stay stable, a written result. Leiden over the `foaf:knows`
   graph of the 10.5M-triple benchmark dataset, 986,333 nodes and 5.0 million edges,
   took 22 s on an 8-vCPU Namespace instance, eleven times the two-second target of
   goal 3. A run inside the request would miss it, and a background run would need
   write grants that a reader of the overview may not hold. The route reads written-back
   communities when a sample of the set's instances holds them and otherwise groups by
   the property that splits the sample best. The UI says that the groups are property
   buckets because no F14 communities were written, as §2.5 asks.

3. Whether the overview should also offer grouping by named graph for datasets that keep
   one graph per source.

   **Answer: not in Phase 2.** Grouping by graph crosses classes, so it does not fit the
   single open path below one class that the breadcrumb shows, and the graph view of
   C12 limits makes its counts a second source of disclosure to review. It can come later
   as a separate level above the classes.

## Provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

- **Spec:** [`U06-large-graph-rendering.md`](U06-large-graph-rendering.md), written on 2026-10-12 independently from:
  - the public documentation of sigma.js 3 and graphology;
  - Jacomy et al., "ForceAtlas2, a Continuous Graph Layout Algorithm for Handy Network
    Visualization Designed for the Gephi Software", PLOS ONE, 2014;
  - the public documentation of Cytoscape.js, cytoscape-fcose, cosmos.gl and Cosmograph;
  - the WebGL 2 specification;
  - the Sparkles code (`GraphView`, `lib/graph.ts`, `lib/explore.ts`) and specs C01, C02,
    C12, F14 and X05.
- **Implementation:** not built. Planned npm dependencies, loaded only by the large view:
  `sigma` (MIT), `graphology` (MIT), `graphology-layout-forceatlas2` (MIT) and the
  graphology utilities it needs (MIT).
- **Rejected** (spec §10):
  - raising Cytoscape's limits;
  - cosmos.gl or Cosmograph (license review needed for Cosmograph);
  - server-rendered tiles;
  - aggregation in the browser.

## Outcome

Phases 1 and 2 shipped on 2026-10-23 under [ROADMAP](../ROADMAP.md) item 29, ahead of
their tier, because the user asked for the overview to be the first thing Explore shows.
Phase 3 is not built and stays under item 29.

**Library and server.** The groups computation is in the library as
`sparkles::overview`, which an embedded program calls as `ds.graph().groups(&req, &opts)`
with the same answer as the route. `POST /$/graph/{ds}/groups` calls it and answers by
communities, by property and by members, as §3 describes. It reads through the caller's view of C12 and C12b, is admitted
through the query memory pool as a query, takes one deadline for all of its queries,
stops when the client goes away and caps its groups at 5,000 and its edges at 20,000. A
group id is `g1.` and the base64url of the class and the partitions that lead to the
group. The server decodes an id only through checks, so an id a client makes up can only
name a set its own SPARQL queries could name. Numeric and date properties with more
values than buckets are grouped by ranges of a round width. The class profiles that the
overview starts from are now computed one predicate per rayon task, and both they and the
group answers are cached per branch, commit, view and request, at most 64 answers and
16 MiB.

**UI.** Explore opens on the class overview when no resource is chosen, with search and
paste-an-IRI above it. The view draws with sigma and graphology in WebGL and lays out
with ForceAtlas2 in a module worker, and falls back to Cytoscape with the reason when
WebGL 2 is missing or its context is lost twice. A tap or Enter opens a class or group,
the breadcrumb and Backspace close levels, and a member opens in the resource view. The
list view, the live region, the keyboard of §2.4 and both themes work at phone and desktop
widths. The query page uses the same view for graph results above 2,000 nodes, with a
renderer switch and node limits up to 100,000.

**GraphRAG communities.** The amendments of §2.3 and §3 for C22's communities shipped on
2026-10-23. `ds.graph().groups` and the route take `by: {reports: {level, graph}}`, and
the Python, Node and JVM bindings pass it through with `within` left out. The route fills
in the dataset's reports graph from its GraphRAG settings. The answer is cached with the
other group answers, under a key that holds the caller's view, so one caller's answer is
never served to another whose view differs. Tests build a hierarchy by hand in the
library and with stub providers through the server's build, and they check the counts
against the Communities routes. Router tests cover an unrestricted reader, a restricted
reader with and without coverage, a build with only the `auth` feature, a branch other
than main and a reports graph that the settings moved. The overview shows the report
beside the canvas through the `CommunityReport` component, which the Communities tab
also uses, and mock browser tests cover the picker, opening levels, the report panel, a
finding's highlight, the unavailable and off-main notices, the links between the two
pages and a 390 px phone.

**Measurements.** On an 8-vCPU Namespace instance with the 10.5M-triple dataset, the
first overview request took a median of 1.12 s with the parallel profiles and 2.30 s
without, which meets goal 3 only with the parallel build. Repeated requests at the same
commit return from the cache in under 1 ms. Opening the largest class, `Researcher` with
300,002 instances, took 1.35 to 2.26 s, and deeper levels took 0.1 to 0.8 s.
[BENCHMARKS](../BENCHMARKS.md#the-explore-overview) has the table.

**Limits.** A2 and A3, the frame rate and the main-thread checks on a GPU runner, are not
automated, because the CI browsers run without a GPU. The headless browser used for the
checks here draws with WebGL in software, which shows the views but measures no frame
rate. Groups by named graph and on-demand community detection are not built, for the
reasons in §11. A picture of up to 30 nodes draws every label, and its fit makes room
for the labels on screen.
