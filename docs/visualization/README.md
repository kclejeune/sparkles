# Visualization and editing capabilities

This page introduces the embedded SvelteKit web UI at `/ui/`, starting with a visual
[Web UI tour](#web-ui-tour) across all major workflows, followed by detailed
[Web UI](#web-ui) capability notes, [known gaps](#known-gaps),
[configuration and usage](#configuration-and-usage) links, and [roadmap](#roadmap)
items. The [capability matrix](../FEATURES.md#capability-status) records delivery status
within each stated scope.

## Web UI tour

The screenshots below walk through the embedded web UI by workflow, covering querying,
history and branches, graph exploration, AI and GraphRAG workflows, ontology and SKOS
authoring, SHACL shape editing, and administration.

### Querying, visual patterns, and profiling

| ![A CONSTRUCT query's results drawn as a graph of people, who they know and where they work, with one person's neighborhood highlighted](../images/graph.png) | ![The query editor with a SPARQL query and its results](../images/query.png) |
|---|---|
| CONSTRUCT results rendered as an interactive graph with neighborhood highlighting. | The SPARQL query editor with syntax completion, formatting, and table results. |
| ![The query editor with the Pattern pane beside it, drawing the query's variables as nodes joined by its triple patterns, an optional part in a dashed hull, a filter as a chip under its variable and a BIND as a locked block, with the projection checkboxes, the count of matches and seven result rows](../images/pattern.png) | ![The Pattern pane adding an edge from ?person, with the predicate picker listing the predicates that instances of ex:Person use, with their counts](../images/pattern-edit.png) |
| The Pattern pane renders a SPARQL query's graph pattern beside its text. | Schema-fed pickers build and edit query edges directly on the canvas. |
| ![The Pattern tab of a query that ran with Plan on pattern, each edge labelled with its estimated and actual rows, ex:memberOf marked as a misestimate, numbered join badges on the variables and the plan tree beside it](../images/plan-pattern.png) | ![A query's graph of 37 freight flights between cities, with the Paths panel beside it listing the three shortest routes from Vancouver to Auckland by distance, each with its cost summed from the ex:km values on the reifiers of its flights, and the routes drawn over the result with each edge numbered by the paths that use it](../images/query-paths.png) |
| Plan on pattern overlays estimated and actual row counts and highlights misestimates. | Weighted shortest paths computed from edge reifiers drawn over a query result graph. |

![The query page's Facets tab counting the values of ?person in every solution, with a brush on the years people joined and the editor above holding the triple and FILTER that Apply to query added](../images/query-facets.png)

*Brushing a result facet writes the corresponding triple pattern and FILTER into the query.*

### Time travel, commit diffs, and branches

| ![The query page with the time slider in compare mode, marking one team membership as added and one as removed between two commits](../images/time-slider.png) | ![The resource explorer with the time slider in compare mode, drawing a new edge in green and a removed one as a dashed ghost](../images/time-slider-graph.png) |
|---|---|
| The time slider compares query results between two retained commits. | Compare mode highlights added edges in green and removed edges as dashed ghosts in Explore. |
| ![The diff graph of one commit, drawing a new employee, a new job title and a move to Los Angeles in green and the removed job title, home city and acquaintance as dashed red ghosts](../images/diff-graph.png) | ![A time-lapse over four commits with ghosts kept, showing the changes of each commit added in green and removed ones as dashed ghosts, with a running total of changes since the start](../images/diff-lapse.png) |
| A commit diff drawn as a graph of additions and removals. | A time-lapse steps through commits one diff at a time with running totals. |
| ![A dataset page listing its branches above a commit history graph that includes a merge](../images/branches.png) | ![The merge page previewing a merge of one branch into main, with two conflicts and a choice of which side to take for each](../images/merge.png) |
| Dataset branches displayed above their commit history graph. | Branch merge previews resolve per-triple conflicts before committing. |

### Graph exploration, lenses, maps, and embeddings

| ![The resource explorer showing one resource as a graph, with its properties and the resources that refer to it](../images/explore.png) | ![The resource explorer with lenses on, sizing nodes by their degree in the dataset, colouring them by class, badging the nodes with inferred edges and drawing those edges dashed, with the legend beside the graph](../images/explore-lenses.png) |
|---|---|
| The resource explorer inspects a node's properties, incoming links, and local graph. | Canvas lenses size nodes by degree, colour by class, and draw inferred edges dashed. |
| ![The resource explorer with algorithm lenses, sizing nodes by PageRank over the drawn graph and colouring them by Leiden communities, with the legend giving the score ranges and each community's size](../images/explore-algo-lenses.png) | ![The resource explorer with the Facets panel beside a person's graph, brushing an age range on a histogram and one employer on a bar list, so that the other facets count the 41 people under both brushes and the drawn people outside them are dimmed](../images/explore-facets.png) |
| Algorithm lenses size nodes by PageRank and colour them by Leiden community. | Brushing facet histograms and bar lists filters the explored graph interactively. |
| ![The resource explorer with the Paths panel open, listing the three shortest foaf:knows paths from Freya Lindqvist to Lukáš Novák, which pass through ten different people, and drawing them over her neighbourhood with each edge numbered by the paths that use it](../images/path-finder.png) | ![The resource explorer with validation badges on three people, the validation lens's legend grouping the nodes by their worst severity, and a results panel previewing an update that adds a missing name](../images/validation-overlay.png) |
| The Paths panel finds and overlays the k shortest paths between two entities. | The validation lens badges SHACL results on the graph and previews a suggested fix. |
| ![The resource explorer with the Why panel beside the graph, explaining that Alice is a Person by rdfs9 from her inferred Employee type, itself derived by rdfs9 from her asserted Manager type and the subclass of Manager, and the subclass of Employee, with every premise highlighted on the canvas after Show on graph](../images/explore-why.png) | ![The Why panel on a dataset whose inferences are two commits stale, with the note that the tree shows the inferences as computed at commit 2, and the second of two derivations, Alice as an Employee by rdfs3 from the range of reportsTo](../images/explore-why-stale.png) |
| The Why panel shows an inferred edge's derivation tree and highlights its premises on the graph. | Stale inferences carry an explicit commit notice alongside alternate derivations. |
| ![GeoSPARQL query results drawn on a map beside a list of the places](../images/map.png) | ![The resource explorer showing a freight company's graph as a world map, with arcs from its head office in Dubai to its offices, partners and employees on six continents, coloured by predicate](../images/map-arcs.png) |
| GeoSPARQL query results plotted on a bundled MapLibre basemap. | Located graph relations rendered as predicate-coloured great-circle arcs in Explore. |
| ![A query result drawn as labelled great-circle arcs for 37 freight routes between cities on six continents, beside the list of rows](../images/result-arcs.png) | ![The embedding map of 6,000 freight shipments projected from 64-dimensional vectors to two dimensions and coloured by cargo class, with the legend counting each class and a lasso around one cluster whose 1,171 shipments form the shared selection, ready for Use in query or Open as graph](../images/embedding-map.png) |
| Query result rows drawn as labelled great-circle arcs between places. | The embedding map projects vector indexes to 2D with lasso selection into SPARQL or Explore. |

### AI, GraphRAG, and agent memory

| ![The Ask page with a thread's answer, its checked query, its rows and its sources, with the first cited row marked and the memory facts about it quoted](../images/ask-answer.png) | ![The Ask drawer docked beside the Query page while it works, showing Summarizing after the query results have arrived, with Stop beside the field](../images/ask-steps.png) |
|---|---|
| The Ask page answers plain-language questions with checked SPARQL queries and cited rows. | The docked Ask drawer streams progress through query generation, execution, and summarization. |
| ![A query handed over by an agent, open in the query editor with its question, assumptions, the terms it uses and a clean check](../images/handoff.png) | ![The Explore overview drawing seven GraphRAG communities of a freight network as circles sized by their members and coloured by report status, with the head office selected and its report, findings, sub-communities and members beside the picture](../images/explore-communities.png) |
| Agent-authored queries open for review with their question, assumptions, and term checks. | Explore maps GraphRAG communities by member count and report status beside each report. |
| ![A dataset's Communities tab drawing the GraphRAG hierarchy of a freight network as a treemap, with a box for each community sized by its members and tinted by its report's status, and the report of the Gulf head office selected](../images/communities-map.png) | ![The Memory page with its counts of facts, agents, sources and branches over a search for the payments team, whose results are cards of facts that each show a review status, a confidence, the agent, the source and the quote, with a conflict between two dates marked on its card](../images/memory.png) |
| The Communities tab renders the GraphRAG hierarchy as a treemap tinted by report status. | The Memory page tracks agent-recorded facts with provenance, review status, and conflicts. |

![The resource explorer's Memory tab for Ana Lima, with a card for each predicate showing the values agents recorded with their review status, confidence, agent, source and quote, and under them a timeline whose entry pairs the superseded team res:platform, struck through, with the res:payments that replaced it](../images/memory-facts.png)

*A resource's Memory tab pairs each fact's provenance with a timeline of superseded values.*

### Ontology authoring, browsing, and SKOS thesauri

| ![The ontology editor with the class tree of a university ontology and the frame of Professor, listing its labels in French and English, its superclasses Employee and teaches some Course, its instance and an axiom on its range, each with an add field](../images/ontology-editor.png) | ![The ontology editor with the asserted and inferred class trees side by side, the EL classifier placing Professor under Teacher, which is defined as a Person who teaches some Course](../images/ontology-inferred.png) |
|---|---|
| The ontology editor pairs class and property trees with an entity's Manchester-syntax frame. | Asserted and EL-inferred class hierarchies appear side by side in the ontology editor. |
| ![The DL Query tab of the ontology editor answering Person and (teaches some Course), labelled as answered completely by the OWL 2 EL classifier, with Teacher as its equivalent class, Person and Agent above it, Professor below it and Ada Lovelace as its instance, beside the frame of Professor](../images/ontology-dl-query.png) | ![The preview of an ontology edit that creates the class teaching assistant, listing the declaration, label and subclass axioms it adds, the three triples it writes and the commit message](../images/ontology-preview.png) |
| DL Query evaluates Manchester class expressions and labels the reasoning tier that answered. | Every ontology edit previews its exact axiom and triple changes before committing. |
| ![The landing page of a cell organelle ontology in the ontology browser, with its title, version and description, a search across the content that lists the classes matching mitoch, the top classes with their definitions and child counts, and the counts of classes and properties](../images/browse-landing.png) | ![The ontology browser's page of mitochondrion, with a line of counts under its title, one text that leads with its property as a chip, disclosures for its other texts, sections with headings of one size, and a rail of cards with its metadata and hierarchy](../images/browse-term.png) |
| An ontology landing page in Browse with full-width search, top classes, and entity counts. | Each entity page in Browse combines definitions, linked axioms, metadata, and hierarchy. |
| ![The ontology browser's page of mitochondrion in edit mode, with a bar naming the branch and holding undo, redo, start session, the ontology editor link and done, the definition as a text field with its language and its sources as chips, and edit and remove buttons on each synonym and annotation with an add row below them](../images/browse-edit.png) | ![The preview of a definition edit in the ontology browser's edit mode, listing the definition it removes and the one it adds, each with its one source annotation, the dry run's six triples removed and six added, and a button that commits it](../images/browse-edit-preview.png) |
| In-place edit mode on a Browse term page supports session branches, undo, and redo. | Browser edits preview dry-run triple diffs before writing a commit. |
| ![The thesaurus editor with the concept tree of an animals scheme, where Whales and Penguins appear under both Mammals and Marine animals marked 2 broader, and the label table of Whales with its English and French preferred labels and an English alternative label, each with buttons that edit it, remove it or turn it into a SKOS-XL label](../images/thesaurus-editor.png) | ![The preview of a drag in the thesaurus editor that moves Bats under Flying animals, removing the broader and narrower triples that linked it to Mammals and adding the two that link it to Flying animals, above the commit message](../images/thesaurus-move.png) |
| The thesaurus editor manages polyhierarchical SKOS trees and multilingual or SKOS-XL labels. | Dragging a concept to a new broader term previews both hierarchy directions as one commit. |
| ![The checks of the thesaurus editor, listing the concept Owls as breaking S13 because one label is both preferred and alternative, with a button that enforces the SKOS rules on writes](../images/thesaurus-checks.png) | ![The Watches page with a subtree watch on Professor opened, listing the commits that put a class below it and labelled it](../images/watches.png) |
| Built-in SHACL shapes verify SKOS Reference integrity conditions and can guard writes. | Entity and subtree watches list the commits that modified watched terms. |

### SHACL shapes, editing forms, and schema

| ![The SHACL shape editor with a tree of a Person node shape and its Email, Employer and Name property shapes, and the form of the Employer shape with a sequence path of worksFor and organization, its targets and its name](../images/shape-editor.png) | ![The preview of a shape edit that raises the minimum count of the Name property shape from 1 to 2, with the RDF patch pinned to the source commit, the one triple it removes and the one it adds, and a button that commits it](../images/shape-editor-preview.png) |
|---|---|
| The SHACL shape editor displays shapes in a tree and edits targets, paths, and constraints in forms. | Every shape edit previews an exact RDF patch pinned to the source commit before saving. |
| ![The resource explorer with a person's graph and an editing form generated from SHACL shapes in the side panel, showing an email that fails the shape's pattern under its field and a server check that reports an employer which is not a company](../images/forms.png) | ![An editing form on its own page after a save while main is protected, with a banner saying it edits the session branch edits.anonymous.20261024 with one commit and a Review and merge button, and an age below the shape's minimum marked under its field](../images/forms-branch.png) |
| SHACL-generated forms validate values in the browser and against the server. | When main is protected, form saves commit to a session branch with a link to review and merge. |
| ![The editing form of a SKOS concept with its SKOS-XL labels as nested forms, the stored label edited to All animals and a new French label added from the label shape's template, with the previewed commit listing the triples it adds and removes](../images/forms-nested.png) | ![The editing form of a SKOS collection, with its member chosen as a Concept among the alternatives of sh:or and its ordered member list after fish moved up, each member with buttons that move or remove it](../images/forms-list.png) |
| Nested blank-node children edit and commit atomically with their parent form. | Forms include ordered list editors and selectors for sh:or and sh:xone alternatives. |

![The schema browser with a class hierarchy, instance counts and the properties of the selected class](../images/schema.png)

*The schema browser combines observed class counts and properties with declared constraints.*

### Navigation, administration, and mobile layout

| ![The command palette on the Query page with nothing typed, listing the page's commands with their shortcuts and the pages of the shell](../images/palette.png) | ![The web UI on a phone, with the drawer open over a dataset page, holding the dataset switcher, the pages, the connection status and the theme](../images/shell-phone.png) |
|---|---|
| Ctrl+K opens the command palette for page commands, entities, datasets, and saved queries. | On narrow viewports, navigation moves into a drawer and search expands across the top bar. |
| ![The model configuration form on the server page, with a provider's allowed models as a list of three with buttons to move and remove each, and its per-model options as two open cards with typed fields for context tokens, output tokens, temperature, structured output, request timeout and prices, the empty fields showing the values they inherit](../images/models-settings.png) | ![The model configuration form with a new model's card marked 2 errors, saying under its fields that context tokens must be a whole number and that the temperature is at most 2, above the role lists shown as rows of provider and model](../images/models-settings-errors.png) |
| The server Models page edits provider allowlists and per-model options in typed forms. | Client and server validation errors appear directly beside the offending configuration fields. |

## Web UI

The SvelteKit UI is embedded in the server binary at `/ui/`. The sections below describe
each page's controls, bounds, and keyboard shortcuts.

### App shell

Every page runs inside one shared shell. A top bar holds the brand, a search field whose
leading segment switches datasets and displays the active branch, connection status,
theme, and account controls. On large and medium viewports, the search field stays
centered.

A navigation rail beside the page lists the working pages above a rule and the
administration pages below it in this order: Query, Explore, Browse, Workbench, Similar,
and Memory, followed by Datasets and Admin. Workbench holds the Ontology, Thesaurus, and
SHACL shapes editors; Similar holds Search and Map; Admin holds Server, Backups, and
Tokens. Group items expand when a child page is open or toggled. The rail shows Workbench
only when the caller can write to the open dataset, leaving readers with Browse alone.
Each page uses at most one tab row, placing secondary choices in a select or segmented
control. Above 1,200 px the rail displays labels, below 1,200 px it collapses to icons
with tooltips, and at 760 px or narrower it moves into a drawer opened from the menu
button alongside the dataset switcher, status, theme, and account. Switching datasets
reopens the current page on the new dataset. A **Prefixed** or **Full IRIs** toggle beside
the theme button (also available in the command palette) controls how every page formats
IRIs using the dataset's prefix map plus conventional prefixes such as `rdf`, `owl`,
`skos`, and `dcterms`, stored per viewer in the browser.

* **Command palette and search.** `Ctrl+K` or `Cmd+K` toggles the command palette from
  anywhere, including inside the query editor. Outside text fields, `/` opens the palette
  unless the page defines its own search input, such as the ontology class-tree filter,
  the thesaurus label search, or Explore's text-search tab. `Escape` restores prior focus,
  and in-place commands return focus before running. On phones, the search button or keys
  expand the input across the top bar with a back button
  ([screenshot](../images/palette-phone.png)).
* **Palette results and ranking.** When empty, the palette lists recent picks, commands
  for the active page, and shell pages ([screenshot](../images/palette.png)). Typing
  searches classes, properties, individuals, and SKOS concepts through
  [entity search](../search/README.md#entity-search), along with commands, datasets, saved
  queries, and branches on pages that support branches. Pressing Enter opens an entity in
  Browse or a saved query in a new Query tab ([screenshot](../images/shell-search.png)).
  On Explore, the same field acts as the resource finder, offering matching resources, an
  **Open** row for typed IRIs or prefixed names, and **Ask a question…** when enabled.
* **Type prefixes.** Prefixing a query with `>` filters to commands and pages, `@` to
  entities, `#` to datasets and branches, and `?` to saved queries; touch chips above the
  results set the same prefixes. Pressing `Escape` on an empty prefix returns to all
  groups before a second `Escape` closes the palette
  ([screenshot](../images/palette-commands.png)). Local items rank by exact match,
  prefix, word start, all-words start, initials (such as `fq` for Format query), and
  single-typo tolerance on five or more characters. Command keywords match word starts
  only, while entities use the server's typo-tolerant ranking.
* **Page commands and recent picks.** Shell commands switch datasets or branches, open
  saved queries, change themes, open admin pages, and create datasets. Query registers
  run, format, explain, save, new tab, history, and the pattern pane; Ontology registers
  session start, entity creation, undo, redo, and DL Query; Thesaurus registers session
  start, concept/scheme/collection creation, checks, undo, and redo; Datasets refreshes the
  list and opens dataset tabs. Recent picks are kept in browser local storage per dataset
  and degrade cleanly when storage is blocked.
* **Extensibility and design tokens.** Pages extend the palette through
  `ui/src/lib/launcher.svelte.ts` using `useCommands`, `useProvider`, `launcher.register`,
  and `usePageSearch`. Shared design tokens in `ui/src/app.css` define light and dark
  palettes, typography, a 4 px spacing scale, responsive gutters (24, 20, and 16 px),
  radii, and shadows. Unit tests verify at least 4.5:1 text contrast in both themes
  ([medium width](../images/shell-medium.png), [phone](../images/shell-phone.png)).
* **Responsive layout conventions.** Toolbars use one `.toolbar` row with the primary
  action at the end, wrapping the end group flush right when space runs out. On phones,
  toolbar and page-head buttons show icons while preserving accessible labels; Query drops
  labels for **History**, **Pattern**, and **Format** below 1,200 px, and Explore does the
  same below 860 px. Dialogs dock as bottom sheets at 760 px and below. Wide tables scroll
  inside their containers with pinned action columns and collapse into row cards on
  phones. Overflowing filter bars fold into a **Filters** button showing the active filter
  count, and overflowing page-head secondary actions fold into a **More** menu.

### Query page

* The query editor provides declared-prefix completion, browser-side linting and
  formatting, and table, graph, or execution-plan result views.
* The toolbar holds query kind and endpoint selectors, **Examples**, **Saved**, and
  **Options**, followed by the pattern pane toggle, **Format**, **Explain**, and **Run**.
  **Options** groups branch selection, point-in-time reads, the history slider, row
  limits, and reasoning controls, summarizing non-default selections on its button.
* **Reasoning selector.** The reasoning control sets the query's `reasoning` parameter:
  **Dataset default** sends no override, **Off** reads asserted data only,
  **Materialized** reads materialized inferences, and **Demand** or **Auto** derive at
  query time ([API.md](../API.md#query-time-reasoning)). Under **Demand** and **Auto**, a
  **Profile** selector overrides the dataset's rule profile. Derived results show a badge
  with the mode, derived-fact count, and cached tables from the `Sparkles-Reasoning`
  header. Selections persist per browser and carry over to result downloads. Under
  query-time modes, Ask bar checks, empty-result diagnosis, and facets read materialized
  inferences without deriving new facts, and graph lenses use the dataset's default mode.
  Tracked in roadmap item 986.

  ![The query page's Options menu with Demand reasoning and the RDFS profile, over a result whose badge reports 322 derived facts](../images/query-reasoning.png)

* **Streaming results.** The result table uses the server's default execution mode, so
  cursor plans stream. When a streamed response hits the row limit, the **Table** tab
  displays a `+` suffix (such as `1,000+`) and notes that the total is unknown; raising
  the limit or downloading from **Raw** reads the full result. Time-slider comparisons
  read both commits eagerly to compute exact diffs
  ([USAGE.md](../USAGE.md#incremental-query-execution)).
* **Saved queries and Ask bar.** **Saved** lists stored dataset queries, builds typed
  parameter forms, and lets administrators save or delete queries. The Ask bar supports
  **Preview first** and **Run, then show**, step indicators, summaries whose `[n]`
  citations select table rows, clarification prompts, follow-ups, and **Summarize again**
  ([USAGE.md](../USAGE.md#the-ask-bar)). Local question history lets users promote good
  questions into stored examples or accept reader suggestions.
* **Plans, visual patterns, and graphs.** Plan explanations sit beside the plan tree with
  clickable operator links, and stopped queries offer **Explain why**. The Pattern pane
  (`Alt+P`) renders and edits the query's graph pattern beside the text, showing classes
  as subtitles, filters as chips, `OPTIONAL` blocks as hulls, and unsupported clauses as
  locked blocks ([U08](../specs/U08-visual-query-builder.md),
  [USAGE.md](../USAGE.md#the-pattern-pane), [pattern view](../images/pattern.png),
  [predicate picker](../images/pattern-edit.png)). Canvas edits splice query text with
  single-step undo. **Plan on pattern** overlays estimated and actual row counts, flags
  misestimates, and numbers join order ([U04](../specs/U04-live-query-profiling.md),
  [screenshot](../images/plan-pattern.png)). Graph results above 2,000 nodes switch to
  WebGL (up to 100,000 nodes), and the path finder works directly over result graphs
  ([USAGE.md](../USAGE.md#finding-paths-on-the-query-page),
  [screenshot](../images/query-paths.png)).

### Explore

* Explore inspects an entity's properties, nearest vector neighbours, ranked `text:query`
  hits, and a Nearby map card. The Similar page searches from an entity or a pasted
  vector. The top bar's search selects the starting resource, and the graph toolbar's
  **View** menu toggles literals and `rdf:type` edges.
* **Dataset overview.** Explore opens on a class-and-predicate overview rendered with
  sigma.js on WebGL and ForceAtlas2 in a web worker
  ([U06](../specs/U06-large-graph-rendering.md), [USAGE.md](../USAGE.md#the-explore-overview),
  [API.md](../API.md#graph-overview)). Opening a class loads its members, or above 5,000
  instances groups them by community or the highest-splitting property. **Group by** can
  display GraphRAG communities beside their reports
  ([screenshot](../images/explore-communities.png)). A list view (`L`) provides keyboard
  and screen-reader parity, and environments without WebGL fall back to Cytoscape.
* **Graph canvas, fan-out grouping, and client filters.** The explored graph retains up to
  100,000 nodes in memory and renders up to a 5,000-node draw budget. Past 1,000 visible
  nodes, rendering switches from Cytoscape to WebGL and switches back below 700. Click,
  double-click expansion, context menus, edge derivation inspection, selection highlights,
  lenses, and path overlays work in both renderers. New nodes stay beside their neighbours
  until **Re-run layout**. In WebGL mode, time-slider diffs tint added and removed nodes
  instead of fading, validation badges are non-interactive, parallel predicates collapse
  to one selectable line, and self-loops are omitted. High-fan-out nodes (>25 edges per
  predicate and direction) collapse into **N via predicate** group nodes whose panel pages
  100 members at a time with name filtering and class splitting. The **Filters** panel
  (`Shift+G`) toggles classes, edge types, minimum degree, leaf hiding, community hiding,
  and draw budgets locally without server round-trips. Retaining the explored graph in
  page memory buys instant filter updates while bounding frame cost to the draw budget
  ([measurements](../benchmarks/web-ui-scale/RESULTS.md)).
* **Paths, lenses, and algorithms.** The **Paths** panel (`Shift+P`) finds one shortest,
  all shortest, or k shortest paths with length, direction, predicate, hub, and reifier
  weight filters ([U09](../specs/U09-path-finder.md),
  [USAGE.md](../USAGE.md#finding-paths-in-explore), [screenshot](../images/path-finder.png)).
  The **Lenses** menu (`Shift+L`) maps node size, colour, and badges and dashed inferred
  edges to interactive legends ([U10](../specs/U10-canvas-lenses.md),
  [USAGE.md](../USAGE.md#lenses-on-the-graph), [screenshot](../images/explore-lenses.png)).
  Algorithm lenses size nodes by PageRank, betweenness, BFS depth, k-core, or triangles
  and colour by WCC, label propagation, Louvain, Leiden, or GraphRAG communities in the
  background with progress and **Cancel** ([USAGE.md](../USAGE.md#algorithm-lenses),
  [API.md](../API.md#view-only-runs), [screenshot](../images/explore-algo-lenses.png)).
* **Inference explanations, facets, maps, embeddings, and validation.** Clicking a dashed
  edge or pressing `W` opens the **Why?** derivation tree beside the graph, highlighting
  premises with **Show on graph**, cycling up to five derivations with **Next**, and
  noting stale inferences ([USAGE.md](../USAGE.md#why-is-this-inferred),
  [derivation](../images/explore-why.png), [stale notice](../images/explore-why-stale.png)).
  **Facets** (`Shift+F`) count numeric ranges, years, or the top 12 values in Explore and
  `SELECT` results; brushing dims non-matching nodes, **Apply to query** writes a triple
  and `FILTER`, and **Use in query** emits up to 1,000 selected entities in a `VALUES`
  block ([U11](../specs/U11-linked-views.md#outcome),
  [USAGE.md](../USAGE.md#facets-and-use-in-query), [API.md](../API.md#facets),
  [screenshot](../images/explore-facets.png)). Explore's **Map** (`Shift+M`) and query
  **Arcs** draw predicate-coloured great-circle arcs with merged markers, shared
  selection, and accessible lists ([USAGE.md](../USAGE.md#arcs-on-the-map)).
  `/ui/similar/map` projects vector indexes by PCA or force layout (sampling above 50,000
  entities) with lasso selection into Explore or SPARQL, costing about 24 bytes of server
  memory per entity across up to four cached projections capped at 1 GiB total
  ([USAGE.md](../USAGE.md#the-embedding-map), [API.md](../API.md#embedding-map),
  [screenshot](../images/embedding-map.png)). The validation overlay badges SHACL and ShEx
  violations on drawn nodes, highlights offending triples, and previews dry-run updates
  via **Suggest fix** ([USAGE.md](../USAGE.md#the-validation-overlay),
  [U07](../specs/U07-data-quality-and-schema-diagram.md#outcome),
  [screenshot](../images/validation-overlay.png)).

### Maps

* Results containing geometry literals open a Map tab with per-row popups. The browser
  decodes CRS84, EPSG:4326, and Web Mercator directly, and converts other CRSs through
  `POST /$/geo/convert`.
* The dataset page's Spatial index panel reports index status, CRSs, and memory use,
  configures or rebuilds the index, and renders indexed geometries in view.
* Maps use MapLibre GL JS over a bundled Natural Earth basemap with no external network
  requests unless `serve --map-style-url URL` specifies a custom MapLibre style.

### History and time travel

* The UI exposes commit history, write receipts, per-commit diffs, named snapshots, and
  point-in-time query results.
* A time slider moves query results, entity properties, the Explore graph, the schema tab,
  and the Memory tab across retained commits, compares two commits or branch heads, plays
  through history, and follows new commits ([USAGE.md](../USAGE.md#the-time-slider)).
* Commit diffs render as graphs (grouped by class for large changes) and play back as
  commit-by-commit time-lapses ([USAGE.md](../USAGE.md#commit-diffs-on-the-graph),
  [diff graph](../images/diff-graph.png), [time-lapse](../images/diff-lapse.png)).
* The History panel draws every branch in a commit graph, while the Branches panel and
  merge page manage branching and conflict-checked merges.

### Datasets, validation and editing

* **Dataset administration.** The dataset page manages full-text, vector (with recall
  checks), and spatial indexes, uploads documents for ingestion, edits settings with
  field-source indicators, and displays quotas and compaction state.
* **Schema browser and validation.** The schema browser reads `/$/schema` with graph
  selection, an inference toggle, observed counts beside SHACL constraints, class
  profiles, **Compare** schema diffs, and **Draft shapes** to install generated SHACL or
  ShEx shapes as a `warn` guard. The Validate panel runs SHACL (Turtle or SHACLC) and ShEx
  with shape maps, exports ShapeMap JSON, and configures write-time guards while showing
  recent rejections.
* **SHACL-generated editing forms.** Forms open from Explore's **Edit** button or
  `/datasets/{name}/edit?iri=…` ([U02](../specs/U02-shape-driven-forms/SPEC.md#outcome),
  [USAGE.md](../USAGE.md#editing-forms), [API.md](../API.md#editing-forms),
  [screenshot](../images/forms.png)). Fields merge all applying shapes using `sh:group`,
  `sh:order`, `sh:name`, and known DASH widgets. Validation runs live in the browser and
  on the server, **Preview** runs a dry run, and **Save** writes one commit to `main` or
  to a session branch when `main` is protected, surfacing concurrent-edit conflicts.
  Validation follows shape dependencies beyond a fixed graph radius, refuses incomplete
  discovery, and invalidates prior browser approval on a failed recheck. Blank-node
  `sh:node` fields become nested forms, SHACL 1.2 lists become ordered editors, and
  `sh:or`/`sh:xone` value shapes offer per-value selectors
  ([nested forms](../images/forms-nested.png), [lists](../images/forms-list.png)).
  `Dataset::forms()` and Python's `Dataset.forms` expose the same model.
* **Agent memory and GraphRAG communities.** A resource's Memory tab renders one card per
  predicate with review status, confidence, agent, source, quote, conflict flags, and a
  newest-first timeline of superseded or retracted values
  ([screenshot](../images/memory-facts.png)). The Memory page summarizes counts, agents,
  sources, activity, review branches, search recall ([screenshot](../images/memory.png)),
  and an Inbox tab with **Accept all that pass**, **Promote selected**, and **Reject
  selected**. The dataset Communities tab renders the GraphRAG hierarchy as a tree or a
  member-sized treemap tinted by report status
  ([screenshot](../images/communities-map.png)).
* **Server page and Models configuration.** The server page reports readiness, requests,
  caches, backups, and runtime model settings
  ([USAGE.md](../USAGE.md#changing-models-and-keys-at-runtime),
  [form](../images/models-settings.png), [errors](../images/models-settings-errors.png)).
  The Models form is generated from the OpenAPI `ModelsSettings` schema with typed bounds,
  reorderable model allowlists, per-model option cards, role lists, inline validation, and
  a synchronized JSON editor under **Advanced**.

### Nested forms, lists and alternatives

A property shape with `sh:node` renders each blank-node value as a nested form built from
every reaching `sh:node` declaration. The nested model tracks each child's RDF identity,
visible parent edges, and value graphs, showing shared children once and referencing
cycles in place. **Add** initializes a child from its shape template, and removing a child
deletes its edge plus any unreferenced visible triples beneath it. One save commits
changes across the root resource and all nested children under the writer lock, reporting
a parent-field conflict if another writer removed a child concurrently.

SHACL 1.2 list fields edit `rdf:List` entries in order with move, add, and remove controls
and live checks for `sh:minListLength`, `sh:maxListLength`, `sh:uniqueMembers`, and member
shapes, replacing the stored list on save. Fields whose `sh:or` or `sh:xone` lists
path-free value shapes provide a **Kind of** selector per value and flag values matching
none (or more than one for `sh:xone`).

![A SKOS concept's form with its stored SKOS-XL label edited and a new label added as nested forms](../images/forms-nested.png)

![A SKOS collection's form with a member chosen among the sh:or alternatives and the ordered member list after a move](../images/forms-list.png)

Rust exposes this model via `dataset.forms().nested_model(&request, &options)`, returning
the root form, `nested_nodes` pool, and `templates`; HTTP accepts `nested=true` on reads
and `nested` entries on checks and saves, and Python accepts `nested=True`. A single
request admits at most 64 nested identities, 512 generated field views, 512 parent
anchors, and 16 discovery levels. Exceeding the shared preparation allowance falls back to
the flat model with nested values read-only. Complex nested paths, visible blank-named
graph contexts, and matching composite range bounds are also refused pending controlled
admission. The allowance bounds adapter preparation and retention, while physical reader
and mask admission are tracked in [roadmap item 395](../ROADMAP.md) (see the
[unit calculation](../specs/U02-shape-driven-forms/SPEC.md#231-nested-read-models-in-the-rust-library)).

### Browse and the Workbench

Reading and authoring live in separate spaces. **Browse** provides a read-focused page per
term (with optional in-place edit mode), while **Workbench** hosts tree-and-frame editors
for ontologies, thesauri, and SHACL shapes. Browse term pages link to **Open in Ontology**
or **Open in Thesaurus** with the selected IRI revealed in the tree, and editor panels
link back via **Open page**. The command palette and Explore's details panel link to both.

URL query parameters capture every active tab and control via `ui/src/lib/routes.ts`, so
shared links, reloads, and browser history preserve exact state.

| Address | View |
|---|---|
| `/ui/browse?ds=NAME&iri=IRI` | Term page for an entity. `edit=1` opens edit mode and `view=graph` opens the term's graph. |
| `/ui/workbench/ontology?ds=NAME` | Ontology editor tree. `kind=ObjectProperty`, `DataProperty`, `AnnotationProperty`, or `NamedIndividual` picks the tree, `iri` selects an entity, and `view=usage` opens usage. |
| `/ui/workbench/ontology?ds=NAME&tab=dlquery` | DL Query tab with the query in `dl`. `tab=header` opens the header and `tab=history` opens history. |
| `/ui/workbench/thesaurus?ds=NAME` | Thesaurus editor tree. `kind=collections` lists collections, `scheme` selects a scheme, and `view` chooses labels, relations, members, or notes. |
| `/ui/workbench/thesaurus?ds=NAME&tab=history` | Thesaurus history. `tab=validation` opens the SKOS validation report. |
| `/ui/similar?ds=NAME` and `/ui/similar/map?ds=NAME` | Similar Search and Embedding Map pages. |
| `/ui/admin/server`, `/ui/admin/backups`, and `/ui/admin/tokens` | Admin pages. `tab=repositories`, `policies`, or `activity` selects a Backups tab. |

Callers with read-only access can still follow links into Workbench; editing controls are
hidden while trees, DL Query, headers, history, and validation remain readable.

Both editor trees virtualize rendered rows and maintain full-tree keyboard navigation
(Arrow keys, Home, End, Page Up/Down, type-ahead, and Enter) as a single tab stop with
ARIA level and position metadata. Indentation is 14 px per level for the first four levels
and 6 px below that, with guide lines, sticky ancestor headers while scrolling, and a
breadcrumb above the entity panel. Levels exceeding 1,000 children paginate with a **Show
more** row. Virtualization keeps DOM size constant across ontologies with millions of
classes at the memory cost of tracking open node IDs.

Edit previews and history tabs share one change-set layout: each subject block is titled
by its `rdfs:label`, `skos:prefLabel`, or `sh:name`, with signed property/value lines and
an **N-Quads** tab for raw lines.

### Rendering preferences

The top bar's **Rendering preferences** button (and the phone drawer) selects **Label**,
**Prefixed name**, or **Full IRI** across editors, Browse, and query results, plus a
comma-separated language preference order. An empty language list uses browser languages;
regional tags fall back to their primary language, then untagged values, English, any
available language, a prefixed name, and finally the full IRI. Preferences persist in
browser storage, and the thesaurus editor's **Label language** field can override the
language order locally.

Changing preferences keeps unsaved drafts and open concept-picker schemes intact, and a
failed label refresh preserves current editor state. Query tables fetch labels in batches
at the result's branch and commit, graph results use label literals present in their
triples, and copying or navigation always preserves the underlying IRI.

### Visual SHACL shape editor

**SHACL shapes** in the Workbench opens `/ui/workbench/shapes`
([editor screenshot](../images/shape-editor.png)), also linked from the dataset Validate
panel. Select a branch and shapes graph (leave empty for the stored default graph, even
when queries use a union default graph; reserved default and union query selectors cannot
be used as named authoring graphs).

The shape tree covers node and property shapes, including anonymous and nested shapes,
showing shared or recursive references as link leaves (displaying up to 5,000 repeated
reference leaves and reporting omissions). Forms edit Core target predicates, implicit
class targets, severity, deactivation, names, descriptions, messages, groups, order,
default values, all SHACL Core constraints, the engine's four SHACL 1.2 draft list
constraints, logical constraints, and recursive SHACL paths. Custom severities accept IRIs
and literals retain datatype, language, and base direction.

Apply field or path edits to the local draft before switching shapes or previewing.
**Undo draft** and **Redo draft** keep up to 30 local states. **Preview commit** displays
the exact RDF additions and removals (up to 50 changed triples shown in the preview,
[screenshot](../images/shape-editor-preview.png)) and commits using the captured source
UUID, commit, and precondition, preserving the draft if another writer committed first.
**Undo commit** and **Redo commit** revert commits on the branch while preserving stored
blank-node identities. Protected branches offer **Start session**, and every request
enforces the caller's graph, endpoint, and write grants.

Sample validation checks up to 100 explicit focus nodes or all declared targets in a
sample graph against the draft at its source commit (including literal targets), with
debouncing, cancellation, a 100-row cap, a 5 s server deadline, and a 10 s client
deadline. Conforming samples do not guarantee whole-dataset conformance, and a sample
report must match the captured source UUID and commit before displaying.

The editor reads complete graphs of at most 5,000 triples, caps decoded responses at 2 MiB
before JSON parsing, and refuses incomplete, oversized, or triple-filtered views. Paths
nest up to 64 levels and expand shared operands up to 1,000 steps. Unrecognized RDF stays
intact under **Additional RDF** and **View Turtle**, while malformed paths or lists
require explicit replacement. **Use in text validator** copies Turtle into the dataset's
text validator draft; installing a write-time guard stays a separate action on the dataset
page.

### Ontology editor

The **Ontology** editor (`/ui/workbench/ontology`) browses and edits the OWL ontology of a
graph and its imports ([U12](../specs/U12-ontology-workbench/SPEC.md#outcome),
[reasoning guide](../reasoning/README.md#owl-2-ontologies),
[screenshot](../images/ontology-editor.png)). Trees of classes, object, data, and
annotation properties, and individuals sit beside the selected entity's frame, whose add
fields accept Manchester syntax (with name/label completion and inline parse underlines)
or functional-style syntax. Tabs cover **Tree**, **DL Query**, **Header**, **History**,
**Discussions**, **Bulk edits**, and **Imports**, with a tree kind selector and a
frame/usage/history/discussion segmented control on the entity panel.

* **Incremental loading and individuals.** Opening the editor loads only the ontology
  header summary, declared entity counts, and top tree level without mapping all axioms.
  Full axiom counts and diagnostics load on demand in the Header tab or via **Count the
  axioms**. Pickers preload kinds up to 5,000 entities and fall back to server entity
  search above that. The Individuals tree groups named individuals by asserted type (100
  members per page, plus an Untyped bucket; anonymous individuals are excluded;
  [screenshot](../images/ontology-individuals.png)). The Individuals filter searches at
  the current branch head and checks memberships at the displayed commit; historical name
  search is tracked in roadmap item 658. Python (`Dataset.ontology.individuals`), Node.js,
  and the JVM (`SparklesOnt.individuals`) expose the same paged API. Paged responses do
  not yet bound server scan work, as bucket discovery and typed pages scan the authorized
  graph scope (tracked in roadmap items 736 and 749). Individual reads reject entities
  with more than 1,000 named types and refuse more than 100,000 type buckets rather than
  returning truncated data.
* **Ontology metrics.** Both the Header view and the Browse landing page include an
  on-demand **Ontology metrics** panel ([screenshot](../images/ontology-metrics.png)).
  **Read metrics** counts mapped axioms and distinct signature entities, streams instance
  assertions, checks all four OWL 2 profiles (listing up to 100 violations per profile),
  and reports a conservative containing DL expressivity label (from EL through SROIQ(D)).
  Header metrics include dataset-local imports, while Browse metrics cover the selected
  graph without imports. Metrics refuse more than one million mapped axioms, assertion
  triples, or signature entities and report limits or deadlines as errors rather than
  partial counts.
* **DL Query and inferred trees.** **DL Query** evaluates a Manchester class expression
  for equivalent, super, sub, and instance answers, labelling whether OWL 2 EL
  classification or OWL 2 RL rules answered and listing axioms outside the profile
  ([screenshot](../images/ontology-dl-query.png)). **Inferred** shows the kept class
  hierarchy beside the asserted tree with its tier and outcome
  ([screenshot](../images/ontology-inferred.png)), while **Classify tree** explicitly
  computes a full taxonomy through bounded POST pages.
* **Previews, sessions, and stale drafts.** Every edit, undo, and redo runs a dry-run
  preview pinned to the displayed commit ([screenshot](../images/ontology-preview.png)),
  either on the active branch or on a session branch created via **Start session**. If
  another writer commits while a frame draft is open, the editor retains the draft beside
  a stale-draft notice and blocks submission until discarded and reloaded; accepted
  previews keep their original `If-Match` precondition.
* **Entity history.** The entity frame's **History** view pages commits touching triples
  that name the entity (200 change-log entries per position per page,
  [screenshot](../images/entity-history.png)), expands each commit via
  `GET /$/ontology/{ds}/diff?entity=`, and lets callers with full-branch write grants
  preview and revert commits. Bulk-load commits and commits older than the change log
  appear at the end, and commits touching only blank nodes of a restriction or annotated
  axiom are omitted.
* **Discussions.** The entity **Discussion** view and ontology **Discussions** tab store
  WebProtégé-style comment threads with `@name` mentions and resolve/reopen actions
  ([screenshot](../images/ontology-discussions.png), reading up to 1,000 comments and
  replies per view). Threads are stored as W3C Web Annotation RDF (`oa:Annotation`,
  `oa:TextualBody`, `dcterms:creator`, `schema:mentions`) in a sibling named graph
  (`urn:x-sparkles:discussions:<graph-iri>` or `urn:x-sparkles:discussions:default`) so
  they stay out of ontology frames and trees while participating in ordinary branch
  commits and access grants. An optional unversioned discussion store is tracked in
  roadmap item 769.
* **Watches.** The entity frame's **Watch** button watches an entity or its asserted
  subtree (`rdfs:subClassOf`, `rdfs:subPropertyOf`, `skos:broader`, `skos:narrower`) with
  optional depth limits ([screenshot](../images/watches.png)). The **Watches** page lists
  matched commits, supports **Mark read** and **Remove**, and updates the top-bar unread
  badge at most once per minute. Anonymous and static-token callers cannot watch. When an
  administrator marks an email [notification](../operations/README.md#operations) channel
  with `watchers`, matches batch every five minutes and mail the owner's verified address
  after re-checking current access grants. See [API: Watches](../API.md#watches).
* **Bulk edits.** The **Bulk edits** tab applies **Find and replace** (literal or regex
  with language filters), **Set annotation**, **Remove annotation**, and **Move classes**
  in a single previewed commit across up to 1,000 selected entities
  ([screenshot](../images/ontology-bulk.png)). Operations refuse more than 1,000 changes,
  graphs with more than 100,000 annotation assertions or `SubClassOf` axioms, moves
  exceeding 10,000 changed axioms, empty-matching regexes, and cyclic class moves. Larger
  ontology bulk edits are tracked in roadmap item 770.
* **Imports.** The **Imports** tab draws the `owl:imports` closure tree, marks cycles and
  unreadable targets, and manages the dataset-wide import map
  ([screenshot](../images/ontology-imports.png)). Imports resolve in order through the
  import map, matching graph IRI, and matching resource-catalog namespace. Administrators
  can load OASIS XML catalogs with local files or **Fetch into graph** / **Refresh** HTTP
  imports through the server's outbound policy. The **Imported axioms** switch toggles
  imported axioms across views while keeping edits targeted at the selected graph. Reads
  never fetch over the network (tracked in roadmap item 559), and reasoning runs use their
  own location mapping (tracked in roadmap item 772).

### Ontology browser

The **Browse** page (`/ui/browse`) provides OLS- and Ontobee-style reference pages for
ontologies and SKOS thesauri ([U12](../specs/U12-ontology-workbench/SPEC.md#ontology-browser),
[screenshot](../images/browse-term.png)). `/ui/browse?ds=NAME` opens the ontology landing
page, `&iri=IRI` opens an entity, and `graph`, `branch`, and `at` select the view scope.

An entity page opens with its label, short form, kind, formatted IRI (always copied in
full), linked non-zero summary counts (parents, children, incoming links, synonyms, and
other datasets), deprecation notice if present, and page-level actions (**Open in
Explore**, **Open in Ontology** or **Open in Thesaurus**, **Ask about this**, and
**Edit**, folding overflow into **More**). Below the header, the left column displays the
term's sections and the right rail holds sticky **Metadata**, **Identifiers** (linking
well-known databases such as Wikipedia, PubMed, and DOI without external lookups),
serialization downloads (Turtle, RDF/XML, OWL functional syntax, JSON-LD), and
**Hierarchy** cards.

One lead text displays directly beneath the counts (selecting the first definition by
`obo:IAO_0000115` or `skos:definition`, then description, comment, scope note, or note),
with a linked property chip on every value and datatype chips on typed literals. Remaining
texts sit in independent closed disclosures (such as other languages, comments, and
notes), all opened automatically in edit mode. Subsequent sections cover **Logical
definition**, **Synonyms** (or SKOS **Labels** with an opt-in Hidden toggle), **Axioms**
in linked Manchester syntax with **Copy**, **See also**, **Mappings**, **What links
here**, **In other datasets** (checking up to 20 readable datasets on the server),
**Similar terms** (from the first ready vector index), and **All other statements**.

Named-class inference regions read kept classifications at the displayed branch and commit
without triggering computation, reporting unavailable, running, mismatch, unsatisfiable,
inconsistent, excluded, and partial states clearly; **Classify** explicitly requests one
bounded frame projection. Property and individual pages read materialized frames and
hierarchies from fresh RDFS or OWL RL producers with exact revision pins and inferred
badges, counting universal entailments (such as `owl:Thing`) instead of listing them
([materialized property view](../images/ontology-materialized-properties.png),
[individual page](../images/browse-materialized-individual.png)). Disabled change logs or
reverted inputs refuse stale reads conservatively.

The **Hierarchy** card toggles between a scrollable **Tree** view (lineage, direct
parents, children with counts, and collapsed siblings) and a local **Graph** preview with
an **Expand** dialog. On viewports below 1,100 px the layout stacks into one column; on
phones the Hierarchy card opens on demand ([screenshot](../images/browse-phone.png)).

* **In-place edit mode.** Writers can press **Edit**, `E`, or **Edit this page** in the
  command palette to edit definitions, synonyms, annotations, parents, Manchester axioms,
  and SKOS labels or links directly on the term page
  ([screenshot](../images/browse-edit.png)). An edit bar shows the target branch,
  **Undo**, **Redo**, **Start session**, a Workbench link, and **Done**. Every save runs a
  dry-run preview against the displayed commit ([screenshot](../images/browse-edit-preview.png)),
  preserving axiom annotations, sharing undo/redo history with the Workbench editors, and
  disabling edits on protected branches until **Start session** is clicked. Shortcuts
  include `Mod+S` to preview, `Esc` to exit, and `S`, `P`, and `A` to focus add rows. On
  phones, the edit bar docks at the bottom ([screenshot](../images/browse-edit-phone.png)).
* **Landing page and routes.** The landing page shows ontology header metadata, a graph
  picker, full-width search with kind filters, top classes (`IAO_0000700` roots or
  parentless classes) with definitions and child counts, entity counts, and on-demand
  metrics ([screenshot](../images/browse-landing.png)). Browse reads the `hierarchy` and
  `term` routes of the [API reference](../API.md#ontology), keeping page cost constant on
  large ontologies such as GO or ChEBI. Its components live in
  `ui/src/lib/components/browse`.

### Thesaurus editor

The **Thesaurus** editor (`/ui/workbench/thesaurus`) edits SKOS concept schemes, concepts,
collections, and SKOS-XL labels within a graph
([U12](../specs/U12-ontology-workbench/SPEC.md#thesaurus-editor),
[screenshot](../images/thesaurus-editor.png)). The concept tree starts at top concepts and
expands `skos:broader` and `skos:narrower` one level at a time, flagging polyhierarchical
concepts with broader counts and marking unlinked non-top concepts. Search matches
preferred, alternative, and hidden labels (including SKOS-XL) in any or one language, and
the tree supports a per-page label language override. Tabs cover **Tree**, **History**,
and **Validation**, with a tree toggle between concepts and collections.

Dragging a concept onto another moves it (holding `Ctrl` or `Alt` adds a broader parent
without removing the old one), previewing both hierarchy directions as one commit
([screenshot](../images/thesaurus-move.png)) and refusing cycles. Dropping onto the top
zone promotes the concept to a top concept.

The resource panel switches between **Labels** (preferred, alternative, and hidden labels
by language, SKOS-XL conversion, and clash detection), **Relations** (broader, narrower,
related, and cross-scheme/cross-dataset mappings, flagging and repairing one-way links),
**Members** (ordered `skos:memberList` editing via
[editing forms](#nested-forms-lists-and-alternatives)), and **Notes** (built-in SKOS
shapes). **Validation** runs the built-in SKOS Reference integrity shapes
([screenshot](../images/thesaurus-checks.png), [API reference](../API.md#skos-shapes)) and
lets administrators install them as the dataset's write guard.

Tree, label, and link edits share the [ontology editor](#ontology-editor) session model:
dry-run previews with `If-Match`, commit-reverting undo/redo, `skos.<user>.<date>` session
branches, live reload on concurrent commits, and read-only mode on protected branches.
Because notes and members use the general editing-forms path, they are editable only in
single-graph datasets without partial-graph grants, and under authentication are read-only
on non-main branches for non-administrators.

## Known gaps

These limits name the roadmap items that address them. Current priorities and dependencies
are tracked in the [central roadmap](../ROADMAP.md).

### Web UI

* **Editing forms.** Forms edit predicate and inverse paths with literal and IRI values,
  nested blank-node children, ordered lists, and value alternatives. Node-level `sh:or`
  and `sh:xone` sub-forms (roadmap item 612), nested values over complex paths (item 613),
  blank-named graph contexts in nested models (item 614), lists of blank nodes, and
  ShEx-driven forms are not supported. Validation checks admit up to 500 additional
  affected targets beyond directly touched resources and refuse incomplete discovery.
  Graph-reading target filters and unsupported target-node expressions are refused before
  evaluation (item 615), and property functions or indirect SPARQL query dependencies
  refuse incomplete checks. Discovery bounds do not enforce a total SHACL heap limit;
  controlled cold-reader admission is tracked in roadmap item 395.
* **Pattern pane and live profiling.** The Pattern pane edits basic graph patterns,
  filters, one level of `OPTIONAL`, simple paths, projections, and modifiers; `UNION`,
  `GRAPH`, inline `VALUES`, `MINUS`, and `FILTER NOT EXISTS` remain locked text blocks,
  waiting nodes do not survive reload, and the pane requires the browser formatter module
  with no server fallback (roadmap item 101). Plan on pattern reflects completed or
  explained plans rather than updating live while a query runs (roadmap item 27).
* **Large-graph rendering.** Layouts run in the browser without server caching or binary
  transfer, GPU frame-rate targets lack automated GPU-runner coverage, and the Explore
  overview opens one path below one class at a time without named-graph grouping (roadmap
  item 29).
* **Linked views and embedding map.** The embedding map cannot colour by a kept Leiden
  run, `Shift+M` does not open it, date facets group by whole years only, facets stack
  below the graph on phones, Explore maps and facets read `main` only, and `VALUES` blocks
  from **Use in query** remain locked on the Pattern canvas (roadmap item 140). Arc and
  embedding-map frame rates have been measured only under software rendering (item 141),
  and projections live in memory only and are unavailable to callers with partial access
  (item 142).
* **Validation overlay.** The overlay re-validates drawn nodes instead of reading recorded
  guard results (roadmap item 120), cannot prove whether a previewed fix clears a result
  under an incremental guard with other dataset violations (item 121), and is unavailable
  to callers with partial-graph grants (item 75).
* **Ontology editor and DL Query.** Classifier-backed property and individual entailments
  remain planned under F24 (roadmap item 609). IRI policy lives in browser storage per
  dataset (item 610), and rename does not cover IRIs inside triple terms; triple-term
  rename and an editable schema diagram are tracked in roadmap item 457. DL Query instance
  answers reclassify assertions per query, and queries outside OWL 2 EL copy the ontology
  into memory for RL rules, where class answers may be incomplete (roadmap item 662).
* **Kept and materialized inference frames.** Named-class frames display up to 1,000 rows
  per part from kept classifications, while property and individual frames read fresh
  RDFS/OWL RL materializations without completeness, consistency, or explanation links.
  General classifier-backed inferred axiom listings and explanations remain planned under
  F24 (roadmap items 588, 622, and 609). The full **Classify tree** action remains
  explicit via legacy POST pages.
* **Thesaurus editor.** The editor reads up to 2,000 narrower concepts per concept and
  5,000 roots per scheme, and label search returns the first 100 substring matches
  (roadmap item 638). Cross-dataset mappings display target IRIs and list only locally
  stated mappings (item 639), `skosxl:labelRelation` links are not edited (item 640), and
  installing SKOS rules as a write guard replaces the existing guard rather than composing
  with it (item 641). Editing forms lack per-request graph and commit preconditions, so
  notes and members forms stay read-only in multi-graph datasets and resources defined
  only in named graphs have no editable form (item 642). Duplicate-text blank-node SKOS-XL
  labels are displayed read-only.
* **Branch grants, history, and browser edit mode.** Write capability in the ontology
  editor checks dataset-level access, so branch-only writers see the branch read-only, and
  undo/revert requires whole-branch write access (roadmap item 611). Entity History omits
  commits that touch only blank nodes of restrictions or annotated axioms (item 765) and
  reads the graph twice per opened commit. Browse edit mode leaves SKOS-XL labels and
  collection members to the thesaurus editor and property assertions, keys, and chains to
  raw axiom entry (item 706); its first preview on a large ontology waits on full axiom
  loading (item 707); derived frames are read-only (item 622); search reads the branch
  head only so historical views hide the search box (item 658); and cross-server term reuse
  is tracked in item 461. Thesaurus label search and DL Query completion use separate name
  lookups rather than entity search (item 660). Deleting a selected entity can leave an
  unhandled tree-reveal rejection (item 751).

## Configuration and usage

The following guides describe configuration and supported UI, command-line, HTTP, and
library workflows. Exact HTTP contracts live in the [API reference](../API.md).

* [The Ask bar](../USAGE.md#the-ask-bar)
* [The Pattern pane](../USAGE.md#the-pattern-pane)
* [Finding paths on the query page](../USAGE.md#finding-paths-on-the-query-page)
* [The Explore overview](../USAGE.md#the-explore-overview)
* [Finding paths in Explore](../USAGE.md#finding-paths-in-explore)
* [Lenses on the graph](../USAGE.md#lenses-on-the-graph)
* [Algorithm lenses](../USAGE.md#algorithm-lenses)
* [Why is this inferred?](../USAGE.md#why-is-this-inferred)
* [Facets and Use in query](../USAGE.md#facets-and-use-in-query)
* [Arcs on the map](../USAGE.md#arcs-on-the-map)

## Roadmap

A standalone desktop application is planned, with its framework comparison and parity
requirements in [U13](../specs/U13-desktop-application/README.md)
([roadmap item 746](../ROADMAP.md)). The current interface is served by the Sparkles
server.

The remaining UI work described above refers to [item 622](../ROADMAP.md),
[item 630](../ROADMAP.md), [item 658](../ROADMAP.md), [item 461](../ROADMAP.md),
[item 612](../ROADMAP.md), [item 613](../ROADMAP.md), [item 614](../ROADMAP.md),
[item 615](../ROADMAP.md), [item 27](../ROADMAP.md), [item 29](../ROADMAP.md),
[item 75](../ROADMAP.md), [item 101](../ROADMAP.md), [item 120](../ROADMAP.md),
[item 121](../ROADMAP.md), [item 140](../ROADMAP.md), [item 141](../ROADMAP.md), and
[item 142](../ROADMAP.md). The central roadmap owns priorities and dependencies, and its
[completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs) link
partially complete specs to their remaining work and acceptance criteria.
