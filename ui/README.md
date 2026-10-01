# Sparkles web UI

SvelteKit 2 + Svelte 5 single-page app for the Sparkles RDF/SPARQL server.
It is built as static files (`build/`) that the Rust server embeds and serves
under `/ui/`, redirecting `/` → `/ui/`. Datasets live at the server root
(`/{ds}/sparql`, `/$/…`), which is why the UI stays under its own prefix.

The HTTP contract is [`docs/API.md`](../docs/API.md).

## Pages

| Route                    | What it does                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| ------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `/ui/query`              | SPARQL editor (CodeMirror 6) with tabs saved to localStorage, Ctrl/Cmd+Enter to run, examples, autocomplete (keywords, functions, variables, prefixes, prefixed names from the dataset's vocabulary) and automatic `PREFIX` insertion. Updates (`INSERT`/`DELETE`/`LOAD`/…) go to `/{ds}/update`. Results: virtualized **Table** (100k+ rows, sortable, resizable columns, copy cell, IRIs open in Explore), **Graph** (Cytoscape + fcose; CONSTRUCT/DESCRIBE directly, SELECT via subject/predicate/object column pickers), **Plan** (operator tree and flame view with estimated vs actual rows, self/total time, cached badges, slowest nodes highlighted), **Raw** (JSON, plus full-result downloads as CSV/TSV/JSON/XML or Turtle/N-Triples/JSON-LD/RDF-XML). **Explain** shows the SSE algebra and the unexecuted plan. Parse errors highlight the line/column in the editor. The result bar shows the commit a result was read at (`meta.commit`); updates ask for a receipt and show the commit they produced ("commit 42 · +2 −0") or that nothing was committed.                                |
| `/ui/explore?ds=…&iri=…` | Resource graph explorer: search by label or IRI, double-click (or **Expand**) to load 100 outgoing + 100 incoming edges, side panel with all properties and back-references. Vector literals (`urn:x-sparkles:vector`) show as `vector(384) [0.12, −0.03, …]` (click for the full value). **Similar** (entities with vector properties): the top-k nearest other entities by `spk:vectorSearch`, with predicate, k and metric (cosine, dot, euclidean) pickers. **Text search** tab (`&tab=search&q=`): ranked `text:query` hits with the matched literal, predicate and graph; an unescaped `:` in the query is sent as `\:`. **Schema** tab (`&tab=schema`) is an ontology browser over the schema report (`/$/schema/{ds}`, all pages of one snapshot): graph selector and inference toggle, `rdfs:subClassOf` hierarchy with instance counts, `undeclared` tags and cycle markers, property list with exact counts, object kinds, datatypes and languages next to the declared domains/ranges, class details.                                                                                         |
| `/ui/datasets`           | Dataset list (with head commit), create (name + persistent/in-memory), delete with typed confirmation, recent tasks.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `/ui/datasets/{name}`    | Stats (quads, terms, graphs, base vs delta, disk, cache hit rate, top predicates and classes), drag-and-drop upload with progress, optional target graph and the commit it produced, compact, N-Quads dump, reasoning (RDFS / OWL 2 RL / custom rules), drop inferences, live task list. **History**: commits newest first (kind, time, net change, quads after, bulk/inexact markers), paged, with the dataset id and the source of clones. **Full-text search**: index status, enable or reconfigure (all predicates or a list), rebuild, disable. **Backups**: this dataset's backups in all repositories (lineage and verify badges), **Back up now** (repository, name, note) and **Restore…**.                                                                                                                                                                                                                                                                                                                                                                                                      |
| `/ui/backups`            | Backup repositories (server admins; dataset admins see their datasets' backups and a short repository list). **Backups**: every backup with repository, dataset, policy and search filters, commit, sizes, dedup ratio and last verify; restore, verify, delete (typed confirmation) and a details drawer with the manifest's files. **Repositories**: cards with location, badges (read-only, single writer, http), reachability, sizes and dedup; add or edit (type-specific fields, credential sources, never secrets) with a connection test, verify, garbage collection (dry run first), locks. **Policies**: schedule, next and last run, failures, enable switch, run now, retention preview; the editor has dataset globs with live matches, schedule presets with the next five runs from the server, a name template sample and a retention sentence. **Activity**: backup tasks with progress and Cancel, policy run history, garbage collection reports. The restore dialog walks through backup, target (a new dataset, or replacing one after typing its name), identity and check options. |
| `/ui/server`             | Version, start time, uptime, endpoints, all tasks.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |

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
reasoning materializes a few RDFS/OWL rules into `urn:x-sparkles:inferred`, and
tasks progress over a couple of seconds. Writes are committed with receipts
(the `foaf` history is seeded, with its oldest commits no longer retained).
`text:query` and `spk:vectorSearch` calls are answered from the call alone (a
subset of the text syntax; the rest of the query is ignored); `foaf` has
`ex:embedding` vectors on people and publications. `PORT=4000 pnpm mock`
changes the port; `MOCK_LATENCY=300 pnpm mock` adds latency to every response;
`MOCK_VECTOR_BUDGET=1000` makes vector searches exceed their budget (507).

Backup repositories are mocked in `mock/backups.mjs`, statefully and in memory:
repositories (`local`, the config-file `s3-main`, the read-only `dr-source` with backups
of a `wiki` dataset this server does not have, the unreachable `minio-lab`), backups of
`foaf` (one of an older lineage, one whose last verify failed), three policies with run
history, stale locks and GC candidates. Backup, restore, verify, GC and policy tasks
queue for two slots, progress over a few seconds and can be cancelled
(`DELETE /$/tasks/{id}`). Restores create or replace mock datasets. A repository at
`/mnt/offline/…` (or bucket `no-such-bucket`) fails its connection test and
`/srv/not-a-repo` is `not-a-repository`. `MOCK_ROLE=dataset-admin pnpm mock` answers
`/$/whoami` as a signed-in admin of `foaf` only, for the reduced Backups view.

GeoSPARQL is mocked in `mock/geo.mjs`: a `places` dataset (Paris sights and two cities, in
CRS84, EPSG:4326, GeoJSON, a UTM zone and one malformed literal) with its spatial index on,
the index status and `geo-index` tasks (`/$/geo/{ds}`), `GET /{ds}/geo`, `POST
/$/geo/convert` and `spatial:nearbyGeom` queries (distances between centroids).

The maps draw with MapLibre GL JS over the bundled Natural Earth basemap
(`src/lib/basemap/`, with its source and license); MapLibre is loaded only when a map opens.
The Playwright configurations run the full Chromium in headless mode (`channel: 'chromium'`)
rather than the headless shell, which has no WebGL.

## Check and build

```sh
pnpm check     # svelte-check, must report 0 errors
pnpm test      # Vitest unit tests for the pure modules in src/lib (*.test.ts)
pnpm e2e       # Playwright end-to-end tests (tests/e2e) against a real server, see below
pnpm build     # writes build/ (index.html + /ui/_app/… assets)
pnpm preview   # serves build/ at http://localhost:4173/ui/ with the same proxy
```

## Formatting in the browser

Format, in the query editor and the shapes editor, runs the formatter in the browser when
the build has its WebAssembly module, and calls `POST /$/format` otherwise. The module is
optional: `mise run ui:wasm` (from the repository root) builds `crates/sparkles-fmt-wasm`
into `src/lib/wasm/` (git-ignored) with `scripts/build-fmt-wasm.sh`, which needs the
`wasm32-unknown-unknown` target (`rust-toolchain.toml` lists it) and the wasm-bindgen CLI of
the version in `Cargo.lock` (the task installs it). Once it is there, `mise run ui:build`
rebuilds it whenever the formatter changes; delete the directory to build without it. The
Nix `sparkles-ui` package always builds it in (`nix/fmt-wasm.nix`).

`lib/fmt-wasm.ts` loads the module the first time something is formatted (a hashed asset
under `/ui/_app/immutable/`, served compressed: about 250 KB with brotli). It takes the
endpoint's JSON and answers with it, errors included, so the pages handle both the same way.
A module that does not load, or fails while running, hands the request to the endpoint. The
pages' Content Security Policy has `'wasm-unsafe-eval'` for it. `VITE_FMT_WASM=off` leaves
the module out of a build or `vite dev` (the mock tests below set it, so the mock's stand-in
formatter answers).

## End-to-end tests

`tests/e2e` holds Playwright smoke tests of the UI against a real `sparkles serve`:
`global-setup.ts` starts `../target/debug/sparkles` (or `$SPARKLES_BIN`) on a free port of
127.0.0.1 with a temporary data directory and an auth configuration (user `alice` with a
password, a static server-admin API token). It loads a small dataset (`data.ts`: labels,
comments for full-text search, `spk:vector` embeddings for Similar) in two commits, enables
full-text search and signs `alice` in. The teardown stops the server and deletes the
directory (`SPARKLES_E2E_KEEP=1` keeps it, with `server.log`). A debug binary reads `build/`
from disk, so run `pnpm build` first. The tests cover signing in with a password and with a
token minted on the tokens page, Bearer tokens on the SPARQL endpoint, the query page,
Explore, Similar, text search and the dataset history.

`@playwright/test` is pinned to the flake's `playwright-driver` version, whose Chromium the
dev shell provides (`PLAYWRIGHT_BROWSERS_PATH`); without Nix, `pnpm exec playwright install
chromium` downloads one. `mise run ui:e2e` does all of this.

On Linux the flake check `ui-e2e` (`nix/ui-e2e.nix`, part of `nix flake check`; alone:
`nix build .#checks.x86_64-linux.ui-e2e -L`) runs the same tests hermetically in the build
sandbox. There, `SPARKLES_BIN` is the flake's `sparkles` package, a release build that embeds
the UI (so no `build/` is needed). The dependencies are the `sparkles-ui` package's pnpm store
(the same hash, refreshed with it when the lockfile changes), and Chromium comes from
nixpkgs' `playwright-driver` (with a fontconfig listing DejaVu, as the sandbox has no system
fonts). With `CI` set, as in the sandbox, a failed test is retried once and reported as
flaky, and `test.only` fails the run.

`tests/mock` tests pages whose server side is not built yet (the Backups area) against the
mock instead: `pnpm e2e:mock` (or `mise run ui:e2e:mock`) uses `playwright.mock.config.ts`,
which starts `mock/server.mjs` and `vite dev` on free ports (`MOCK_E2E_PORT` and the port
after it) and runs the tests in one worker, in order, since they share the mock's state. They
cover the backup table and details drawer, adding a repository with a failing and a passing
connection test, backing up from the dataset page and restoring into a new dataset,
replacing a dataset (lost commits, typed name, identity), the policy editor's schedule
preview, name template and retention sentence, a GC dry run then a run, and cancelling a
task from Activity. Once the backup endpoints exist they move to `tests/e2e`. `geo.spec.ts`
covers the Map tab, the explorer's Nearby and the Spatial index panel against the mock's
`places`.

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
  lib/fmt-wasm.ts          formatting in the browser (the WebAssembly formatter) or through /$/format
  lib/rdf.ts               prefix shortening, term formatting, query-kind/update detection, PREFIX insertion
  lib/sparql-lang.ts       CodeMirror SPARQL tokenizer, highlighting, completion, error-line decoration
  lib/explore.ts           SPARQL used by Explore; the schema browser's model
  lib/commits.ts           commit and receipt formatting, history paging
  lib/backups.ts           typed client for backup repositories, backups, policies, task cancel
  lib/backups-format.ts    dedup ratios, retention sentences, name templates, globs, presets
  lib/textsearch.ts        text:query building and escaping, hits, error hints
  lib/vectors.ts           spk:vectorSearch building, vector literals, scores
  lib/graph.ts             triples → Cytoscape elements
  lib/components/          SparqlEditor, ResultTable, GraphView, PlanView, ClassTree, TaskList, Modal, …
  routes/                  query, explore, datasets, datasets/[name], backups, server
mock/                      dev-only mock backend
```
