# U05: Notebooks stored in the dataset

> **Status:** specified
>
> **Phases:** None shipped. Phase 1 is notebooks kept as RDF in a reserved named graph
> per notebook, the HTTP routes, and a notebook page with Markdown, SPARQL and parameter
> cells. Phase 2 adds Ask, chart and map cells, pinned outputs, and the MCP tools and
> resources. Phase 3 adds export to and import from the Jupyter notebook format and
> Markdown.
>
> **User docs:** none, because nothing is built.
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end will record how it lands.

This design was written from the Jupyter notebook format documentation (nbformat 4), the
public documentation of Observable notebooks and of Observable Plot, the W3C RDF 1.1
Concepts (for `rdf:JSON` as defined in JSON-LD 1.1), and the Sparkles code and specs:
point-in-time reads ([F06](F06-snapshots-and-point-in-time.md)), branches and merges
([F09](F09-branches-and-merges.md)), graph access control
([C12](C12-graph-access-control.md)), stored queries ([C16](C16-stored-queries.md)),
questions in natural language ([C18](C18-natural-language-questions-and-ingest.md)), the
MCP server ([C11](C11-mcp-server.md)), GeoSPARQL maps ([G01](G01-geosparql.md)) and
backups and clones ([F05](F05-snapshot-repositories.md),
[C06](C06-clone-to-sandbox.md)). The time slider of [U01](U01-time-slider.md) is reused
for a notebook's state.

## 1. Summary, goals, non-goals

An analysis of a dataset is a sequence of queries with notes between them: what was
asked, what came back, what it means. Today the query page keeps tabs and a local
history in the browser, and C16 stores single queries by name. Neither keeps an analysis
together, shares it with a colleague, or ties it to the state of the data it described.

This spec adds notebooks: ordered cells of Markdown, SPARQL, parameters, questions,
charts and maps, kept in the dataset itself. Because they are data in the dataset, a
notebook has history, branches, merges, backups, clones and per-graph grants without new
machinery, and an agent reads and runs it over MCP.

**Goals**

1. A notebook is stored in the dataset and changes with commits, so its history and
   diffs are F06's and its branches are F09's.
2. A notebook can be pinned to a commit, so it reruns against the state it was written
   for, and moved to head on purpose.
3. Parameter cells bind values into SPARQL cells as terms, as C16 binds parameters.
4. Grants on a notebook are C12 graph grants.
5. Agents list, read, run and, with the update opt-in, write notebooks over MCP.
6. Export to the Jupyter format and Markdown, so a notebook can leave Sparkles.

**Non-goals**

- Running code other than SPARQL. There is no Python or JavaScript kernel. Chart and map
  cells are declarative.
- Real-time co-editing. Two people editing one notebook commit separately, and branches
  and merges reconcile them.
- Scheduled runs and reports. A later spec can run notebooks on C21's notifications.
- Executing a Jupyter notebook written elsewhere. Import accepts only the cell kinds
  this spec defines.

## 2. User-visible behaviour

### 2.1 Where notebooks live in the UI

A **Notebooks** entry in the navigation lists the notebooks of the selected dataset and
branch, with title, author, last commit and whether it is pinned. **New notebook** asks
for a name. The query page's tab menu gets **Add to notebook…**, which appends the tab's
query as a SPARQL cell.

### 2.2 The notebook page

`/notebooks/{name}?ds=&branch=` shows the cells in order.

```
┌─ org › Notebooks › team-churn ───────── main ▾  pinned at commit 318 ▾  [Run all] ┐
│ ## Who left teams this quarter                                                    │
│ Teams are read from ex:memberOf. A person left when the triple was removed.      │
├──────────────────────────────────────────────────────────────────────────────────┤
│ ⚙ Parameters   team [ex:commerce ▾]   since [2026-07-01]                          │
├──────────────────────────────────────────────────────────────────────────────────┤
│ ▸ SPARQL  [1]  ran at commit 318 in 42 ms · 14 rows            [Table|Graph|Raw]  │
│   SELECT ?person ?time WHERE { SERVICE hist:changes { << ?person ex:memberOf ?team >>│
│     hist:op "remove" ; hist:time ?time } FILTER(?time >= ?since) }               │
│   ┌ person        │ time               ┐                                           │
├──────────────────────────────────────────────────────────────────────────────────┤
│ ▸ Chart  of [1]   bar · x = month(?time) · y = count                             │
├──────────────────────────────────────────────────────────────────────────────────┤
│ + Markdown  + SPARQL  + Ask  + Parameters  + Chart  + Map                         │
│                         3 unsaved changes  Message [add chart]  [Save]           │
└──────────────────────────────────────────────────────────────────────────────────┘
```

- **Cells.** Each cell has a kind, an optional title and its content. Cells move with
  drag handles or `Alt+↑`/`Alt+↓`, and are added between cells with `+`.
- **Running.** `Shift+Enter` runs a cell, **Run all** runs them in order. A SPARQL cell
  runs at the notebook's state (§2.3) with the parameters bound, and shows its results in
  the query page's views (`ResultTable`, `GraphView`, `ResultMap`). A cell's caption names
  the commit it read.
- **Editing and saving.** Edits are local until **Save**, which makes one commit with the
  message. The page warns before leaving with unsaved changes. A save on a protected
  branch is refused as any write is.
- **History.** The notebook's own history is F06's history of its graph: **History**
  opens U01's slider over the commits that changed the notebook, and compare mode shows
  cells added, removed and changed.

### 2.3 Pinning

A notebook is either **at head** or **pinned** at a commit (`nb:at`). Pinned SPARQL cells
always read that commit with `at=`, so results do not drift. **Move to head** runs every
cell at head, shows which results changed in row counts, and saving records the new pin.
A pin to a commit that retention has dropped shows "the pinned state is gone" and offers
head. Pinning a notebook also offers creating a named snapshot of that commit (F06), so
retention keeps it.

### 2.4 Cell kinds

| Kind | Content | Phase |
|---|---|---|
| Markdown | CommonMark text with GFM tables, rendered without raw HTML (§9). The UI has no Markdown renderer today. | 1 |
| SPARQL | A read query. Updates are refused at save. | 1 |
| Parameters | Named inputs with C16's parameter types and defaults. Every SPARQL cell below binds them by variable name. | 1 |
| Ask | A question. Running it calls C18's Ask flow and stores the drafted query with the question, the provider and model. Later runs reuse the stored query and do not call a model unless the person presses **Ask again**. | 2 |
| Chart | A chart of a named earlier cell's result: type (bar, line, area, point, table summary), x, y, series and aggregation. | 2 |
| Map | A map of an earlier cell's geometries, as the query page's map view draws them. | 2 |

### 2.5 Outputs

Results are not stored by default: a pinned notebook reruns to the same results. A cell
can **pin its output** (Phase 2), which stores up to 1,000 rows of its result as JSON with
the commit it read, for notebooks shared with readers who should not rerun queries or
whose pinned state may expire. A stored output is shown with "stored output from commit
318".

### 2.6 Empty and error states

| Case | What the page shows |
|---|---|
| No notebooks | "No notebooks on this branch" and **New notebook**. |
| A failed cell | The error under the cell, as the query page shows it. **Run all** stops there. |
| A parameter missing | The SPARQL cell names the parameter and does not run. |
| No Ask provider | The Ask cell shows its stored query, and **Ask again** is disabled with the reason. |
| A merge conflict on a cell | The merge page shows the cell's source as the conflicting value. |

### 2.7 Accessibility

Cells are a list of regions, each labelled by kind, number and title. Keyboard
shortcuts have buttons too. Charts have a data table alternative one toggle away and a
text summary ("bar chart, 6 bars, largest July with 9"). Focus stays on a cell after it
runs, and a polite live region announces "cell 1 ran, 14 rows".

## 3. Storage

### 3.1 Decision: RDF in a reserved named graph per notebook

A notebook is RDF in its own named graph, `urn:x-sparkles:notebook:{name}`.

| Option | History and branches | Merges | Grants | Agents | Cost |
|---|---|---|---|---|---|
| **RDF graph per notebook** (chosen) | F06 and F09 as for any data | Per cell and property, with F09's conflict report | C12 graph grants per notebook | SPARQL and MCP read it directly | Queries over `union` must leave the graphs out (§3.3) |
| One JSON literal per notebook | Yes | Every concurrent edit conflicts on the one literal | Per graph, if each has one | Must parse the JSON | Diffs show the whole document as changed |
| A file in the dataset directory, as `queries.json` | No: C16's files are not per branch and have their own versions | None | Dataset-level only | Through new routes only | No commit history |

The RDF form gives line-level diffs of each cell's source in history, conflicts only when
two people change the same cell, and per-notebook grants. Its cost is the reserved graphs,
which must not leak into queries over the data.

### 3.2 Vocabulary

`nb:` is `urn:x-sparkles:notebook#`. The notebook node and its graph share the IRI.

```turtle
GRAPH <urn:x-sparkles:notebook:team-churn> {
  <urn:x-sparkles:notebook:team-churn> a nb:Notebook ;
      nb:title "Team churn" ; nb:at 318 ; nb:formatVersion 1 .
  <urn:x-sparkles:notebook:team-churn#c-5f2a> a nb:Cell ;
      nb:notebook <urn:x-sparkles:notebook:team-churn> ;
      nb:kind nb:Sparql ; nb:position "a0" ; nb:label "left" ;
      nb:source "SELECT ?person ?time WHERE { … }" .
  <urn:x-sparkles:notebook:team-churn#c-77b1> a nb:Cell ;
      nb:notebook <urn:x-sparkles:notebook:team-churn> ;
      nb:kind nb:Chart ; nb:position "a1" ;
      nb:config "{\"of\":\"left\",\"type\":\"bar\",\"x\":\"time\",\"y\":\"count\"}"^^rdf:JSON .
}
```

- **Order.** `nb:position` is a string compared by code point, a fractional index: a cell
  inserted between `a0` and `a1` gets `a0V`. Two people inserting between the same cells
  on different branches get different positions, or the same position, which ties break
  by cell IRI. Moving a cell changes one triple, so it merges cleanly with edits to other
  cells.
- **Ids.** Cell IRIs end in a random id, never reused.
- **Configuration.** Parameters, chart and map configuration and stored outputs are
  `rdf:JSON` literals, validated against a schema per kind at save.
- **Limits.** At most 500 cells, 256 KiB of source per cell and 4 MiB per notebook,
  stored outputs included.

### 3.3 Keeping notebooks out of the data

The notebook graphs hold text, not data, and must not change query results or schema
reports. The server treats graphs whose IRI starts with `urn:x-sparkles:notebook:` as
system graphs:

- they are left out of `graph=union`, `urn:x-arq:UnionGraph`, `FROM` of the union, the
  default graph under `--union-default-graph`, schema reports, full-text and vector
  indexes, write-time validation's data graph and RDFS or OWL reasoning inputs;
- they are readable by naming them in `GRAPH <…>` or `?graph=`, and listed by
  `GET /$/notebooks/{ds}`;
- C12 grants may name them like any graph, and a grant pattern `urn:x-sparkles:notebook:*`
  covers all notebooks.

This needs one predicate in the engine's graph selection, `is_system_graph(iri)`, used
where the union is built. Whether other reserved graphs should become system graphs too
is open (§11).

## 4. Server and API

| Method | Path | Needs | Description |
|---|---|---|---|
| GET | `/$/notebooks/{ds}` | `read` on the notebook's graph | The notebooks the caller may read, with title, `at`, cell count and the last commit that changed each. `branch=` and `at=` apply. |
| GET | `/$/notebooks/{ds}/{name}` | `read` | The notebook as JSON (`NotebookJson`, below), or as `application/x-ipynb+json` and `text/markdown` in Phase 3. `branch=` and `at=` apply. The answer carries the entity tag of the commit that last changed the graph. |
| PUT | `/$/notebooks/{ds}/{name}` | `write` on the graph | Replaces the notebook with the JSON body as one commit, with `message`. The server computes the triple-level change, so unchanged cells produce no change. `If-Match` with the entity tag from `GET` refuses a save over a newer version with `412`, and the page then offers to reload or to save on a new branch. A body with an update in a SPARQL cell is `400`. |
| DELETE | `/$/notebooks/{ds}/{name}` | `write` | Clears the graph as one commit. |
| POST | `/$/notebooks/{ds}/{name}/run` | `read` | Runs cells server-side: `{cells?: [id], params?: {…}, at?}`. Answers each cell's result in the C16 run format, under the caller's budgets. Used by MCP and by clients without a browser. |

```ts
type NotebookJson = {
  name: string; title: string; at: number | null; formatVersion: 1;
  cells: { id: string; kind: "markdown" | "sparql" | "parameters" | "ask" | "chart" | "map";
           label?: string; title?: string; source?: string; config?: unknown;
           output?: { commit: number; result: unknown } }[];   // in position order
};
```

The JSON form is a view of the triples, so a client never writes positions: the server
assigns them from the order of `cells`, keeping the positions of cells whose relative
order did not change. All routes are added to `docs/API.md`, the route table and the
OpenAPI description.

### 4.1 MCP (Phase 2)

- Tools: `list_notebooks`, `read_notebook` (cells as Markdown with fenced SPARQL, the
  form agents read most easily), and `run_notebook` (`cells`, `params`), which returns
  each cell's result with the commit it read. `write_notebook` takes `NotebookJson` and is
  offered only when C11's update tool is enabled, with the same opt-in.
- Resources: `sparkles://{ds}/notebooks/{name}` with the Markdown form.
- Stored queries remain the MCP tool surface for single queries. A notebook's cells are
  not tools.

### 4.2 Export and import (Phase 3)

- **Jupyter.** nbformat 4. Markdown cells become `markdown` cells. SPARQL cells become
  `code` cells whose `source` is the query, with `metadata.sparkles` holding the kind,
  label and configuration, and the notebook's `metadata.kernelspec` naming `sparql` with
  language `sparql`, so a Jupyter kernel for SPARQL can open it. Parameters, Ask, chart
  and map cells become `code` cells whose source is their configuration as JSON, marked
  in `metadata.sparkles`. Stored outputs become `execute_result` outputs with a
  `text/html` table and `application/json` data. Import reads notebooks with
  `metadata.sparkles` back losslessly, and a plain notebook's markdown and code cells as
  Markdown and SPARQL cells.
- **Markdown.** Headings and text as written, SPARQL cells as fenced `sparql` blocks with
  the label in the info string, other cells as fenced `json` blocks.

## 5. Architecture

| Part | Where |
|---|---|
| Vocabulary, JSON to triples and back, positions | `sparkles` crate: a `notebooks` handle module (P06), so bindings get it too |
| System graphs | `sparkles-core` dataset view and union construction, schema reports, index builders, reasoner input selection |
| Routes, run | `sparkles-server` `http` (new `notebooks` module) |
| MCP tools and resource | `sparkles-server/src/mcp` |
| Notebook page and list | `ui/src/routes/notebooks/+page.svelte`, `ui/src/routes/notebooks/[name]/+page.svelte` (new) |
| Cells | `ui/src/lib/components/NotebookCell.svelte`, `ChartCell.svelte` (new), with the existing `SparqlEditor`, `ResultTable`, `GraphView`, `ResultMap` and `AskBar` |
| Model and parameters | `ui/src/lib/notebooks.ts` (new), reusing `lib/stored-queries.ts` for parameter types |
| Charts, Markdown | Observable Plot and micromark, new dependencies (§9) |
| Export | `sparkles` crate for Jupyter and Markdown, so the CLI gets `sparkles notebook export` |

## 6. Phases

1. **Phase 1.** Storage and system graphs, the routes, the notebook list and page with
   Markdown, SPARQL and parameter cells, pinning, saving with messages and `If-Match`.
2. **Phase 2.** Ask, chart and map cells, stored outputs, the MCP tools and resource,
   and U01's slider on a notebook's history.
3. **Phase 3.** Jupyter and Markdown export and import, `sparkles notebook` commands.

## 7. Acceptance examples

- **A1. Create and save.** Creating notebook `churn` with a Markdown cell and a SPARQL cell
  and saving with message "start" makes one commit whose diff adds triples only in graph
  `urn:x-sparkles:notebook:churn`.
- **A2. Kept out of the data.** After A1, `SELECT (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p
  ?o } }` over the union, `/$/schema/{ds}?graph=union` and a dataset with
  `--union-default-graph` report the same counts as before A1.
- **A3. Pinning.** Pinning `churn` at commit 10, then adding data at commit 11, a run shows
  the results of commit 10 and the caption "ran at commit 10".
- **A4. Parameters.** A parameters cell with `team` of type `iri` and default
  `ex:commerce` binds `?team` in the SPARQL cell. A value `Ann" } UNION { ?s ?p ?o } #`
  for a `string` parameter is one literal and matches nothing, as in C16.
- **A5. Branch merge.** On branch `b1` one person edits cell 1's source; on `b2` another
  moves cell 2 above cell 1. Merging both into `main` gives no conflict, and the result
  has the edit and the new order. Editing cell 1 on both branches gives one conflict on
  that cell's `nb:source`.
- **A6. Grants.** A caller whose grants exclude `urn:x-sparkles:notebook:private-*` does not
  see notebook `private-1` in the list, and `GET` answers `404`.
- **A7. Concurrent save.** Two pages save the same notebook. The second gets `412` and is
  offered reload or a new branch.
- **A8. MCP (Phase 2).** `read_notebook` returns the Markdown form with a fenced `sparql`
  block, and `run_notebook` returns the SPARQL cell's rows with the commit read.
- **A9. Jupyter round trip (Phase 3).** Exporting `churn` and importing the file as
  `churn2` gives an equal `NotebookJson` apart from the name, and the exported file
  validates against nbformat 4's JSON schema.

## 8. Testing

- **Rust.** JSON to triples and back, position assignment under reordering, limits,
  system graph exclusion from each place listed in §3.3, merges of A5 with F09, MCP
  tools, nbformat validation against the schema from the `nbformat` repository as test
  data.
- **Vitest.** `lib/notebooks.ts`: cell order, unsaved change tracking, parameter
  binding, chart configuration checks.
- **Playwright.** A1, A3, A4, A7 against a real server, and A5 through the merge page.

## 9. Dependencies

Markdown cells use micromark (MIT) with its GFM extensions (MIT). micromark follows
CommonMark and does not pass raw HTML through unless asked to, so cell text cannot inject
markup. Links get `rel="noopener noreferrer"`, and images are not loaded from other
origins unless the person allows it per notebook.

Chart cells use Observable Plot (`@observablehq/plot`, ISC license), which depends on d3
modules (ISC). Plot draws SVG from a declarative specification, which matches the chart
cell's configuration, and it is loaded only on the notebook page. The alternatives were
drawing charts by hand in SVG, which is enough for bars and lines but grows with every
chart kind, and Vega-Lite (BSD-3-Clause), whose runtime is larger.

## 10. Rejected alternatives

- **Notebooks as files beside `queries.json`.** They would not follow branches, and their
  history would need its own store.
- **One JSON literal per notebook.** Every concurrent edit would conflict, and diffs would
  show the whole notebook.
- **Storing results by default.** Results can be large and would grow the dataset with
  every run. Pinning to a commit makes reruns reproducible.
- **A code kernel.** Running user code in the server is out of scope, and SPARQL plus
  declarative cells covers the analyses this spec targets.

## 11. Open questions

1. Whether the system-graph rule of §3.3 should become a configurable list, for other
   graphs that hold metadata rather than data. Today only notebooks would use it.
2. Whether a notebook may span datasets, with a dataset per cell.
3. Whether **Run all** should run on the server through `/run`, so a notebook can run
   from the CLI and a cron-like schedule with the same results as the browser.

## Outcome

Not built. See [ROADMAP](../ROADMAP.md) item 30.
