# U10: Lenses on the graph canvas

> **Status:** Phases 1 and 2 shipped
>
> **Phases:** Phases 1 and 2 shipped on 2026-10-23. Phase 1 adds lenses to the Explore graph and the WebGL view, a
> shared way to size, colour, badge and style drawn elements from data computed for them,
> with a legend and a list column, and the first lenses: degree, a property's values, class,
> graph and inferred edges drawn dashed. Phase 2 adds algorithm lenses, which size nodes by
> PageRank or another centrality and colour them by a Leiden run that is never written back,
> after a server prerequisite that caches view-only algorithm runs. Phase 3 answers "Why is
> this inferred?" with the reasoner's derivation tree for a dashed edge, after a server
> prerequisite that explains inferences over a materialized closure.
>
> **User docs:** [USAGE.md](../USAGE.md#lenses-on-the-graph).
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how Phases 1 and 2 landed.

This design was written from the Sparkles code and specs: graph algorithms
([F14](F14-graph-algorithms.md)), community summaries ([C22](C22-graph-rag-community-summaries.md)),
inference freshness and RDFS on read ([C08](C08-inference-freshness.md)), reasoning at scale
([F16](F16-materialization-at-scale.md)), explanations ([F18](F18-explanations-and-entailment-diffs.md)),
large-graph rendering ([U06](U06-large-graph-rendering.md)), the time slider
([U01](U01-time-slider.md)) and graph and triple access control
([C12](C12-graph-access-control.md), [C12b](C12b-triple-access-control.md)). It also used the
public documentation of Gephi's ranking and partition panels and of Cytoscape.js styles and
mappers as examples of data-driven graph styling, the ColorBrewer qualitative palettes, the
PageRank paper (Brin and Page, 1998) and the Leiden paper (Traag, Waltman and van Eck,
"From Louvain to Leiden", Scientific Reports, 2019), and the WAI-ARIA Authoring Practices.

## 1. Summary, goals, non-goals

Explore draws entities and their links, coloured by kind and sized alike. Questions such as
"which of these nodes matter most", "which belong together" and "which of these links did
the reasoner add" need data the canvas does not have. F14 computes PageRank, centralities and
communities, the reasoner keeps inferred triples in `urn:x-sparkles:inferred`, and F18 has
designed derivation trees, but the UI uses none of them on the graph. FEATURES lists the
missing community colouring as a gap of roadmap item 8.

A **lens** is a mapping from data about the drawn elements to how they look. Lenses are
computed for what is on the canvas, by the server where the data is large, and change only
the drawing, never the data. This spec adds the lens machinery once and builds three groups of
lenses on it. Validation badges, U07 Phase 4, use the same machinery.

**Goals**

1. One lens model for the Cytoscape view and the WebGL view, with four channels: size,
   colour, badge and edge style. At most one lens per channel is on at a time.
2. Every lens has a legend, a column in the list view and a text description, so no lens
   is colour alone.
3. Inferred edges are drawn dashed on every graph view, under materialized reasoning and
   RDFS on read.
4. Algorithm lenses run F14 algorithms as view-only runs, on the drawn subgraph or on a
   projection of the dataset, and never write back.
5. "Why is this inferred?" shows a well-founded derivation tree from facts the caller can
   read.
6. Lens data is bounded and computed under the caller's view, at the state the canvas
   shows.

**Non-goals**

- Writing scores or communities back. F14's write-back and C22's builds stay where they are,
  and the lens says that its numbers are not saved.
- New algorithms. Lenses use F14's list.
- "Why not?" and justifications of inconsistencies. They stay in F18's panels on the dataset
  page.
- Lenses on the Explore overview's class level, where nodes are classes. U06 already sizes
  them by instance count.

## 2. User-visible behaviour

### 2.1 The lens bar

Explore's graph toolbar and the query page's graph toolbar get a **Lenses** menu, shortcut
`Shift+L`, with one row per channel.

```
┌─ Lenses ─────────────────────────────────────────────────────────────────────┐
│ Size     [PageRank ▾]  scope: drawn graph ▾   ✓ 214 nodes · 0.2 s            │
│ Colour   [Leiden communities ▾]  resolution 1.0   9 communities               │
│ Badges   [none ▾]                                                             │
│ Edges    [inferred dashed ✓]                                                  │
│ These views are computed for this picture and are not saved.                  │
└──────────────────────────────────────────────────────────────────────────────┘
```

| Channel | Lenses | Encoding |
|---|---|---|
| Size | degree, a numeric property, PageRank, betweenness, closeness of BFS depth from a node, k-core, triangles | Node area by quantile, five steps, with the value in the tooltip and list. |
| Colour | class, graph, a property's values, WCC, label propagation, Louvain, Leiden, C22 community | Twelve qualitative colours by size of category, then "other". |
| Badge | validation results (U07 Phase 4), has inferred edges | A glyph with a count in a node's corner. |
| Edge style | inferred dashed | Dashed with an `i` marker. |

The legend docks in the canvas's corner and lists each encoding with its text, such as
"size is PageRank, larger is higher" and "colour 3 is community 12, 41 nodes". Clicking a
legend entry selects its nodes. The list view of U06 §2.4 and Explore's node list gain a
column per active lens.

### 2.2 Inferred edges (Phase 1)

When the drawn graph includes inferred triples, they are dashed with an `i` marker, and the
legend says "dashed edges were inferred by the reasoner". An edge is inferred when it is
visible with reasoning on and absent with reasoning off, which covers materialized
inferences and RDFS on read alike. The lens is on by default when the dataset has reasoning,
and it can be turned off. A node whose every drawn edge is inferred keeps its normal border,
since the node itself exists in the data.

### 2.3 Algorithm lenses (Phase 2)

Choosing an algorithm lens asks for its **scope**.

- **Drawn graph.** The algorithm runs over the drawn nodes and the edges among them, which
  answers "who is central in what I am looking at". It is the default and takes well under
  a second at Explore's sizes.
- **Projection.** The algorithm runs over a named F14 projection or over the predicates the
  person picks, on the whole dataset, and the drawn nodes show their scores in that larger
  graph. Such a run can take seconds to minutes, so it runs in the background with a
  progress bar and **Cancel**, and its result is kept on the server for reuse (§3.2).

Parameters are the F14 ones a person would change: damping and iterations for PageRank,
samples for betweenness, resolution and seed for Leiden and Louvain, and the level for
hierarchical communities. A changed parameter runs again. The lens shows the run's summary,
such as iterations, convergence and modularity, from `algo:summary`.

Community colours stay stable across runs. The colours go to communities in order of their
size in the drawn graph, and when a new run's community overlaps an earlier one's members by
half or more, it keeps that one's colour.

**C22 communities.** When the dataset has GraphRAG reports and the caller may read them, the
colour menu also offers "GraphRAG community", which colours by the stored `gr:member`
triples at the chosen level and links a community's legend entry to its report in the
Communities tab.

### 2.4 Why is this inferred? (Phase 3)

Clicking a dashed edge, or pressing `W` on a focused one, opens **Why?** in the side panel.

```
┌─ Why is this inferred? ────────────────────────────────────────────────────────┐
│ ex:alice  rdf:type  ex:Employee                                   [Show on graph]│
│ ▾ derived by rdfs9 (subclass)                                                   │
│   ├ ex:alice rdf:type ex:Manager          ▾ derived by rdfs2 (domain)           │
│   │   ├ ex:alice ex:manages ex:team7      asserted · graph ex:hr                │
│   │   └ ex:manages rdfs:domain ex:Manager asserted · graph ex:ontology          │
│   └ ex:Manager rdfs:subClassOf ex:Employee   asserted · graph ex:ontology       │
│ 1 of 2 derivations  [Next]                         depth 2 · from commit 318    │
└────────────────────────────────────────────────────────────────────────────────┘
```

- **Tree.** Each derived node names its rule, with its profile and, for OWL 2 RL, the rule's
  name in the OWL 2 RL tables, and its premises. Asserted leaves name their graph. Axioms say
  so. A subtree cut at the depth limit expands on request.
- **Show on graph.** The premises are drawn on the canvas and highlighted, so the person sees
  the chain that produced the edge.
- **Several derivations.** **Next** shows another derivation whose top rule differs, up to
  five.
- **Stale inferences.** When C08 reports the inferences as stale, the panel says so above the
  tree, because the tree explains the closure as it was computed.

### 2.5 Keyboard

| Key | Action |
|---|---|
| `Shift+L` | Open the lens menu. Its rows are a grid of comboboxes. |
| `Shift+1` to `Shift+4` | Turn the size, colour, badge or edge lens off and on again. |
| `W` | Open Why? for the focused inferred edge. |
| `Enter` on a legend entry | Select its nodes. |

A polite live region announces lens results, such as "Size by PageRank on 214 nodes, top
node ex:acme with 0.081", and each element's description gains its lens values. Under reduced
motion, sizes and colours change at once instead of animating.

### 2.6 The 390 px layout

The lens menu is a bottom sheet with one row per channel, and the legend is a collapsible
strip above the graph that shows the active encodings in one line. Why? opens as a full-height
sheet with the tree indented by 12 px per level and long terms shortened to local names,
with the full term on tap.

### 2.7 Empty and error states

| Case | What the lens shows |
|---|---|
| A numeric lens over nodes without values | Those nodes keep the smallest size and the legend counts them as "no value". |
| A projection run over its budget | F14's `507` message, with the advice to narrow the projection. |
| A dataset without reasoning | The inferred-edge lens is absent from the menu. |
| A server without the Phase 2 prerequisite | Projection scope is disabled with the reason, and drawn-graph scope still works through `/{ds}/sparql`. |
| A derivation not found from visible facts | "No derivation from facts you can read was found." Nothing else is said. |
| A derivation over the budget | "The derivation is deeper than the search allows", with the tree found so far. |

## 3. Server and API changes

### 3.1 Phase 1

Phase 1 needs no new route. Property and class lenses read the drawn nodes' values with one
`SELECT` per 200 nodes, using a `VALUES` block, as Explore's time-slider checks already do.
Degree is counted in the browser for the drawn graph, or with one grouped count per 200 nodes
for the degree in the dataset.

The inferred-edge lens asks `/{ds}/sparql` with `reasoning=false` which of the drawn edges
exist, in batches of 200 edges in a `VALUES ?s ?p ?o` block. An edge visible with reasoning
and absent without it is inferred. The check reads at the canvas's `at` and branch. Explore
draws at most 1,500 nodes, so a full check is at most a few dozen small queries.

### 3.2 Phase 2 prerequisite, cached view-only algorithm runs

F14's `POST /$/algo/{ds}/run` without `write` already streams rows without writing anything.
Two additions let a lens use it on large graphs.

1. **`nodes`.** A request may list up to 20,000 node terms, and the rows are filtered to
   those nodes after the run. A community run also returns the size of each community in the
   whole projection, so the legend can say "community 12, 41 drawn of 3,980".
2. **`keep`.** With `keep: true`, the run's full result is kept in a per-server cache of
   view-only results, keyed by the dataset, branch, commit, the caller's read view
   (`GraphAccess::read_key`), and the request with `nodes` left out. A later request with the
   same key and other `nodes` reads the cache instead of running again. The cache holds at
   most 8 results and 512 MiB, charged to the server memory pool, and drops the oldest first.
   A result is about 16 bytes per node for scores and 8 for communities, so a run over ten
   million nodes takes about 160 MiB.
3. **Background runs.** With `keep: true` and `async: true`, the run is a task, followed and
   cancelled through `/$/tasks/{id}` as write-back runs already are, with progress from F14's
   iteration counters. It writes nothing, needs only `read`, and its result goes into the
   cache. A request for the same key while the task runs joins it instead of starting
   another.

The run is admitted through F14's thread admission and the query memory pool, as any run is.
The routes, the task kind and the cache metrics are added to `docs/API.md`, the route table,
the OpenAPI description and `/metrics`.

Drawn-graph scope needs no prerequisite. It runs `SERVICE algo:…` through `/{ds}/sparql` with
a nested-pattern projection over the drawn nodes:

```sparql
PREFIX algo: <urn:x-sparkles:algo#>
SELECT ?node ?score WHERE {
  SERVICE algo:pageRank {
    [] algo:start ?s ; algo:end ?o ; algo:node ?node ; algo:score ?score .
    VALUES ?s { … }
    VALUES ?o { … }
    ?s ?p ?o .
  }
}
```

The nested pattern follows F14 §3.1, which evaluates it once and takes its distinct pairs as
the projection. The UI writes the drawn nodes into both `VALUES` blocks, at most 1,500 each,
so the projection is the drawn subgraph, and it leaves out `rdf:type` edges unless the class
nodes are drawn.

### 3.3 Phase 3 prerequisite, explanations over a materialized closure

F18 Phase 1 designs `POST /$/reason/{ds}/explain`, whose answer is a derivation tree of
`Derivation` nodes (F18 §2.1). Its engine is the demand transformation of F17 Phase 1, and
neither is built. Phase 3 needs only the `why` mode for triples that are already
materialized or derived by RDFS on read, so it adds a first engine for the same route and
answer.

- **Search.** For an inferred triple, the server enumerates the rule instances whose head is
  the triple, with the reasoner's existing instance search over the dataset's rule program,
  and keeps those whose premises are all in the closure. It then explains each premise the
  same way, by iterative deepening. A premise counts at depth `d` only when it is asserted, an
  axiom, or has an instance whose premises count at depth `d − 1`. Every fact of a closure
  computed to its fixed point has a finite such depth, so the search finds a well-founded
  tree whenever one exists within `maxDepth`, and a cycle of facts that only support each
  other is never accepted.
- **Why not the kept closure's records.** F18 §3.1 rejects reading first derivations from a
  kept closure, because the closure's row order stops giving well-founded trees after
  incremental deletions. This search does not read row order. It checks well-foundedness
  itself, so it is correct after any sequence of incremental runs.
- **Budgets.** The search visits at most 100,000 rule instances and stops at the request's
  timeout, ten seconds by default. Repeated premises are memoized per request and shared in
  the answer by `id`, as F18 §2.1 already allows.
- **Scope.** The answer has the shape of F18 §2.1 with `mode: "why"` only. A `whyNot` request
  answers `501` until F18 Phase 1 lands. When F18 Phase 1 lands, its demand engine replaces
  this search behind the same route, and the tests of both compare their trees on the same
  closures.
- **Access.** The search reads the caller's view, as F18 §7.3 requires. A derivation is
  returned only when every leaf is visible to the caller. When no such derivation exists, the
  answer is `404` with `code: "no-visible-derivation"`, the same answer as for a triple that is
  not entailed, so the caller cannot tell a hidden derivation from none.

The route, the library call `Dataset::reasoning().explain`, the CLI and the MCP tool follow
F18 §2 for the `why` mode. This prerequisite is F18's first deliverable in practice, so F18's
Outcome will record it when it lands.

## 4. Design

### 4.1 The lens model

`lib/lens.ts` holds the model, with no Svelte and no fetches.

```ts
type Channel = "size" | "colour" | "badge" | "edge";
type LensValue = number | string | null;
type LensResult = {
  channel: Channel; lens: string; values: Map<string, LensValue>;   // element id to value
  legend: { key: string; text: string; count: number }[];
  summary?: string; computedAt: { commit: number; branch: string };
};
```

A lens source is a function from the drawn element ids, the dataset, the state and an
`AbortSignal` to a `LensResult`. The canvas maps results to styles through one encoding table
per channel, so the Cytoscape and WebGL views draw the same lens the same way. When the drawn
graph changes, only the new elements are fetched, and a lens whose source depends on the whole
drawn graph, such as drawn-graph PageRank, runs again after the canvas has been still for one
second.

### 4.2 Scales and palettes

Sizes use five quantile steps over the drawn values, so one outlier does not flatten the rest,
and the legend gives each step's range. Colours use a twelve-colour qualitative palette with
light and dark variants, defined as CSS tokens beside `--added` and `--removed`, and checked
for contrast against both backgrounds. Categories beyond twelve share "other", which the
legend lists with its count.

### 4.3 Interaction with the time slider

A lens is computed at the canvas's state. When U01's slider moves, lens sources rerun at the
new commit after the graph has redrawn. In compare mode, numeric lenses show the value at the
right handle and the change since the left one in the tooltip and the list. Projection-scope
runs are not rerun on every step of **Play**, which would start a background run per commit.
The lens pauses with "paused while playing" and runs again when play stops.

## 5. Limits and budgets

- **Drawn nodes.** Lenses apply to at most 20,000 drawn nodes. Above that, in the WebGL view,
  a numeric lens fetches the top 1,000 drawn nodes by value from a kept projection run and
  gives the rest the smallest size, and says so.
- **Batches.** Property, class, degree and inferred-edge queries go in batches of 200 terms
  or edges, at most four in flight.
- **Drawn-graph runs.** At most 1,500 nodes, under the query timeout and budgets.
- **Projection runs.** F14's budgets, thread admission and memory pool apply. The cache of
  §3.2 bounds what is kept.
- **Derivations.** 100,000 rule instances and ten seconds per request, at most five trees,
  depth 32, with deeper subtrees cut as `ref` nodes.

## 6. Access, coverage, branches and past states

Every lens reads under the caller's view. Property, class and degree lenses are ordinary
queries. Algorithm runs build their projection from the caller's view, as F14 §2.3 says, so a
score never counts a hidden edge, and the cache key includes the view so no caller reads a
result computed for a wider one. The `nodes` filter returns rows only for nodes the projection
contains, so asking for a hidden node returns nothing rather than an error that would confirm
it.

The C22 colour lens follows C22's coverage rule (C22 §6.3). It is offered only when the read
guard answers `proven` for the caller and the reports graph, and it reads `gr:member` triples
through that guard, so a reader whose view differs from the build's view does not see
memberships computed over facts they cannot read.

The inferred-edge lens compares two reads of the same view. Derivations come from the
caller's view under F18 §7.3, as §3.3 says.

Lenses read at the canvas's `at` and branch. A projection run at a past state reads that
state's snapshot under F06, and its cache entry is keyed by its commit. GraphRAG reports exist
only on `main`, so the C22 lens is absent on other branches and at past states, with the
reason.

## 7. Architecture

| Part | Where |
|---|---|
| Lens model, scales, palettes | `ui/src/lib/lens.ts` (new) |
| Lens sources | `ui/src/lib/lens-sources.ts` (new), with the algorithm sources in `ui/src/lib/algo.ts` (new) |
| Lens menu and legend | `ui/src/lib/components/LensMenu.svelte`, `LensLegend.svelte` (new) |
| Drawing | `GraphView.svelte` and `LargeGraphView.svelte` gain a `lens` prop of encodings per element |
| Why? panel | `ui/src/lib/components/WhyPanel.svelte` (new) |
| Cached view-only runs | `sparkles-server`'s algorithm routes and task kinds, and `sparkles-core`'s run code (`sparql/algorun.rs`) |
| Explanation search | `sparkles-reasoner`, beside its incremental instance search, and the reasoning routes of `sparkles-server` |
| Mock server | `ui/mock/` gains canned algorithm rows and derivation trees |

## 8. Phases

1. **Phase 1.** The lens model, menu, legend and list columns in both views, the degree,
   property, class and graph lenses, and inferred edges drawn dashed.
2. **Phase 2.** First the server prerequisite of §3.2. Then drawn-graph and projection
   scopes, the algorithm lenses, stable community colours and the C22 colour lens. This phase
   closes the community-colouring part of roadmap item 8's gap.
3. **Phase 3.** First the server prerequisite of §3.3. Then the Why? panel, **Show on
   graph** and **Next**.

## 9. Acceptance examples

The data is a dataset with 300 people in three groups densely linked inside each group by
`ex:knows`, one person `ex:hub` linked to everyone, an ontology with
`ex:Manager rdfs:subClassOf ex:Employee` and `ex:manages rdfs:domain ex:Manager`, the triple
`ex:alice ex:manages ex:team7`, and materialized RDFS.

- **A1. Dashed.** Expanding `ex:alice` draws `ex:alice rdf:type ex:Employee` dashed with an
  `i` marker, and `ex:alice ex:manages ex:team7` solid.
- **A2. RDFS on read.** With the same data and RDFS on read instead of materialization, A1
  holds.
- **A3. Drawn-graph PageRank.** With the 300 people drawn, size by PageRank makes `ex:hub` the
  largest node, and the list column shows its score, which equals the score of the same
  `SERVICE algo:pageRank` query run in the editor.
- **A4. Leiden colours.** Colour by Leiden at resolution 1.0 and seed 42 colours three
  communities, and running again with seed 43 keeps the colours of communities whose members
  overlap by half or more.
- **A5. Kept run (Phase 2 prerequisite).** A projection-scope PageRank with `keep: true` runs
  once, and a second request for other `nodes` at the same commit and view returns from the
  cache without a new run, which the cache metric counts.
- **A6. View in the cache key.** A caller whose grants hide the graph of half the `ex:knows`
  triples gets different scores from A5, and no request of theirs reads A5's cache entry.
- **A7. Derivation (Phase 3 prerequisite).** `POST /$/reason/{ds}/explain` for
  `ex:alice rdf:type ex:Employee` returns a tree with `rdfs9` at the root, an `rdfs2` subtree
  for `ex:alice rdf:type ex:Manager`, and asserted leaves naming their graphs.
- **A8. Cycle.** With `ex:a owl:sameAs ex:b` and OWL 2 RL, the derivation of
  `ex:b owl:sameAs ex:a` is well founded and never lists a fact among its own premises.
- **A9. Hidden premise.** For a caller whose grants hide the ontology graph, A7's request
  answers `404 no-visible-derivation`, as for a triple that is not entailed.
- **A10. Why? panel.** Clicking the dashed edge of A1 shows A7's tree, and **Show on graph**
  draws `ex:alice ex:manages ex:team7` and the two ontology triples highlighted.
- **A11. Frame time.** With a colour lens and a size lens on 20,000 nodes in the WebGL view,
  panning stays at 30 frames per second or more on the reference machine of U06 A2.
- **A12. Keyboard and phone.** A3 and A10 can be done with the keyboard alone, and at 390 px
  with taps alone, without horizontal scrolling.

## 10. Testing

- **Rust.** For §3.2, the `nodes` filter, community sizes, cache keys with branch, commit and
  view, the cache bound and its pool charge, joined background runs, cancellation, and
  admission. For §3.3, trees against hand-checked RDFS and OWL 2 RL derivations, the
  well-foundedness of every returned tree, memoized sharing, budgets, `ref` cuts, closures
  after incremental deletions, RDFS on read, and the `404` for hidden leaves.
- **Router.** The run route with `nodes`, `keep` and `async` under a restricted principal, the
  task's read-only permission, the explain route's `why` and `whyNot` answers, and OpenAPI
  equality.
- **Vitest.** `lib/lens.ts` for quantile steps, palettes and "other", stable community
  colours, and the encoding tables. `lib/lens-sources.ts` for batching, incremental fetches
  and the inferred-edge comparison. `lib/algo.ts` for query text and rows.
- **Playwright against the mock server.** A1, A3, A4, A10 and A12, and the empty states.
- **Playwright against a real server.** A1, A2, A3, A7, A9 and A10.
- **Performance.** A11 in the performance project of U06.

## 11. Rejected alternatives

- **Computing algorithms in the browser** with graphology's metrics. They would see only the
  drawn graph, so projection scope would be impossible, and the numbers would disagree with
  F14's for the same graph.
- **Writing lens runs back to a scratch graph.** It needs write grants a reader may not hold,
  creates commits for a view, and leaves data behind. A cache keyed by view needs none of
  that.
- **Marking inferred edges by asking for their graph.** RDFS on read has no graph, and a
  triple can be both asserted and inferred. Comparing reads with and without reasoning covers
  both.
- **Waiting for F17 before any explanation.** F17 is a large item, and the derivation search of
  §3.3 answers the common question over closures that already exist, with the answer shape
  F18 Phase 1 will keep.

## 12. Open questions

1. Whether the Phase 3 prerequisite should be built before F17, as this spec proposes, or the
   Why? panel should wait for F18 Phase 1 and its demand engine.
2. Whether the cache of kept runs should also serve U06's overview, so a kept Leiden run could
   group a class into communities without a write-back, which U06 §11 declined for runs inside
   the request.
3. Whether lens choices should be part of Explore's URL, so a link reproduces the colouring as
   it reproduces the slider's state.

## Outcome

### Phase 1, 2026-10-23

Phase 1 shipped the lens model, the menu, the legend and the list columns, with the degree,
property, class, graph and inferred-edge lenses, on Explore's resource graph and on the query
page's graph view with both renderers. Phases 2 and 3 remain [ROADMAP](../ROADMAP.md) item
105.

**What was built.** `ui/src/lib/lens.ts` holds the model, the quantile steps, the palette and
the encoding table, and `ui/src/lib/lens-sources.ts` the reads. `ui/src/lib/lenses.svelte.ts`
keeps one canvas's lens state, so that both pages share it. `LensMenu.svelte` and
`LensLegend.svelte` are the menu and the legend, `GraphView.svelte` and
`LargeGraphView.svelte` take a `lens` encoding and a `marked` set, and `GraphListView.svelte`
takes a column per lens. The twelve colours and "other" are the `--lens-1` to `--lens-12` and
`--lens-other` tokens of `app.css`, and a unit test checks that each has at least 3:1
contrast against the surface in the light theme and both dark ones. The palette was chosen
for the task rather than taken from ColorBrewer, some of whose qualitative colours fall below 3:1 on
white.

**Decisions the spec left open.**

- *`reasoning=false` and RDFS on read.* The server's `reasoning=false` left RDFS on read on,
  so the comparison of §3.1 could not see inferences on read. `reasoning=false` now turns
  RDFS on read off for the request as well, which a router test checks and
  [API.md](../API.md) documents. This is the only server change of the phase.
- *Where the lenses are.* The Explore overview is left out. Its class level is a non-goal,
  and its member levels belong with the algorithm lenses of Phase 2. Explore's resource
  graph and the query page had no node list, so both gained a **List** view, which is the
  list that §2.1 gives a column per lens.
- *Degree.* Two size lenses count it, one in the drawn graph in the browser and one in the
  dataset as the triples a node is subject or object of.
- *A node with several values.* The class, graph and property colour lenses give the node
  the value most drawn nodes share, so that the categories gather. A numeric property takes
  the node's largest value, and a value is numeric when its lexical form is a number. The
  property picker lists the properties of the first 200 drawn IRIs, and the size channel
  only those whose values are mostly numbers.
- *Class.* The class lens reads the asserted classes with `reasoning=false`, because under
  reasoning every node is also in each superclass.
- *Graph.* The graph lens names a node's named graph, "several graphs" for a node used in
  more than one and "the default graph" for one used in none. The inferred graph is left
  out, since the edge lens shows what it adds.
- *Dashes.* While the edge lens is on, `rdf:type` edges are drawn solid, so that a dash means
  an inferred edge only. The `i` marker is a `ⓘ` before the edge's label on the canvas
  renderer and a circled `i` at the middle of the edge in the WebGL view.
- *Edges a read cannot name.* An edge with a blank node or a triple term, or with a
  predicate that is a result column's name, is not checked, and the legend counts it.
- *Results of a query.* The edges of a `CONSTRUCT` result need not be in the data, so the
  query page also reads them with reasoning, and an edge is inferred only when that read
  finds it and the read without reasoning does not.
- *The badge.* The Phase 1 badge counts a node's drawn inferred edges.
- *WebGL.* Sigma has no dashed edges, so the WebGL view hides the inferred edges and draws
  them dashed on its overlay canvas, up to 4,000 of them, with markers while there are at
  most 600. Badges go on the same overlay, up to 1,500 on screen.
- *Selection.* A legend entry rings its nodes and fades the rest, as a finding's entities
  are marked in the overview, and leaves the single selection of the side panel alone.
- *Keys.* `Shift+1` to `Shift+4` read the key's position, so they work on keyboards whose
  shifted digits are other characters.
- *Budgets and freshness.* The four requests in flight of §5 are shared by all of a
  canvas's lenses, and the badge and edge lenses share one inferred-edge read. A read that
  reaches 100,000 rows is refused with an error instead of being kept, since its answer may
  be cut. Lens values are kept per state and per drawing, so a graph drawn again from new
  reads, such as a refocus or a re-read at the head, reads them again. The query page reads
  them at the commit its result names.
- *Open question 3.* Lens choices are not part of the URL.

**Deferred.** The compare-mode deltas of §4.3, a numeric lens's change since the left
handle, wait for Phase 2, where projection runs need the same pairing of two states. Above
20,000 drawn nodes the lenses are off with the reason, since the top 1,000 of §5 needs the
kept runs of §3.2. Category colours follow category sizes on each run, and the stable
community colours of §2.3 come with Phase 2.

**Tests.** Vitest covers the quantile steps, "other", the palette's contrast, the encoding
table, batching, incremental reads and the inferred-edge comparison
(`lens.test.ts`, `lens-sources.test.ts`). The mock Playwright suite `tests/mock/lenses.spec.ts`
covers A1, the size, colour and graph lenses with their legend and list columns, the
numeric property's "no value", the absent edge lens without reasoning, the query page on both
renderers and the 390 px layout of A12. `tests/e2e/lenses.spec.ts` runs A1 and A2 against a
real server. A3 to A11 need Phases 2 and 3.

### Phase 2, 2026-10-23

Phase 2 shipped the cached view-only algorithm runs of §3.2, the algorithm lenses of §2.3
with drawn-graph and projection scope, stable community colours, the GraphRAG community
lens, the pause while playing and the compare-mode deltas of §4.3, and the top 1,000 of §5
above 20,000 drawn nodes. Phase 3 remains [ROADMAP](../ROADMAP.md) item 105.

**What was built on the server.** `crates/sparkles-server/src/http/algo/view.rs` takes the
`nodes`, `keep`, `async` and `top` members out of a run request before the rest parses as
F14's request, so F14's request type and its other callers are unchanged. A view-only run
computes the whole result at the snapshot that `at` and `branch` select, under the caller's
query view and the query timeout and memory budget, and answers in SPARQL JSON with
`communitySize`, the run's `summary` and a `run` member. The cache key, the bounds and the
pool charge are those of §3.2, with three additions. The key also holds the request's
`default-graph-uri` and `named-graph-uri`, the dataset's inferred overlay and its RDFS on
read, as the overview cache's key does, and the definition of a named projection, which can
change without a commit. The request's part of the key is the query text it stands for,
with every prefixed name expanded by the dataset's prefixes at the time of the request,
because a prefix change makes no commit, and the run computes that same text. `POST /$/graphrag/{ds}/memberships` answers which communities hold
the listed nodes, on main and through the coverage guard. `/metrics` gains six
`sparkles_algo_view_cache_*` series, and API.md, the route table and the OpenAPI
description document the members, the task kind and the route.

**What was built in the UI.** `ui/src/lib/algo.ts` holds the algorithm list, the drawn-graph
query, the projection run with its task and the membership reads. The size channel offers
PageRank, betweenness, BFS depth from a node, k-core and triangles, and the colour channel
WCC, label propagation, Louvain, Leiden and GraphRAG community. The menu shows each
algorithm lens's scope, projection, parameters, level and start node, and a background run's
progress bar and **Cancel**. `ui/mock/algo.mjs` answers `SERVICE algo:`, the run route and
the memberships route in the mock, with small textbook versions of the algorithms.

**Decisions the spec left open.**

- *Request members.* `async` needs `keep`, and all four members are refused with `400` on a
  run with `write`. `nodes` takes IRIs only, since a blank node cannot be named across
  requests. `top` orders by the first numeric output, highest first, and a request without
  `nodes` answers every row up to the server's row limit, with `truncated` set when it cut.
- *Reading the cache.* A request without `keep` still reads a kept result, and only `keep`
  stores one.
- *Memory.* A kept result keeps decoded terms, about 100 to 150 bytes per node and output
  instead of the 16 bytes of §3.2, so a hit needs no dictionary lookup. Each result holds a
  grant of the memory pool of its estimated size, which queries then cannot use, and the
  grant lives with the result, so a request that still reads a result the cache dropped
  keeps it charged. The cache never holds more than half of the pool, so the other half
  always stays for query admission. A result larger than that, or one the pool has no room
  for even after dropping the older results, is answered and not kept, and the task's
  `detail` says `kept: false`. The decoded rows are charged to the request's memory budget
  and pool grant every 1,024 rows while they are decoded, and the summary literal is
  decoded once rather than for every row. A result that is kept gives its bytes back to the
  run's grant before the cache asks the pool for its own, so the two never hold it at once.
- *Budgets.* A view-only run takes the request budget parameters that a query takes, and its
  answer is held to the result-byte limit, whether it was computed or read from the cache.
- *Background runs.* A background run has an update's deadline, none unless the request or
  `--update-timeout` gives one, and takes its pool grant when its task starts, as a
  write-back run does. F14's algorithms have no iteration hooks, so the progress moves by
  phase. The callers that start or join a run can follow its task even under a restricted
  view, and the caller that started it, or a dataset admin, can cancel it. These rights
  are checked again on every request against the caller's current read access and view,
  and they are remembered for as long as the server lists the task. A joining caller's
  **Cancel** only stops following the run. A cancelled run is never joined, so a request just
  after a cancel starts a new run.
- *Branches.* A run without `write` may read a branch, and one with `write` answers `400`
  there. Named projections belong to main, so on a branch the menu offers only predicate
  projections.
- *Drawn-graph runs.* A drawn-graph lens runs after the canvas has been still for a second
  and takes at most 1,500 drawn IRIs. Projection scope offers the drawn edges' predicates,
  each of them alone, and the stored projections.
- *Levels.* A hierarchical lens shows the last level, which is the coarsest and the one
  whose modularity the summary reports, and a level picker appears when a run has several.
  The GraphRAG lens starts at the lowest level.
- *Stable colours.* A community keeps an earlier community's colour when its members hold
  half or more of that community's members. Among the earlier communities that pass this
  test and whose colour is still free, it takes the one it overlaps most. Communities claim colours in order of their
  drawn size, each earlier colour goes to one community at most, and the colour memory
  carries over between the community lenses of one canvas.
- *GraphRAG.* The lens is offered on main at the head when the dataset has built reports
  that the caller may read. On a branch, at a past state, or for a caller whose access does
  not cover the build, the menu says why it is absent. A legend entry links to the
  community's report in the Communities tab.
- *Projection reads.* After a background run, the lens reads the kept result at the run's
  commit, and every later chunk at that same commit, so a commit that lands during the
  reads cannot mix two states. An answer from another commit, or one the server cut at its
  row limit, is shown as an error instead of a partial lens.
- *Compare mode.* Deltas apply to size lenses whose values come from the dataset, the
  degree in the dataset, a numeric property and projection runs, and they read both handles.
  Drawn-graph values belong to one drawing, so they show no delta.
- *Above 20,000 drawn nodes.* Only a size lens with projection scope runs. It asks each
  chunk of 20,000 nodes for its top 1,000 and keeps the top 1,000 of all, and the rest are
  drawn at the smallest size.

**Deferred and limits.** The BFS start node is chosen from the first 500 drawn IRIs. A
background run's progress has phases, not iterations, until F14 reports its iterations.
Lens choices are still not part of the URL. No measured slowdown was accepted, so no
follow-up item was added.

**Tests.** Rust unit tests cover the member parsing, the `nodes` filter, community sizes per
level, `top`, and the cache's keys, bound and pool charge. Router tests cover the filter,
kept results across commits and parameters, branches, background runs that are joined,
cancelled and kept, a request right after a cancel, a queued run cancelled before it
starts, the memory budget against decoded rows, the result-byte limit, a changed prefix,
and caller records that outlive a full table. Under restricted principals they cover a view
that never reads a wider kept result, a caller that follows and cancels its own run, and
callers that lose the task when their grants change. Unit tests cover the cache's half of
the pool and a reader that keeps a dropped result charged. The
GraphRAG route tests cover memberships and coverage. Vitest covers the query text, rows and
levels, projection runs with their tasks, chunks and top, reads pinned to the run's commit,
truncated answers, polling that stops on abort, stable colours, the stillness delay, the
pause while playing, compare deltas, the top 1,000, a late cancel answer and the GraphRAG
lens at a past state
(`algo.test.ts`, `lens.test.ts`, `lenses.test.ts`). The mock Playwright suite
`tests/mock/algo-lenses.spec.ts` covers A3, A4, projection scope with progress and Cancel,
the GraphRAG lens and the 390 px sheet, and `tests/e2e/algo-lenses.spec.ts` runs A3, A4 and
A5 against a real server. A6 is covered by the router tests, and A11 still needs its frame
time measurement in U06's performance project.

### Phase 3, 2026-10-24

Phase 3 shipped the server prerequisite of §3.3, `POST /$/reason/{ds}/explain` in the `why`
mode over a materialized closure and over RDFS on read, and the Why? panel of §2.4 with
**Show on graph**, **Next**, **Expand** and the stale note. Open question 1 is settled in
favour of building the search before F17. With this phase every part of
[ROADMAP](../ROADMAP.md) item 105 has shipped, and items 135 to 139 carry what it left out.

**What was built in the engine.** `crates/sparkles-reasoner/src/explain.rs` reads the
closure as the input graphs plus the inferred graph through one masked snapshot, and walks
backward from the triple one level at a time. Each level enumerates the rule instances
whose head is a fact of the frontier, with the incremental search's compiled plans in an
instances mode, and keeps those whose premises all hold in the closure. After each level a
bucket-queue fixpoint gives every fact its least well-founded depth, 0 for a fact of a
readable input graph, 1 for an axiom, and one more than the deepest premise for a derived
fact. The walk stops at the first level where the triple has a depth, then walks three
more levels to find derivations with other top rules, and gives up after 128 levels. A
cycle of facts that only support each other never gets a depth, so it is never shown, and
the search reads no row order of a kept closure, so it is correct after any sequence of
incremental runs. Rules are compiled per source, so a node names the profile or
vocabulary its rule came from, and an OWL 2 RL rule also carries its name in the OWL 2 RL
tables. `crates/sparkles-core/src/sparql/rdfs/explain.rs` explains what RDFS on read
answers with the rules of Jena's `MatchRDFS`, rdfs2, rdfs3, rdfs5, rdfs7, rdfs9 and rdfs11,
over the schema graph's direct edges.

**What was built around it.** The answer types `Derivation`, `RuleRef`, `Explained` and
`ExplainLimits` live in `crates/sparkles-core/src/explain.rs` with `finish`, which numbers
the nodes, shares a repeated fact by `id` and cuts nodes deeper than `maxDepth` as `ref`.
`crates/sparkles/src/reasoning/explain.rs` holds the library call
`Dataset::reasoning().explain`, the JSON request and answer, the Turtle form of a triple
and the text form of a tree. The server route is `crates/sparkles-server/src/http/explain.rs`,
with `read` on the dataset, the query rate limit class and the query view. `sparkles infer
--why`, the MCP tool `explain_inference`, `Dataset.reasoning.explain` in Python and
`reasoning.explain` in Node call the library. API.md documents the route, and the OpenAPI
description names it `explainInference`.

**What was built in the UI.** `ui/src/lib/why.ts` asks the route and turns its answer into
what the panel shows, and `WhyPanel.svelte` draws the tree as an ARIA tree with folds.
Explore opens it from a click on an inferred edge, from the **Why?** button beside an
inferred value, or with `W`. **Show on graph** draws the premises and marks them on the
canvas through GraphView's `whyMarked`. `ui/mock/reason.mjs` explains the mock reasoner's
rules by iterative deepening, so the mock suites run the panel.

**Decisions the spec left open.**

- *Which closure.* A materialized tree reads the store at the commit of the last run, so a
  stale tree explains the closure as it was computed. When the history no longer has that
  commit, the tree reads the head and the answer says `readAtHead`. Freshness comes from
  C08's record.
- *Leaves.* A leaf is a fact of a graph the last run read that the caller may read. The
  inferred graph is read only when the caller may read it. A triple that is itself
  asserted answers with one asserted node.
- *Without materialized inferences.* When the dataset has RDFS on read and no materialized
  inferences cover the triple, the answer comes from RDFS on read with `source:
  "rdfsOnRead"`. Its data graphs are the default graph and the readable named graphs, and a
  schema leaf names the schema graph or none when the schema is a document.
- *Several derivations.* `maxTrees` is at most 10 and the panel asks for 5. Derivations
  with other top rules come shallowest first, and `more` says whether the search found more
  than were returned.
- *Budgets.* The search visits at most 100,000 rule instances and stops at the request's
  timeout, ten seconds by default and never more than the server's query timeout. Either
  limit answers `422` with `code: "reasoning-budget"`. The CLI exits with 2 there and with 1
  when no derivation is found. `maxDepth` is 32 by default and at most 64.
- *Memory.* The search holds each fact it reached with its depth and the instances that
  derive it, about 10 MB at the instance limit. The library call charges that state, the
  rule evaluations and the decoded terms of the answer to an optional memory limit. The
  route and the MCP tool set it to the query memory budget and take no pool grant, and the
  route holds its answer to the result-byte limit of a query. An RDFS on read answer is
  measured against the same limit once its trees are built, since it holds at most one
  tree per rule. Passing the memory limit answers `422` as the other limits do.
- *Deadline.* The timeout is checked between rules and inside each rule's evaluation, so a
  rule whose candidates all fail stops at it, and a stale run's history replay and the
  RDFS on read search stop at it and at a cancelled request too. A timeout too large to
  represent answers `400`. The timeout stays within the server's ceiling as a query's
  does, even when the ceiling is below the ten-second default.
- *Other top rules.* The trees after the first read depths worked out with the explained
  triple excluded, so no tree derives its triple from itself through another top rule.
- *Sharing.* A repeated fact is shared by `id` within one tree only, so the panel shows
  each tree on its own, and ids stay unique across the answer.
- *RDFS on read limits.* Every schema neighbour the search visits counts as one of its
  100,000 reads, so a very wide hierarchy meets the limit and the request's controls as
  the rows stream. A subclass or subproperty fact of the schema's closure is explained on
  its own, which is what **Expand** asks for when a chain is cut at the depth limit.
- *Past states.* The route explains the latest inferences only. While the time slider
  shows a past commit, the panel says so instead of loading a tree, and the premises it
  marked are cleared.
- *Expand.* A `ref` node asks the route for its own first tree and grafts it in place,
  with the subtree's ids moved past every tree's, so a deep chain costs one request per
  level the person opens. An expansion whose explanation was replaced is dropped, and one
  that reads inferences of another commit is refused with a note to open Why? again.
- *Show on graph.* The panel turns off **Hide rdf:type** and turns on **Literals** when the
  premises need them, so the drawn chain is complete.

**Deferred and limits.** A `422` answer has no partial tree, which item 135 adds. A
derivation through a generalized triple, such as one with a literal subject that the
closure uses but the store does not hold, or through a GeoSPARQL default geometry, is not
found and answers `404`, which item 136 covers. A tree leaves out the builtins and the
bindings of the rule that fired, which item 137 adds. The RDFS on read schema that a
search needs is built, as for a query, without the request's timeout or cancellation,
which are checked before and after it, and item 138 makes that build interruptible. A
schema the caller cannot read is not built, though a triple a readable graph asserts is
still its own answer. When a dataset has both materialized inferences and RDFS on read, a
derivation that needs both, such as an RDFS subclass over a materialized type, is not
found, and item 139 joins the two. The search reads only what the caller
reads, so its answer and its time cannot tell a hidden derivation from none. `whyNot` answers `501` until F18 Phase 1.

**What F18 can build on.** The answer types and `finish` are F18 §2.1's `Derivation` with
its sharing and depth cuts, and F18's engines can fill them as they are. The route, its
access and budget answers, the library call, the CLI, the MCP tool, the bindings and the
Why? panel are F18 §2's surfaces for the `why` mode, so F18 Phase 1 adds `whyNot` behind
them and replaces the search with its demand engine. The tests in
`crates/sparkles-reasoner/tests/explain.rs` check well-foundedness and the visible leaves on
any tree, so they can compare the two engines on the same closures as §3.3 asks. The RDFS
on read explainer stays as it is, because F17 does not cover that path. The mock's
explainer gives F18's UI work a backend without a reasoner build.

**Tests.** The reasoner tests cover A7, A8 with `owl:sameAs`, A9, an asserted triple, an
axiom, the depth cut, the instance budget, a shared premise, derivations with other top
rules, another top rule that could only derive the root from itself, the deadline in a
rule whose candidates all fail, the memory limit, and every inferred triple after
incremental deletions. The library tests cover
input graphs, a hidden ontology, a stale run, RDFS on read with and without a readable
schema, and its deadline. Router tests cover the route and A9 as the same answer as a triple that is not
entailed, and the MCP, CLI and parity suites cover their surfaces. Vitest covers the tree
helpers and the route's answers (`why.test.ts`). The mock Playwright suite
`tests/mock/why.spec.ts` covers A10, the stale note, a past state, no derivation, and A12
with the keyboard and at 390 px, and `tests/e2e/why.spec.ts` runs A7, A9 and A10 against a real
server.
