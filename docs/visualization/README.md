# Visualization and editing capabilities

This page describes the available capabilities and their current limits. The
[capability matrix](../FEATURES.md#capability-status) records delivery status within
each stated scope.

## Web UI

The SvelteKit UI is embedded in the server binary.

### App shell

* Every page sits in one shell. A top bar holds the brand, a search field whose first
  segment switches the dataset and names the branch a page has open, the connection
  status, the theme and the account. A rail beside the page lists the working pages and,
  below a rule, the administration pages. From 1200 pixels the rail shows labels, below
  that it shows icons with their names as tooltips, and from 760 pixels down it moves into
  a drawer that the menu button opens, together with the dataset switcher, the status,
  the theme and the account. Choosing a dataset on a page that names one in its address
  reopens that page on the new dataset.
* `Ctrl+K` or `Cmd+K` focuses the search field from anywhere, including the query
  editor, and `/` does the same outside a text field. Escape gives the focus back to
  where it was. On a phone the bar shows a search button, and the button or the keys
  expand the field over the whole bar with a back button. The field searches the open
  dataset's classes, properties, individuals and SKOS concepts through the
  [entity search](../search/README.md#entity-search), with the matched text
  highlighted, and Enter opens a class or property in the ontology editor, a concept in
  the thesaurus editor and anything else in Explore
  ([screenshot](../images/shell-search.png)).
* The search is the launcher that item 631's command palette fills. A page or feature
  registers a provider with `launcher.register` in `ui/src/lib/launcher.svelte.ts`, and
  the field shows each provider's items as a group.
* One set of design tokens in `ui/src/app.css` gives the colours of the light and dark
  themes, a type scale, a 4-pixel spacing scale, the page gutter of 24, 20 and 16
  pixels at large, medium and phone widths, control heights, radii and shadows. Every text
  colour is at least 4.5:1 against the page and panel backgrounds in both themes, which
  a unit test checks. Pages share the panel, button, count badge, table, tab, form row,
  page head and context row styles, and a list beside its detail ends both panes on the
  same line ([medium width](../images/shell-medium.png),
  [phone](../images/shell-phone.png)).

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
  Blank-node values of `sh:node` fields are nested forms, SHACL 1.2 list fields are
  ordered list editors, and `sh:or` or `sh:xone` value alternatives offer a choice for
  each value (screenshots of [nested forms](../images/forms-nested.png) and
  [lists](../images/forms-list.png)).
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

<a id="nested-form-read-models"></a>

### Nested forms, lists and alternatives

A field whose property shape names `sh:node` shows each blank-node value as a nested form,
built from every `sh:node` declaration that reaches the child. The form reads them from the
nested model, which keeps each child's RDF identity, the visible edges it hangs from and
the graphs of its values. Shared children appear once, and a cycle refers to the child
already shown. **Add** starts a new child from the empty form of the field's shapes.
Removing a child deletes its edge and, when nothing else refers to it, its visible triples
and those of the blank nodes under it. One save commits the changes to the resource and
to every child, after the same guard, access and conflict checks as a flat form. Under the
writer lock the save checks that each stored child still hangs from its parent, and a
child removed meanwhile shows a conflict on the parent's field.

A field with SHACL 1.2 list constraints edits its `rdf:List` in order, with buttons that
move, remove and add members, and checks `sh:minListLength`, `sh:maxListLength`,
`sh:uniqueMembers` and the member shape in the browser. A save replaces the stored list.
A field whose `sh:or` or `sh:xone` names value shapes without paths offers a **Kind of**
select for each value. The choice selects the alternative's widget and constraints, and
the browser reports a value that matches none of the alternatives, or for `sh:xone` more
than one.

![A SKOS concept's form with its stored SKOS-XL label edited and a new label added as nested forms](../images/forms-nested.png)

![A SKOS collection's form with a member chosen among the sh:or alternatives and the ordered member list after a move](../images/forms-list.png)

The Rust library exposes the same model as `dataset.forms().nested_model(&request,
&options)`, with the ordinary root form, a `nested_nodes` pool and `templates`. HTTP
takes `nested=true` on the model route and `nested` entries in check and save bodies, and
Python takes `nested=True`. One request admits at most 64 nested identities, 512
generated field views, 512 parent anchors and 16 levels of first discovery. A shared
preparation allowance refuses a larger or unfinished model, and the form then falls back
to the flat model with nested values read-only. Complex nested paths, visible
blank-named graph contexts and matching composite range bounds also refuse until their
representation or producers have controlled admission. The allowance covers adapter
preparation and retention, while physical reader and mask preparation depend on
[item 395](../ROADMAP.md). The
[specification](../specs/U02-shape-driven-forms/SPEC.md#231-nested-read-models-in-the-rust-library)
explains the unit calculation.

### Ontology editor

The **Ontology** page, `/ui/ontology`, browses and edits the OWL ontology of one graph
and its imports
([U12](../specs/U12-ontology-workbench/SPEC.md#outcome),
[reasoning guide](../reasoning/README.md#owl-2-ontologies),
[screenshot](../images/ontology-editor.png)). Trees of classes, object, data and
annotation properties and individuals sit beside the selected entity's frame, which
lists its annotations, characteristics and axioms and adds and removes them. The frame's
add fields take class expressions and whole axioms in Manchester syntax, with
completion of entity names and labels and an underline under the part the server could
not read, or in functional-style syntax. One strip of tabs holds the entity kinds, the
DL Query tab and the header, and it wraps onto a second line in a narrow window.

The **DL Query** tab answers a Manchester class expression with its equivalent classes,
its direct and inferred superclasses and subclasses, and its instances
([screenshot](../images/ontology-dl-query.png)). Each answer opens the entity's frame
beside the query, and the query is kept in the page's address. A label above the
answers names the reasoning that found them, the EL classifier inside OWL 2 EL or the
OWL 2 RL rules outside it, says whether they are complete, and lists the axioms outside
the profile that can leave them incomplete. The
header tab edits the ontology IRI, version IRI, imports and header annotations, and the
usage tab lists every axiom that mentions the entity. **Inferred** shows the EL
classifier's taxonomy beside the asserted class tree
([screenshot](../images/ontology-inferred.png)). Every edit is previewed as a dry run and
committed with the commit it was previewed against
([screenshot](../images/ontology-preview.png)), on the current branch or on a session
branch that **Start session** creates. Undo and redo are previewed commits too, and
another writer's commit reloads the page through the change feed.

### Thesaurus editor

The **Thesaurus** page, `/ui/thesaurus`, edits the SKOS concept schemes, concepts and
collections of one graph, with their SKOS-XL labels
([U12](../specs/U12-ontology-workbench/SPEC.md#thesaurus-editor),
[screenshot](../images/thesaurus-editor.png)). The concept tree of the chosen scheme
starts at its top concepts and reads one level of `skos:broader` and `skos:narrower` at a
time, in both directions. A concept with several broader concepts appears under each of
them with a count of its broader concepts, and a concept of the scheme without a broader
one is marked when it is not a top concept. The search finds concepts by any preferred,
alternative or hidden label, SKOS-XL labels included, in every language or in one, and the
tree can show its labels in a chosen language.

Dragging a concept onto another moves it there, and holding Ctrl or Alt adds the new
broader concept and keeps the old one. Each move writes both directions of the hierarchy
and is previewed as a commit before it is written
([screenshot](../images/thesaurus-move.png)). Dropping a concept on the top zone makes it a
top concept of the scheme. The page refuses a move under one of the concept's own
narrower concepts.

The **Labels** tab lists a resource's preferred, alternative and hidden labels by language,
marks the SKOS-XL ones and converts a label between its plain and SKOS-XL forms. It
refuses a second preferred label in one language and a label that is both preferred and
alternative, and it lists the clashes the data already has. The **Relations** tab edits
broader, narrower, related and mapping links, with a picker that searches concepts by
label in another scheme or another dataset. A link written in one direction only is
marked, and its repair adds the other direction. The **Members** tab edits a collection's members and an
ordered collection's `skos:memberList` with the list editor of the
[editing forms](#nested-forms-lists-and-alternatives), and the **Notes** tab edits notes
and documentation through a form of the built-in SKOS shapes.

The **Checks** tab runs the built-in SKOS shapes over the graph
([screenshot](../images/thesaurus-checks.png)), which check the integrity conditions of the
SKOS Reference, and an administrator can install them as the dataset's write guard. The
page shares the session model of the [ontology editor](#ontology-editor). Every edit of
the tree, labels and links is a dry run with `If-Match` before it commits, undo and redo
revert commits, **Start session** creates a `skos.<user>.<date>` branch, another writer's
commit reloads the page, and a protected branch is read-only. The page is also read-only
when it cannot read the commit its graph is at. The notes and members forms save through
the editing forms, which check the stored values of each field they change instead of the
commit. They are editable only when the dataset keeps no graph but the one the page edits
and their target is that graph, since a form's save could otherwise change another graph,
and they are read-only for a caller whose grants cover only some graphs. With
authentication on, they are also read-only on branches other than main, whose grants the
page cannot read, for every caller but a server administrator. The [API reference](../API.md#skos-shapes) lists the shapes'
conditions.

## Known gaps

These limits name the roadmap items that address them. Current priorities and
dependencies are recorded in the [central roadmap](../ROADMAP.md).

### Web UI

* Forms edit predicate and inverse paths with literal and IRI values, nested blank-node
  children, lists and value alternatives. Node-level `sh:or` and `sh:xone` alternatives
  with their own properties are not offered as sub-forms (612), nested values
  reached through complex paths are read-only (613), and blank-named graph
  contexts refuse the nested model (614). List members are IRIs or literals, so a
  list of blank nodes is read-only. ShEx schemas do not drive forms. Checks admit up to 500
  additional distinct affected targets beyond the directly touched resources, shared
  across shapes and snapshots. They refuse a larger or unfinished discovery instead of
  returning partial conformance. Forms admit graph-free target-node expressions made
  from constants, lists, focus variables, basic list operators, conditionals and simple
  aggregates. Target filters that inspect only their input node are admitted. Graph-reading
  target filters and other target-node expression producers are refused before evaluation and
  remain under roadmap item 615. Directly anchored SPARQL triple and path dependencies are
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
* The ontology editor reads every entity and hierarchy axiom of a graph to draw its
  trees, up to 50,000 of each, 607. When that listing stops early, as on the Gene
  Ontology, one SPARQL query reads the labels of the selected graph so the trees still
  show labels, but the hierarchy above the cut is missing. Only the class tree has an inferred view, 609, and
  the IRI policy is kept in the browser per dataset, 610. Rename covers the graph the
  page reads and not IRIs inside triple terms, under item 457's refactoring phase. The
  editable schema diagram is a further U12 phase of item 457.
* The DL Query tab's instances come from a classification with the assertions, which
  the dataset does not keep, so each instance query classifies the ontology again, and
  a query outside OWL 2 EL copies the ontology into memory for the RL rules, item
  662. Class answers under the RL rules can miss entailed subsumptions, and the
  label marks them as possibly incomplete.
* The thesaurus editor reads at most 2,000 narrower concepts of a concept and 5,000 roots
  of a scheme, and the label search scans the labels for a substring and answers
  its first 100 matches, 638. A mapping's target in another dataset is named by its
  IRI, and the page lists only the mappings a concept states itself, 639.
  `skosxl:labelRelation` links between SKOS-XL labels are not edited, 640. The
  SKOS rules become a write guard only by replacing the dataset's guard, so a dataset
  cannot keep its own shapes and the SKOS shapes together, 641. Editing forms
  read every graph and have no graph or commit precondition of a request's own, so the
  thesaurus editor's notes and members forms are read-only in a dataset with more than one
  graph and their saves do not name the commit the page shows. A form finds a resource's
  shapes through the default graph, so a resource kept only in a named graph gets no
  editable form, 642. Blank-node SKOS-XL labels of
  one kind with the same literal form cannot be told apart by an update and are shown
  without edit buttons.
* The ontology editor decides whether the caller can write from the caller's level on the
  dataset, so a caller whose grants allow writes on a branch only sees the branch
  read-only, 611. For the same reason the thesaurus editor's notes and members forms are
  read-only on branches other than main when authentication is on. Undo and redo revert commits, which needs write access to the
  whole branch, so a caller whose grants cover some graphs only can edit but not undo.
* The ontology browser has no pages in the web UI. Its typo-tolerant typeahead runs on
  the server as the [entity search](../search/README.md#entity-search), and the entity
  pages, hierarchy and graph views and the browser's search box remain in item 616. The
  ontology editor's tree filter matches the names it has loaded and does not call the
  entity search, item 660.

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

The remaining work described above refers to [item 616](../ROADMAP.md), [item 612](../ROADMAP.md), [item 613](../ROADMAP.md), [item 614](../ROADMAP.md), [item 615](../ROADMAP.md), [item 27](../ROADMAP.md), [item 29](../ROADMAP.md), [item 75](../ROADMAP.md), [item 101](../ROADMAP.md), [item 120](../ROADMAP.md), [item 121](../ROADMAP.md), [item 140](../ROADMAP.md), [item 141](../ROADMAP.md), [item 142](../ROADMAP.md).
The central roadmap owns priority and dependencies. Its
[completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs)
link partially complete specs to their remaining work and acceptance criteria.
