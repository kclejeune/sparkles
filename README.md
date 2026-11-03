# Sparkles ✨

Sparkles is an RDF database written in Rust, with SPARQL queries, validation and
reasoning. It implements selected [Apache Jena](https://jena.apache.org/) and Fuseki
protocols and behavior using indexes and execution inspired by
[QLever](https://github.com/ad-freiburg/qlever). It provides an embeddable library with
Rust, Python, JVM and JavaScript APIs, a Fuseki-compatible server and CLI, and a web UI.
The JVM binding implements Jena's `DatasetGraph`, letting Java and Kotlin applications
use Sparkles through Jena's existing query, model and transaction APIs.

> [!WARNING]
> Sparkles is experimental. There are no releases, and the on-disk format, HTTP API, CLI
> and library APIs can change in any commit without a migration path. Don't use it for
> data you can't regenerate.
>
> Much of the code, tests and documentation was written with AI models, directed and
> reviewed by the maintainer. The W3C conformance suites and differential tests are the
> main safeguard, but expect bugs. Issues and bug reports are welcome.

![A CONSTRUCT query's results drawn as a graph of people, who they know and where they work, with one person's neighborhood highlighted](docs/images/graph.png)

The [web UI tour](#web-ui) shows the main workflows in screenshots.

## Why Sparkles

Jena and Fuseki have the protocols, tools and operational model that RDF applications
depend on, and QLever has an index and executor built for speed. Sparkles puts the first
on top of the second:

1. **Jena and Fuseki integration.** Clients using the supported Fuseki
   protocols can keep their existing request format. Sparkles implements the SPARQL 1.1
   Query, Update and Graph Store protocols, supported Fuseki administration endpoints,
   and Jena's RDF formats and dataset semantics. It also provides bulk loads, transactions,
   compaction and backups. The JVM binding
   brings that compatibility into the application: open a Sparkles dataset and keep
   using Jena's `Model`, RIOT, `Txn` and `QueryExecution` APIs.
2. **QLever-style internals where performance matters.** Terms are 64-bit ids with
   numbers and dates stored inline, indexes are sorted and compressed permutations, and
   a cost-based planner drives column-at-a-time execution. Sparkles adds MVCC
   transactions, durable commits, online compaction and a history that can be read,
   diffed and branched.
3. **Library first.** The engine is a library with no HTTP server. The server, the CLI
   and the Python, JVM and Node.js bindings use its public API and query semantics.
   The [feature list](docs/integrations/README.md#libraries-and-bindings) identifies the administration
   and integration surfaces each binding exposes. The term model, parsers and SPARQL
   algebra come from the Oxigraph project's crates.

## Highlights

* **Query performance.** Published comparisons cover synthetic and standard query
  workloads, loading, writes and memory. See [Performance](#performance) and the
  [benchmark summary](docs/BENCHMARKS.md).
* **Standards conformance.** Sparkles passes the W3C suites for SPARQL 1.0, 1.1 Query,
  1.1 Update and 1.2, SHACL Core and SHACL-SPARQL, and supports RDF 1.2, Jena's ARQ
  extensions and GeoSPARQL 1.1. See [Standards conformance](docs/querying/README.md#standards-conformance) and
  [GeoSPARQL](docs/search/README.md#geosparql).
* **Fuseki compatibility.** The server speaks the SPARQL 1.1 Protocol, the Graph Store
  Protocol and Fuseki's `/$/` admin API, and converts Fuseki configurations. See
  [Server and protocols](docs/server/README.md#server-and-protocols).
* **Branches and time travel.** Queries can read any commit that compaction has retained,
  two retained commits can be diffed, and a branch can start from one and merge back with
  conflict review. Full-text search reads only the head. See
  [Versioning and history](docs/storage/README.md#versioning-and-history).
* **Vector and full-text search.** BM25 full-text search compatible with jena-text, HNSW
  vector indexes with embeddings computed on write, hybrid ranking, path search and graph
  algorithms all run inside SPARQL. See [Search](docs/search/README.md#search).
* **Reasoning and validation.** RDFS, OWL 2 RL and Jena rules can be materialized
  incrementally or derived at query time, and SHACL and ShEx can validate every commit.
  See [Reasoning, validation and schema](docs/reasoning/README.md#reasoning-validation-and-schema).
* **GraphRAG and agent memory.** An MCP server gives agents grant-checked tools, questions
  are answered with checked queries and cited summaries, and agent memory records each
  fact with its source for review on a branch. GraphRAG builds write reports for Leiden
  communities. See [AI and agents](docs/ai/README.md#ai-and-agents).
* **Web UI.** The embedded UI has a query editor with table, graph and map views, a
  resource explorer, a visual query builder, forms generated from SHACL shapes, and views
  of history, branches and agent memory. See [Web UI](#web-ui) and
  [the feature list](docs/visualization/README.md#web-ui).
* **Bindings.** The Rust, Python, JVM and Node.js libraries embed the engine in process,
  and the JVM binding implements Jena's `DatasetGraph`. See
  [Libraries and bindings](docs/integrations/README.md#libraries-and-bindings).
* **Access control.** Grants apply per dataset, named graph and endpoint, with rate
  limits and query budgets. See [Access control](docs/server/README.md#access-control-and-security).

[docs/FEATURES.md](docs/FEATURES.md) lists every feature and the
[known gaps](docs/FEATURES.md#known-gaps), including compatibility differences from Jena.

## Getting started

To build the server, install Rust with the toolchain in `rust-toolchain.toml`. Linux
servers also need system CA certificates. Build the optional web UI first if you want
it embedded in the binary. The UI build requires Node.js and pnpm.

```sh
pnpm -C ui install && pnpm -C ui build  # optional web UI
cargo build --release -p sparkles-server
export PATH="$PWD/target/release:$PATH"
```

The binary is `target/release/sparkles`. Create a small dataset and query it before
starting the server.

```sh
cat > books.ttl <<'EOF'
<https://example.org/book> <https://example.org/title> "A book" .
EOF
sparkles load --loc ./books books.ttl
sparkles query --loc ./books --results csv 'SELECT (COUNT(*) AS ?n) { ?s ?p ?o }'
```

The query returns the CSV header `n` and one row with a count of `1`. Start the server
in a terminal.

```sh
sparkles serve --data ./data --loc books=./books
```

In another terminal, query the running server. The response contains a CSV header `n`
and a row with `1`.

```sh
curl -fsS localhost:3030/books/sparql -H 'Accept: text/csv' \
  --data-urlencode 'query=SELECT (COUNT(*) AS ?n) { ?s ?p ?o }'
```

The server opens datasets after it starts listening. Retry the request if the dataset is
still opening. The server holds the database lock while it runs. Use HTTP while it is
running, or stop it before using a command with `--loc`. If built, the UI is at
<http://localhost:3030/ui/>. `sparkles help COMMAND` describes each command.

Nix and Docker provide alternative ways to run the server.

```sh
nix run github:kclejeune/sparkles -- serve --data ./data
docker compose up --build -d
```

See [getting started and operations](docs/GETTING-STARTED.md) for setup, security and
recovery guidance, and [libraries and clients](docs/README.md#libraries-and-clients)
for the Rust, Python, JVM and Node.js APIs. The binding packages are not published to a
registry.

## Documentation

| Document | Contents |
|---|---|
| [Features](docs/FEATURES.md) | Every capability, area by area, and the known gaps. |
| [Benchmarks](docs/benchmarks/README.md) | Results against Jena, QLever, Fluree and Oxigraph, with setup, raw-run catalogs, charts and measured losses. |
| [Architecture](docs/ARCHITECTURE.md) | Library boundaries, storage, execution and the main design decisions. |
| [Comparison](docs/COMPARISON.md) | Feature gaps per engine and where Sparkles departs from Jena and QLever. |
| [Usage](docs/USAGE.md) | The server, the CLI, the bindings, MCP, backups and deployment. |
| [HTTP API](docs/API.md) | Fuseki's endpoints and the Sparkles extensions. |
| [Development](docs/DEVELOPMENT.md) | Project layout, builds, tests and mise tasks. |
| [Roadmap](docs/ROADMAP.md) | Planned work in priority order. |
| [Design specs](docs/specs/README.md) | The design and outcome of each larger feature. |

[The documentation index](docs/README.md) lists every guide and reference.

## Performance

These comparisons use an Intel Core i5-13500 with 16 GiB of RAM, with engines running
alone and result caches off. Answer checks qualify the reported comparisons.

* At 10.5M synthetic triples, Sparkles loads in **3.67 s** and has the lowest native
  endpoint mean on **27 of 28 queries** among the qualified results. It ties Fluree on
  `count-all` within the 10% band and has a median **3.8×** speedup over QLever. Fluree
  uses a different result encoding and one query exceeds its memory limit. The
  [synthetic report](docs/benchmarks/synthetic/RESULTS.md) describes those limits.
* With 16 clients running `star-join` at that scale, throughput is **239 queries/s**,
  against QLever's 90.
* On WatDiv at 10.97M triples, geometric mean latency is **4.63 ms**, against
  6.97–17.9 ms for the other engines.
* English DBpedia at 1.24B triples loads in **480 s**, against QLever's 1,674 s and
  Fluree's 2,919 s.

The [benchmark reports](docs/benchmarks/README.md) give the full results, methodology,
[memory tradeoffs](docs/benchmarks/memory/RESULTS.md) and
[remaining losses](docs/BENCHMARKS.md#where-sparkles-loses).

## Web UI

The web UI is embedded in the server at `/ui/`. The first table shows what sets Sparkles
apart: time travel, branch merges, forms generated from shapes, GraphRAG communities,
questions in plain language, agent memory, path finding, inference, maps, validation,
graph algorithms, facets, the embedding map, the visual query builder, the ontology
editor and browser and the app shell. The collapsed panel below it has more workflows and detail views.

<table>
  <tr>
    <td><img src="docs/images/time-slider.png" alt="The query page with the time slider in compare mode, marking one team membership as added and one as removed between two commits"></td>
    <td><img src="docs/images/diff-graph.png" alt="The diff graph of one commit, drawing a new employee, a new job title and a move to Los Angeles in green and the removed job title, home city and acquaintance as dashed red ghosts"></td>
  </tr>
  <tr>
    <td>Time travel: a query's results compared between two commits</td>
    <td>A commit's changes drawn as a graph</td>
  </tr>
  <tr>
    <td><img src="docs/images/merge.png" alt="The merge page previewing a merge of one branch into main, with two conflicts and a choice of which side to take for each"></td>
    <td><img src="docs/images/forms.png" alt="The resource explorer with a person's graph and an editing form generated from SHACL shapes in the side panel, showing an email that fails the shape's pattern under its field and a server check that reports an employer which is not a company"></td>
  </tr>
  <tr>
    <td>Branch merges with conflicts to resolve</td>
    <td>An editing form generated from SHACL shapes, checked in the browser and on the server</td>
  </tr>
  <tr>
    <td><img src="docs/images/explore-communities.png" alt="The Explore overview drawing seven GraphRAG communities of a freight network as circles sized by their members and coloured by report status, with the head office selected and its report, findings, sub-communities and members beside the picture"></td>
    <td><img src="docs/images/ask-answer.png" alt="The Ask bar with a drafted SPARQL query, its results and a summary whose numbered citations point to result rows"></td>
  </tr>
  <tr>
    <td>GraphRAG communities with their reports</td>
    <td>A question answered with a checked query and a cited summary</td>
  </tr>
  <tr>
    <td><img src="docs/images/memory.png" alt="The Memory page listing facts that agents recorded, each with its source and review status, and a conflict between two values"></td>
    <td><img src="docs/images/query-paths.png" alt="A query's graph of 37 freight flights between cities, with the Paths panel beside it listing the three shortest routes from Vancouver to Auckland by distance, each with its cost summed from the ex:km values on the reifiers of its flights, and the routes drawn over the result with each edge numbered by the paths that use it"></td>
  </tr>
  <tr>
    <td>Agent memory with sources and review status</td>
    <td>The shortest routes by a distance on the edges' reifiers, drawn on a query's graph</td>
  </tr>
  <tr>
    <td><img src="docs/images/explore-lenses.png" alt="The resource explorer with lenses on, sizing nodes by their degree in the dataset, colouring them by class, badging the nodes with inferred edges and drawing those edges dashed, with the legend beside the graph"></td>
    <td><img src="docs/images/map-arcs.png" alt="The resource explorer showing a freight company's graph as a world map, with arcs from its head office in Dubai to its offices, partners and employees on six continents, coloured by predicate"></td>
  </tr>
  <tr>
    <td>Inferred edges and lenses on the canvas</td>
    <td>A graph's relations drawn as arcs on a map</td>
  </tr>
  <tr>
    <td><img src="docs/images/validation-overlay.png" alt="The resource explorer with validation badges on three people, the validation lens's legend grouping the nodes by their worst severity, and a results panel previewing an update that adds a missing name"></td>
    <td><img src="docs/images/explore-algo-lenses.png" alt="The resource explorer with algorithm lenses, sizing nodes by PageRank over the drawn graph and colouring them by Leiden communities, with the legend giving the score ranges and each community's size"></td>
  </tr>
  <tr>
    <td>SHACL results on the graph, with a suggested fix previewed</td>
    <td>Lenses that size nodes by PageRank and colour them by community</td>
  </tr>
  <tr>
    <td><img src="docs/images/explore-facets.png" alt="The resource explorer with the Facets panel beside a person's graph, brushing an age range on a histogram and one employer on a bar list, so that the other facets count the 41 people under both brushes and the drawn people outside them are dimmed"></td>
    <td><img src="docs/images/embedding-map.png" alt="The embedding map of 6,000 freight shipments projected from 64-dimensional vectors to two dimensions and coloured by cargo class, with the legend counting each class and a lasso around one cluster whose 1,171 shipments form the shared selection, ready for Use in query or Open as graph"></td>
  </tr>
  <tr>
    <td>Facet histograms whose brushes filter the graph</td>
    <td>A vector index's embedding map, where a lasso selects entities for a query</td>
  </tr>
  <tr>
    <td><img src="docs/images/pattern.png" alt="The query editor with the Pattern pane beside it, drawing the query's variables as nodes joined by its triple patterns, an optional part in a dashed hull, a filter as a chip under its variable and a BIND as a locked block, with the projection checkboxes, the count of matches and seven result rows"></td>
    <td><img src="docs/images/pattern-edit.png" alt="The Pattern pane adding an edge from ?person, with the predicate picker listing the predicates that instances of ex:Person use, with their counts"></td>
  </tr>
  <tr>
    <td>A query's pattern drawn beside its text</td>
    <td>A query built on the canvas from the schema</td>
  </tr>
  <tr>
    <td><img src="docs/images/ontology-editor.png" alt="The ontology editor with the class tree of a university ontology and the frame of Professor, listing its labels in French and English, its superclasses Employee and teaches some Course, its instance and an axiom on its range, each with an add field"></td>
    <td><img src="docs/images/ontology-inferred.png" alt="The ontology editor with the asserted and inferred class trees side by side, the EL classifier placing Professor under Teacher, which is defined as a Person who teaches some Course"></td>
  </tr>
  <tr>
    <td>The ontology editor: class trees and an entity's frame</td>
    <td>Asserted and inferred class hierarchies side by side</td>
  </tr>
  <tr>
    <td colspan="2" align="center"><img width="50%" src="docs/images/ontology-dl-query.png" alt="The DL Query tab of the ontology editor answering Person and (teaches some Course), labelled as answered completely by the OWL 2 EL classifier, with Teacher as its equivalent class, Person and Agent above it, Professor below it and Ada Lovelace as its instance, beside the frame of Professor"></td>
  </tr>
  <tr>
    <td colspan="2" align="center">A DL query in Manchester syntax, labelled with the reasoning that answered it</td>
  </tr>
  <tr>
    <td><img src="docs/images/browse-term.png" alt="The ontology browser's page of mitochondrion, with its lineage from cellular component on the left, its definition and sources, its subclass axioms with part of some cytoplasm as linked prose, its exact and related synonyms, and an infobox with its ID, subsets, ontology, hierarchy counts and downloads"></td>
    <td><img src="docs/images/browse-landing.png" alt="The landing page of a cell organelle ontology in the ontology browser, with its title, version and description, a search that lists the classes matching mitoch, the top class with its definition and child count, and the counts of classes and properties"></td>
  </tr>
  <tr>
    <td>The ontology browser: a page for each entity, with its lineage and linked axioms</td>
    <td>An ontology's landing page with its search and top classes</td>
  </tr>
  <tr>
    <td><img src="docs/images/thesaurus-editor.png" alt="The thesaurus editor with the concept tree of an animals scheme, where Whales and Penguins appear under both Mammals and Marine animals marked 2 broader, and the label table of Whales with its English and French preferred labels and an English alternative label, each with buttons that edit it, remove it or turn it into a SKOS-XL label"></td>
    <td><img src="docs/images/thesaurus-checks.png" alt="The checks of the thesaurus editor, listing the concept Owls as breaking S13 because one label is both preferred and alternative, with a button that enforces the SKOS rules on writes"></td>
  </tr>
  <tr>
    <td>The thesaurus editor: a concept tree with polyhierarchy and multilingual labels</td>
    <td>The SKOS Reference's integrity conditions checked by built-in shapes</td>
  </tr>
  <tr>
    <td><img src="docs/images/forms-nested.png" alt="The editing form of a SKOS concept with its SKOS-XL labels as nested forms, the stored label edited to All animals and a new French label added from the label shape's template, with the previewed commit listing the triples it adds and removes"></td>
    <td><img src="docs/images/forms-list.png" alt="The editing form of a SKOS collection, with its member chosen as a Concept among the alternatives of sh:or and its ordered member list after fish moved up, each member with buttons that move or remove it"></td>
  </tr>
  <tr>
    <td>Nested blank-node children edited in one form and one commit</td>
    <td>An ordered list editor and a choice among sh:or alternatives</td>
  </tr>
  <tr>
    <td><img src="docs/images/palette.png" alt="The command palette on the Query page with nothing typed, listing the page's commands with their shortcuts and the pages of the shell"></td>
    <td><img src="docs/images/shell-phone.png" alt="The web UI on a phone, with the drawer open over a dataset page, holding the dataset switcher, the pages, the connection status and the theme"></td>
  </tr>
  <tr>
    <td>Ctrl+K opens a command palette of the page's commands, entities, datasets and saved queries</td>
    <td>On a phone the pages move into a drawer and the search expands over the bar</td>
  </tr>
</table>

<details>
<summary>More screenshots and UI workflows</summary>

<table>
  <tr>
    <td><img src="docs/images/explore-why.png" alt="The resource explorer with the Why panel beside the graph, explaining that Alice is a Person by rdfs9 from her inferred Employee type, itself derived by rdfs9 from her asserted Manager type and the subclass of Manager, and the subclass of Employee, with every premise highlighted on the canvas after Show on graph"></td>
    <td><img src="docs/images/explore-why-stale.png" alt="The Why panel on a dataset whose inferences are two commits stale, with the note that the tree shows the inferences as computed at commit 2, and the second of two derivations, Alice as an Employee by rdfs3 from the range of reportsTo"></td>
  </tr>
  <tr>
    <td>The derivation tree of an inferred edge, with its premises drawn</td>
    <td>A derivation from stale inferences, with its note and a second top rule</td>
  </tr>
  <tr>
    <td><img src="docs/images/forms-branch.png" alt="An editing form on its own page after a save while main is protected, with a banner saying it edits the session branch edits.anonymous.20261024 with one commit and a Review and merge button, and an age below the shape's minimum marked under its field"></td>
    <td><img src="docs/images/branches.png" alt="A dataset page listing its branches above a commit history graph that includes a merge"></td>
  </tr>
  <tr>
    <td>A form whose saves go to the editor's session branch, with a link to review and merge it</td>
    <td>Branches and their commit graph</td>
  </tr>
  <tr>
    <td><img src="docs/images/diff-lapse.png" alt="A time-lapse over four commits with ghosts kept, showing the changes of each commit added in green and removed ones as dashed ghosts, with a running total of changes since the start"></td>
    <td><img src="docs/images/time-slider-graph.png" alt="The resource explorer with the time slider in compare mode, drawing a new edge in green and a removed one as a dashed ghost"></td>
  </tr>
  <tr>
    <td>A time-lapse that applies one commit's diff per step</td>
    <td>A graph compared between two commits</td>
  </tr>
  <tr>
    <td><img src="docs/images/handoff.png" alt="A query handed over by an agent, open in the query editor with its question, assumptions, the terms it uses and a clean check"></td>
    <td><img src="docs/images/ask-steps.png" alt="The Ask bar while it works, showing Summarizing after the query results have arrived"></td>
  </tr>
  <tr>
    <td>A query an agent handed over for review</td>
    <td>An answer in progress, at the summary step</td>
  </tr>
  <tr>
    <td><img src="docs/images/plan-pattern.png" alt="The Pattern tab of a query that ran with Plan on pattern, each edge labelled with its estimated and actual rows, ex:memberOf marked as a misestimate, numbered join badges on the variables and the plan tree beside it"></td>
    <td><img src="docs/images/result-arcs.png" alt="A query result drawn as labelled great-circle arcs for 37 freight routes between cities on six continents, beside the list of rows"></td>
  </tr>
  <tr>
    <td>A query's plan drawn on its pattern, with misestimates marked</td>
    <td>Result rows drawn as arcs between places</td>
  </tr>
  <tr>
    <td><img src="docs/images/map.png" alt="GeoSPARQL query results drawn on a map beside a list of the places"></td>
    <td><img src="docs/images/query-facets.png" alt="The query page's Facets tab counting the values of ?person in every solution, with a brush on the years people joined and the editor above holding the triple and FILTER that Apply to query added"></td>
  </tr>
  <tr>
    <td>GeoSPARQL results on a map</td>
    <td>A brush on a result's facets written into the query</td>
  </tr>
  <tr>
    <td><img src="docs/images/query.png" alt="The query editor with a SPARQL query and its results"></td>
    <td><img src="docs/images/explore.png" alt="The resource explorer showing one resource as a graph, with its properties and the resources that refer to it"></td>
  </tr>
  <tr>
    <td>The query editor and its results</td>
    <td>The resource explorer</td>
  </tr>
  <tr>
    <td><img src="docs/images/schema.png" alt="The schema browser with a class hierarchy, instance counts and the properties of the selected class"></td>
    <td><img src="docs/images/path-finder.png" alt="The resource explorer with the Paths panel open, listing the three shortest foaf:knows paths from Freya Lindqvist to Lukáš Novák, which pass through ten different people, and drawing them over her neighbourhood with each edge numbered by the paths that use it"></td>
  </tr>
  <tr>
    <td>The schema browser</td>
    <td>The k shortest paths between two people</td>
  </tr>
  <tr>
    <td><img src="docs/images/models-settings.png" alt="The model configuration form on the server page, with a provider's allowed models as a list of three with buttons to move and remove each, and its per-model options as two open cards with typed fields for context tokens, output tokens, temperature, structured output, request timeout and prices, the empty fields showing the values they inherit"></td>
    <td><img src="docs/images/models-settings-errors.png" alt="The model configuration form with a new model's card marked 2 errors, saying under its fields that context tokens must be a whole number and that the temperature is at most 2, above the role lists shown as rows of provider and model"></td>
  </tr>
  <tr>
    <td>Allowed models and per-model options as form fields</td>
    <td>Errors beside the fields of the model configuration</td>
  </tr>
  <tr>
    <td><img src="docs/images/ontology-preview.png" alt="The preview of an ontology edit that creates the class teaching assistant, listing the declaration, label and subclass axioms it adds, the three triples it writes and the commit message"></td>
    <td><img src="docs/images/thesaurus-move.png" alt="The preview of a drag in the thesaurus editor that moves Bats under Flying animals, removing the broader and narrower triples that linked it to Mammals and adding the two that link it to Flying animals, above the commit message"></td>
  </tr>
  <tr>
    <td>An ontology edit previewed as axioms and triples before it commits</td>
    <td>A concept dragged to a new broader concept, previewed as one commit</td>
  </tr>
</table>

</details>

## Status, capabilities and license

Sparkles has no release. The [roadmap](docs/ROADMAP.md) covers the remaining correctness,
performance, recovery and packaging work and the planned stability contract.

* RDF and SPARQL support passes the documented conformance suites.
* Reasoning, incremental views, GeoSPARQL, GraphRAG, bindings and the web UI are
  available, with further phases planned.
* Evidence and valid-time vocabularies, shapes and query resources are available.
  Authenticated evidence proposals and reference resolution are available. Review-controlled
  projections remain planned.
* Durable jobs, maintained analysis, temporal runtime helpers and ChatGPT-plan model access
  remain planned.

The [feature matrix](docs/FEATURES.md#capability-status) gives the complete status and
remaining scope of each capability.

Sparkles is licensed under the [Apache License 2.0](LICENSE). The vendored `spargebra`
keeps its MIT OR Apache-2.0 license. The licenses of the crates and npm packages that ship
in the binary are in [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) and
[THIRD_PARTY_LICENSES-UI.md](THIRD_PARTY_LICENSES-UI.md).
