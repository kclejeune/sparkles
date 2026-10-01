# Sparkles web UI

The Sparkles web UI is a SvelteKit 2 and Svelte 5 single-page app for the Sparkles
RDF/SPARQL server. It is built as static files (`build/`) that the Rust server embeds and
serves under `/ui/`, and `/` redirects to `/ui/`. Datasets live at the server root
(`/{ds}/sparql`, `/$/…`), so the UI stays under its own prefix.

[`docs/API.md`](../docs/API.md) defines the HTTP contract.

## Pages

| Route                    | What it does                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| ------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `/ui/query`              | A SPARQL editor (CodeMirror 6). Tabs are saved to localStorage, and Ctrl/Cmd+Enter runs the query. The editor has examples and autocomplete for keywords, functions, variables, prefixes and prefixed names from the dataset's vocabulary, and it inserts `PREFIX` declarations automatically. Updates (`INSERT`/`DELETE`/`LOAD`/…) go to `/{ds}/update`. Results have four views. **Table** is virtualized for 100k+ rows, with sortable, resizable columns, cell copy, and IRIs that open in Explore. **Graph** uses Cytoscape and fcose; it draws CONSTRUCT and DESCRIBE results directly, and SELECT results through subject, predicate and object column pickers. **Plan** shows the operator tree and a flame view, with estimated and actual rows, self and total time, cached badges and the slowest nodes highlighted. **Raw** shows the JSON and downloads the full result as CSV, TSV, JSON or XML, or as Turtle, N-Triples, JSON-LD or RDF-XML. **Explain** shows the SSE algebra and the unexecuted plan. Parse errors highlight their line and column in the editor. The result bar shows the commit a result was read at (`meta.commit`). Updates ask for a receipt and show the commit they produced ("commit 42 · +2 −0"), or that nothing was committed.                                                                |
| `/ui/explore?ds=…&iri=…` | A resource graph explorer. Search by label or IRI, and double-click a node (or use **Expand**) to load 100 outgoing and 100 incoming edges. A side panel lists all properties and back-references. Vector literals (`urn:x-sparkles:vector`) show as `vector(384) [0.12, −0.03, …]`; click one for the full value. For entities with vector properties, **Similar** lists the top-k nearest other entities by `spk:vectorSearch`, with pickers for the predicate, k and the metric (cosine, dot, euclidean). The **Text search** tab (`&tab=search&q=`) lists ranked `text:query` hits with the matched literal, predicate and graph. An unescaped `:` in the query is sent as `\:`. The **Schema** tab (`&tab=schema`) is an ontology browser over the schema report (`/$/schema/{ds}`, all pages of one snapshot). It has a graph selector and an inference toggle, the `rdfs:subClassOf` hierarchy with instance counts, `undeclared` tags and cycle markers, and class details. Its property list shows exact counts, object kinds, datatypes and languages next to the declared domains and ranges.                                                                                                                                                                                                                                  |
| `/ui/datasets`           | The dataset list, with each dataset's head commit. Create a dataset (a name, persistent or in-memory), delete one after typing its name to confirm, and see recent tasks.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `/ui/datasets/{name}`    | Stats for the dataset: quads, terms, graphs, base vs delta, disk, cache hit rate, and the top predicates and classes. Upload files by drag and drop, with progress, an optional target graph and the commit the upload produced. Compact, dump N-Quads, run reasoning (RDFS, OWL 2 RL or custom rules), drop inferences and watch a live task list. **History** lists commits newest first (kind, time, net change, quads after, bulk and inexact markers), paged, with the dataset id and the source of a clone. **Full-text search** shows the index status, and enables or reconfigures the index (all predicates or a list), rebuilds it or disables it. **Backups** lists this dataset's backups in all repositories, with lineage and verify badges, and has **Back up now** (repository, name, note) and **Restore…**.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `/ui/backups`            | Backup repositories, for server admins. Dataset admins see their datasets' backups and a short repository list. **Backups** lists every backup with its commit, sizes, dedup ratio and last verify, with filters for repository, dataset, policy and search. Each backup can be restored, verified or deleted (after typing to confirm), and a details drawer shows the manifest's files. **Repositories** shows a card for each repository, with its location, badges (read-only, single writer, http), reachability, sizes and dedup. A repository can be added or edited with type-specific fields and credential sources (never secrets), and a connection test. The tab also covers verify, garbage collection (dry run first) and locks. **Policies** lists each policy's schedule, next and last run and failures, with an enable switch, run now and a retention preview. The policy editor has dataset globs with live matches, schedule presets with the next five runs from the server, a name template sample and a retention sentence. **Activity** shows backup tasks with progress and Cancel, the policy run history and garbage collection reports. The restore dialog walks through the backup, the target, identity and check options. The target is a new dataset, or an existing one replaced after typing its name. |
| `/ui/server`             | Version, start time, uptime, endpoints and all tasks.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |

## Develop

You need Node 22+ and pnpm.

```sh
pnpm install

# Option A: against the bundled mock backend (no Rust needed)
pnpm mock            # http://localhost:3030 — real SPARQL via oxigraph (WASM), fake plans/timings/stats
pnpm dev             # http://localhost:5173/ui/

# Option B: against a real Sparkles server
SPARKLES_API=http://localhost:3030 pnpm dev
```

The Vite dev and preview servers proxy everything outside `/ui/` to `SPARKLES_API`
(default `http://localhost:3030`). That covers the `/$/…` admin API and every `/{ds}/…`
endpoint.

The mock (`mock/server.mjs`) loads a small FOAF-style organisation graph with an OWL
ontology (`mock/data.mjs`) into a `foaf` dataset, plus a tiny `scratch` dataset. It
implements every endpoint in API.md. Queries and updates are evaluated for real, uploads
parse Turtle, N-Triples, N-Quads, TriG and RDF-XML, reasoning materializes a few RDFS/OWL
rules into `urn:x-sparkles:inferred`, and tasks progress over a couple of seconds. Writes
are committed with receipts. The `foaf` history is seeded, and its oldest commits are no
longer retained. The mock answers `text:query` and `spk:vectorSearch` calls from the call
alone: it understands a subset of the text syntax and ignores the rest of the query.
`foaf` has `ex:embedding` vectors on people and publications. `PORT=4000 pnpm mock`
changes the port, `MOCK_LATENCY=300 pnpm mock` adds latency to every response, and
`MOCK_VECTOR_BUDGET=1000` makes vector searches exceed their budget (507).

`mock/backups.mjs` mocks backup repositories statefully, in memory. There are four
repositories: `local`, `s3-main` from the config file, the read-only `dr-source` with
backups of a `wiki` dataset this server does not have, and the unreachable `minio-lab`.
There are backups of `foaf`, one of them from an older lineage and one whose last verify
failed. There are three policies with run history, stale locks and GC candidates. Backup,
restore, verify, GC and policy tasks queue for two slots, progress over a few seconds and
can be cancelled (`DELETE /$/tasks/{id}`). Restores create or replace mock datasets. A
repository at `/mnt/offline/…`, or the bucket `no-such-bucket`, fails its connection
test, and `/srv/not-a-repo` is `not-a-repository`. `MOCK_ROLE=dataset-admin pnpm mock`
answers `/$/whoami` as a signed-in admin of `foaf` only, which shows the reduced Backups
view.

`mock/geo.mjs` mocks GeoSPARQL. Its `places` dataset holds Paris sights and two cities,
in CRS84, EPSG:4326, GeoJSON, a UTM zone and one malformed literal, with the spatial index
on. The mock answers the index status and `geo-index` tasks (`/$/geo/{ds}`),
`GET /{ds}/geo`, `POST /$/geo/convert` and `spatial:nearbyGeom` queries, which use
distances between centroids.

The maps use MapLibre GL JS over the bundled Natural Earth basemap (`src/lib/basemap/`,
with its source and license). MapLibre loads only when a map opens. The Playwright
configurations run the full Chromium in headless mode (`channel: 'chromium'`), because
the headless shell has no WebGL.

## Check and build

```sh
pnpm check     # svelte-check, must report 0 errors
pnpm test      # Vitest unit tests for the pure modules in src/lib (*.test.ts)
pnpm e2e       # Playwright end-to-end tests (tests/e2e) against a real server, see below
pnpm build     # writes build/ (index.html + /ui/_app/… assets)
pnpm preview   # serves build/ at http://localhost:4173/ui/ with the same proxy
```

## Formatting in the browser

In the query editor and the shapes editor, Format runs the formatter in the browser when
the build has its WebAssembly module, and calls `POST /$/format` otherwise. The module is
optional. `mise run ui:wasm`, run from the repository root, builds
`crates/sparkles-fmt-wasm` into `src/lib/wasm/` (git-ignored) with
`scripts/build-fmt-wasm.sh`. That script needs the `wasm32-unknown-unknown` target, which
`rust-toolchain.toml` lists, and the wasm-bindgen CLI at the version in `Cargo.lock`,
which the task installs. Once the module exists, `mise run ui:build` rebuilds it whenever
the formatter changes. Delete the directory to build without it. The Nix `sparkles-ui`
package always builds it in (`nix/fmt-wasm.nix`).

`lib/fmt-wasm.ts` loads the module the first time something is formatted. The module is a
hashed asset under `/ui/_app/immutable/`, served compressed (about 250 KB with brotli).
It takes the endpoint's JSON and answers in the same shape, errors included, so the pages
handle both paths the same way. If the module does not load, or fails while running, the
request goes to the endpoint. The pages' Content Security Policy includes
`'wasm-unsafe-eval'` for it. `VITE_FMT_WASM=off` leaves the module out of a build or of
`vite dev`. The mock tests below set it, so that the mock's stand-in formatter answers.

## End-to-end tests

`tests/e2e` holds Playwright smoke tests of the UI against a real `sparkles serve`.
`global-setup.ts` starts `../target/debug/sparkles` (or `$SPARKLES_BIN`) on a free port of
127.0.0.1, with a temporary data directory and an auth configuration. The configuration
has a user `alice` with a password and a static server-admin API token. The setup loads a
small dataset (`data.ts`) in two commits, enables full-text search and signs `alice` in.
The dataset has labels, comments for full-text search and `spk:vector` embeddings for
Similar. The teardown stops the server and deletes the directory. `SPARKLES_E2E_KEEP=1`
keeps the directory, with `server.log`. A debug binary reads `build/` from disk, so run
`pnpm build` first. The tests cover signing in with a password and with a token minted on
the tokens page, Bearer tokens on the SPARQL endpoint, the query page, Explore, Similar,
text search and the dataset history.

`@playwright/test` is pinned to the version of the flake's `playwright-driver`, and the
dev shell provides that version's Chromium (`PLAYWRIGHT_BROWSERS_PATH`). Without Nix,
`pnpm exec playwright install chromium` downloads one. `mise run ui:e2e` does all of
this.

On Linux, the flake check `ui-e2e` (`nix/ui-e2e.nix`) runs the same tests hermetically in
the build sandbox. It is part of `nix flake check`; to run it alone, use
`nix build .#checks.x86_64-linux.ui-e2e -L`. In the sandbox, `SPARKLES_BIN` is the flake's
`sparkles` package, a release build that embeds the UI, so no `build/` is needed. The
dependencies come from the `sparkles-ui` package's pnpm store, which has the same hash and
is refreshed with it when the lockfile changes. Chromium comes from nixpkgs'
`playwright-driver`, with a fontconfig that lists DejaVu, because the sandbox has no
system fonts. With `CI` set, as in the sandbox, a failed test is retried once and reported
as flaky, and `test.only` fails the run.

`tests/mock` tests the pages whose server side is not built yet, the Backups area,
against the mock instead. `pnpm e2e:mock` (or `mise run ui:e2e:mock`) uses
`playwright.mock.config.ts`. It starts `mock/server.mjs` and `vite dev` on free ports
(`MOCK_E2E_PORT` and the port after it) and runs the tests in order in one worker,
because they share the mock's state. The tests cover:

- the backup table and the details drawer;
- adding a repository, with a failing and then a passing connection test;
- backing up from the dataset page and restoring into a new dataset;
- replacing a dataset (lost commits, typed name, identity);
- the policy editor's schedule preview, name template and retention sentence;
- a GC dry run, then a real run;
- cancelling a task from Activity.

Once the backup endpoints exist, these tests move to `tests/e2e`. `geo.spec.ts` covers
the Map tab, the explorer's Nearby and the Spatial index panel against the mock's
`places`.

## Serving from the Rust server

- Serve `ui/build/` at `/ui/`. Unknown paths under `/ui/` must fall back to
  `build/index.html` for SPA routing (for example `/ui/datasets/foaf`).
- Redirect `/` to `/ui/`.
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
