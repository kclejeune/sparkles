# U10: Lenses on the graph canvas

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 adds lenses to the Explore graph and the WebGL view, a
> shared way to size, colour, badge and style drawn elements from data computed for them,
> with a legend and a list column, and the first lenses: degree, a property's values, class,
> graph and inferred edges drawn dashed. Phase 2 adds algorithm lenses, which size nodes by
> PageRank or another centrality and colour them by a Leiden run that is never written back,
> after a server prerequisite that caches view-only algorithm runs. Phase 3 answers "Why is
> this inferred?" with the reasoner's derivation tree for a dashed edge, after a server
> prerequisite that explains inferences over a materialized closure.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

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

Not built. See [ROADMAP](../ROADMAP.md) item 105.
