# Features

What Sparkles implements, area by area, and what it does not do yet. The HTTP details
are in [API.md](API.md), the command line in [USAGE.md](USAGE.md), and how each gap
compares with Jena/Fuseki, QLever, Fluree and Oxigraph in [COMPARISON.md](COMPARISON.md).
The design specs in [specs/](specs/README.md) explain why each larger feature is built the
way it is and how it was implemented.

Legend: ✅ done and tested · 🚧 in progress · ⏳ planned · ❌ out of scope for v1

* [Storage (TDB2 equivalent)](#storage-tdb2-equivalent)
* [SPARQL (ARQ equivalent)](#sparql-arq-equivalent)
* [Server (Fuseki equivalent), reasoning, validation, UI](#server-fuseki-equivalent-reasoning-validation-ui)
* [Formatter and editor support](#formatter-and-editor-support)
* [Known gaps](#known-gaps)

## Storage (TDB2 equivalent)

| Feature | Status |
|---|---|
| 64-bit tagged ids, inline `xsd:integer` / `xsd:decimal` / `xsd:double` / `xsd:boolean` / `xsd:dateTime` / `xsd:date` (canonical forms only) | ✅ |
| Bulk write path: large update/inference batches are merged into a rebuilt generation (atomic `CURRENT` switch) | ✅ |
| Sorted, front-coded, mmapped base vocabulary; append-only delta vocabulary | ✅ |
| 7 permutations (SPO SOP PSO POS OSP OPS GSPO), 32k-row compressed blocks | ✅ |
| Parallel bulk loader (Turtle / N-Triples / N-Quads / TriG / RDF/XML / JSON-LD; gzip, zstd, brotli or LZ4 compressed) | ✅ |
| External sort for inputs larger than the memory budget | ✅ |
| Planner statistics (per predicate counts, distinct S/O, classes, graphs) | ✅ |
| MVCC snapshots, single writer (MR+SW), WAL with crash-safe replay | ✅ |
| Durable commit ids: dataset UUID, gap-free commit sequence with timestamps and net counts, receipts on writes, `Sparkles-Commit` headers, commit catalog (`/$/commits`, `sparkles log`) | ✅ |
| Point-in-time reads (`?at=commit:N`, `time:…`, `snapshot:NAME` on queries, explain and Graph Store GET, with Memento headers) and named snapshots that keep a commit readable across compaction; optional retention window (`/$/snapshots`, `/$/history`, `sparkles snapshot`, `query --at`, `dump --at`) | ✅ |
| Compaction into a new generation (`gen-NNNN`, atomic `CURRENT` switch) | ✅ |
| N-Quads backups (`/$/backup`) and dumps (zstd by default, or gzip, brotli, LZ4); compressed request bodies and responses (`zstd`, `br`, `gzip`) | ✅ |
| Read-only integrity check (`sparkles check`, `sparkles::check`): layout, every block of the 7 permutations, cross-permutation consistency, vocabulary order and id ranges, WAL checksums and commit continuity, catalog, full-text segment checksums, the spatial index's `geo.json` and index files; safe next to a running server ([USAGE.md](USAGE.md#checking-a-database)) | ✅ |
| In-memory datasets (same engine, temp-dir base) | ✅ |

## SPARQL (ARQ equivalent)

| Feature | Status |
|---|---|
| Value space: numeric promotion, comparisons, EBV, ORDER BY total order | ✅ |
| SPARQL 1.1 Query: BGP, OPTIONAL, UNION, MINUS, FILTER, BIND, VALUES, subqueries, GROUP BY / aggregates, ORDER BY, DISTINCT, LIMIT/OFFSET, EXISTS | ✅ |
| RDF 1.2 / SPARQL 1.2: triple terms (`<<( s p o )>>`, reification syntax `<< >>`, annotations), base-direction literals (`"x"@en--rtl`), `TRIPLE`/`SUBJECT`/`PREDICATE`/`OBJECT`/`isTRIPLE`/`LANGDIR`/`hasLANG`/`hasLANGDIR`/`STRLANGDIR`, all RDF syntaxes | ✅ |
| Property paths (index-backed BFS for `p*`/`p+`/`p?`, bound-side traversal from join input) | ✅ |
| Function library (SPARQL 1.1 built-ins, XSD casts, selected `fn:` / `afn:` / `math:`) | ✅ |
| SPARQL 1.1 Update (INSERT/DELETE DATA, DELETE/INSERT WHERE, LOAD, CLEAR, DROP, CREATE; ADD/COPY/MOVE) | ✅ |
| SERVICE (federated query, SILENT), under an outbound network policy: public destinations only by default on a server (the local `query` and `update` also reach private ones), allowlists, DNS pinning, checked redirects, timeouts, response ceiling ([USAGE.md](USAGE.md#outbound-requests-service-and-load)) | ✅ |
| Vector similarity: `spk:vector` literals, `spk:cosine`/`dot`/`euclidean`, exact top-k `spk:vectorSearch` scoped to the active graph (no approximate / HNSW index yet) | ✅ |
| Full-text search: Jena `text:query` subset, BM25 via Tantivy, per-quad documents kept current in each commit (staged, committed by the next search or a 1 s tick), graph-scoped top-k (`text` cargo feature, on in the server) | ✅ |
| GeoSPARQL 1.1 functions (`geo` cargo feature, on in the server): `geo:wktLiteral` and `geo:geoJSONLiteral` (Z/M layouts, EMPTY, byte offsets in parse errors), built-in CRSs (CRS84, CRS84h, EPSG:4326/4979 with their latitude-first axes, Web Mercator) and OGC/QUDT/EPSG units, the 24 topological relations and `relate` on DE-9IM, `distance` (geodesic on WGS 84 by default, haversine per dataset, Euclidean for projected CRSs), `buffer` (metric buffers through a local projection), `convexHull`, `envelope`, `boundary`, `centroid`, the four overlay operations, `area`/`length`/`perimeter` (geodesic) and the accessors; see [API.md](API.md#geosparql) | ✅ |
| Spatial index per dataset (`geo.json`, `/$/geo/{ds}`, `sparkles geo-index`, `serve --geo`): packed Hilbert R-tree over a generation's geometry literals plus an overlay of committed writes, exact for every snapshot (MVCC), within a memory budget (`--geo-mb`); status, rows, skipped literals and CRSs; `sparkles_geo_*` metrics | ✅ |
| Spatial index files (`gen-NNNN/geo/`, checksummed, read in place): opening a database or re-enabling the same configuration parses no literal; damaged or foreign files are rebuilt; compactions and bulk loads parse only new literals; never in backups or clones | ✅ |
| W3C Basic Geo (`wgs84_pos:lat`/`long` pairs, `geo.json` `"wgs84": true`) as indexed points for the `spatial:` functions and the map view | ✅ |
| `GET /{ds}/geo?bbox=…`: the indexed geometries in a box as simplified CRS84 GeoJSON, for map views | ✅ |
| Jena `spatial:` property functions (`nearby`, `withinCircle`, `withinBox`, `intersectBox`, cardinal directions, their `…Geom` forms) and spatial FILTERs answered from the index (`SpatialScan`, `SpatialPf` in EXPLAIN, per-operator counters, plan warnings) | ✅ |
| GeoSPARQL `boundingCircle`, `concaveHull`, `isSimple`; the six `geof:agg…` aggregates (GROUP BY, DISTINCT); Jena's 15 `spatialF:` filter functions; the 120 UTM zones (EPSG:326NN/327NN, Krüger's series); `POST /$/geo/convert` (literals as CRS84 GeoJSON for maps); Oxigraph's GeoSPARQL tests (37/44, the rest listed with reasons) and Jena-derived tests; see [API.md](API.md#hulls-aggregates-jena-filter-functions-utm-and-conversion) | ✅ |
| Spatial joins (a GeoSPARQL relation, `relate` or distance bound between the geometries of two parts of a group: an index nested loop or trees packed per query, instead of a cross product) and nearest-neighbour `ORDER BY geof:metricDistance(?w, C) LIMIT k` from the index (`SpatialJoin`, `SpatialKnn` in EXPLAIN, `geo-not-joined`/`geo-not-knn` warnings); see [API.md](API.md#spatial-joins-and-nearest-neighbours) | ✅ |
| GeoSPARQL query rewrite of the 24 topological properties (asserted ∪ derived through default geometries and serializations, opt-in per dataset with `queryRewrite`, `serve --no-geo-rewrite` for the server), Jena's `spatial:equals`, RDFS entailment of the GeoSPARQL and Simple Features vocabulary (`infer --vocab geosparql`) and materialized default geometries (`infer --geo-default-geometry`); see [API.md](API.md#query-rewrite-spatialequals-and-rdfs-entailment) | ✅ |
| Results: JSON, XML, CSV, TSV, `x-sparkles+json` (with executed plan); RDF: Turtle, N-Triples, N-Quads, TriG, JSON-LD, RDF/XML | ✅ |
| W3C conformance: SPARQL 1.1 query **328/328**, SPARQL 1.1 update **157/157**, SPARQL 1.0 **482/482**, SPARQL 1.2 **269/269** (with the vendored, patched `spargebra`, see [`vendor/spargebra/PATCHED.md`](../vendor/spargebra/PATCHED.md)) | ✅ |

## Server (Fuseki equivalent), reasoning, validation, UI

| Feature | Status |
|---|---|
| SPARQL protocol, GSP, upload, `/$/` admin (datasets, stats, compact, backup, tasks), Jena special graphs (`urn:x-arq:DefaultGraph`/`UnionGraph`) | ✅ |
| Jena-style CLI (`load`, `query`, `update`, `dump`, `compact`, `backup`, `repo`, `stats`, `infer`, `shacl`, `shex`, `schema`, `clone`, `check`) that works on the database directory | ✅ |
| Backup repositories (`sparkles-backup`; `backup` cargo feature, on by default): online backups of persistent datasets to a file system or S3 (AWS, MinIO, R2, Ceph RGW) that hold the writer lock only for a few system calls; incremental and deduplicated (content-addressed 32 MiB pieces, only the appended bytes of the WAL and catalog), manifest written last; restore to a new dataset or in place (requests get `503` + `Retry-After` during the swap, never `404`) with dataset-id rules (`auto` / `new` / `keep`) and an integrity check before publishing; verification (`exists`, `data`, `restore`); lifecycle policies (cron or `every` schedules in an IANA time zone, catch-up, retention, optional GC); two-phase GC with a grace period; lease locks judged by the storage server's clock, so several servers and the CLI share a repository; repositories from a config file or registered through the API within operator limits (named credential sources, the outbound policy, `fs` roots); `/$/repositories`, `/$/backups/{ds}`, `/$/backup-policies`, `sparkles repo`, `sparkles backup create\|list\|show\|restore\|verify\|delete\|policy`, a Backups page in the UI, `sparkles_backup_*` metrics and audit events; see [API.md](API.md#backup-repositories) and [USAGE.md](USAGE.md#backup-repositories). Not yet: in-memory datasets, encryption, server-wide backups, running policies offline | ✅ |
| Clone a dataset into an independent sandbox from one snapshot (`POST /$/datasets/{ds}/clone`, `sparkles clone`): same quads and blank-node ids, new dataset id with `forkedFrom`, inferences copied or dropped | ✅ |
| Embedded Rust API (`sparkles::Dataset`) and fluent query builder (`sparkles::querybuilder`); see [USAGE.md](USAGE.md#embedding-the-library) | ✅ |
| RDFS / OWL 2 RL materialization, Jena rule syntax (`sparkles-reasoner`, `/$/reason`, `sparkles infer`) | ✅ |
| Inference freshness: the commit inferences were made at, `stale` / `commitsSince` in `GET /$/reason/{ds}`, dataset info, `/$/stats` and a `Sparkles-Inferences` header; re-run of the recorded profile; opt-in automatic re-runs (`serve --auto-reason`) | ✅ |
| Inconsistency diagnostics: 7 checks from the OWL 2 RL rules with a `false` conclusion (`owl:Nothing` members, disjoint classes, sameAs/differentFrom, functional-property literals, …), `GET /$/reason/{ds}/diagnostics`, `sparkles infer --check`; a subset, never a consistency proof | ✅ |
| SHACL Core + SHACL-SPARQL validation (`sparkles-shacl`): W3C suite **98/98** Core, **20/20** SPARQL; parallel, index-backed | ✅ |
| Write-time SHACL and ShEx validation: every commit's post-state is validated before anything is written (`reject` refuses with `422`, `warn` commits and reports), SHACL shapes from dataset graphs or a file, or a ShEx schema (imports resolved and copied into the database when set) with a query shape map expanded per write, `validation.json` format 2 (format 1 still read), relevance skip (graphs; for ShEx also predicates no shape reads), fail-closed without a guard (`/$/validation/{ds}`, `sparkles validation`, `--no-validate`); `sparkles_validation_*` metrics, `validation` / `validation_ms` access-log fields, the status in CLI summaries and `sparkles stats`; full validation per write (incremental validation is planned) | ✅ |
| Fuseki `/{ds}/shacl` endpoint (`graph=default\|union\|<iri>`, report as Turtle / N-Triples / JSON-LD / JSON, validates data ∪ inferences) and `sparkles shacl` command | ✅ |
| ShEx 2.1 validation (`sparkles-shex`; `shex` cargo feature, on by default): ShExC, ShExJ and ShExR (RDF in any syntax Sparkles reads: `text/turtle`, N-Triples, RDF/XML, TriG, N-Quads; `.ttl`, `.nt`, … files) schemas with imports (inline, `--load-dir` files, http(s) through the outbound policy), `EXTERNAL` shapes, annotations and the Test semantic-action extension; compact and JSON shape maps with `{FOCUS p o}` selectors and `SPARQL """SELECT …"""` selectors (run on the data graph with the query's row and memory budgets, no SERVICE); recursion and negation by stratified greatest-fixed-point typing without deep stacks; parallel, index-backed; shexTest: 100% of the syntax, negative-syntax, negative-structure and representation tests, 100% of the ShExR tests (each `.ttl` schema read as ShExJ, each `.shex` written as the `.ttl` graph), **99.9%** of the validation tests from ShExC, ShExJ and ShExR (the 42 that test blank-node labels are skipped, and one more is listed: the store does not keep them). `POST /{ds}/shex` (a Sparkles extension) with JSON, ShapeMap JSON, compact and text reports, and `sparkles shex validate\|parse` (Jena's flag names as aliases; `parse --out shexr` writes Turtle); see [API.md](API.md#shex-validation); in the UI, the dataset page's Validate panel. Not yet: ShEx 2.2 | ✅ |
| Query result cache controls: `--result-cache-mb`, `nocache=true`, cache stats in `/$/stats`, `POST /$/cache/clear/{ds}` | ✅ |
| Schema discovery (`GET /$/schema/{ds}`, `sparkles schema`, `sparkles::schema`): classes and predicates with exact per-graph counts (triples, distinct subjects/objects, object kinds, datatypes, languages, max objects per subject) kept apart from their RDFS/OWL declarations; subClassOf roots and cycles; cursor pagination bound to one snapshot; time and entry budgets that fail instead of truncating | ✅ |
| MCP server for LLM agents (`sparkles mcp`, stdio; `mcp` cargo feature, on by default): list datasets, describe the schema, run bounded SPARQL (compact table or JSON, truncation announced with the exact total), explain with warnings, describe a resource, list commits, full-text and vector similarity search, SHACL and ShEx validation (counts and the first results, compactly); `atCommit` keeps several calls on one snapshot; engine budgets on every call, SERVICE off, no writes; MCP revisions `2026-07-28`, `2025-11-25` and `2025-06-18`; see [USAGE.md](USAGE.md#mcp-server-llm-agents) | ✅ |
| Observability: `X-Request-Id`, one structured access-log line per request (text or JSON), Prometheus `/$/metrics`, readiness `/$/ready`, graceful drain on SIGTERM | ✅ |
| Per-query budgets (estimated intermediate-result memory, response size, rows) failing with `507`; queries and writes stop when their client disconnects | ✅ |
| OpenTelemetry (`otel` cargo feature, off at run time unless `--otel` or `OTEL_*` enable it): OTLP traces with W3C `traceparent` in and out (SERVICE, LOAD), HTTP/database semantic-convention attributes, query phase and operator-tree spans synthesized from recorded timings, commit and background-task spans; metrics (`http.server.request.duration` plus the Prometheus registry, bridged); optional OTLP logs with trace correlation | ✅ |
| Rate limiting: per-client GCRA buckets and concurrency caps per request class (`auth`, `query`, `update`, `admin`) with per-dataset overrides, signed-in callers counted per owner, trusted-proxy client addresses, `429`/`503` with `Retry-After` and `RateLimit` headers, bounded client tracking that remembers evicted debts, SIGHUP reload that keeps client state and in-flight counts; a `preauth` stage limits failed credential checks per address and IPv6 /48 before any password is hashed, and once spent refuses only password checks and unknown tokens (on by default with auth); off by default otherwise | ✅ |
| Authentication and per-dataset access control (`serve --auth-config`, off by default): levels `read` < `write` < `admin` by dataset name or pattern plus `metrics` / `federate` / `server-admin`, deny by default, hidden datasets answer `404`; HTTP Basic users (argon2id), scoped, expiring, revocable API tokens (`Authorization: Bearer spk_…`, hashed at rest, never above their owner), OIDC sign-in for the UI (native, authorization code + PKCE), trusted forward-auth proxy headers from configured CIDRs or a Unix socket, group-to-role mapping; CSRF and CORS rules for cookies; `sparkles auth login` (browser loopback or device code) and remote `query` / `update` / `load --server`; see [API.md](API.md#authentication-and-access-control) | ✅ |
| SvelteKit UI: datasets, query editor, results table/graph/plan, explorer, server page with readiness, request and cache panels, schema browser on `/$/schema` (graph selection, inference toggle, observed counts and object kinds next to declarations), commit history and write receipts, full-text search (index admin panel, ranked `text:query` search in Explore), vector similarity ("Similar" in the explorer, compact vector literals); embedded in the server binary; Vitest unit tests, Playwright end-to-end smoke tests against a real server (sign-in with a password and an API token, query, explore, Similar, text search, history), and a mock server for UI development | ✅ |
| UI validation: the dataset page's Validate panel switches between SHACL and ShEx (kept per dataset); the ShEx side validates a schema against a shape map (an example built from the dataset's classes, drafts in localStorage) and lists node, shape, status and reason, each row expanding into its failures; ShapeMap JSON download | ✅ |
| UI maps (GeoSPARQL): a Map tab for results with geometry literals (CRS84, EPSG:4326 and Web Mercator read in the browser, other CRSs through `POST /$/geo/convert`, undrawable literals listed, a popup per row), a map card with Nearby (`spatial:nearbyGeom`) in the explorer, and a Spatial index panel on the dataset page (status, CRSs, memory, enable, configure, rebuild, disable, a map of the indexed geometries in view from `GET /{ds}/geo`); MapLibre GL JS, loaded when a map first opens, over a bundled Natural Earth 1:110m basemap: nothing is fetched from elsewhere unless `serve --map-style-url URL` names a MapLibre style, whose origin the pages' Content Security Policy then allows (the style's tiles, glyphs and sprites must come from it too) | ✅ |

## Formatter and editor support

| Feature | Status |
|---|---|
| Formatter (`sparkles-fmt`, `sparkles fmt`, `POST /$/format`; `fmt` cargo feature, on by default) for SPARQL queries and updates, Turtle, TriG, N-Triples, N-Quads and JSON-LD: comment-preserving and self-checking (the output must parse to the same SPARQL algebra, RDF dataset or JSON, keep every comment and format to itself); style options (line width, indentation, prefix groups, `a` shorthand, IRI compaction, quote style, operator position, aligned `VALUES`, `prune-prefixes`, directive style); the endpoint (JSON and raw bodies, the editor's cursor carried across in UTF-16 units, refusals as `422`; see [API.md](API.md#formatting)) and the command line (directory walks with ignore files, config discovery, `--check`, `--write`, `--diff`, Prettier's exit codes; see [USAGE.md](USAGE.md#formatting)) | ✅ |
| Turtle and TriG: `sort` (statements, entries and objects within runs between directives and detached comments), `prune-prefixes`, `turtle-layout = "conventional"` next to the default diff-friendly layout, the ignore pragma; files over `--max-bytes` are formatted statement by statement in bounded memory unless sorted | ✅ |
| N-Triples and N-Quads: canonical term spelling with comments kept, formatted in bounded memory on every core; `--sort` (merging identical statements) with an external sort past `--sort-memory`; `--canonicalize` (RDFC-1.0 blank node labels, sorted canonical form) | ✅ |
| JSON-LD: Prettier's JSON layout, keywords first in a fixed order, `--sort` for terms; duplicate keys and comments refused | ✅ |
| `sparkles lsp`: a language server over stdio for every language `sparkles fmt` formats: formatting and range formatting, syntax errors and the formatter's warnings as diagnostics, options from the nearest `.sparklesfmt.toml`; setups for Neovim, Helix, Emacs and VS Code in [editors.md](editors.md) | ✅ |
| Formatting in the browser: a WebAssembly build of the formatter (`sparkles-fmt-wasm`) used by the UI's query and shapes editors, with `POST /$/format` as the fallback when the build has no module or it fails; optional for `mise run ui:build` (`mise run ui:wasm`), always in the Nix `sparkles` and `sparkles-ui` packages (see [ui/README.md](../ui/README.md#formatting-in-the-browser)) | ✅ |

## Known gaps

Features other RDF stores have that Sparkles does not have yet.
[COMPARISON.md](COMPARISON.md) covers each engine's side, and
[BENCHMARKS.md](BENCHMARKS.md#where-sparkles-loses) the performance side.

* **Scale and execution.** Measured up to 10.5M triples only. Every operator materializes
  its result (bounded by budgets). No lazy, block-wise execution, FSST vocabulary
  compression, IRI encoding, pattern trick, pinned results, materialized views or live
  query monitoring.
* **Inference.** Forward materialization only: no on-the-fly or backward (LP) rules, no
  incremental maintenance (stale inferences are re-run in full), and inconsistency checks
  cover a fixed subset of OWL 2 RL.
* **SPARQL extensions.** None of ARQ's (property functions such as `list:member` and
  `apf:*`, `LET`, custom aggregates, `cdt:` literals, JavaScript functions); common
  `fn:`/`afn:`/`math:` functions only. Plain SERVICE only (no batching or caching).
* **Search.** Full-text search has no highlighting, per-language stemming or multi-field
  documents; vector search is exact (no HNSW index).
* **GeoSPARQL.** No geometry-type entailment, GML/KML literals or EPSG database.
* **Shapes.** No ShEx 2.2; write-time validation is a full validation per write.
* **Formats and change logs.** No RDF Thrift, RDF Protobuf, TriX, RDF/JSON or RDF Patch.
* **Operations.** No graph-level access control, Fuseki metric names, assembler
  configuration or ontology object API. Backups of in-memory datasets, encrypted and
  server-wide backups, and offline policy runs are not there yet.
* **History.** No history queries across commits, diffs, branches or merges.
* **Deployment.** Single node on local disk; no clustering, replicas or object-storage
  data, and no encryption at rest.
* **Embedding.** Rust only: no Python or WebAssembly bindings of the database.
