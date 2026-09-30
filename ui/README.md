# Sparkles web UI

SvelteKit 2 + Svelte 5 single-page app for the Sparkles RDF/SPARQL server.
It is built as static files (`build/`) that the Rust server embeds and serves
under **`/ui/`**, redirecting `/` → `/ui/`. Datasets live at the server root
(`/{ds}/sparql`, `/$/…`), which is why the UI stays under its own prefix.

The HTTP contract is [`docs/API.md`](../docs/API.md).

## Pages

| Route | What it does |
|---|---|
| `/ui/query` | SPARQL editor (CodeMirror 6) with tabs saved to localStorage, Ctrl/Cmd+Enter to run, examples, autocomplete (keywords, functions, variables, prefixes, prefixed names from the dataset's vocabulary) and automatic `PREFIX` insertion. Updates (`INSERT`/`DELETE`/`LOAD`/…) go to `/{ds}/update`. Results: virtualized **Table** (100k+ rows, sortable, resizable columns, copy cell, IRIs open in Explore), **Graph** (Cytoscape + fcose; CONSTRUCT/DESCRIBE directly, SELECT via subject/predicate/object column pickers), **Plan** (operator tree and flame view with estimated vs actual rows, self/total time, cached badges, slowest nodes highlighted), **Raw** (JSON, plus full-result downloads as CSV/TSV/JSON/XML or Turtle/N-Triples/JSON-LD/RDF-XML). **Explain** shows the SSE algebra and the unexecuted plan. Parse errors highlight the line/column in the editor. |
| `/ui/explore?ds=…&iri=…` | Resource graph explorer: search by label or IRI, double-click (or **Expand**) to load 100 outgoing + 100 incoming edges, side panel with all properties and back-references. **Schema** tab (`&tab=schema`) is an ontology browser: `rdfs:subClassOf` hierarchy with instance counts, property list with domains/ranges, class details. |
| `/ui/datasets` | Dataset list, create (name + persistent/in-memory), delete with typed confirmation, recent tasks. |
| `/ui/datasets/{name}` | Stats (quads, terms, graphs, base vs delta, disk, cache hit rate, top predicates and classes), drag-and-drop upload with progress and optional target graph, compact, backup, reasoning (RDFS / OWL 2 RL / custom rules), drop inferences, live task list. |
| `/ui/server` | Version, start time, uptime, endpoints, all tasks. |

## Develop

Requires Node 22+ and pnpm.

```sh
pnpm install

# Option A: against the bundled mock backend (no Rust needed)
pnpm mock            # http://localhost:3030 — real SPARQL via oxigraph (WASM), fake plans/timings/stats
pnpm dev             # http://localhost:5173/ui/

# Option B: against a real Sparkles server
SPARKLES_API=http://localhost:3030 pnpm dev
```

The Vite dev/preview server proxies everything outside `/ui/` (the `/$/…`
admin API and every `/{ds}/…` endpoint) to `SPARKLES_API`
(default `http://localhost:3030`).

The mock (`mock/server.mjs`) loads a small FOAF-style organisation graph with
an OWL ontology (`mock/data.mjs`) into a `foaf` dataset plus a tiny `scratch`
dataset. It implements every endpoint in API.md: queries and updates are
evaluated for real, uploads parse Turtle/N-Triples/N-Quads/TriG/RDF-XML,
reasoning materializes a few RDFS/OWL rules into `urn:sparkles:inferred`, and
tasks progress over a couple of seconds. `PORT=4000 pnpm mock` changes the
port; `MOCK_LATENCY=300 pnpm mock` adds latency to every response.

## Check and build

```sh
pnpm check     # svelte-check, must report 0 errors
pnpm build     # writes build/ (index.html + /ui/_app/… assets)
pnpm preview   # serves build/ at http://localhost:4173/ui/ with the same proxy
```

## Serving from the Rust server

- Serve `ui/build/` at `/ui/`; unknown paths under `/ui/` must fall back to
  `build/index.html` (SPA routing, e.g. `/ui/datasets/foaf`).
- Redirect `/` → `/ui/`.
- Everything under `/ui/_app/immutable/` is content-hashed and can be cached forever.

## Layout

```
src/
  app.css                  design tokens (light/dark), base controls
  lib/api.ts               typed client for docs/API.md
  lib/app.svelte.ts        global state: datasets, current dataset, prefixes, theme, ping, toasts
  lib/rdf.ts               prefix shortening, term formatting, query-kind/update detection, PREFIX insertion
  lib/sparql-lang.ts       CodeMirror SPARQL tokenizer, highlighting, completion, error-line decoration
  lib/explore.ts           SPARQL used by Explore and the schema browser
  lib/graph.ts             triples → Cytoscape elements
  lib/components/          SparqlEditor, ResultTable, GraphView, PlanView, ClassTree, TaskList, Modal, …
  routes/                  query, explore, datasets, datasets/[name], server
mock/                      dev-only mock backend
```
