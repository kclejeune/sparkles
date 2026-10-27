# Visualization and editing capabilities

This page describes the available capabilities and their current limits. The
[capability matrix](../FEATURES.md#capability-status) records delivery status within
each stated scope.

## Web UI

The SvelteKit UI is embedded in the server binary.

### Query page

* The query page has an editor with completion of declared prefixes, linting and
  formatting in the browser, and results as a table, graph or plan.
* The **Saved** menu lists the dataset's stored queries, shows a form for a query's
  parameters and runs it. An admin can save the editor's query with parameters picked from
  its variables, or delete one.
* The Ask bar has **Preview first** and **Run, then show**, a step indicator, summaries
  whose `[n]` markers select table rows, clarification choices, follow-up questions and
  **Summarize again** ([USAGE.md](../USAGE.md#the-ask-bar)). The page keeps a local history
  of questions, saves a question and its query as a stored example, and takes suggested
  examples from readers.
* The explanation of a plan shows beside the plan tree, with links that select
  operators, and a stopped query offers **Explain why**.
* The Pattern pane, opened with `Alt+P`, draws the query's graph pattern beside the editor
  and edits it ([U08](../specs/U08-visual-query-builder.md),
  [USAGE.md](../USAGE.md#the-pattern-pane)). Classes show as subtitles, filters as chips,
  `OPTIONAL` parts as hulls, and the rest as locked blocks that keep their text. Edits on
  the canvas are splices of the text that one undo reverts. Pickers fed by the schema
  supply nodes and edges, and forms chosen by datatype supply filters. See the
  [pane beside a query](../images/pattern.png) and the
  [predicate picker](../images/pattern-edit.png).
* **Plan on pattern** draws the plan of the query that ran on its pattern, with estimated
  and actual rows on each edge, misestimates marked, the rows that passed each filter and
  join badges in run order ([U04](../specs/U04-live-query-profiling.md),
  [screenshot](../images/plan-pattern.png)).
* Graph results above 2,000 nodes draw with the WebGL view, up to 100,000 nodes.
* The path finder also works over a result graph and adds the nodes a path needs
  ([USAGE.md](../USAGE.md#finding-paths-on-the-query-page),
  [screenshot](../images/query-paths.png)).

### Explore

* The explorer shows an entity's properties, its nearest vector neighbours, ranked
  `text:query` search and a map card with Nearby. The Similar page searches from an entity
  or a pasted vector.
* Explore opens on an overview of the dataset's classes and the predicates between them,
  drawn with sigma.js on WebGL and laid out with ForceAtlas2 in a web worker
  ([U06](../specs/U06-large-graph-rendering.md), [USAGE.md](../USAGE.md#the-explore-overview),
  [API.md](../API.md#graph-overview)). A class opens into its members, or above 5,000
  instances into communities or groups by its best splitting property. **Group by** can
  draw the GraphRAG communities instead, with a community's report beside the picture
  ([screenshot](../images/explore-communities.png)). A list view (`L`) is the accessible
  companion, and without WebGL the overview falls back to Cytoscape.
* The **Paths** panel (`Shift+P`) finds the shortest path, every shortest path or the k
  shortest paths between two entities, with filters for length, direction, predicates and
  hubs and an optional weight from reifier properties
  ([U09](../specs/U09-path-finder.md), [USAGE.md](../USAGE.md#finding-paths-in-explore),
  [screenshot](../images/path-finder.png)). A path opens as a query or joins the drawn graph.
* The **Lenses** menu (`Shift+L`) sets a node's size, colour and badge and an edge channel
  on the canvas and the WebGL view ([U10](../specs/U10-canvas-lenses.md),
  [USAGE.md](../USAGE.md#lenses-on-the-graph), [screenshot](../images/explore-lenses.png)).
  Size follows degree or a numeric property, colour follows class, graph or a property's
  values, and the edge lens draws inferred edges dashed. Each lens has a legend whose
  entries select their nodes.
* Algorithm lenses size nodes by PageRank, betweenness, BFS depth, k-core or triangles,
  and colour them by WCC, label propagation, Louvain, Leiden or GraphRAG communities
  ([USAGE.md](../USAGE.md#algorithm-lenses), [API.md](../API.md#view-only-runs),
  [screenshot](../images/explore-algo-lenses.png)). A lens runs over the drawn graph or over
  a projection of the dataset, in the background with progress and **Cancel**.
* A click on a dashed edge, a **Why?** button or `W` opens the derivation tree of an
  inferred triple beside the graph ([USAGE.md](../USAGE.md#why-is-this-inferred),
  [screenshot](../images/explore-why.png)). **Show on graph** draws and highlights the
  premises, **Next** shows up to five derivations with other top rules, and a note says
  when the inferences are stale ([screenshot](../images/explore-why-stale.png)).
* **Facets** (`Shift+F`), in Explore and as a tab of `SELECT` results, count a set's
  values by numeric or year ranges or by the 12 most frequent values
  ([U11](../specs/U11-linked-views.md#outcome), [USAGE.md](../USAGE.md#facets-and-use-in-query),
  [API.md](../API.md#facets), [screenshot](../images/explore-facets.png)). Brushing a histogram
  dims the entities outside it, **Apply to query** writes the matching triple and
  `FILTER`, and **Use in query** writes up to 1,000 selected entities as a `VALUES` block.
* Explore's **Map** (`Shift+M`) and a result's **Arcs** draw relations between located
  entities as great-circle arcs coloured by predicate
  ([USAGE.md](../USAGE.md#arcs-on-the-map)). Close markers and parallel arcs merge, and a
  list of markers and arcs serves keyboards and screen readers. A selection is shared by
  the maps and the graph canvases.
* `/ui/embedding` draws a vector index's entities on a plane, placed by PCA or a force
  layout of the neighbour graph and coloured by class or a predicate's values
  ([USAGE.md](../USAGE.md#the-embedding-map), [API.md](../API.md#embedding-map),
  [screenshot](../images/embedding-map.png)). An index above 50,000 entities is sampled, a
  lasso selects entities, and **Open as graph** draws them in Explore. A kept projection
  costs about 24 bytes of server memory per entity, with at most four and 1 GiB in all.
* The validation overlay badges the SHACL and ShEx results of the drawn nodes as a lens,
  from the write-time guard, a shapes graph or the Validate panel's shapes
  ([USAGE.md](../USAGE.md#the-validation-overlay),
  [U07](../specs/U07-data-quality-and-schema-diagram.md#outcome),
  [screenshot](../images/validation-overlay.png)). **Show triples** marks the triples
  involved, and **Suggest fix** writes an update, previews it as a dry run and runs it only
  after that preview.

### Maps

* Results with geometry literals get a Map tab with a popup per row. The browser reads
  CRS84, EPSG:4326 and Web Mercator, and other CRSs go through `POST /$/geo/convert`.
* The dataset page's Spatial index panel shows the index's status, CRSs and memory,
  configures or rebuilds it, and maps the indexed geometries in view.
* The maps use MapLibre GL JS over a bundled Natural Earth basemap, and fetch nothing from
  elsewhere unless `serve --map-style-url URL` names a MapLibre style.

### History and time travel

* The UI shows commit history, write receipts, the changes of each commit, named
  snapshots, and query results of a past state.
* A time slider moves the query results, an entity's properties, the Explore graph, the
  schema tab and the Memory tab through the commits, compares two commits or two branch
  heads with changes marked, plays through the commits and follows new ones
  ([USAGE.md](../USAGE.md#the-time-slider)).
* The changes between two commits can be drawn as a graph, grouped by class when they are
  many, and played as a time-lapse of one commit's diff per step
  ([USAGE.md](../USAGE.md#commit-diffs-on-the-graph)). See the screenshots of
  [a diff graph](../images/diff-graph.png) and [a time-lapse](../images/diff-lapse.png).
* The History panel draws every branch as a commit graph, and the Branches panel and merge
  page manage branches.

### Datasets, validation and editing

* The dataset page has cards for the full-text, vector and spatial indexes, with create,
  edit, rebuild, drop and a recall measurement for vector indexes. It uploads documents
  for ingestion, edits settings with each field's source, and shows quotas and compaction.
* The schema browser reads `/$/schema`, with graph selection, an inference toggle, and
  observed counts and object kinds beside the declarations and SHACL constraints. Its
  class panel shows profiles, **Compare** shows a schema diff, and **Draft shapes** drafts
  SHACL or ShEx shapes and installs them as a guard in `warn` mode.
* The Validate panel switches between SHACL, in Turtle or SHACLC, and ShEx with a shape
  map, lists each failure, and downloads ShapeMap JSON. The Write-time validation panel
  configures the guard and shows the head's results, the last validated write and recent
  rejections. The query editor shows the results of a rejected write.
* Editing forms generated from SHACL shapes open from Explore's **Edit** or
  `/datasets/{name}/edit?iri=…` ([U02](../specs/U02-shape-driven-forms/SPEC.md#outcome),
  [USAGE.md](../USAGE.md#editing-forms), [API.md](../API.md#editing-forms),
  [screenshot](../images/forms.png)). Fields merge every applying shape and follow
  `sh:group`, `sh:order`, `sh:name` and known DASH editors. The browser checks values as
  the person types, **Preview** runs a dry run, and **Save** commits one update, to `main`
  or to a session branch when `main` is protected. A concurrent change to a field shows a
  conflict view. Checks follow shape dependencies beyond a fixed graph radius and
  report all current violations on the affected targets. Incomplete discovery refuses
  the check, and a failed recheck invalidates earlier browser approval.
  `Dataset::forms()` and Python's `Dataset.forms` expose the same forms.
* The Memory page shows what memory holds about an entity with each fact's source, quote
  and history, and its Inbox tab reviews facts with **Accept all that pass**. The
  Communities tab shows the GraphRAG hierarchy and reports.
* The server page shows readiness, requests, caches, backups and a Models section for
  providers, the model configuration and write-only keys
  ([USAGE.md](../USAGE.md#changing-models-and-keys-at-runtime),
  screenshots of the [form](../images/models-settings.png) and its
  [errors](../images/models-settings-errors.png)). The model configuration is a form built from
  the `ModelsSettings` schema of the OpenAPI description. Its fields are typed and
  bounded, allowed models are a list that can be reordered, per-model options have a card
  per model, and role lists have a row per entry. Each field is checked as it is edited,
  a refusal from the server shows under the field it names, and the JSON view under
  **Advanced** shares the form's unsaved change.

### Nested form read models

The Rust library exposes `dataset.forms().nested_model(&request, &options)` for
applications that need declared blank-node child layouts. The result retains the
ordinary root form and a `nested_nodes` pool. Shared children keep one RDF identity,
cycles reference that identity, and parent anchors and field values retain their graph
provenance. The target-graph policy resolves independently for each child's focus.
The request uses the same shapes settings, visibility, historical-state
selection and cancellation controls as an ordinary form.

One request admits at most 64 nested identities, 512 generated field views, 512 parent
anchors and 16 levels of first discovery. A shared preparation allowance refuses a
larger or unfinished model. Complex nested paths, visible blank-named graph contexts
and matching composite range bounds also refuse until their representation or producers
have controlled admission. The allowance covers adapter preparation and retention.
Source-shape and prefix preparation remain under item 21, while physical reader and
mask preparation depend on [item 395](../ROADMAP.md). The
[specification](../specs/U02-shape-driven-forms/SPEC.md#231-nested-read-models-in-the-rust-library)
explains the unit calculation.

The HTTP forms and browser still use the existing flat model. Atomic nested changes,
fresh children and nested check/save and browser consumers remain under item 21.

## Known gaps

These limits name the roadmap items that address them. Current priorities and
dependencies are recorded in the [central roadmap](../ROADMAP.md).

### Web UI

* Forms edit predicate and inverse paths with literal and IRI values. Blank node values
  with `dash:DetailsEditor`, list editors for `sh:memberShape` and choices for `sh:or` and
  `sh:xone` remain planned, and ShEx schemas do not drive forms. Checks admit up to 500
  additional distinct affected targets beyond the directly touched resources, shared
  across shapes and snapshots. They refuse a larger or unfinished discovery instead of
  returning partial conformance. Forms admit graph-free target-node expressions made
  from constants, lists, focus variables, basic list operators, conditionals and simple
  aggregates. Target filters that inspect only their input node are admitted. Graph-reading
  target filters and other target-node expression producers are refused before evaluation and
  remain under roadmap item 21. Directly anchored SPARQL triple and path dependencies are
  admitted. Property functions, computed predicates and indirect query-dependency producers
  refuse an incomplete check until bounded
  localization is available, while genuinely global queries enumerate bounded targets.
  Ordinary SHACL validation retains its separate language
  support. Discovery work bounds do not establish a total SHACL heap limit. Controlled
  cold-reader admission waits for item 395.
* The Pattern pane builds and edits basic graph patterns, filters, one level of
  `OPTIONAL`, simple paths, the projection and the modifiers. `UNION`, `GRAPH`, inline
  `VALUES`, `MINUS` and `FILTER NOT EXISTS` are not editable on the canvas, a waiting node
  is not kept across a reload, and the pane needs the formatter's browser module with no
  server fallback. Roadmap item 101.
* The plan drawn on the pattern shows the numbers of a plan after it ran or was
  explained, and does not update while a query runs. Roadmap item 27.
* Layouts are not computed or cached on the server and there is no binary transfer
  format, so a WebGL view lays out every result in the browser. Frame-rate and main-thread
  targets have no automated test on a GPU runner, and the overview opens one path below
  one class at a time, with no grouping by named graph. Roadmap item 29.
* The embedding map cannot colour by a kept Leiden run, `Shift+M` does not open it, date
  facets count whole years only, and at phone width the facets sit below the graph. The
  Explore map and facets read one dataset on its main branch. The `VALUES` block of
  **Use in query** is a locked block on the Pattern canvas. Roadmap item 140.
* The frame rate of arcs and of the embedding map has been measured only in software
  rendering, roadmap item 141. Projections are kept in memory only and are not offered to
  callers with partial access, item 142.
* The validation overlay validates the drawn nodes instead of reading a recorded result,
  roadmap item 120. Under an incremental guard with other results in the dataset, a
  preview cannot say whether a fix resolves its result, item 121. Callers whose grants
  cover only some graphs get no overlay, item 75.

## Configuration and usage

The following guides describe configuration and supported UI, command-line, HTTP
or library workflows. Exact HTTP contracts remain in the API reference.

* Read [The Ask bar](../USAGE.md#the-ask-bar).
* Read [The Pattern pane](../USAGE.md#the-pattern-pane).
* Read [Finding paths on the query page](../USAGE.md#finding-paths-on-the-query-page).
* Read [The Explore overview](../USAGE.md#the-explore-overview).
* Read [Finding paths in Explore](../USAGE.md#finding-paths-in-explore).
* Read [Lenses on the graph](../USAGE.md#lenses-on-the-graph).
* Read [Algorithm lenses](../USAGE.md#algorithm-lenses).
* Read [Why is this inferred?](../USAGE.md#why-is-this-inferred).
* Read [Facets and Use in query](../USAGE.md#facets-and-use-in-query).
* Read [Arcs on the map](../USAGE.md#arcs-on-the-map).

## Roadmap

The remaining work described above refers to [item 21](../ROADMAP.md), [item 27](../ROADMAP.md), [item 29](../ROADMAP.md), [item 75](../ROADMAP.md), [item 101](../ROADMAP.md), [item 120](../ROADMAP.md), [item 121](../ROADMAP.md), [item 140](../ROADMAP.md), [item 141](../ROADMAP.md), [item 142](../ROADMAP.md).
The central roadmap owns priority and dependencies. Its
[completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs)
link partially complete specs to their remaining work and acceptance criteria.
