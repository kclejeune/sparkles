# Visualization and editing capabilities

This page describes the available capabilities and their current limits. The
[capability matrix](../FEATURES.md#capability-status) records delivery status within
each stated scope.

## Web UI

The SvelteKit UI is embedded in the server binary.

### App shell

* Every page sits in one shell. A top bar holds the brand, a search field whose first
  segment switches the dataset and names the branch a page has open, the connection
  status, the theme and the account. The search field stays in the middle of the
  viewport on large and medium screens. A rail beside the page lists the working pages and,
  below a rule, the administration pages, in this order: Query, Explore, Browse,
  Workbench, Similar and Memory, then Datasets and Admin. Workbench, Similar and Admin are
  groups whose pages show under them while one of their pages is open or when the chevron
  opens them. Workbench holds the Ontology and Thesaurus editors, Similar holds Search and
  Map, and Admin holds Server, Backups and Tokens. The rail shows Workbench only to a caller
  who may write the open dataset, so a reader's rail has Browse alone. A page has at most one
  row of tabs, and a further choice inside a page, such as the editor's entity kind, is a
  select or a segmented control. From 1200 pixels the rail shows labels, below
  that it shows icons with their names as tooltips, and from 760 pixels down it moves into
  a drawer that the menu button opens, together with the dataset switcher, the status,
  the theme and the account. Choosing a dataset on a page that names one in its address
  reopens that page on the new dataset. A **Prefixed** or **Full IRIs** switch beside the theme
  button, also in the palette, chooses how every page writes IRIs. Prefixed names come from
  the dataset's prefix map together with the conventional prefixes such as `rdf`, `owl`,
  `skos` and `dcterms`, and the choice is kept per viewer in the browser.
* The search field is also the command palette. `Ctrl+K` or `Cmd+K` opens it from
  anywhere, including the query editor, and pressing it again closes it. `/` opens it
  outside a text field, except on a page with a search of its own, where `/` focuses that
  search instead, such as the class tree filter of the ontology editor, the label search
  of the thesaurus editor and the field of Explore's text-search tab. Escape gives the focus back to where it was, and a
  command that runs in place returns the focus there before it runs. On a phone the bar
  shows a search button, and the button or the keys expand the field over the whole bar
  with a back button ([screenshot](../images/palette-phone.png)).
* With nothing typed, the palette lists the viewer's recent picks, the commands of the
  open page and the pages of the shell ([screenshot](../images/palette.png)). A typed
  query also searches the open dataset's classes, properties, individuals and SKOS
  concepts through the [entity search](../search/README.md#entity-search), the shell's
  commands, the datasets, the open dataset's saved queries and, on the Query, Ontology,
  Thesaurus and dataset pages, the dataset's branches. Enter opens an entity's page in
  Browse and a saved query in a new tab of the Query page
  ([screenshot](../images/shell-search.png)). On Explore the field is also the resource
  finder. Its placeholder says so, and its first group lists the dataset's resources by
  label or IRI, an **Open** row for a typed IRI or prefixed name and **Ask a question…**
  when the dataset answers questions. A pick focuses the resource in the graph.
* A prefix shows one kind of item. `>` lists commands and pages, `@` entities, `#`
  datasets and branches, and `?` saved queries, and a row of chips above the results sets
  the same prefixes by touch. Escape on a prefix with nothing after it returns to every
  kind before a second Escape closes the palette ([screenshot](../images/palette-commands.png)). Commands,
  pages, datasets, branches and saved queries are ranked in the browser by an exact
  match, a prefix, a word start, every word at a word start, initials such as `fq` for
  Format query, and one typo in a query of five or more letters. A command's keywords
  match only at the start of a word. Entities keep the server's typo-tolerant ranking.
* The shell's commands switch the dataset or branch, open a saved query, set the theme,
  open the dataset settings, the server status, the access tokens and the backups, and
  create a dataset. The Query page adds running, formatting, explaining and saving the
  query, a new tab, history and the pattern pane. The Ontology page adds starting a
  session, creating a class, property or individual, undo, redo and the DL query tab,
  and the Thesaurus page adds a session, new concepts, schemes and collections, the
  checks, undo and redo. The Datasets page refreshes its list and the dataset page opens
  its tabs. Each row shows its keyboard shortcut where it has one.
* The recent picks are kept per viewer in the browser's local storage, newest first, and
  the palette works without them when storage is blocked. A recent command shows only
  while its page offers it, and a recent entity or saved query only on its dataset.
* A page or feature extends the palette through `ui/src/lib/launcher.svelte.ts`.
  `useCommands` registers the page's commands, each with a label, an optional shortcut,
  keywords, an `active` predicate and a run function, for as long as the page is
  mounted. `useProvider` and `launcher.register` add a group of items, and
  `usePageSearch` gives `/` to the page's own search field.
* One set of design tokens in `ui/src/app.css` gives the colours of the light and dark
  themes, a type scale, a 4-pixel spacing scale, the page gutter of 24, 20 and 16
  pixels at large, medium and phone widths, control heights, radii and shadows. Every text
  colour is at least 4.5:1 against the page and panel backgrounds in both themes, which
  a unit test checks. Pages share the panel, button, count badge, table, tab, form row,
  page head and context row styles, and a list beside its detail ends both panes on the
  same line ([medium width](../images/shell-medium.png),
  [phone](../images/shell-phone.png)).
* Every page's content starts at the gutter. A workspace page's tools are one
  `.toolbar` row with the primary action at its end, and where the row does not fit the
  end group takes a row of its own, flush right. On a phone, buttons in a toolbar or a
  page head keep their icons and drop their labels, which stay their accessible names.
  The Query toolbar does the same for **History**, **Pattern** and **Format** below
  1,200 pixels, and Explore's graph tools do it whenever the canvas is narrower than
  860 pixels, such as beside the details panel, where a still narrower canvas wraps
  the tools to more rows.
  Panels side by side in a `.panel-grid` are as tall as the tallest in their row,
  headline numbers share one `.figures` grid, a choice of two to four values is one
  `.segmented` control, and the menus under a button share `.dropdown-menu`. At 760
  pixels and below a dialog docks at the bottom of the screen as a sheet whose body
  scrolls under a fixed head and footer.
* A data table wider than its box scrolls inside the box with its actions column pinned to
  the right edge. On a phone such a table shows each row as a card, with the first cell as
  the card's title, the other cells under their column names and the actions in a row at
  the bottom. A row of filters that no longer fits beside its search folds into a
  **Filters** button that counts the active filters, and the search stays in view. A page's
  own search, such as Browse's and Memory's, is one tall field across the content width.
  A page head is a grid from 640 pixels, with the title and what describes it on the left and
  the page's buttons level with the title on the right. Buttons marked primary stay in view
  and the others fold into a **More** menu once the buttons would take more than two fifths of
  the head. A page's headline counts are one row of chips.

### Query page

* The query page has an editor with completion of declared prefixes, linting and
  formatting in the browser, and results as a table, graph or plan.
* The toolbar holds the query's kind and endpoint, **Examples**, **Saved** and
  **Options**, then the pattern pane, **Format**, **Explain** and **Run** at its end.
  **Options** holds the branch, the past state to read, the history slider, the row
  limit and the inferences toggle, and its button names any of them that differ from
  the default, including inferences left out on a dataset that reasons.
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
  or a pasted vector. The top bar's search finds the resource to start from, and the
  graph toolbar's **View** menu shows literals and hides `rdf:type` edges.
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
* `/ui/similar/map` draws a vector index's entities on a plane, placed by PCA or a force
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
* Memory shows what agents recorded as cards. On Explore, a resource's Memory tab gives one
  card for each predicate, with the values it holds, each with its review status, the
  confidence the agent gave it and a line naming the agent, the source and the start of the
  quote it cited. A value that several facts state shows a marker for each, a conflict marks the card it
  belongs to, and a citation marker opens the graph, time and every other detail. The
  history of facts that were superseded or retracted is a timeline, newest first, that pairs
  each old value, struck through, with the value that replaced it and says when and by what
  it was replaced ([screenshot](../images/memory-facts.png)). The Memory page heads its
  overview with counts of facts, agents, sources, review branches and open inbox items. It
  lists the agents and sources as tables, recent activity as a timeline, and the review
  branches and settings, and a search across the page recalls facts as the same cards
  ([screenshot](../images/memory.png)). Its Inbox tab reviews facts, with **Accept all that pass** to select them,
  and **Promote selected** and **Reject selected** to act on the selection.
* The Communities tab of a dataset shows the GraphRAG hierarchy as a tree or, with **Map**, as
  a treemap in which each community is a box sized by its members and a parent holds its
  sub-communities under its title ([screenshot](../images/communities-map.png)). A box is
  tinted by its report's status, and says the status in words wherever it is large
  enough to hold them. Choosing a box opens the community's members and report below the map,
  and the choice of view is kept in the browser. A parent whose children would be too small
  to read shows as one box that opens its own report.
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

### Browse and the Workbench

Reading and authoring are separate places. **Browse** is the reader, with a page for each
term and the edit mode of its pages. The **Workbench** holds the editors that work on a
whole ontology or thesaurus, a tree beside the selected entity, and it is where further
authoring tools such as SHACL shapes and forms go. A Browse term page has **Open in
Ontology** or **Open in Thesaurus**, which lands on the same IRI selected in the editor's
tree with its path open, and an editor's entity panel has **Open page**, which goes back to
the term's page. The palette offers both, and Explore's details panel has **Open page** for
a resource that is a class, a property or a concept.

Every tab and control of these pages is part of the address, so a link, a reload and the
Back button return to the same view. The `routes` module in `ui/src/lib/routes.ts` builds
these addresses for the rail, the palette and the links.

| Address | View |
|---|---|
| `/ui/browse?ds=NAME&iri=IRI` | The term page of an entity. `edit=1` opens edit mode and `view=graph` the term's graph. |
| `/ui/workbench/ontology?ds=NAME` | The Ontology editor's tree. `kind=ObjectProperty`, `DataProperty`, `AnnotationProperty` or `NamedIndividual` picks the tree, `iri` the selected entity and `view=usage` its usage. |
| `/ui/workbench/ontology?ds=NAME&tab=dlquery` | DL Query, with the query in `dl`. `tab=header` opens the header and `tab=history` the history. |
| `/ui/workbench/thesaurus?ds=NAME` | The Thesaurus editor's tree. `kind=collections` lists collections, `scheme` picks the scheme and `view` the resource's labels, relations, members or notes. |
| `/ui/workbench/thesaurus?ds=NAME&tab=history` | The thesaurus's history. `tab=validation` opens the validation report. |
| `/ui/similar?ds=NAME` and `/ui/similar/map?ds=NAME` | Similar's Search and Map pages. |
| `/ui/admin/server`, `/ui/admin/backups` and `/ui/admin/tokens` | The Admin pages. `tab=repositories`, `policies` or `activity` picks a Backups tab. |

A caller who may read a dataset but not write it can still follow a link to the Workbench.
The editor then says that it is read-only and hides its editing controls, while the tree,
DL Query, the header, the history and the validation report still read.

The trees of both editors keep a deep hierarchy readable. Indentation is 14 pixels a level
for the first four levels and 6 pixels a level below them, with a guide line under each open
node of the first levels. A row gives its label the room first, hides the entity's short
form when the row is narrow and then shortens the label, and the selected row shows its
whole label and short form. When the tree scrolls, the ancestors of the first visible row
stay at its top, in the manner of an editor's sticky scroll, and clicking one scrolls back to
it. A breadcrumb above the entity panel gives the selected entity's path from a root of the
tree. A level with more than 1,000 children shows its
first 1,000 with a **Show more** row that reads the next 1,000 from the hierarchy route.

A change set is drawn the same way wherever the pages show one, in the edit previews and in
the history tabs. Each subject is a block named by its label, with every added or removed
property and value on a line of its own and a sign beside it, and the plain N-Quads are one
tab away.

### Ontology editor

The **Ontology** editor, `/ui/workbench/ontology`, browses and edits the OWL ontology of one graph
and its imports
([U12](../specs/U12-ontology-workbench/SPEC.md#outcome),
[reasoning guide](../reasoning/README.md#owl-2-ontologies),
[screenshot](../images/ontology-editor.png)). Trees of classes, object, data and
annotation properties and individuals sit beside the selected entity's frame, which
lists its annotations, characteristics and axioms and adds and removes them. The frame's
add fields take class expressions and whole axioms in Manchester syntax, with
completion of entity names and labels and an underline under the part the server could
not read, or in functional-style syntax. The page's tabs are **Tree**, **DL Query**,
**Header** and **History**. A select at the top of the tree picks the classes, object,
data or annotation properties or individuals, and a segmented control on the entity panel
switches between its frame and its usage.

The **DL Query** tab answers a Manchester class expression with its equivalent classes,
its direct and inferred superclasses and subclasses, and its instances
([screenshot](../images/ontology-dl-query.png)). Each answer opens the entity's frame
beside the query, and the query is kept in the page's address. A label above the
answers names the reasoning that found them, the EL classifier inside OWL 2 EL or the
OWL 2 RL rules outside it, says whether they are complete, and lists the axioms outside
the profile that can leave them incomplete. The
header tab edits the ontology IRI, version IRI, imports and header annotations, and the
usage view lists every axiom that mentions the entity. The **History** tab lists the
branch's commits newest first in the manner of WebProtégé's changes. Each commit opens to
what it changed in the graph as a change set, one block a subject named by its label with
each added or removed property and value on a line, and links to the commit's full diff. Every other view of a commit's, a merge's or a preview's changes shows the same change
set, with the signed N-Quads lines behind a **N-Quads** switch. **Inferred** shows the EL
classifier's taxonomy beside the asserted class tree
([screenshot](../images/ontology-inferred.png)). Every edit is previewed as a dry run and
committed with the commit it was previewed against
([screenshot](../images/ontology-preview.png)), on the current branch or on a session
branch that **Start session** creates. Undo and redo are previewed commits too, and
another writer's commit reloads the page through the change feed.

### Ontology browser

The **Browse** page, `/ui/browse`, is a reference to the ontologies and thesauri of a
dataset, with a page for each entity in the manner of the term pages of OLS and
Ontobee ([U12](../specs/U12-ontology-workbench/SPEC.md#ontology-browser),
[screenshot](../images/browse-term.png)). The top bar's search opens an entity's page,
and every name on a page links to the page of that entity. `/ui/browse?ds=NAME` is the
landing page of the default graph's ontology, and `&iri=IRI` opens an entity. `graph`,
`branch` and `at` choose what the pages read, as on the other pages.

An entity's page leads with its label, short form, kind and IRI, and a row of counts under
them, its parents, children, incoming links, synonyms and the other datasets that mention
it, each a link to its section. The buttons that act on the page sit level with the title.
**Open in Explore**, **Open in Ontology** or **Open in Thesaurus**, **Ask about this** and
**Edit** show while they fit, and the rest fold into a **More** menu, so the title never
shares a row with a wrapped column of buttons. A deprecated entity says so under its title and
links its replacement. The rest of the page is cards. **Definition** leads with every
definition, description, comment and scope note the entity has, in groups under a heading
that names the kind and the predicate, such as "Definition · obo:IAO_0000115" and
"Comment · rdfs:comment". The heading follows the **Prefixed** or **Full IRIs** switch and
shows the full IRI as its title. A group shows its first three values with a count of the
rest, folds the other languages behind "In N other languages", and keeps blank lines as
paragraphs. The same text under two predicates is one entry. A note leads only when the entity
has nothing else, and an entity with none says so. Synonyms follow by kind, each with its
languages, sources and its synonym type as a chip. **Axioms** gives the entity's frame in
Manchester syntax, with the names linked to their pages and a **Copy** button, and an
**Inferred** section that a **Classify** button fills from the server's classification. A SKOS
concept has no Axioms card, since SKOS statements are not axioms.
**Hierarchy** gives the entity's place in its tree, and **References** its cross-references,
what links here, the other datasets and, folded away under **All other statements**, every
statement the other cards do not show. Beside them, in a column of the same width as on
a dataset's page, **Metadata** lists the kind, status, identifiers, ontology and subsets,
and **Download** downloads the entity in Turtle, RDF/XML, OWL functional syntax or JSON-LD.
When one column grows longer than the other, References or Download moves to the shorter
one so the two end close together, without moving a card that holds focus, and a shorter
side column stays in view as the page scrolls.

The Hierarchy card scrolls on its own within half the window's height with the entity in
view. It shows the lineage from a root to the entity on one line, the entity's other direct
parents under it, its children with their counts and its siblings behind one row, and it
reads each further level when it opens. **Graph** in the card's header draws the entity's
neighbourhood in the card in place of the tree, on the Explore canvas with Explore's
lenses. An entity outside any hierarchy has **Graph** beside its title, which shows the
graph in place of the cards. The graph holds its parents, its first children, the entities
its axioms relate it to and those whose axioms hold it, with each edge named by its
property. Double-clicking a node adds that entity's neighbourhood, and **Open in Explore**
opens the entity there. **Ask about this** opens the Ask drawer with the entity as its
context.

A SKOS concept gets a table of its labels by language and its related concepts and
mappings. Cross-references to well-known databases such as Wikipedia, PubMed and DOI are
links. A class that is defined by an expression shows it as a **Logical definition**
under its definition, and only class expressions count, never the other axioms.
**What links here** lists the axioms of other entities that mention it, grouped by the
restriction that holds it as in "part of some mitochondrion", then the other statements that
point at it by property, with their counts and further pages. **In other datasets** asks
every other dataset the caller can read, up to 20, whether it mentions the IRI, and links
its page there. **Similar terms** lists the entity's nearest neighbours in the dataset's
first ready vector index, through the same search as Similar's Search page, and links to
that page for more. A dataset without a vector index or a term without a vector shows no
such section. Below 1,100 pixels the cards are one column, in the order Definition, Axioms,
Metadata, Hierarchy, References and Download, and on a phone the Hierarchy card opens on
request ([screenshot](../images/browse-phone.png)).

A caller who can write to the dataset gets an **Edit** button beside the title, and `E`
or the palette's **Edit this page** does the same. Edit mode changes the page in place
([screenshot](../images/browse-edit.png)). A bar under the top bar names the branch the
edits go to and holds **Undo**, **Redo**, **Start session**, a link to the term in the
Workbench's Ontology or Thesaurus editor and **Done**. The definition becomes a text field with its language
and its sources as chips, and each synonym, annotation and parent gets edit and remove
buttons with an add row below them. On an OWL page the parents are picked through the
entity search and the axioms are added in Manchester syntax with completion and the
server's parse errors underlined. On a SKOS page the broader, narrower,
related and mapping concepts are picked the same way, and the labels are edited by language.

Each change is one commit through the editors' write path. It is previewed as a dry run
against the commit the page shows ([screenshot](../images/browse-edit-preview.png)),
and the preview names the axioms or triples it removes and adds. A definition or synonym
keeps its sources and other axiom annotations through an edit. Undo and redo are
previewed commits that share the editors' history for the dataset and graph. When the
branch is protected the fields are disabled and **Start session** creates a session
branch, as in the editors. A write the caller's grants refuse names the dataset and
graph in the preview, and a commit by another writer makes the preview stale until
**Reload** reads the page again. Statements that another graph states are shown without
buttons. `Mod+S` previews the field that has focus, `Esc` leaves edit mode and asks
first when a field holds an unsaved change, and `S`, `P` and `A` focus the add rows of
synonyms, parents and axioms when focus is outside a text field. On a phone the bar
moves to the bottom of the screen with **Undo**, **Redo**, a menu and **Done**, and the
hierarchy comes before the article ([screenshot](../images/browse-edit-phone.png)).

The landing page shows the ontology or scheme header with its title, version, license and
description, a graph picker when named graphs hold ontologies or schemes, a search field
and its results across the full width of the content, with kind filters, the top classes or
concepts with their definitions and child counts, and the counts of each kind of entity
beside them ([screenshot](../images/browse-landing.png)). The ontology's own annotations
are listed under **About this ontology** by their labels, with long names wrapped. The top classes are the root terms the
ontology names with `IAO_0000700` when it names them, and the classes without a named
parent otherwise.

The pages read two routes of the [API reference](../API.md#ontology), `hierarchy` and
`term`, which read the store's indexes around one entity, so a page costs the same on
GO or ChEBI as on a small ontology. The landing page search takes `/`,
which opens the command palette on the other browser pages. The components of the page
live in `ui/src/lib/components/browse` and take their data as properties, so other pages
can reuse them.

### Thesaurus editor

The **Thesaurus** editor, `/ui/workbench/thesaurus`, edits the SKOS concept schemes, concepts and
collections of one graph, with their SKOS-XL labels
([U12](../specs/U12-ontology-workbench/SPEC.md#thesaurus-editor),
[screenshot](../images/thesaurus-editor.png)). The concept tree of the chosen scheme
starts at its top concepts and reads one level of `skos:broader` and `skos:narrower` at a
time, in both directions. A concept with several broader concepts appears under each of
them with a count of its broader concepts, and a concept of the scheme without a broader
one is marked when it is not a top concept. The search finds concepts by any preferred,
alternative or hidden label, SKOS-XL labels included, in every language or in one, and the
tree can show its labels in a chosen language. The page's tabs are **Tree**, **History**
and **Validation**, and a select at the top of the tree switches it between concepts and
collections. The history is the Ontology editor's, a change set for each commit.

Dragging a concept onto another moves it there, and holding Ctrl or Alt adds the new
broader concept and keeps the old one. Each move writes both directions of the hierarchy
and is previewed as a commit before it is written
([screenshot](../images/thesaurus-move.png)). Dropping a concept on the top zone makes it a
top concept of the scheme. The page refuses a move under one of the concept's own
narrower concepts.

The **Labels** view lists a resource's preferred, alternative and hidden labels by language,
marks the SKOS-XL ones and converts a label between its plain and SKOS-XL forms. It
refuses a second preferred label in one language and a label that is both preferred and
alternative, and it lists the clashes the data already has. The **Relations** view edits
broader, narrower, related and mapping links, with a picker that searches concepts by
label in another scheme or another dataset. A link written in one direction only is
marked, and its repair adds the other direction. The **Members** view edits a collection's members and an
ordered collection's `skos:memberList` with the list editor of the
[editing forms](#nested-forms-lists-and-alternatives), and the **Notes** view edits notes
and documentation through a form of the built-in SKOS shapes.

A segmented control on the resource panel chooses among these views. The **Validation**
tab runs the built-in SKOS shapes, which check the integrity conditions of the SKOS
Reference, over the graph ([screenshot](../images/thesaurus-checks.png)). Each result opens
its resource beside the report, and an administrator can install them as the dataset's write guard. The
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
  trees, up to 50,000 of each, and its trees move onto the hierarchy route that the
  browser reads under item 607. When that listing stops early, as on the Gene
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
* The Axioms card of a Browse term page shows inferred superclasses only after a
  **Classify** click, since no read-only route says whether a kept classification exists.
  Item 726 adds one.
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
* The ontology browser's edit mode changes definitions, synonyms, annotations, parents,
  axioms and SKOS labels and links. SKOS-XL labels and collection members are edited in
  the thesaurus editor, and property assertions, keys and chains have no form of their
  own and are added as an axiom, 706. Its first preview on a large
  ontology waits for the axioms route to open the whole ontology, 707. Its
  pages and hierarchy show asserted axioms only, and inferred ones are item 622. Its search reads
  the head of a branch, so a page at a past state has no search box, item 658. It shows
  entities by label with the reader's languages first, and a choice of label, prefixed
  name or IRI is item 625. Reuse is looked up in the other datasets of the same server,
  and other servers' term pages are item 461. The landing page counts entities by kind
  without axiom counts or expressivity, item 621.
* The ontology editor's tree filter matches the names it has loaded and does not call the
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

The remaining work described above refers to [item 607](../ROADMAP.md), [item 621](../ROADMAP.md), [item 622](../ROADMAP.md), [item 625](../ROADMAP.md), [item 630](../ROADMAP.md), [item 658](../ROADMAP.md), [item 461](../ROADMAP.md), [item 612](../ROADMAP.md), [item 613](../ROADMAP.md), [item 614](../ROADMAP.md), [item 615](../ROADMAP.md), [item 27](../ROADMAP.md), [item 29](../ROADMAP.md), [item 75](../ROADMAP.md), [item 101](../ROADMAP.md), [item 120](../ROADMAP.md), [item 121](../ROADMAP.md), [item 140](../ROADMAP.md), [item 141](../ROADMAP.md), [item 142](../ROADMAP.md).
The central roadmap owns priority and dependencies. Its
[completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs)
link partially complete specs to their remaining work and acceptance criteria.
