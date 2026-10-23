# U11: Linked views beside the graph

> **Status:** partly implemented
>
> **Phases:** Phase 1 shipped on 2026-10-23. It draws the relations between located
> entities as arcs in the map view, for the Explore graph and for query results, with
> aggregation, clustering and one selection shared by the graph canvas and the maps.
> Phase 2 adds facet histograms of literal properties beside the canvas, where brushing a
> range filters the canvas and can add the matching `FILTER` to the query, after a server
> route that computes the histograms. Phase 3 adds an embedding map, a two-dimensional
> projection of a vector index coloured by class or community, where a lasso selection
> becomes a `VALUES` block, after a server route that computes the projection. Phases 2
> and 3 are not built.
>
> **User docs:** [Usage: Arcs on the map](../USAGE.md#arcs-on-the-map) ·
> [Features](../FEATURES.md#web-ui)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how Phase 1 landed.

This design was written from the Sparkles code and specs: GeoSPARQL and the UI's maps
([G01](G01-geosparql.md)), vector search and indexes ([F04](F04-vector-search.md)), local
embeddings ([F12](F12-local-embeddings.md)), graph algorithms
([F14](F14-graph-algorithms.md)), large-graph rendering and its groups route
([U06](U06-large-graph-rendering.md)), the visual query builder
([U08](U08-visual-query-builder.md)), canvas lenses ([U10](U10-canvas-lenses.md)), the time
slider ([U01](U01-time-slider.md)), and graph and triple access control
([C12](C12-graph-access-control.md), [C12b](C12b-triple-access-control.md)). It also used
B. Shneiderman's "Dynamic Queries for Visual Information Seeking" (IEEE Software, 1994) and
Becker and Cleveland's "Brushing Scatterplots" (Technometrics, 1987) for brushing and
linking, K. Pearson's 1901 paper on principal components and the public description of
UMAP (McInnes, Healy and Melville, 2018) for projections, the MapLibre GL JS documentation
of line layers, and the WAI-ARIA Authoring Practices for the slider and listbox patterns.

## 1. Summary, goals, non-goals

The graph canvas shows how entities connect. Three other pictures of the same entities
answer questions the canvas cannot. A map shows where connected entities are. A histogram
shows how a property's values are spread, and brushing it narrows the graph to a range. An
embedding map shows which entities a model considers alike, whether or not they are linked.
Sparkles has maps of geometries (G01), vector indexes (F04) and grouping by property values
(U06), but none of these views is linked to the canvas or to each other.

This spec adds the three views and one shared selection. Selecting entities in any of them
selects them in the others, and a selection can become part of the query.

**Goals**

1. Arcs on the map for relations between located entities, from the Explore graph and
   from a query result.
2. Histograms of the literal properties of the canvas's entities, computed on the server
   for every matching entity, not only the drawn ones.
3. Brushing a histogram filters the canvas at once and can add the equivalent `FILTER`
   to the query text through U08.
4. A projection of a vector index to two dimensions, coloured by class or community, where
   a lasso selects entities and the selection becomes a `VALUES` block.
5. One selection model for the canvas, the map, the histograms and the embedding map.
6. Bounded transfers. No view moves more than its cap of points, arcs or bins to the
   browser, and larger sets arrive aggregated.

**Non-goals**

- A general charting tool. Notebooks (U05) have chart cells for that.
- Routing or geodesic analysis on the map. GeoSPARQL functions answer spatial questions.
- Training or changing embeddings. The projection reads vectors that F04 and F08 already
  keep.
- A projection of vectors that are not in an index.

## 2. User-visible behaviour

### 2.1 The shared selection

The canvas, the map, the facet panel and the embedding map read and write one selection,
a set of entity terms. A selection made in one view highlights the same entities in the
others and dims the rest. The selection bar under the toolbar names its size and its
source, such as "41 selected by the lasso", with **Clear**, **Open as graph**, which draws
the selected entities in Explore, and **Use in query**, which writes them into the query
as a `VALUES` block (§2.4). `Escape` clears it from any view.

### 2.2 Arcs on the map (Phase 1)

Explore's graph toolbar gets **Map**, which shows the drawn graph on MapLibre. Each drawn
entity with a geometry becomes a point at the geometry's point, or at its centroid for
lines and polygons. Each drawn edge between two located entities becomes an arc.

```
┌─ Explore · map ─────────────────────────────────── 84 located · 12 not ──┐
│ Arcs [ex:worksIn ☑] [ex:knows ☑] [ex:partOf ☐]   width by count ▾        │
│                                                                           │
│        ●Lyon ═══════ 14 ═══════╗                                          │
│                                 ╚═══ 6 ═══●Paris                          │
│                                                                           │
│ Not located: ex:acme, ex:beta, … (12)                                     │
└───────────────────────────────────────────────────────────────────────────┘
```

- **Arcs.** Arcs are great-circle curves, raised slightly from the straight line so that
  arcs in both directions between two places stay apart, with an arrowhead at the target.
  Arcs that cross the antimeridian are split there. Colour is by predicate, with a legend
  and a checkbox per predicate.
- **Aggregation.** When several arcs join the same pair of places, as when many people work
  in one city, they are drawn as one arc whose width and label give the count. At low zoom,
  points closer than 20 px are clustered and their arcs merged.
- **Not located.** Entities without a geometry are listed below the map, and their edges are
  not drawn.
- **Query results.** The query page's Map tab gets **Arcs** when a result has two columns
  with geometries, or two entity columns whose entities have geometries. Each row is an
  arc from the first column to the second, labelled by a third column when one is chosen.
- **Time.** The slider's compare mode marks added and removed arcs with the same tokens as
  map features, as U01 Phase 3 already does for features.

### 2.3 Facet histograms (Phase 2)

The canvas gets a **Facets** panel, shortcut `Shift+F`, beside the properties panel on a
wide screen.

```
┌─ Facets · 3,980 people ───────────────────────── [+ facet] ─┐
│ ex:age               ▁▂▅█▇▅▃▂▁   18 ━━[30 ▓▓▓▓ 45]━━ 90     │
│ ex:joined (date)     ▂▃▅▆█▇▆▅▃   2001 ━━━━━━━━━━━━━ 2026     │
│ ex:country           FR ████ 1,204  DE ███ 980  … 14 more    │
│ 1,102 of 3,980 match · [Apply to query] [Clear]             │
└─────────────────────────────────────────────────────────────┘
```

- **The set.** In Explore the set is the class of the focused entity, or the drawn
  entities when they have no single class. On the query page it is the values of a chosen
  variable of the result's query, all of them rather than the rows on screen.
- **Facets.** The panel offers the literal-valued properties of the set, ranked by the share
  of the set that has a value, from class profiles. A number or a date becomes a histogram of
  up to 40 bins of round width. A string, a language-tagged string or an IRI becomes a bar
  list of its 12 most frequent values and "other".
- **Brushing.** Dragging across a histogram selects a range, and clicking bars of a bar list
  selects values. The canvas dims the drawn entities outside the brush at once, from values
  the panel already fetched for them. Every other facet redraws for the brushed set, so the
  facets filter each other, and the count line says how many of the set match.
- **Apply to query.** On the query page, **Apply to query** adds, for each brushed facet, a
  triple that binds the property and a `FILTER` over it. It goes through U08's splices, so
  the rest of the text stays as it was, and U08's canvas shows the filter as a chip on its
  variable. In Explore, **Apply to query** opens the query page with a query for the set and
  its filters.

### 2.4 The embedding map (Phase 3)

The dataset page's Vector indexes card, Explore and the Similar page get **Embedding map**
for an index.

```
┌─ Embedding map · ex:abstractVec · 184,220 points ──── colour by [class ▾] ──┐
│  ·::.     ..:::.      PCA ▾   showing 50,000 of 184,220 · density shaded    │
│ .:::::.  ::Person::                                                         │
│   ':'      ':::'      ◌ lasso            ●Org   ●Place   ●Person   other     │
│ 312 selected · [Use in query] [Open as graph] [Clear]                       │
└─────────────────────────────────────────────────────────────────────────────┘
```

- **Points.** Each point is one entity with a vector in the index. An entity with several
  vectors under the predicate is placed at the mean of their projections. Above 50,000
  points, the map draws a density shading of all points and a sample of 50,000 on top, and
  zooming into a region fetches that region's points at full detail, at most 50,000 at a
  time.
- **Colour.** By class, by a written-back F14 community predicate, by a U10 kept Leiden run
  over the same entities, or by a literal property's facet buckets. The legend lists the
  categories with their counts.
- **Lasso and box.** Drawing a lasso or a box selects the points inside it, which become the
  shared selection. `Shift` adds to the selection and `Alt` removes from it.
- **Hover.** A point's tooltip shows its label, class and nearest neighbours from the index,
  and clicking opens the entity in Explore.
- **Use in query.** The selection becomes `VALUES ?x { … }` through U08's inline `VALUES`
  form, with the variable the person picks. Selections above 1,000 entities offer the first
  1,000 by distance from the selection's centre and say how many were left out.
- **Method.** PCA is the default. **Neighbour layout** uses the index's neighbour graph and
  places alike entities closer, which separates clusters better at a higher cost, and it runs
  as a task with progress.

### 2.5 Keyboard

| Key | Action |
|---|---|
| `Shift+F` | Open the facet panel. |
| `Shift+M` | Open the map, or the embedding map when the canvas shows one. |
| Arrow keys on a histogram | Move the focused bin. `Shift` with arrows extends the brush. `Enter` brushes the focused bin. |
| `Space` on a bar list value | Toggle the value in the brush. |
| `Tab` on a map | Move through points in order of size, and `Enter` selects one. |
| `Escape` | Clear the selection or the brush. |

Each histogram is a set of two sliders in the WAI-ARIA slider pattern for the brush's ends,
with `aria-valuetext` such as "age from 30 to 45, 1,102 entities", and a **Table** toggle
that lists the bins. The map and the embedding map have list alternatives that list the
points, arcs and clusters in view with their counts. A polite live region announces each
selection change. Colours always come with legend text, and a brush is shown by shading and
handles as well as colour.

### 2.6 The 390 px layout

The facet panel, the map and the embedding map open as full-screen tabs beside the graph,
sharing the selection bar at the bottom. A histogram is brushed by dragging its two handles,
each with a 44 px target, and a bar list by tapping values. The lasso is a one-finger draw
after tapping **Lasso**, so a plain drag still pans. **Use in query** opens the query page's
text tab.

### 2.7 Empty and error states

| Case | What the view shows |
|---|---|
| No drawn entity has a geometry | "None of these entities has a geometry", with the predicates the map looks for. |
| A set without literal properties | "These entities have no literal properties to facet." |
| A facet request over the budget | The route's `507`, and that facet shows its reason while the others stay. |
| No vector index | **Embedding map** is absent, with a link to the Vector indexes card for admins. |
| A caller whose access covers part of the dataset | **Embedding map** is absent with "Embedding maps need access to every graph" (§5). |
| A projection still being computed | A progress bar with **Cancel**, and the last projection, if any, dimmed. |

## 3. Server and API changes

### 3.1 Phase 1

Phase 1 needs no server change. Geometries come from the queries the map card already uses
(`geometryQuery` in `lib/geo.ts`), batched with `VALUES` over 200 entities at a time, and
from `POST /$/geo/convert` for geometries the browser cannot read. Arcs come from the drawn
edges or the result rows.

### 3.2 Phase 2 prerequisite, a facets route

U06's groups route already buckets a class's instances by a property's values into ranges of
round width or most frequent values. Phase 2 generalizes that code into
**`POST /$/graph/{ds}/facets`**.

```ts
type FacetsRequest = {
  within: { class: string } | { nodes: string[] }            // up to 20,000 terms
        | { query: string; variable: string };               // a SELECT whose variable gives the set
  facets: { property: string; bins?: number; top?: number }[];   // at most 6 facets, bins up to 40, top up to 12
  filters?: { property: string; range?: [Lit, Lit]; values?: Term[] }[];  // the brushes
  candidates?: boolean;                                      // also list the literal properties of the set
  at?: string; branch?: string; reasoning?: boolean;
};
type FacetsAnswer = {
  commit: number; size: number; matching: number;
  facets: { property: string; kind: "numeric" | "date" | "values";
            bins?: { from: Lit; to: Lit; count: number }[];
            values?: { value: Term; count: number }[]; other?: number; missing: number }[];
  candidates?: { property: string; share: number; kind: string }[];
  truncated: boolean;
};
```

- **Filters.** Each facet counts the set filtered by every brush except its own, which is
  what lets the facets filter each other while a facet still shows what a wider brush on it
  would add.
- **Query sets.** A `query` set runs the query once under the caller's budgets and keeps the
  distinct values of the variable. A set above 1,000,000 values is a `507`.
- **Cost.** One pass over each facet property's values for the set, through the same scans
  as the groups route. It runs under the caller's view, timeout and memory budget, through
  the query memory pool, and it shares the groups route's answer cache, keyed by branch,
  commit, view and request.

The route is added to `docs/API.md`, the route table and the OpenAPI description, and it is
in the library as the groups computation already is (`sparkles::overview`).

**Filters in the query.** The text that **Apply to query** writes is the same filter the route
applied, in SPARQL, such as `?x ex:age ?age . FILTER(?age >= 30 && ?age < 45)`, so the query's
count equals the panel's `matching`. The tests check that equality.

### 3.3 Phase 3 prerequisite, a projection route

No route returns vectors in bulk or a projection. Phase 3 adds
**`POST /$/vector/{ds}/{name}/projection`**, which computes a two-dimensional projection of
an index's vectors and keeps it.

- **Methods.** `pca` computes the two leading principal components from a sample of up to
  100,000 vectors and projects every vector onto them. It runs in the request for indexes of
  up to a million vectors and as a task above that. `neighbours` lays out the index's own
  neighbour graph, the bottom layer of its HNSW graph, with a ForceAtlas2 layout seeded by
  the PCA positions, and always runs as a task. It needs no new dependency.
- **Answer.** `GET /$/vector/{ds}/{name}/projection` returns the projection's method, commit,
  point count and bounds, a density grid of 256 by 256 cells, and up to 50,000 points as
  `{entity, x, y, class?}`. A `bbox` parameter returns the points inside a region, again at
  most 50,000, sampled evenly when there are more. `Accept: application/x-sparkles-points`
  returns the points as a dictionary of terms followed by `Float32` coordinates and `Uint32`
  class indexes, for large answers. `colour=` with a predicate returns that predicate's value
  per point for written-back communities or property buckets.
- **Keeping it.** A projection is kept in memory per index, keyed by the index's commit and
  method, at most four per server and 1 GiB in all, charged to the server memory pool. It is
  about 12 bytes per vector, so ten million vectors take 120 MiB. A projection older than the
  index's commit is served with `stale: true` until a new one is computed. Projections are not
  written to disk in this spec (§10).

The routes need `read`. They are added to the `whole_dataset` list with `/$/vector/{ds}`, so a
caller whose access covers part of the dataset gets `403`, for the reason of §5. They are
added to `docs/API.md`, the route table, the OpenAPI description and `/metrics`.

## 4. Limits and budgets

| View | Cap |
|---|---|
| Map | 20,000 points and 20,000 arcs before aggregation, the `MAX_DRAWN` of `ResultMap`. Geometry lookups in batches of 200, four in flight. |
| Facets | 6 facets, 40 bins or 12 values each, sets of up to 1,000,000 entities, one request per brush change after a 200 ms pause. |
| Embedding map | 50,000 points per answer, a 256 by 256 density grid, 1,000 entities in a `VALUES` block. |
| Selection | 20,000 entities. A larger lasso selects the first 20,000 by distance from its centre and says so. |

## 5. Access, coverage, branches and past states

The map and the facets read under the caller's view. Geometry lookups are ordinary queries,
and the facets route computes over the view as the groups route does, so counts never include
hidden triples, and `missing` counts entities whose value is hidden as entities without a
value.

The embedding map is different. A vector index holds the vectors of every entity in the
dataset, and F04 builds it without regard to views. A point in the projection would reveal
that an entity exists and what it is like, even when the caller cannot read its triples. The
projection routes therefore follow the vector status routes and answer `403` to a caller whose
grants or protections cover part of the dataset, and the UI does not offer the map to them.
Serving a projection filtered by view is open question 1. The class colouring reads the class
of each point under the caller's view, which for an unrestricted caller is the whole dataset.

Facets and the map read at the canvas's `at` and branch, and move with U01's slider. Vector
indexes answer at the head only, and F04 falls back to exact scans at past states, so the
embedding map is offered at the head of the branch that holds the index, and at other states
it says "The embedding map shows the head".

## 6. Architecture

| Part | Where |
|---|---|
| Shared selection | `ui/src/lib/selection.svelte.ts` (new) |
| Arcs, great circles, aggregation | `ui/src/lib/arcs.ts` (new), and a line layer in `MapView.svelte` |
| Explore map tab, result arcs | `routes/explore/+page.svelte`, `ResultMap.svelte` |
| Facet panel, brushes, filter text | `ui/src/lib/components/FacetPanel.svelte` and `ui/src/lib/facets.ts` (new) |
| Facets route | `sparkles-server`'s graph module beside the groups route, and `sparkles::overview` |
| Embedding map | `ui/src/lib/components/EmbeddingMap.svelte` (new), drawn with WebGL points through sigma's node programs as `LargeGraphView` does |
| Projection routes | `sparkles-server`'s vector module and `sparkles-core`'s vector index code |
| Apply to query and Use in query | U08's splices in `ui/src/lib/pattern.ts` |

## 7. Phases

1. **Phase 1.** The shared selection, arcs on the map for the Explore graph and for query
   results, aggregation and clustering, and the list alternative. No server change.
2. **Phase 2.** First the facets route. Then the facet panel, brushing with cross-filtering,
   dimming on the canvas, and **Apply to query**, which needs U08 Phase 2's splices.
3. **Phase 3.** First the projection routes with PCA, then the embedding map, colouring, the
   lasso, **Use in query**, which needs U08 Phase 3's inline `VALUES`, and the neighbour
   layout.

## 8. Acceptance examples

- **A1. Arcs.** With ten people located in Lyon and four in Paris who all `ex:worksIn` an
  organization in Paris, Explore's map draws one arc from Lyon to Paris labelled 10 and the
  Paris people as a local cluster.
- **A2. Result arcs.** A `SELECT ?from ?to` over two geometry columns draws one arc per row,
  and a route across the antimeridian is split there.
- **A3. Facet counts.** For 3,980 people with ages from 18 to 90, the age facet's bins sum to
  the people with an age, and `missing` equals those without one.
- **A4. Cross-filtering.** Brushing ages 30 to 45 changes the country facet's counts to those
  of the brushed people, and the age facet keeps its own counts.
- **A5. Round trip.** After A4, **Apply to query** adds the triple and filter of §3.2, the
  query's count equals the panel's `matching`, and the rest of the text is byte-identical.
- **A6. Projection.** For an index of 10,000 vectors in three separated clusters with
  classes, the PCA projection's points of each class fall in three regions that a lasso can
  separate, and the class colouring matches the classes.
- **A7. Lasso to VALUES.** Lassoing one region selects its entities in the canvas, and **Use in
  query** writes a `VALUES` block with those entities, up to 1,000.
- **A8. Restricted caller.** A caller whose grants hide one graph gets `403` from the projection
  routes and sees no **Embedding map**, while the map and the facets work and their counts
  leave out the hidden graph.
- **A9. Frame time.** The embedding map with 50,000 points and density shading pans at 30
  frames per second or more on the reference machine of U06 A2, and a lasso over 20,000
  points updates the selection within 100 ms.
- **A10. Keyboard and phone.** A4 can be done with the keyboard alone, and at 390 px with taps
  alone, without horizontal scrolling.

## 9. Testing

- **Rust.** The facets route against SPARQL `GROUP BY` queries over the same data, for each
  set kind, cross-filtering, missing values, round widths, limited views and budgets. The
  projection's PCA against a reference computation on a fixed sample, the neighbour layout's
  determinism with a seed, the density grid, `bbox` answers, the binary format, the memory
  bound, staleness after a commit, and the `403`.
- **Router.** Both routes' parameters, their `400` and `507` answers, OpenAPI equality, and
  the `whole_dataset` rule for the projection routes.
- **Vitest.** `lib/arcs.ts` for great circles, the antimeridian split, offsets for arcs both
  ways and aggregation. `lib/facets.ts` for brushes, filter text and the equality with the
  route's filters. `lib/selection.svelte.ts` for selection sync and caps.
- **Playwright against the mock server.** A1, A2, A4, A7 and A10, and the empty states, with
  the mock serving facet and projection answers.
- **Playwright against a real server.** A1, A3, A5, A6 and A8.
- **Performance.** A9 in the performance project of U06.

## 10. Open questions

1. Whether a projection should be served to callers with partial access, filtered to the
   entities they can read. The filtering is cheap, but the positions of visible points were
   computed with hidden vectors, which reveals a little about them.
2. Whether projections should be written to disk beside the index, which would need a
   versioned file and a refusal of unknown versions, so a restart does not recompute them.
3. Whether facets should also offer counts of IRI-valued properties by the class of the value,
   such as "works for an Org in France", which needs a join per facet.

## Outcome

Phase 1 shipped on 2026-10-23 under [ROADMAP](../ROADMAP.md) item 107, with no server
change. Phases 2 and 3 are not built and stay under item 107.

**Selection.** `ui/src/lib/selection.svelte.ts` holds one selection of entity terms, keyed
by their SPARQL form, with the name of the view that made it. It keeps at most 20,000
entities, keeps the first ones given when a larger set arrives, and counts the rest so that
the selection bar can say so. The map and Explore's graph canvas read and write it. A click,
or `Enter` on a focused marker, selects the entities the marker or arc stands for. `Shift`
adds them and `Alt` removes them. A click on a node of the canvas selects that node. The
selection bar under the toolbar names the count and the source and has **Clear** and, in
Explore, **Open as graph**, which draws up to 1,500 selected entities as nodes and expands
the first 25 of them. `Escape` clears the selection from any view that has no dialog or
field open. The canvas dims the nodes outside the selection and the edges that do not join
two selected nodes, and the map dims its markers and arcs and draws the selected ones again
in the accent colour. **Use in query** is not offered, because it needs the inline `VALUES`
of U08 Phase 3. The query page's graph views do not read the selection, so a selection made
on a result's map ends when the Map tab closes.

**Arcs.** `ui/src/lib/arcs.ts` is a pure module that the maps feed with places and links.
An arc is the great circle between two positions, raised toward the left of its direction by
12 percent of its length, and by 7 percent more for each further predicate between the same
two markers, so that the arcs of the two directions stay apart. An arc that crosses the
antimeridian is cut there into two parts that end on it at one latitude. The arrowhead sits
the marker's radius and 8 px before the end of the arc, so that the marker does not cover it.
Places within 20 px of the seed of a cluster at the current zoom merge into one marker, in
the order the places were given, so that the same input always gives the same markers. The
links between places of one marker are counted on the marker and not drawn. The remaining
links merge by marker pair, direction, predicate and change into one arc whose width is
`1.6 + 2.2 log2(count)` pixels up to 14, with the count as its label, or the caption of its
single link when a result names a labelling column. Colours come from an eight-colour
palette by predicate, and arcs added or removed between two states of the time slider use
the same tokens as map features, green and a plus sign, red, dashed and a minus sign. The
layout is recomputed when the map stops moving, because it depends on the zoom.

**Places.** `ui/src/lib/locate.ts` finds the position of an entity with the routes of the
map card, an own geometry literal, a geometry reached through `geo:hasGeometry` or
`geo:hasDefaultGeometry`, or WGS84 latitude and longitude. One query with a `VALUES` block
asks for 200 entities, four requests run at once, and the first literal that can be drawn
stands for an entity with several. The row limit of a batch is shared by its entities, so
when an answer comes back full, the entities it has no row for are asked again in a batch of
their own. A line or a polygon stands at its centroid, weighted by
length or area with holes taken out. The queries run through `api.select` on the dataset and
branch of the page, at the `at` of the slider or of the result, and under the caller's
view, so an entity whose geometry the caller may not read is simply not located and is
listed under the map. The map never asks for relations. Explore's arcs are the edges the
graph already holds, and a result's arcs are its own rows.

**Maps.** Explore has **Graph** and **Map** beside its toolbar's other buttons, and
`Shift+M` toggles them. A result with two geometry columns, or with two entity columns, has
**Points** and **Arcs** on its Map tab, with **From**, **To** and **Label** columns. A
result with entity columns and no geometry column gets the tab only when a lookup of its
first 200 entities finds a geometry, so a result of plain resources does not grow a useless
tab. The bundled basemap has no glyphs, so counts, labels and arrowheads are canvas images
that MapLibre asks for by name when a layer needs one. Tab walks the markers in view,
largest first, `Enter` selects one and a list of the markers and arcs in view below the map
is the alternative for a screen reader and for taps. At 390 px the controls are 44 px
targets, the map takes the page's width and the page scrolls to the details below it.

**Caps.** A map draws 20,000 places and 20,000 links. A result of more rows reads the first
20,000, and a map of more places keeps the first 20,000 and leaves out the links that end in
the others. Both say so under the map. Clustering wraps at the antimeridian, so places at 179 and -179
degrees merge when they are within 20 px across the seam.

**Tests.** The Vitest suites for `arcs.ts`, `selection.svelte.ts`, `locate.ts`,
`result-arcs.ts` and the centroid and lookup query of `geo.ts` cover great circles, the
antimeridian split, the bow in both directions, clustering at several zooms, aggregation,
the caps, the cap and counts of the selection, batching and failure of lookups. The
Playwright suite `ui/tests/mock/arcs.spec.ts` runs A1, A2 and the keyboard and phone parts
of A10 against the mock's `flows` dataset, with the empty state, a caller who cannot read
the people's geometries, a past state, a branch and the 390 px layout. The mock runs real
SPARQL through Oxigraph, but not Sparkles' engine, so A1 against a real server is not
automated.

**Measurements.** `ui/tests/mock/arcs-perf.spec.ts` draws 20,000 distinct arcs between 300
places from one query and pans the map for 90 frames. Headless Chromium draws with WebGL in
software, so the frame times are an upper bound for a machine with a GPU. The run below
was repeated three times on a 16 thread laptop that was also building other work, with a load
average between 20 and 40, and the table gives the median of the three means.

| View | Arcs drawn | Layout | Mean frame while panning |
|---|---|---|---|
| World, 27 clusters | 425 | 8 to 16 ms | 42 ms, about 24 frames per second. |
| Zoom 3.5, 300 clusters | 20,000 | 160 to 210 ms | 144 ms, about 7 frames per second. |
| Zoom 8, 300 clusters | 20,000 | 120 to 185 ms | 16.6 ms, 60 frames per second. |

The slow case is 20,000 distinct arcs drawn at once under software rendering, which a result
seldom reaches because arcs between the same two clusters merge into one. A result with a
few hundred distinct cluster pairs pans at the world row. The layout figure is the time to
cluster the places and aggregate the links after a zoom, and it is the same work on a machine
with a GPU.

**Limits.** The explicit frame-rate gate of this spec, A9, belongs to the embedding map of
Phase 3. The frame times above were measured in software rendering on a busy machine and
are not a published figure. The layout of 20,000 distinct arcs runs on the main thread and
takes about 100 to 220 ms, which is felt as a pause after a zoom, and moving it to a worker
is a later option. Explore's map reads the dataset and not a branch, because Explore itself
has no branch selector. In compare mode with a branch, the nodes that only the other branch
had are listed as not located. On the query page, the entities that only removed rows use are
located at the state of the left side, and an entity that moved between the two states is
drawn at its current position. The lookup that offers the Map tab to a result of plain
resources does not run while the time slider is open, because it would be one more read of
every state the slider visits, so such a result gets the tab once the bar is closed. Colours are read from the theme when a map opens, so
a change of theme shows at the next map. Markers at one position stay merged at
every zoom, and there is no spiral that opens them.
