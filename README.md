# Sparkles

A high-performance RDF / SPARQL / OWL database in Rust. It aims to be a
**functional re-implementation of [Apache Jena](https://jena.apache.org/) + Fuseki**
(same protocols, same semantics, same operational model), built on the
**index and execution architecture of [QLever](https://github.com/ad-freiburg/qlever)**.
It also ships a SvelteKit UI for database management, graph visualization and
interactive querying.

* `docs/AUDIT.md` covers the Jena and QLever audits, what Sparkles reuses from Oxigraph, why
  Fluree was not audited, and the language decision (Rust vs. Go).
* `docs/API.md` is the HTTP API contract (Fuseki-compatible, plus `/$/` extensions).

## Philosophy

1. **Jena-compatible where users can see it.** The goal is that anything talking to
   Fuseki keeps working: the SPARQL 1.1 Query/Update/Graph Store protocols, Fuseki
   endpoint names (`/{ds}/sparql|query|update|data|get|upload`), the `/$/` admin API,
   RDF formats, result formats, dataset semantics (default + named graphs, optional
   union default graph), and TDB2's operational model (bulk load, transactions,
   compaction, backups).
2. **QLever-style internals where performance lives.** Terms are dictionary-encoded
   into 64-bit tagged ids with inline literals. Indexes are fully sorted, compressed
   permutation files. Execution is column-at-a-time with a cost-based DP planner.
3. **Reuse the Rust RDF ecosystem.** We don't re-implement parsers that already
   exist: `oxrdf`, `oxttl`, `oxrdfxml`, `oxjsonld`, `spargebra`, `sparesults` and
   `oxsdatatypes` from the Oxigraph project supply the term model, parsers, SPARQL
   algebra and XSD value space. Sparkles provides the storage, planner, executor,
   server, reasoner and UI.
4. **Library first.** `crates/sparkles` is an embeddable engine with no HTTP or async
   dependencies (Jena `core`/`arq`/`tdb2`). The server (`sparkles-server`, the Fuseki
   equivalent) and the reasoner are separate crates built on its public API.

## Layout

| Path | Role | Jena analogue |
|---|---|---|
| `crates/sparkles` | ids, vocabulary, permutation index, bulk builder, store (MVCC + WAL), SPARQL engine, RDF I/O | jena-core, jena-arq, jena-tdb2, jena-db, jena-querybuilder, jena-rdfconnection (in-process) |
| `crates/sparkles-reasoner` | RDFS / OWL 2 RL / Jena rule syntax, semi-naive forward chaining into `urn:x-sparkles:inferred` | jena-core `reasoner` |
| `crates/sparkles-shacl` | SHACL Core + SHACL-SPARQL validation over store snapshots | jena-shacl |
| `crates/sparkles-shex` | ShEx 2.1 validation (ShExC, ShExJ, shape maps) over store snapshots | jena-shex |
| `crates/sparkles-server` | axum HTTP server + `sparkles` CLI | jena-fuseki2, jena-cmds |
| `crates/sparkles-backup` | backup repositories (file system or S3): incremental, deduplicated backups, restore, lifecycle policies | Fuseki `/$/backup` (N-Quads dumps only) |
| `vendor/spargebra` | Oxigraph's SPARQL parser, vendored with fixes (`PATCHED.md`) | ARQ's JavaCC grammar |
| `ui/` | SvelteKit management / query / graph-exploration UI *(in progress)* | jena-fuseki-ui |

## Status

Legend: ✅ done and tested · 🚧 in progress · ⏳ planned · ❌ out of scope for v1

### Storage (TDB2 equivalent)

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
| Read-only integrity check (`sparkles check`, `sparkles::check`): layout, every block of the 7 permutations, cross-permutation consistency, vocabulary order and id ranges, WAL checksums and commit continuity, catalog, full-text segment checksums, the spatial index's `geo.json`; safe next to a running server | ✅ |
| In-memory datasets (same engine, temp-dir base) | ✅ |

### SPARQL (ARQ equivalent)

| Feature | Status |
|---|---|
| Value space: numeric promotion, comparisons, EBV, ORDER BY total order | ✅ |
| SPARQL 1.1 Query: BGP, OPTIONAL, UNION, MINUS, FILTER, BIND, VALUES, subqueries, GROUP BY / aggregates, ORDER BY, DISTINCT, LIMIT/OFFSET, EXISTS | ✅ |
| RDF 1.2 / SPARQL 1.2: triple terms (`<<( s p o )>>`, reification syntax `<< >>`, annotations), base-direction literals (`"x"@en--rtl`), `TRIPLE`/`SUBJECT`/`PREDICATE`/`OBJECT`/`isTRIPLE`/`LANGDIR`/`hasLANG`/`hasLANGDIR`/`STRLANGDIR`, all RDF syntaxes | ✅ |
| Property paths (index-backed BFS for `p*`/`p+`/`p?`, bound-side traversal from join input) | ✅ |
| Function library (SPARQL 1.1 built-ins, XSD casts, selected `fn:` / `afn:` / `math:`) | ✅ |
| SPARQL 1.1 Update (INSERT/DELETE DATA, DELETE/INSERT WHERE, LOAD, CLEAR, DROP, CREATE; ADD/COPY/MOVE) | ✅ |
| SERVICE (federated query, SILENT), under an outbound network policy: public destinations only by default on a server (the local `query` and `update` also reach private ones), allowlists, DNS pinning, checked redirects, timeouts, response ceiling | ✅ |
| Vector similarity: `spk:vector` literals, `spk:cosine`/`dot`/`euclidean`, exact top-k `spk:vectorSearch` scoped to the active graph (no approximate / HNSW index yet) | ✅ |
| Full-text search: Jena `text:query` subset, BM25 via Tantivy, per-quad documents kept current in each commit (staged, committed by the next search or a 1 s tick), graph-scoped top-k (`text` cargo feature, on in the server) | ✅ |
| GeoSPARQL 1.1 functions (`geo` cargo feature, on in the server): `geo:wktLiteral` and `geo:geoJSONLiteral` (Z/M layouts, EMPTY, byte offsets in parse errors), built-in CRSs (CRS84, CRS84h, EPSG:4326/4979 with their latitude-first axes, Web Mercator) and OGC/QUDT/EPSG units, the 24 topological relations and `relate` on DE-9IM, `distance` (geodesic on WGS 84 by default, haversine per dataset, Euclidean for projected CRSs), `buffer` (metric buffers through a local projection), `convexHull`, `envelope`, `boundary`, `centroid`, the four overlay operations, `area`/`length`/`perimeter` (geodesic) and the accessors; see [docs/API.md](docs/API.md#geosparql) | ✅ |
| Spatial index per dataset (`geo.json`, `/$/geo/{ds}`, `sparkles geo-index`, `serve --geo`): packed Hilbert R-tree over a generation's geometry literals plus an overlay of committed writes, exact for every snapshot (MVCC), rebuilt on open and by compaction, within a memory budget (`--geo-mb`); status, rows, skipped literals and CRSs; `sparkles_geo_*` metrics | ✅ |
| Jena `spatial:` property functions (`nearby`, `withinCircle`, `withinBox`, `intersectBox`, cardinal directions, their `…Geom` forms) and spatial FILTERs answered from the index (`SpatialScan`, `SpatialPf` in EXPLAIN, per-operator counters, plan warnings) | ✅ |
| Results: JSON, XML, CSV, TSV, `x-sparkles+json` (with executed plan); RDF: Turtle, N-Triples, N-Quads, TriG, JSON-LD, RDF/XML | ✅ |
| W3C conformance: SPARQL 1.1 query **328/328**, SPARQL 1.1 update **157/157**, SPARQL 1.0 **482/482**, SPARQL 1.2 **269/269** (with the vendored, patched `spargebra`, see `vendor/spargebra/PATCHED.md`) | ✅ |

### Server (Fuseki equivalent), reasoning, validation, UI

| Feature | Status |
|---|---|
| SPARQL protocol, GSP, upload, `/$/` admin (datasets, stats, compact, backup, tasks), Jena special graphs (`urn:x-arq:DefaultGraph`/`UnionGraph`) | ✅ |
| Jena-style CLI (`load`, `query`, `update`, `dump`, `compact`, `backup`, `repo`, `stats`, `infer`, `shacl`, `shex`, `schema`, `clone`, `check`), operating on the database directory directly | ✅ |
| Backup repositories (`sparkles-backup`; `backup` cargo feature, on by default): online backups of persistent datasets to a file system or S3 (AWS, MinIO, R2, Ceph RGW) that hold the writer lock only for a few system calls; incremental and deduplicated (content-addressed 32 MiB pieces, only the appended bytes of the WAL and catalog), manifest written last; restore to a new dataset or in place (requests get `503` + `Retry-After` during the swap, never `404`) with dataset-id rules (`auto` / `new` / `keep`) and an integrity check before publishing; verification (`exists`, `data`, `restore`); lifecycle policies (cron or `every` schedules in an IANA time zone, catch-up, retention, optional GC); two-phase GC with a grace period; lease locks judged by the storage server's clock, so several servers and the CLI share a repository; repositories from a config file or registered through the API within operator limits (named credential sources, the outbound policy, `fs` roots); `/$/repositories`, `/$/backups/{ds}`, `/$/backup-policies`, `sparkles repo`, `sparkles backup create\|list\|show\|restore\|verify\|delete\|policy`, a Backups page in the UI, `sparkles_backup_*` metrics and audit events; see [docs/API.md](docs/API.md#backup-repositories). Not yet: in-memory datasets, encryption, server-wide backups, running policies offline | ✅ |
| Clone a dataset into an independent sandbox from one snapshot (`POST /$/datasets/{ds}/clone`, `sparkles clone`): same quads and blank-node ids, new dataset id with `forkedFrom`, inferences copied or dropped | ✅ |
| Embedded Rust API (`sparkles::Dataset`) and fluent query builder (`sparkles::querybuilder`) | ✅ |
| RDFS / OWL 2 RL materialization, Jena rule syntax (`sparkles-reasoner`, `/$/reason`, `sparkles infer`) | ✅ |
| Inference freshness: the commit inferences were made at, `stale` / `commitsSince` in `GET /$/reason/{ds}`, dataset info, `/$/stats` and a `Sparkles-Inferences` header; re-run of the recorded profile; opt-in automatic re-runs (`serve --auto-reason`) | ✅ |
| Inconsistency diagnostics: 7 checks from the OWL 2 RL rules with a `false` conclusion (`owl:Nothing` members, disjoint classes, sameAs/differentFrom, functional-property literals, …), `GET /$/reason/{ds}/diagnostics`, `sparkles infer --check`; a subset, never a consistency proof | ✅ |
| SHACL Core + SHACL-SPARQL validation (`sparkles-shacl`): W3C suite **98/98** Core, **20/20** SPARQL; parallel, index-backed | ✅ |
| Write-time SHACL validation: every commit's post-state is validated before anything is written (`reject` refuses with `422`, `warn` commits and reports), shapes from dataset graphs or a file, relevance skip, fail-closed without a guard (`/$/validation/{ds}`, `sparkles validation`, `--no-validate`); `sparkles_validation_*` metrics, `validation` / `validation_ms` access-log fields, the status in CLI summaries and `sparkles stats`; full validation per write (incremental validation is planned) | ✅ |
| Fuseki `/{ds}/shacl` endpoint (`graph=default\|union\|<iri>`, report as Turtle / N-Triples / JSON-LD / JSON, validates data ∪ inferences) and `sparkles shacl` command | ✅ |
| ShEx 2.1 validation (`sparkles-shex`; `shex` cargo feature, on by default): ShExC and ShExJ schemas with imports (inline, `--load-dir` files, http(s) through the outbound policy), `EXTERNAL` shapes, annotations and the Test semantic-action extension; compact and JSON shape maps with `{FOCUS p o}` selectors; recursion and negation by stratified greatest-fixed-point typing without deep stacks; parallel, index-backed; shexTest: 100% of the syntax, negative-syntax and negative-structure tests, **99.9%** of the validation tests from ShExC and from ShExJ (the 42 that test blank-node labels are skipped: the store does not keep them). `POST /{ds}/shex` (a Sparkles extension) with JSON, ShapeMap JSON, compact and text reports, and `sparkles shex validate\|parse` (Jena's flag names as aliases); see [docs/API.md](docs/API.md#shex-validation). Not yet: ShExR schemas, ShEx 2.2, SPARQL selectors, write-time ShEx validation, a UI | ✅ |
| Query result cache controls: `--result-cache-mb`, `nocache=true`, cache stats in `/$/stats`, `POST /$/cache/clear/{ds}` | ✅ |
| Schema discovery (`GET /$/schema/{ds}`, `sparkles schema`, `sparkles::schema`): classes and predicates with exact per-graph counts (triples, distinct subjects/objects, object kinds, datatypes, languages, max objects per subject) kept apart from their RDFS/OWL declarations; subClassOf roots and cycles; cursor pagination bound to one snapshot; time and entry budgets that fail instead of truncating | ✅ |
| MCP server for LLM agents (`sparkles mcp`, stdio; `mcp` cargo feature, on by default): list datasets, describe the schema, run bounded SPARQL (compact table or JSON, truncation announced with the exact total), explain with warnings, describe a resource, list commits, full-text and vector similarity search; `atCommit` keeps several calls on one snapshot; engine budgets on every call, SERVICE off, no writes; MCP revisions `2026-07-28`, `2025-11-25` and `2025-06-18` | ✅ |
| SPARQL formatter (`sparkles-fmt`, `sparkles fmt`, `POST /$/format`; `fmt` cargo feature, on by default): comment-preserving, self-checking (the output must parse to the same algebra, keep every comment and format to itself); the pipeline, the style options, the endpoint (JSON and raw bodies, the editor's cursor carried across in UTF-16 units, refusals as `422`; see [docs/API.md](docs/API.md#formatting)) and the command line (directory walks with ignore files, config discovery, `--check`, `--write`, `--diff`, Prettier's exit codes) are in place, the formatting rules are not yet (documents come back as written, syntax errors with their position) | 🚧 |
| Observability: `X-Request-Id`, one structured access-log line per request (text or JSON), Prometheus `/$/metrics`, readiness `/$/ready`, graceful drain on SIGTERM | ✅ |
| Per-query budgets (estimated intermediate-result memory, response size, rows) failing with `507`; queries and writes stop when their client disconnects | ✅ |
| OpenTelemetry (`otel` cargo feature, off at run time unless `--otel` or `OTEL_*` enable it): OTLP traces with W3C `traceparent` in and out (SERVICE, LOAD), HTTP/database semantic-convention attributes, query phase and operator-tree spans synthesized from recorded timings, commit and background-task spans; metrics (`http.server.request.duration` plus the Prometheus registry, bridged); optional OTLP logs with trace correlation | ✅ |
| Rate limiting: per-client GCRA buckets and concurrency caps per request class (`auth`, `query`, `update`, `admin`) with per-dataset overrides, signed-in callers counted per owner, trusted-proxy client addresses, `429`/`503` with `Retry-After` and `RateLimit` headers, bounded client tracking that remembers evicted debts, SIGHUP reload that keeps client state and in-flight counts; a `preauth` stage limits failed credential checks per address and IPv6 /48 before any password is hashed, and once spent refuses only password checks and unknown tokens (on by default with auth); off by default otherwise | ✅ |
| Authentication and per-dataset access control (`serve --auth-config`, off by default): levels `read` < `write` < `admin` by dataset name or pattern plus `metrics` / `federate` / `server-admin`, deny by default, hidden datasets answer `404`; HTTP Basic users (argon2id), scoped, expiring, revocable API tokens (`Authorization: Bearer spk_…`, hashed at rest, never above their owner), OIDC sign-in for the UI (native, authorization code + PKCE), trusted forward-auth proxy headers from configured CIDRs or a Unix socket, group-to-role mapping; CSRF and CORS rules for cookies; `sparkles auth login` (browser loopback or device code) and remote `query` / `update` / `load --server`; see [docs/API.md](docs/API.md#authentication-and-access-control) | ✅ |
| SvelteKit UI: datasets, query editor, results table/graph/plan, explorer, server page with readiness, request and cache panels, schema browser on `/$/schema` (graph selection, inference toggle, observed counts and object kinds next to declarations), commit history and write receipts, full-text search (index admin panel, ranked `text:query` search in Explore), vector similarity ("Similar" in the explorer, compact vector literals); embedded in the server binary; Vitest unit tests, Playwright end-to-end smoke tests against a real server (sign-in with a password and an API token, query, explore, Similar, text search, history), and a mock server for UI development | ✅ |

## Performance

See [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md). It compares Jena/Fuseki, QLever, Fluree
and Oxigraph using hyperfine over HTTP, with every result cache off and each engine measured
on its own. Before timing, it checks that all engines return the same answers.

| | 1.05M triples | 10.5M triples |
|---|---|---|
| Bulk load | **0.6 s** (Oxigraph 1.0, Fluree 1.4, QLever 1.5, TDB2 4.3) | **4.7 s** (Oxigraph 9.0, QLever 9.1, Fluree 10.2, TDB2 42.6) |
| Fastest of the five | 17 of 20 queries | 17 of 20 queries |
| Loses to QLever | none | `minus` ≈ |
| Loses to Fluree | `distinct-obj` 1.3×; `count-all`, `two-hop-count` ≈ | `distinct-obj` 2.2×, `contains` 1.7× |
| vs. Fuseki | 2.0–25× faster; Fuseki errors on `foaf:knows*` | 1.8–580× faster |
| vs. Oxigraph | 1.4–46× faster | 1.5–465× faster |
| Update latency (1 triple, real insert) | 7.3 ms (**Fluree 6.5**, Oxigraph 11.2, QLever 11.8, Fuseki 41.7) | **5.4 ms** (Fluree 6.8, Oxigraph 10.7, QLever 15.8, Fuseki 38.6) |
| Throughput, 16 clients | **912 q/s** (Fluree 497, QLever 408, Fuseki 53, Oxigraph 25) | **193 q/s** (QLever 57, Fluree 51, Fuseki 7, Oxigraph 2) |
| Server memory | 440 MiB (**QLever 225**, Oxigraph 890, Fuseki 1.7 GiB, Fluree 2.2 GiB) | 921 MiB (**QLever 362 MiB**, Oxigraph 2.3 GiB, Fluree 3.1 GiB, Fuseki 3.9 GiB) |

The Sparkles column was re-measured on 2026-09-30 after the ordered-scan top-k; the other
engines' numbers are from earlier runs on the same machine and data. At 1.05M most queries
take 5–30 ms and run-to-run noise is of the same order, so the 1.05M wins and losses
within a few ms are ties.
Against QLever at 10.5M, Sparkles wins 19 of 20 queries (`minus` is a tie), several by
8–18× (`distinct-obj`, `contains`, `lang-filter`, `regex-iri`, `knows-reach`,
`count-all`).
Fluree is 1.7–74× slower than Sparkles on general joins, OPTIONAL, subqueries, grouping,
sorting and path traversal, and was OOM-killed (26 GB) on `optional-chain` at 10.5M.

Where Sparkles still loses on performance:
* **Single-predicate scans:** Fluree is faster at 10.5M on `distinct-obj` (2.2×) and
  `contains` (1.7×).
* **Update latency:** Fluree commits slightly faster at 1.05M (6.5 vs 7.3 ms, within
  noise; it indexes in the background); Sparkles is faster at 10.5M.
* **Memory:** Sparkles materializes every intermediate result and buffers whole
  responses, and it keeps a decoded-block cache (about 450 MiB of the 921 MiB at 10.5M).
  The server uses mimalloc and releases free heap memory when idle; with glibc malloc
  the same 10.5M run ended at 1.6 GiB.
* **Untested ground:** nothing above 10.5M triples, cold caches, standard benchmarks
  (LUBM/BSBM/WatDiv) and sustained update workloads. QLever's design targets billions
  of triples.

## Where Sparkles still falls short

The performance side (which queries and datasets we lose on, and what the benchmarks do
not cover) is in [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md#where-sparkles-loses). The
feature gaps are:

### vs. Apache Jena / Fuseki

| Area | Jena / Fuseki has | Sparkles |
|---|---|---|
| Full-text search | jena-text (Lucene), `text:query` | `text:query` subset over string literals (Tantivy, BM25), updated in the commit path; no highlighting, per-language stemming or entity-style multi-field documents yet |
| Spatial | GeoSPARQL 1.0/1.1: `geof:` and `spatialF:` functions, `spatial:` property functions with a spatial index, query rewrite of the topological properties, RDFS entailment of the geometry hierarchy, GML and KML literals, EPSG CRSs through Apache SIS | the GeoSPARQL 1.1 `geof:` functions over WKT and GeoJSON literals in the built-in CRSs, Jena's `spatial:` property functions, and a spatial index per dataset that FILTERs and property functions use; no `spatialF:` functions, query rewrite, geometry-type entailment, GML/KML literals or EPSG database yet (see `docs/AUDIT.md` §5) |
| Shape languages | ShEx (jena-shex) | SHACL and ShEx 2.1 (ShExC, ShExJ); no ShExR (RDF) schemas, ShEx 2.2 or ShEx write-time validation |
| Inference | on-the-fly `InfModel`, backward / hybrid rules (LP engine), OWL Micro/Mini/Full | forward materialization only (RDFS, OWL 2 RL subset, Jena forward rules); not maintained incrementally: after updates the inferences are reported stale and re-run on request or, opt-in, automatically (a full recomputation); inconsistency detection covers a fixed subset of the OWL 2 RL `false` rules (`owl:Nothing`, `disjointWith`, `AllDisjointClasses`, sameAs/differentFrom, functional literals), not full consistency checking |
| Ontology API | jena-ontapi `OntModel` object API | ✗ none (triples / SPARQL only) |
| SPARQL extensions | property functions (`list:member`, `apf:*`), `LET`, custom aggregates (`MEDIAN`, `MODE`, `FOLD`), `cdt:` list/map literals, JavaScript functions, full `afn:`/`fn:` library | ✗ none of the extensions; common `fn:`/`afn:`/`math:` functions only |
| SPARQL parser | JavaCC grammar | `spargebra` 0.4.7, vendored with fixes for the W3C syntax/evaluation tests it failed (see `vendor/spargebra/PATCHED.md`) |
| RDF formats | RDF Thrift, RDF Protobuf, TriX, RDF/JSON | ✗ (Turtle, N-Triples, N-Quads, TriG, RDF/XML, JSON-LD only) |
| Change logs | RDF Patch (jena-rdfpatch), Fuseki `/patch` endpoint | ✗ none |
| Fuseki operations | Shiro authentication, per-graph access control (fuseki-access), Prometheus `/$/metrics`, assembler (`config.ttl`) service definitions, `/$/validate/*`, prefix read/write endpoints | Basic, Bearer tokens, OIDC (UI) and trusted proxy headers, with per-dataset levels; no graph-level ACLs yet; Prometheus `/$/metrics` with Sparkles metric names (not Fuseki's `fuseki_requests_*`), no JVM metrics; datasets are configured by CLI flags / admin API only; prefixes via `/{ds}/prefixes` |
| SERVICE | bulk / batched / cached SERVICE (serviceenhancer) | plain SERVICE only |
| Transactions over HTTP | — | — (same as Fuseki: one request = one transaction) |
| Backups | `/$/backup/{ds}`: a gzipped N-Quads dump of the whole dataset per backup, in the server's directory; restored by loading it into a new dataset | the same dumps (`/$/backup/{ds}`), zstd by default (`?compression=gzip` for Fuseki's `.nq.gz`; also brotli or LZ4), plus backup repositories on a file system or S3: incremental, deduplicated backups that restore to a ready database without a reload, with verification, schedules and retention |

### vs. QLever

| Area | QLever has | Sparkles |
|---|---|---|
| Scale | tested to tens of billions of triples (Wikidata, UniProt) | tested to 10.5M; the external-sort path is covered by tests but not measured at 100M+ |
| Streaming execution | lazy, block-wise evaluation of scans, joins, filters and GROUP BY; results streamed to the client | every operator materializes its full result (bounded by row and memory budgets); responses over 1 MiB are streamed to the client as they are serialized |
| Block prefiltering | FILTER ranges / STRSTARTS evaluated against block min/max to skip blocks | numeric range FILTERs on a scan's sort column read only the matching id ranges (inline integers and decimals); non-canonical numerals are still tested row by row |
| Pattern trick | `ql:has-predicate`, per-subject predicate patterns | ✗ (predicate counts use index runs instead) |
| Text / spatial | `ql:contains-word`, BM25 scoring, spatial joins, geo index | BM25 full-text search via `text:query` (no text/entity co-occurrence index); GeoSPARQL functions and a spatial index, no spatial joins yet |
| Vocabulary compression | FSST string compression, IRI-as-id encoding for numeric IRIs | front coding, no IRI encoding |
| Named / pinned results, materialized views | `pin-result-with-name`, materialized views | result cache only (no pinning) |
| Live query monitoring | websocket runtime-information updates | executed plan returned after completion only |

Conversely, Sparkles provides Jena behaviour that QLever does not aim for: exact term
identity (no lossy inlining), the Graph Store Protocol, Fuseki endpoints and admin API,
materialized reasoning, SHACL validation, and an embedded library API.

### vs. Fluree

[Fluree DB](https://github.com/fluree/db) 4.x is the closest peer: a Rust RDF database
with a SPARQL 1.1 endpoint, a compressed columnar index and in-memory novelty over
immutable index files. Its product focus is different, though. It is a versioned,
permissioned ledger with JSON-LD as its primary interface. It is licensed under
**BUSL-1.1**: free to use except as a hosted database service, converting to Apache-2.0
four years after each release. So Sparkles does not depend on it or borrow from it. It
appears here only as a benchmark comparison (downloaded at benchmark time).

| Area | Fluree has | Sparkles |
|---|---|---|
| History | immutable commit chain (content-addressed), time travel (`@t:`, `@iso:`, `@commit:`), history queries, branches / merge / revert | durable, ordered commit ids and a commit catalog; point-in-time reads of every commit since the last compaction, and of older ones kept by named snapshots or a retention window; no history queries across commits, diffs, branches or merges yet |
| Security | ledger-stored access policies, JWS / `did:key` signed requests and commits, OIDC, encryption at rest | per-dataset access levels with Basic, API tokens, OIDC sign-in for the UI and trusted proxy headers; no policy language, signed requests or encryption at rest |
| Interfaces | JSON-LD transactions and queries (FQL), openCypher + Bolt, GraphQL, SQL / R2RML / Iceberg graph sources, MCP server | SPARQL, the Rust API and an MCP server (stdio, read-only tools); JSON-LD as an RDF format only |
| Search | BM25 full-text, vector (HNSW), geospatial | BM25 full-text (`text:query`), exact vector search (`spk:vectorSearch`) and GeoSPARQL with a spatial index; no approximate (HNSW) vector index yet |
| Deployment | S3 / DynamoDB / IPFS storage, Raft clustering, read replicas ("query peers") | single node, local disk, plus incremental, deduplicated backups to a file system or S3 |
| Reasoning | at query time (RDFS / OWL 2 QL rewriting, OWL 2 RL / Datalog with a fact budget) | materialized (RDFS, OWL 2 RL, Jena rules) |

Where Sparkles is ahead:
* **Conformance.** Sparkles passes the W3C SPARQL 1.1 suites in full and the SHACL
  Core and SHACL-SPARQL suites (98/98, 20/20). We have not run Fluree's conformance
  ourselves. Its TSV output is not W3C-formatted.
* **Jena/Fuseki compatibility.** Fuseki endpoints and `/$/` admin, the Jena-style CLI
  and rules, and external `SERVICE` federation (Fluree federates only between its own
  ledgers).
* **Index design.**
  * Fluree keeps 4 index orders against Sparkles' 7.
  * Its planner is greedy; Sparkles' is dynamic-programming.
  * It indexes in the background once uncommitted changes pass a threshold. Sparkles
    folds updates into an in-memory delta and compacts on request.

See [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md) for the head-to-head numbers.

### vs. Oxigraph

[Oxigraph](https://github.com/oxigraph/oxigraph) (MIT / Apache-2.0) is a Rust RDF
database and toolkit on RocksDB. Sparkles is built on Oxigraph's libraries: `oxrdf`,
`oxttl`/`oxrdfio`, `spargebra`, `sparesults` and `oxsdatatypes` supply its term model,
parsers, serializers, SPARQL parser and XSD datatypes. The storage engine, query planner
and executor are Sparkles' own. So the two share the front end and differ in how
queries run.

| Area | Oxigraph has | Sparkles |
|---|---|---|
| Embedding | Rust library, Python (`pyoxigraph`) and JavaScript/WebAssembly packages, an in-memory store | Rust library (persistent or in-memory); no Python or WebAssembly bindings |
| Storage | RocksDB (an LSM tree; C++), 9 index orders (6 for named graphs, 3 for the default graph) plus a string dictionary; updates in place; online backups via RocksDB checkpoints (each a complete database in a new local directory, hard-linked when on the same file system) | immutable sorted blocks in 7 orders plus a WAL-logged in-memory delta, merged by compaction; online backups into backup repositories on a file system or S3 (incremental and deduplicated across backups and datasets), with restore, verification, schedules and retention |
| Spatial | GeoSPARQL functions (`spargeo`, on by default in the CLI; no spatial index) | GeoSPARQL 1.1 functions (geodesic measures, EPSG:4326 axis order, metric buffers) and a spatial index per dataset |
| Write durability | a RocksDB transaction per request, written to RocksDB's WAL without an fsync (RocksDB's default write options) | the WAL is fsynced before a write is acknowledged |

Oxigraph describes its query evaluation as "not optimized yet": it evaluates lazily,
iterator by iterator over RocksDB scans. In the benchmarks it loads quickly (second to
Sparkles) but joins, grouping, sorting and counting are 10–400× slower at 10.5M
triples, and it serves 2 concurrent star-join queries/s against Sparkles' 191. Sparkles
adds what Oxigraph leaves out: reasoning, SHACL, full-text and vector search, point-in-time
reads, authentication and per-dataset permissions, Fuseki's admin API, budgets and a
result cache, and the web UI.

## Notable optimizations adopted from QLever

* **Ids with inline values.** The top 4 bits hold a tag and the low 60 bits a payload.
  UNDEF is 0, so it sorts first. Numbers and booleans never touch the dictionary.
* **Sorted vocabulary.** Id order equals term order, so prefix and range restrictions
  become id ranges (`Vocab::prefix_range`).
* **Permutation files.** Blocks store columns separately (delta + zig-zag varint +
  LZ4). Each block's first and last key stays in RAM, which enables block skipping
  for bound prefixes and exact counts with at most two block decodes.
* **Bulk build pipeline.** Parallel chunked parsing feeds per-batch partial
  vocabularies. These are k-way merged into a global vocabulary, ids are remapped in
  parallel, and each permutation is built with a parallel sort (or sorted runs plus a
  k-way merge when the data exceeds the memory budget).
* **Immutable base plus delta.** Updates are layered on the immutable index, in the
  style of QLever's `DeltaTriples`. Snapshots are versioned, and caches are keyed
  by snapshot version.
* **Columnar execution and planning.** Execution is column-major; the planner is a DP
  over interesting sort orders with a greedy fallback. Merge joins run on sorted
  scans.
* **Decoded-block cache.** A shared cache of decoded blocks, weighted by bytes.
* **Result cache.** Executed subtrees are cached under a canonical plan key plus the
  snapshot version, so updates invalidate naturally. Results with query-local terms or
  non-deterministic functions are skipped. Each operator reports `cached` in the plan.
* **GROUP BY + COUNT from index runs.** When the group key is a scan's sort column,
  counts come from runs in the blocks without materializing the scan.
* **Planner details.** Filters are placed as soon as their variables are bound. Scan
  sizes are exact from block metadata (at most two block decodes). Join estimates use
  per-predicate distinct subject/object statistics with QLever's 0.7 correction factor.
  Merge joins use galloping for skewed inputs. `COUNT(*)` over a single pattern is
  answered from index metadata. Transitive paths traverse from the bound side (index
  lookups per frontier node) instead of materializing the closure.
* **Executed-plan feedback.** Every query returns a runtime-information tree
  (estimated vs. actual rows, time per operator), like `qlever-json`. The UI renders it.

### Further scan and filter optimizations

* **`COUNT(DISTINCT ?v)` from index runs.** Over a single triple pattern, the scan is
  re-targeted to a permutation sorted on `?v` and the distinct values are counted as
  runs of equal ids (`CountDistinctFromIndex`). No rows are materialized or hashed.
* **Filters on vocabulary keys.** `CONTAINS` / `STRSTARTS` / `STRENDS` / `REGEX` over
  `?v` or `STR(?v)`, and `LANGMATCHES(LANG(?v), …)`, are tested directly on the stored
  key bytes (`"lexical 0xFF @lang`, `<iri`). Each front-coded block is read once, in
  parallel, with no per-term string allocation. Terms added by updates are tested on
  their delta keys; inline values (numbers, dates) fall back to the general evaluator.
* **Per-distinct-value filters.** A deterministic filter over one variable is
  evaluated once per distinct id, and rows look up the outcome. When the column is
  sorted on the variable, the distinct ids are its runs.
* **Numeric range scans.** A FILTER comparing a scan's sort column with numeric
  constants reads only the id ranges that can match. Inline integers, and inline
  decimals of one scale, sort by value within their id segment. So each segment's
  matching ids form one range, found by binary search with the ordinary comparison.
  Doubles and literals from the vocabulary are read and tested; booleans, dates and
  blank nodes are skipped. The planner costs the scan from an exact count of the rows
  in those ranges (`IndexRangeScan` in EXPLAIN).
* **Numeric top-k.** `ORDER BY ?v LIMIT k` over numbers ranks cheap rounded keys first.
  Exact values are computed only for rows that can still reach the first k.
* **Ordered-scan top-k.** `ORDER BY ?v LIMIT k` (and OFFSET) over a single triple
  pattern, with FILTERs over it, reads the pattern in the order of `?v` and stops once
  the first k rows are proven (`IndexTopK` in EXPLAIN). The scan is re-targeted to a
  permutation sorted on `?v`. Inline integers, decimals of one scale and doubles of
  one sign sort by value within their id segment. Each segment is read from its best
  end, a few rows at a time, until the k-th candidate is strictly better than its best
  unread value. Vocabulary literals (non-canonical numerals, other numeric types,
  strings), IRIs and blank nodes are not in value order by id, so they are read whole.
  The candidates are ranked by the ordinary ORDER BY in the plain scan's row order, so
  ties come out the same. NaN, dates and durations have no total order, and fall back
  to the plain sort. The planner picks it from exact row counts per segment when it
  reads at most half of the pattern's rows.
* **Incremental GROUP BY.** With one group key and COUNT / SUM / AVG / MIN / MAX /
  SAMPLE over variables, each group keeps a running state in a hash map on the key id.
  Sums stay exact 64-bit integers until a value is not an inline integer.
* **Count joins from key runs.** `COUNT(*)` over two scans joined on one variable reads
  both sides as (key, run length) pairs from indexes sorted on that variable and sums
  the products (`CountJoinFromRuns`).
* **Class counts from statistics.** `GROUP BY ?class` with a count over `?s a ?class`
  uses the per-class counts in the index statistics when they are exact: no delta, and
  all data in the default graph (`GroupCountFromMetadata`).
* **Batched path frontiers.** `p*` / `p+` traversals expand a large BFS level with one
  merged pass over the predicate's index rows, instead of one seek per node.
* **Selective column decoding.** The block cache holds decoded columns. Scans decode
  only the key columns they read (variables, graph, repeated variables), which also
  leaves room for more of the cache.
* Every one of these can be switched off per query (`QueryOptions::optimizations`) or
  per process (`SPARKLES_DISABLE_OPTIMIZATIONS=range_pushdown,…`), and EXPLAIN shows
  which one ran.
* **Whole-block scans under graph filters.** A block slice is copied column-wise
  whenever every row passes the graph filter (one pass over the graph column), so
  default-graph queries no longer fall back to row-by-row filtering.

## Divergences from Jena / QLever (decisions)

| Decision | Rationale |
|---|---|
| Rust instead of Java/C++ | See `docs/AUDIT.md` §3: `spargebra`, `oxttl` and friends provide the parser and format stack; there is no GC, and performance is predictable. |
| Sorted-block permutations instead of TDB2's B+trees | Scan-heavy analytics are much faster and the files are smaller. Updates go into a delta that is periodically compacted, rather than being done in place. |
| Values are inlined only when the lexical form is canonical | QLever inlines lossily (doubles lose 4 bits, and the lexical form is dropped). Sparkles keeps exact RDF term identity (`"01"^^xsd:integer` ≠ `"1"^^xsd:integer`), as Jena does. Doubles whose low mantissa bits are nonzero go to the vocabulary. |
| Graph stored as a 4th key column in every permutation, plus a GSPO permutation | This matches QLever's graph column. GSPO gives TDB2-style graph-scoped access (dumps, `GRAPH ?g {}` enumeration). |
| Deltas held as persistent ordered sets (`imbl`) and WAL-logged | Gives O(1) snapshot publication for MVCC. QLever locates delta triples per block instead; we may adopt that later. |
| Blank nodes are stored ids and serialize as `_:b<hex>` | Labels round-trip through the protocol, like Jena's `<_:…>` handling. |
| LZ4 instead of zstd for index blocks; front coding instead of FSST for the vocabulary | Very fast decoding on the query path. zstd is used where ratio matters more than decode speed (backups, dumps, HTTP, the full-text document store); zstd blocks and FSST remain possible upgrades. |
| Canonical decimal output follows XSD 1.1 (`"4"^^xsd:decimal`), whereas Jena writes `"4.0"` | Comes from `oxsdatatypes`; the values are equal, so value-based result comparison is unaffected. |
| SPARQL parsing and algebra via `spargebra` instead of a port of ARQ's JavaCC grammar | The algebra matches SPARQL 1.1 §18. ARQ syntax extensions (LET, `apf:` property functions, custom aggregates) are not supported. |
| Filter placement and equality substitution happen in the planner rather than as ARQ-style algebra transforms | Same effect as `TransformFilterPlacement` / `TransformFilterEquality`, with one less pass over the algebra. |
| `REDUCED` is a no-op | Allowed by the spec. |
| `GRAPH ?g { P }` binds `?g` as a scan column when `P` is a plain join group; otherwise `P` is evaluated per named graph and joined with `?g`, like Jena's `OpGraph` | The fast path covers the common case, and the fallback keeps SPARQL scoping exact (e.g. OPTIONAL or MINUS inside GRAPH). |
| `GROUP_CONCAT` always returns a simple literal | Spec behaviour; Jena keeps a common language tag. |
| Triple terms are vocabulary entries with a canonical nested key (blank nodes inside them keep store identity); patterns with variables inside `<<( … )>>` bind a hidden variable and are decomposed by a `TripleTerm` operator | Keeps the 64-bit id model and all permutations unchanged; key order puts triple terms between literals and IRIs, so term-kind checks stay O(1). |
| Effective boolean value of ill-typed boolean/numeric literals is an error | SPARQL 1.2 §17.2.2 (SPARQL 1.1 said `false`). |
| Reasoning is materialized (forward chaining into the `urn:x-sparkles:inferred` graph, queried as default ∪ inferred) instead of Jena's on-the-fly `InfGraph` | Query speed stays that of the plain index. The trade-off is re-running `/$/reason` after updates: the reasoning status records the commit it was made at, so stale inferences are reported (and can be re-run automatically with `serve --auto-reason`). Backward (LP) rules are not supported. |
| `AS ?v` targets that are already in scope are rejected (SPARQL §18.2.1) | `spargebra` does not check this, so Sparkles validates it itself, matching Jena and QLever. |
| `serve` listens on `127.0.0.1` by default and refuses a non-loopback address without `--auth-config` unless `--allow-open-network` (or `SPARKLES_ALLOW_OPEN_NETWORK=1`) is given (Fuseki listens on all interfaces) | Without authentication every caller may read, write and administer everything, so exposing that is an explicit choice; the override still logs a warning, as does a network listener without rate limits. |
| Without `--auth-config`, `serve` sends no CORS headers unless `--cors-origin` names an origin, refuses cross-site writes (`Origin`, `Sec-Fetch-Site`) and answers only IP addresses, `localhost`, `--host` and `--public-host` names in `Host` (Fuseki answers CORS from any origin) | Every caller of an open server is its administrator, so any web page the operator opens could otherwise read, write and `LOAD` local files through the browser, directly or by rebinding its DNS name. |
| `--max-export-mb` stays `0` (unlimited) by default, while query responses are capped at 1 GiB (`--max-result-mb`) | A Graph Store GET of a graph or a whole dataset is the export path, streamed from one snapshot, and a finite default would cut off legitimate dumps. The cost: any reader can make the server stream its whole dataset (CPU and bandwidth, not memory). Deployments that expose reads to untrusted clients should set `--max-export-mb` and rate-limit the `query` class. |
| A client's `timeout=` is capped at `--max-timeout` (default 1800 s, `0`: no cap) for queries, updates and Graph Store writes alike; the default query timeout stays 60 s, and writes have no default deadline (`--update-timeout 0`) but are cancelled when their client disconnects | A request may ask for a longer timeout than the default, but not hold a worker indefinitely; a long load is not cut off by a default it did not ask for, and a disconnected one stops (its rate-limit concurrency slot stays taken until it has). |
| The full-text index is committed lazily: a write stages its documents, and the next text query that needs them (or a tick about once a second) commits them | A Tantivy commit flushes a segment and cost more than the indexing itself; a burst of writes now shares one. Each snapshot still searches exactly its own documents (later ones are filtered out against it, removed ones are kept until their batch is committed), and after a crash the WAL restores what was only staged. Jena's text index commits with each transaction. |
| N-Quads backups (`/$/backup/{ds}`, `sparkles backup`) are zstd (level 3, `.nq.zst`) by default, where Fuseki writes gzip (`.nq.gz`); `?compression=gzip` or `--compress gzip` writes Fuseki's format | At 10.5M triples zstd took 8.2 s for 81.5 MB and gzip (level 6) 41 s for 74.9 MB: five times faster for a file 9% larger. `sparkles load` and uploads read both. |
| GeoSPARQL distances, lengths and areas on geographic CRSs are geodesic on the WGS 84 ellipsoid (Karney); `geo.json` `"distance": "haversine"` gives Jena's sphere (R = 6,371,008.7714 m) | Up to 0.5% more accurate than the sphere, at a small cost per call. Jena computes great-circle distances on a sphere. |
| GeoSPARQL literals are read with their CRS's own axis order (EPSG:4326 is latitude first); the legacy `…/def/crs/EPSG/4326` (without `/0/`) is CRS84, as in Jena | GeoSPARQL Req 16. `minX`…`maxY` report the literal's own axes, as in Jena. |
| GeoSPARQL literals in a CRS this build does not know are valid geometries: same-CRS planar relations, accessors and constructions work, metric functions and mixes with other CRSs are type errors, and the index leaves them out (counted in its status) | Jena logs a warning and treats the coordinates as CRS84 degrees, which gives wrong answers silently. |
| GeoSPARQL relations follow DE-9IM: an empty geometry is disjoint from everything (`sfDisjoint` true, every other relation false), equal points are `sfEquals`, `sfCrosses` of two curves is `0********`, RCC8 relations hold between regions only | Jena returns false for every relation on an empty geometry and compares `sfEquals` with the tables' `TFFFTFFFT` pattern, under which two equal points are not equal. |
| `geof:getSRID` returns an `xsd:anyURI`; `geof:dimension` of an empty geometry is its type's dimension (`-1` for an empty collection) | The GeoSPARQL 1.1 signature (Jena returns `xsd:string`); never a type error. |
| The spatial index is opt-in per dataset (`geo.json`); the `geof:` functions work without it, and every answer from the index is refined with the exact test, so answers are the same with or without it | Jena's index is built for the whole dataset at start-up and its `spatial:withinBox`/`intersectBox` return envelope hits for an unbound subject. |
| Out of scope for v1 | JS scripting functions, RDF Thrift/Protobuf/TriX, jena-ontapi object mapping, jena-text's Lucene index format and assembler configuration (Sparkles implements `text:query` itself), SHACL-AF rules (also absent in Jena), RDF Patch, backward-chaining (LP) rules, Shiro auth. |

## Using the library (no server)

`crates/sparkles` is a plain Rust library. The CLI (everything except `serve`) and the
server are built on it, so anything they do can be done in-process. A database
directory is locked while open (`sparkles.lock`, like TDB2's `tdb.lock`).

```rust
use sparkles::{Dataset, io::RdfFormat};
use sparkles::querybuilder::{SelectBuilder, UpdateBuilder, expr, lit, var};

let ds = Dataset::open("mydb")?;                 // or Dataset::memory()
ds.load_file("data.ttl.gz")?;                    // parallel bulk path for large inputs

// SPARQL text
for row in &ds.select("SELECT ?s ?name WHERE { ?s foaf:name ?name } LIMIT 10")? {
    println!("{} {}", row.get("s").unwrap(), row.get("name").unwrap());
}

// fluent builder (jena-querybuilder): typed terms, escaped literals, prepared queries
let adults = SelectBuilder::new()
    .select("?name")
    .where_("?p", "foaf:name", "?name")
    .where_("?p", "foaf:age", "?age")
    .filter(expr::gt(var("age"), 17))
    .order_by("?name")
    .limit(100)
    .execute(&ds)?;
UpdateBuilder::new()
    .insert_data("<http://ex/carol>", "foaf:name", lit(user_input))
    .execute(&ds)?;

// term-level graph access (Jena Graph / DatasetGraph)
let g = ds.default_graph();                       // named_graph(iri), union_graph()
let knows = g.find(Some(&alice), Some(&foaf_knows), None)?;
ds.transaction(|tx| {                              // committed on Ok, rolled back on Err
    tx.remove_triple(&knows[0])?;
    tx.insert_triple(&new_triple)?;
    Ok(())
})?;
ds.dump(std::io::stdout(), RdfFormat::TriG)?;
```

| Jena | Sparkles |
|---|---|
| `TDB2Factory.connectDataset(dir)` / `DatasetFactory.createTxnMem()` | `Dataset::open(dir)` / `Dataset::memory()` |
| `RDFDataMgr.read` / `RDFParser` | `Dataset::load_file`, `load_str`, `load_*_into(graph)`; `sparkles::io` |
| `QueryExecution` / `RDFConnection.query` | `Dataset::query`, `select`, `ask`, `construct` (`QueryOptions` for timeouts, datasets, initial bindings) |
| `UpdateExecution` / `RDFConnection.update` | `Dataset::update` |
| `Graph.find/add/delete/size`, `DatasetGraph.find` | `GraphView::find/insert/remove/len`, `Dataset::find` |
| `Txn.executeWrite` | `Dataset::transaction` |
| `jena-querybuilder` `SelectBuilder` & co. | `sparkles::querybuilder` |
| `QueryExec.substitution` / `setVar` | `QueryOptions::initial_bindings` / builder `set_var` |
| reasoners (`InfModel`) | `sparkles_reasoner::materialize` (crate `sparkles-reasoner`) |
| `ShaclValidator` | `sparkles_shacl::validate` (crate `sparkles-shacl`) |
| `ShexValidator` | `sparkles_shex::validate` (crate `sparkles-shex`) |

Lower-level access (ids, snapshots, raw index scans, the bulk `Builder`) is available
through `Dataset::store()` and the `store` / `index` / `builder` modules.

## Building & running

```sh
pnpm -C ui install && pnpm -C ui build        # optional: the UI is embedded at compile time
cargo build --release
./target/release/sparkles serve --data ./data --port 3030   # UI at http://localhost:3030/ui/
```

The server and CLI use [mimalloc](https://github.com/microsoft/mimalloc) as their
allocator (the default `mimalloc` cargo feature of `sparkles-server`; the `sparkles`
library leaves the choice to its embedder). Once no request has been active for
`--idle-release-ms` (default 1000 ms), `sparkles serve` hands free heap memory back to
the OS. Built with `--no-default-features --features reasoning,shacl` it uses the
system allocator and `malloc_trim` instead.

`serve` listens on `127.0.0.1` by default. A non-loopback `--host` (such as `0.0.0.0`)
without `--auth-config` is refused at startup, since without authentication every caller
may read, write and administer every dataset; `--allow-open-network` (or
`SPARKLES_ALLOW_OPEN_NETWORK=1`) serves it open anyway, with a warning in the log. A
network listener without request rate limits (`query`, `update` or `admin`) is logged as
a warning too, with or without auth. An authenticating reverse proxy in front does not
make an open backend safe: bind the backend to loopback or a Unix socket
(`--unix-socket`), or firewall it, so that nothing can bypass the proxy.

A server without `--auth-config` also guards against the web pages its operator opens: it
answers only requests whose `Host` is an IP address, `localhost` (or `*.localhost`),
`--host` or a `--public-host` name (anything else is `421`, which stops a page that
rebinds its own DNS name to the server), it refuses unsafe requests and anything that
writes or administers from another site (`403 cross-origin request refused`, by `Origin`
and `Sec-Fetch-Site`), and it sends no CORS headers unless `--cors-origin` names an
origin. The UI served by the server itself, the CLI and other non-browser clients are
unaffected. Behind a reverse proxy, pass the name the proxy is reached by with
`--public-host` (the NixOS module does this for its nginx virtual host).

Every response carries `X-Content-Type-Options: nosniff` and `X-Frame-Options: DENY`.
The UI's pages have a Content Security Policy that allows scripts only from the UI
itself (its inline start-up scripts by hash) and no framing; API responses have
`Content-Security-Policy: default-src 'none'; frame-ancestors 'none'`.

Fuseki-style endpoints for a dataset `ds`: `/ds/sparql`, `/ds/update`, `/ds/data` (GSP),
`/ds/upload`, plus `/$/datasets`, `/$/stats/ds`, `/$/compact/ds`, `/$/backup/ds`, `/$/tasks`
(see `docs/API.md`). `--mem NAME` adds an in-memory dataset, `--loc NAME=PATH` serves an
existing database.

Operations: `/$/ping` is the liveness check and `/$/ready` the readiness check (`503`
once shutdown starts on SIGINT or SIGTERM); `/$/metrics` serves Prometheus metrics.
Every response carries an `X-Request-Id`, and each request is logged once under the
`sparkles::access` target. Other `serve` options:

| Flag | Default | Meaning |
|---|---|---|
| `--host ADDR` | `127.0.0.1` | listen address; a non-loopback address needs `--auth-config` or `--allow-open-network` |
| `--allow-open-network` | off | serve without `--auth-config` on a non-loopback address (also `SPARKLES_ALLOW_OPEN_NETWORK=1`); logged as a warning |
| `--public-host NAME` | | a host name clients reach the server by, such as a reverse proxy's (repeatable); without `--auth-config` other names than IP addresses, `localhost` and `--host` are refused with `421`, and with it so are requests carrying trusted proxy headers from loopback or the Unix socket |
| `--cors-origin ORIGIN` | none | a browser origin (`https://yasgui.example`) whose pages may call the API cross-origin, without credentials (repeatable; with `--auth-config`, added to `cors.origins`); without auth such a page may do everything the server allows |
| `--timeout S` | `60` | default query timeout in seconds (`timeout=` per request) |
| `--update-timeout S` | `0` | default SPARQL update timeout in seconds (`0`: none; `timeout=` per request); a timed-out update changes nothing |
| `--max-timeout S` | `1800` | largest `timeout=` a query or update may ask for (`0`: unlimited; never below `--timeout` / `--update-timeout`) |
| `--query-memory-mb N` | `8192` | budget for the estimated memory of a query's intermediate results (`0`: unlimited) |
| `--max-result-mb N` | `1024` | budget for the body of a SPARQL query response (`0`: unlimited) |
| `--max-export-mb N` | `0` | budget for the body of a Graph Store GET, i.e. a graph or whole-dataset export (`0`: unlimited) |
| `--max-rows N` | `200000000` | rows of any intermediate result |
| `--max-query-body-mb N` | `16` | largest SPARQL query body (also explain and `/shacl` shapes); `413` past it (`0`: unlimited) |
| `--max-update-body-mb N` | `256` | largest SPARQL update body; bulk data goes through the Graph Store or `/upload` (`0`: unlimited) |
| `--max-admin-body-mb N` | `16` | largest `/$/…` or prefix-change body (`0`: unlimited); `/$/auth/*` bodies are capped at 64 KiB |
| `--max-upload-mb N` | `4096` | largest Graph Store write or upload body, streamed to a temporary file and counted after HTTP decompression (`0`: unlimited) |
| `--min-free-disk-mb N` | `1024` | refuse (`507`) to spool a request body once the temporary directory's file system would keep less free, and to commit, rebuild, clone or write an N-Quads backup (`/$/backup`) once the data directory's would (`0`: no check) |
| `--max-mem-dataset-mb N` | `4096` | largest in-memory dataset; a commit that would grow one past it fails with `507` (`0`: unlimited) |
| `--max-tasks N` | `4` | background tasks (compaction, clones, reasoning, full-text and spatial index builds, N-Quads backups) running at once; more wait `queued` (`0`: no limit) |
| `--backup-config FILE` | | backup repositories, policies, credential sources and the limits of repositories registered through the API (TOML, also `$SPARKLES_BACKUP_CONFIG`; re-read on SIGHUP; read-only through the API) |
| `--backup-max-tasks N` | `2` | backup, restore, verify and GC tasks running at once; more wait `queued` |
| `--format-endpoint on\|authenticated\|off` | `on` | who may use `POST /$/format` (see `docs/API.md`, Formatting): every caller the server admits, every caller but the anonymous principal (`401`), or nobody (`404`) |
| `--format-max-mb N` | `16` | largest `POST /$/format` body (`0`: unlimited) |
| `--format-timeout S` | `10` | seconds a `POST /$/format` request may take, waiting for a free slot (one per core) included; `408` past it |
| `--vector-memory-mb N` | `4096` | memory for the packed vectors of `spk:vectorSearch`, per index generation |
| `--text NAME[=FILE]` | | enable full-text search for a dataset (with a `text.json`-shaped configuration file) |
| `--geo NAME[=FILE]` | | enable the spatial index for a dataset (with a `geo.json`-shaped configuration file); the build runs before the server starts listening |
| `--geo-mb N` | `4096` | memory for each dataset's spatial index (geometry column and trees); a build that would exceed it is refused, the status says `over-budget`, and queries run without the index |
| `--geo-op-vertices N` | `2000000` | largest sum of input vertices of one geometry operation (overlay, buffer, hull, relate); larger ones are a type error |
| `--log-format text\|json` | `text` | log format on stderr (global flag); `RUST_LOG` filters as usual |
| `--no-access-log` | | no per-request log lines |
| `--no-metrics` | | `/$/metrics` answers `404` and no request metrics are kept |
| `--metrics-max-datasets N` | `100` | datasets with their own metric labels (the rest share `$other`) |
| `--otel` | off | export traces and metrics over OTLP (also enabled by `OTEL_EXPORTER_OTLP_ENDPOINT`; the standard `OTEL_*` variables apply, see `docs/API.md`, OpenTelemetry) |
| `--otel-logs` | off | export log events over OTLP too |
| `--otel-query-text` | off | record query text (`db.query.text`) and plan operator descriptions in spans; they may hold data |
| `--otel-plan-spans` | off | one span per executed plan operator |
| `--rate-limit SPEC` | off (`preauth=30/min,burst=60` with `--auth-config`) | limit a request class per client, e.g. `query=100/s,burst=200,concurrency=64` or `auth=10/min,burst=5`; `preauth=…` limits authentication failures per address before credentials are checked (repeatable; see `docs/API.md`, Rate limiting) |
| `--rate-limit-config FILE` | | JSON rate-limit configuration, re-read on SIGHUP; `--rate-limit` applies on top |
| `--rate-limit-trusted-proxy CIDR` | | proxy whose `X-Forwarded-For` names the client (repeatable; `unix`: the `--unix-socket`); limits by address need a peer address clients cannot choose, so list only proxies that overwrite or append to the header |
| `--rate-limit-trusted-proxy-header H` | `x-forwarded-for` | the one header trusted proxies name the client in: `x-forwarded-for` or `forwarded` (RFC 7239); the other is ignored |
| `--no-service` | | refuse `SERVICE` for everyone |
| `--outbound-allow-private` | off | let `SERVICE` and `LOAD <http…>` reach loopback, private, shared (CGNAT) and unique-local addresses (see [Outbound requests](#outbound-requests-service-and-load)) |
| `--outbound-block-private` | | refuse those addresses: already the default of `serve` and `mcp`, an opt-in for the local `query` and `update` (which allow them by default) |
| `--outbound-allow HOST_OR_CIDR` | | contact only these destinations (repeatable) |
| `--outbound-timeout S` | `60` | total time of one outbound request, until the end of its response |
| `--outbound-max-mb N` | `256` | largest outbound response, decompressed |
| `--outbound-request-max-mb N` | 4 × `--outbound-max-mb` (`1024`) | bytes all the SERVICE calls and LOADs of one query or update may receive (`507` past it) |
| `--outbound-request-timeout S` | 4 × `--outbound-timeout` (`240`) | time all the SERVICE calls and LOADs of one query or update may take, summed |
| `--load-dir DIR` | | let `LOAD <file:…>` read the regular files under `DIR` (symbolic links resolved, nothing outside it); without it the server refuses file loads |
| `--max-prefixes N` | `1000` | prefixes per dataset (global flag; `0`: unlimited); a new one past it is refused with `400`, and loaded data stops adding its prefixes |

Over-budget requests fail with `507` and a JSON body naming the budget (`outbound-bytes` for
the outbound total); a query or write stops as soon as its client disconnects (a write then
commits nothing). `sparkles query --memory-mb N` applies the memory budget
on the command line (unlimited by default).

Command line tools (Jena `tdb2.*` / `arq` equivalents):

```sh
sparkles load    --loc db data/*.ttl.gz       # parallel bulk load (tdb2.tdbloader)
sparkles query   --loc db 'SELECT ...'        # --results text|json|xml|csv|tsv, --explain, --time
sparkles query   --data file.ttl --query q.rq # query files in memory (arq --data)
sparkles update  --loc db 'INSERT DATA {...}' # also LOAD <http…>
sparkles compact --loc db                     # merge updates into a new generation
sparkles dump    --loc db > dump.nq
sparkles dump    --loc db --out dump.nq.zst   # compression from the extension, or --compress
sparkles backup  --loc db --out backups/      # zstd; --compress gzip --level 9, --threads 8
sparkles clone   --loc db --to sandbox        # independent copy (same blank nodes, new dataset id)
sparkles stats   --loc db
sparkles log     --loc db                     # commit history (works next to a running server)
sparkles check   --loc db                     # verify the files, read-only (--quick, --format json)
sparkles infer   --loc db --profile owl-rl    # materialize inferences
sparkles infer   --loc db --status            # are the inferences up to date?
sparkles infer   --loc db --check             # OWL 2 RL inconsistency checks (exit 1 on violations)
sparkles text-index --loc db                  # full-text index: --predicate, --exclude-graph, --rebuild, --status, --disable
sparkles geo-index  --loc db                  # spatial index: --predicate, --feature-link, --exclude-graph,
                                              #   --distance geodesic|haversine, --rebuild, --status (JSON), --disable
```

`sparkles geo-index` enables the spatial index with the defaults when it is off (or with
the given options), builds it and prints its status to stderr; on an enabled index it
reports the status after the build that opening the database starts. It exits 2 when the
binary was built without the `geo` feature.

`sparkles fmt` formats SPARQL queries and updates (`.rq`, `.ru`, `.sparql`) with
Prettier's modes and exit codes: 0 when everything is formatted (or was written), 1 when
`--check` or `-l` found a file that would change, 2 on any error (a syntax error, a
refused output, an unreadable file, a bad option). Every file is processed, so one run
reports every problem, and many files are formatted in parallel (`--threads N`).

```sh
sparkles fmt q.rq                    # print the formatted query (stdin to stdout without a path)
sparkles fmt --write queries/        # rewrite in place: temporary file, fsync, rename; unchanged files keep their mtime
sparkles fmt --check queries/        # [warn] per unformatted file on stderr; --diff adds unified diffs on stdout
sparkles fmt -l queries/             # names of the files that would change
sparkles fmt --stdin-filepath queries/q.rq < q.rq   # stdin named for detection, config and ignore files
```

- **Files.** Directories are walked recursively for the extensions of the languages this
  build formats, with `.gitignore`, `.git/info/exclude` and the global gitignore applied
  (hidden files included; never `.git`, `node_modules` or `target`). A file named on the
  command line is formatted whatever its extension: `--language` names its language, else
  the extension does, else (an unknown extension) its content. Turtle, TriG, N-Triples, N-Quads and
  JSON-LD files are skipped by walks and refused when named ("… formatting is not
  available yet"); RDF/XML and compressed files are refused too.
- **Ignore file.** `.sparklesfmtignore` in the current directory (gitignore syntax), or
  `--ignore-path FILE` (repeatable). It applies to walks and to named files, which it
  skips silently; an ignored `--stdin-filepath` passes stdin through unchanged.
- **Config.** `.sparklesfmt.toml` (or `sparklesfmt.toml`), found by walking up from each
  file's directory; the nearest one wins and files are not merged. Flags override it;
  `--config FILE` uses one file for every input and `--no-config` the defaults. An unknown
  key or a bad value is an error naming the key and the file.

  ```toml
  line-width = 100               # 40..=400
  indent-width = 2               # 1..=8
  prefix-groups = []             # e.g. [["rdf", "rdfs", "xsd", "owl"]]; --prefix-group rdf,rdfs,xsd,owl
  type-shorthand = true          # rdf:type → a
  compact-iris = true            # full IRI → prefixed name
  quote-style = "double"         # "double" | "preserve"
  operator-position = "leading"  # "leading" | "trailing": where a broken || or && chain puts its operator
  # also accepted, for the RDF formats to come: sort, prune-prefixes, directive-style,
  # turtle-layout, align-values
  ```

  Every key has a flag of the same name (`--no-type-shorthand`, `--quote-style preserve`, …).
- **Messages.** Errors read `path:LINE:COL: error: …` (1-based lines and columns, in
  characters); a refused output reads `path: error: formatter refused its own output
  (algebra differs); input left unchanged; please report`.

Backup repositories (see [docs/API.md](docs/API.md#backup-repositories)) work offline
too, on a stopped database; a server's own datasets are backed up through its HTTP API or
UI, or by its policies. `--repo` takes a name from the backup config file
(`--backup-config FILE`, `$SPARKLES_BACKUP_CONFIG`, default
`$XDG_CONFIG_HOME/sparkles/backup.toml`) or a URL: `file:///srv/backups/r`,
`s3://bucket/prefix?region=…&endpoint=…&path_style=true&allow_http=true`, or `memory://`.
Credentials never go in URLs; they come from the environment or a credentials file.
Only `repo add` and `backup create` initialize an empty location; the other commands
attach to an existing repository. Manifests are cached in
`$XDG_CACHE_HOME/sparkles/backup/`, progress goes to stderr, Ctrl-C cancels (twice:
quits), and the commands that print a result take `--format json` (or `--json`). Exit
codes: 0 ok, 1 errors, 2 warnings only (orphaned blobs in `repo verify`).

```sh
sparkles repo add local --path /srv/backups/r    # edits the config file (mode 0600), initializes, tests
sparkles repo add s3 --s3 kg-backups --prefix prod --region eu-central-1 --credentials env
#   --endpoint URL --path-style --allow-http (MinIO, R2, …); --credentials default | env |
#   env:KEY_VAR,SECRET_VAR[,TOKEN_VAR] | file:PATH; --readonly; --no-init (attach only)
sparkles repo add lab --s3 lab --endpoint http://127.0.0.1:9000 --path-style --allow-http \
  --credentials-name minio --credentials env:MINIO_ACCESS_KEY,MINIO_SECRET_KEY
#   keeps the source as [credentials.minio], which the repository names (without
#   --credentials: uses the one defined); a server reading the file can then name it too
sparkles repo list | show local | test local | remove local   # remove leaves the contents alone
sparkles repo verify local --level data          # every backup, plus orphaned blobs
sparkles repo gc local --dry-run --grace 24h     # delete blobs no backup references
sparkles repo locks local [--break ID]
sparkles backup create  --loc db --repo local [--name N] [--note T] [--dataset NAME]  # refused while a server has db open
sparkles backup list    --repo file:///srv/backups/r [--dataset ds | --dataset-id UUID] [--policy P]
sparkles backup show    --repo local b2
sparkles backup verify  --repo local b2 --level restore   # exists | data | restore; exit 1 on failure
sparkles backup restore --repo local b2 --to /srv/dr/ds [--replace]
sparkles backup restore --repo local b2 --data /srv/sparkles [--as ds]  # into a stopped server
sparkles backup delete  --repo local b1          # blobs go at the next gc
sparkles backup policy list | show P | history P # policies of the config file (run: on a server)
sparkles backup policy preview '30 2 * * *' --tz Europe/Berlin [--count 5]
```

`restore --identity auto|new|keep` picks the dataset id (`auto` keeps it unless a dataset
of the target data directory has it) and `--check quick|full|none` the integrity check
before the restored database is published. `sparkles serve` holds a lock on
`<data>/sparkles-server.lock` (one server per data directory), and `restore --data`
refuses while a server holds it.

A server (`sparkles serve --backup-config FILE`) serves the file's repositories and
policies read-only through its API, and also takes repositories registered through its
API and UI (`POST /$/repositories`), under the operator's limits from that file. Their
credentials only name a source defined there, never environment variables, files or the
instance's default chain of the caller's choosing; their S3 endpoints go through the
outbound policy below (a MinIO on localhost needs `--outbound-allow 127.0.0.1` or
`--outbound-allow-private`), never through a proxy of the environment (`HTTPS_PROXY`;
the config file's repositories and the CLI's use it); `fs` ones stay out of the data directory and the config
files' directories, and under `[api] fs_roots` when it is set:

```toml
[credentials.minio]              # named by {"source": "named", "name": "minio"}
source = "env"
access_key_id_var = "MINIO_ACCESS_KEY"
secret_access_key_var = "MINIO_SECRET_KEY"

[api]
fs_roots = ["/srv/backups"]
```

So to register an S3 repository through the API, define its credential source in the
file first (by hand, or with `sparkles repo add … --credentials-name minio --credentials
…` on the same file), start the server with it (or send it SIGHUP), then
`POST /$/repositories` with `"credentials": {"source": "named", "name": "minio"}`.

`scripts/backup-bench.sh DB` (`mise run bench:backup DB`) measures a full backup, an
incremental one after small commits, a restore and a data verification of an existing
database to an `fs` repository.

`scripts/gen-data.py N` generates a synthetic dataset for benchmarking.
`scripts/gen-geo.py N` generates a GeoSPARQL one (points around cities, lines, polygons,
an administrative hierarchy) with its queries, and `scripts/bench-geo.sh N` times them
with and without the spatial index after checking that both give the same answers.
`scripts/geosparql-benchmark.sh` runs the GeoSPARQL Compliance Benchmark (GPL-2.0, so it
is fetched into `target/` at a pinned commit only with `SPARKLES_ALLOW_GPL_BENCHMARK=1`,
and never added to the repository).

### Outbound requests (SERVICE and LOAD)

`SERVICE <url>` and `LOAD <http…>` make the server open connections, so they follow a
network policy (with authentication on, they also need the `federate` permission). The
local `sparkles query` and `sparkles update` follow it too, with a different default
(below):

* only `http` and `https` URLs;
* the host is resolved once and the connection goes to exactly the addresses that were
  checked, so DNS rebinding cannot swap in another address; if any address a name
  resolves to is refused, the name is refused;
* by default only public addresses are contacted. Refused: loopback (`127.0.0.0/8`,
  `::1`), private (`10/8`, `172.16/12`, `192.168/16`), shared (`100.64.0.0/10`),
  link-local (`169.254.0.0/16` with the `169.254.169.254` metadata service, `fe80::/10`),
  unique-local (`fc00::/7`), multicast, broadcast, unspecified, documentation,
  benchmarking and reserved ranges, and the IPv4-mapped, IPv4-compatible, NAT64 and 6to4
  IPv6 forms of those;
* every redirect hop is checked the same way (at most 5 hops);
* a 10 s connect timeout, a total timeout (`--outbound-timeout`, default 60 s, and never
  past the query's own timeout), and a response ceiling counted as the body streams in and,
  for a compressed `LOAD`, after decompression (`--outbound-max-mb`, default 256);
* one budget for all the SERVICE calls and LOADs of a query or update, so that many
  requests cannot add up to more than a few large ones: the bytes they receive
  (`--outbound-request-max-mb`, by default 4 × `--outbound-max-mb`; a compressed `LOAD`
  counts its decompressed bytes) and the time they take, summed
  (`--outbound-request-timeout`, by default 4 × `--outbound-timeout`). Past the bytes the
  request fails with `507` (`"budget": "outbound-bytes"`) and an update commits nothing;
  past the time the next call gets only what is left and then fails like a timeout;
* a `LOAD` is parsed as its response streams in, not buffered first.

A refused destination fails with `403` before any connection is made, and `SILENT` does
not hide it (it hides failures of the remote side, such as timeouts), nor a spent budget.
Proxy environment variables (`HTTP_PROXY`, …) are ignored for these requests.

Error messages name the URL and its host but not what the host resolved to, nor the
connection's own error: a refused name answers `… is refused by the outbound policy`, a
failed connection `cannot connect`. The server logs the details (target
`sparkles::outbound`: the resolved address and its kind, the OS error), so an operator can
tell why while a caller cannot map internal names to addresses.

`--outbound-allow-private` opens loopback, private, shared and unique-local addresses, for
example a local Fuseki during development:

```sh
sparkles serve --data ./data --outbound-allow-private
# SELECT * { SERVICE <http://localhost:3030/ds/sparql> { ?s ?p ?o } }
```

Link-local addresses, the metadata service among them, stay refused. In production,
prefer an allowlist: with `--outbound-allow` (repeatable) only the listed destinations are
contacted. An entry is a host name (`--outbound-allow sparql.example.org`) or
`*.example.org`, the subdomains of a name, both at public addresses (private ones too with
`--outbound-allow-private`); or an address or CIDR network (`--outbound-allow
10.20.0.0/16`), any address in it, private and link-local ones included. A name vouches
for the name, not for its addresses: to reach a name that resolves to a private or
link-local address, list that address or network as well (`--outbound-allow
fuseki.internal --outbound-allow 10.20.0.0/16`), so a hijacked or mistyped DNS record
never opens the metadata service. `sparkles mcp` takes the same flags. Library users set `QueryOptions::outbound`
(`sparkles::outbound::OutboundPolicy`, same defaults); the default refusal of non-public
addresses is the constant `BLOCK_PRIVATE_BY_DEFAULT`.

**Local commands.** `sparkles query` and `sparkles update` without `--server` run on the
operator's own machine, so they allow loopback, private, shared and unique-local
destinations by default, as `--outbound-allow-private` does for a server: a SERVICE call
to a local Fuseki or a `LOAD` from an intranet host needs no flag. Link-local addresses (the
metadata service) stay refused. They take the same `--outbound-*` flags;
`--outbound-block-private` restores the strict default of `serve` and `mcp`, e.g. for a
query from an untrusted source:

```sh
sparkles query --data local.ttl 'SELECT * { SERVICE <http://localhost:3030/ds/sparql> { ?s ?p ?o } }'
sparkles query --data local.ttl --outbound-block-private --query untrusted.rq
```

With `--server`, the request runs on that server under its own policy, and these flags
do not apply.

**Local files.** `LOAD <file:…>` over HTTP needs `server-admin` (with authentication on)
and `serve --load-dir DIR`: the file must be a regular file under `DIR` once `..` and
symbolic links are resolved (a link inside `DIR` may point elsewhere inside it). Without
the flag the server refuses file loads (`403`); `DIR` may not hold the data directory.
The local `sparkles update` reads any file its user can. Library users set
`QueryOptions::file_loads` (`FileLoads::Anywhere` by default, `FileLoads::under(dir)`, or
`FileLoads::Disabled`).

### Checking a database

`sparkles check --loc db` verifies a database directory without modifying it: it takes
no lock, truncates no WAL, repairs no catalog and rebuilds no full-text index, so it can
run next to a server that holds the database (`--data DIR` checks every database of a
server data directory). Each check prints one line, `ok`, `warning` or `error`, with the
file, offset, block, row, commit or id of every problem below it; `--format json`
prints the same report as JSON. The exit status is **0** when everything is clean,
**1** when any check found an error, and **2** when there are warnings only.

| Check | What it verifies |
|---|---|
| `layout` | `CURRENT` names an existing generation; `dataset.json`, the generation's `commit.json` (same dataset id) and `prefixes.json` parse; leftovers of interrupted work (`*.tmp`, `text.new`/`text.old`, old or unfinished `gen-NNNN`, a set-aside catalog) are warnings |
| `generation` | `meta.json` (index format) and `stats.json` parse and agree on the quad count |
| `vocabulary` | the front-coded vocabulary decodes, its keys strictly increase, its size matches `meta.json`, and every vocabulary id in the permutations is below it |
| `delta-vocabulary` | the update vocabulary is well formed and holds no duplicate (a torn tail is a warning) |
| `perm.spo` … `perm.gspo` | block metadata is contiguous and sorted and fits the file; the row count matches `meta.json`; every block decodes to its row count, its first and last keys match the metadata, keys strictly increase within and across blocks, and every id is valid for its position |
| `permutations` | the 7 permutations hold the same number of rows and, compared by an order-independent hash, the same quads |
| `wal` | records are well formed; every commit record's checksum matches (a damaged final transaction is a warning: open truncates it); commit numbers continue from the generation's base commit; ids resolve |
| `catalog` | `commits.bin` record checksums and continuity, its dataset id, and agreement with the WAL; a lagging catalog, or damage open can rebuild from the WAL, is a warning, lost history before the generation an error |
| `text` | `text.json` parses; the index opens read-only, every committed segment file exists and matches its checksum; its commit against the WAL (behind is a warning: caught up or rebuilt on open) |
| `geo` | `geo.json` parses and is a valid configuration (the index itself is built in memory when the database is opened) |
| `reasoning` | `reasoning.json` parses and names an existing commit |

Errors are what `Store::open` refuses, what loses acknowledged data or history, or what
queries would read wrongly; warnings are states open handles by itself. A server writing
meanwhile can cause transient warnings (an in-flight transaction looks like a torn
tail), never errors. `--quick` reads metadata only: block metadata instead of every
block, the first key of each vocabulary block, segment files' presence instead of
their checksums. On the 10.5M-quad benchmark database a full check takes about 0.3 s
and a quick one 20 ms (16 cores, warm page cache). The same check is a library call:
`sparkles::check::check(root, &CheckOptions::default())` returns a serializable
`CheckReport`.

### mise tasks

[`mise.toml`](mise.toml) pins Node, pnpm, hyperfine, prek, shfmt and shellcheck; Rust comes from `rust-toolchain.toml`.
It also defines the everyday tasks (`mise tasks` lists them all):

```sh
mise run build        # UI + release binary
mise run serve        # build, then serve ./data on :3030
mise run fmt          # cargo fmt + oxfmt        (fmt:check for CI)
mise run lint         # clippy -D warnings + svelte-check
mise run test         # all Rust tests            (test:w3c, test:shacl for suite summaries)
mise run ui:test      # UI unit tests (Vitest)
mise run ui:e2e       # UI end-to-end tests (Playwright; Chromium from `nix develop`, see below)
mise run ci           # fmt:check + lint + test + ui:test + licenses:check
mise run gen-data 1000000 target/bench-data/10m.nt
mise run bench        # Sparkles vs Fuseki vs QLever; `bench 1000000 --runs 5` for 10.5M triples
mise run bench:shacl 100000; mise run bench:reasoner 100000 owl-rl
mise run bench:shacl-write 100000   # 1-triple INSERT DATA latency with validation off / warn / reject
mise run licenses     # regenerate THIRD_PARTY_LICENSES.md after a Cargo.lock change (licenses:check)
```

[`THIRD_PARTY_LICENSES.md`](THIRD_PARTY_LICENSES.md) holds the license and NOTICE files of
every crate the binary links (on Linux and macOS), each text once; crates that ship no
license file get their license's standard text. `scripts/third-party-licenses.py`
generates it from `cargo metadata`, so it only changes with `Cargo.lock`; ship it with
binaries (the Nix packages install it as `share/doc/sparkles/THIRD_PARTY_LICENSES.md`).

Git hooks live in [`.pre-commit-config.yaml`](.pre-commit-config.yaml) and run with
[prek](https://github.com/j178/prek) (plain `pre-commit` reads the same file). On staged files
they run `cargo fmt`, oxfmt on `ui/` (the UI's formatter, a devDependency), `nix fmt` (nixfmt,
from the flake) on `*.nix`, and shfmt and shellcheck on shell scripts. `mise run hooks:install` installs the
hook once per clone; `mise run hooks:run` runs every hook over the whole tree.

`mise run ui:e2e` builds the UI and a debug server, starts `sparkles serve` on a free port of
127.0.0.1 with a temporary data directory, a small dataset and an auth configuration (one user,
one API token), runs the Playwright tests in `ui/tests/e2e` in headless Chromium and stops the
server. `SPARKLES_BIN=path/to/sparkles` tests another binary; extra arguments go to
`playwright test` (`mise run ui:e2e -- -g Similar`). The flake's dev shell provides a Chromium
matching the pinned `@playwright/test` (`nix develop -c mise run ui:e2e`); elsewhere the task
downloads one with `playwright install chromium`. It is not part of `mise run ci`; on Linux the
flake runs the same tests as the check `ui-e2e` (see Nix below).

### Nix

The flake (flake-parts + rust-overlay, using the toolchain from `rust-toolchain.toml`)
provides:

* **Packages:**
  * `sparkles` (default): the binary with the UI embedded, and the third-party licenses
    and notices in `share/doc/sparkles/`.
  * `sparkles-cli`: the same binary without the UI, so the build needs no Node.js.
  * `sparkles-ui`: the static UI build.
* **Other outputs:**
  * `overlays.default`;
  * a dev shell;
  * `checks`: the packages; on Linux also a NixOS VM test of the module behind nginx and
    `ui-e2e`, the Playwright UI tests against the release binary in nixpkgs' headless
    Chromium (in the build sandbox, on 127.0.0.1);
  * `nixosModules.default`.

```sh
nix run github:kclejeune/sparkles -- serve --data ./data
nix build .#sparkles-cli
nix flake check          # packages + NixOS VM test (Linux, needs KVM) + UI end-to-end tests
nix build .#checks.x86_64-linux.ui-e2e -L   # only the UI end-to-end tests
```

On NixOS, `services.sparkles` runs the server as a hardened systemd service. Its state
lives in `/var/lib/sparkles` (the dataset registry, databases created from the UI or
admin API, and backups). It can put an nginx virtual host in front of the server, which
you then extend through the usual `services.nginx.virtualHosts.<name>` options:

```nix
{
  inputs.sparkles.url = "github:kclejeune/sparkles";
  outputs = { nixpkgs, sparkles, ... }: {
    nixosConfigurations.host = nixpkgs.lib.nixosSystem {
      modules = [
        sparkles.nixosModules.default
        {
          services.sparkles = {
            enable = true;
            datasets = {
              wiki = { };                       # persistent: /var/lib/sparkles/declarative/wiki
              scratch.type = "mem";
              archive.path = "/srv/rdf/archive"; # an existing database directory
            };
            queryTimeout = 120;
            resultCacheMb = 1024;
            # users, tokens, OIDC (docs/API.md); a secret, never in the Nix store
            auth.configFile = "/run/secrets/sparkles-auth.toml";
            # readOnly = true; allowService = false; unionDefaultGraph = true;
            nginx = {
              enable = true;
              virtualHost = "sparql.example.org";
            };
          };
          # standard nginx semantics: TLS, extra locations …
          services.nginx.virtualHosts."sparql.example.org" = {
            enableACME = true;
            forceSSL = true;
          };
          security.acme.acceptTerms = true;
          security.acme.defaults.email = "admin@example.org";
        }
      ];
    };
  };
}
```

The server listens on `127.0.0.1:3030` by default (`listenAddress`, `port`,
`openFirewall`). Another `listenAddress` needs `auth.configFile` or
`allowOpenNetwork = true` (an assertion checks it). The nginx location sets:

* `client_max_body_size` to `nginx.clientMaxBodySize` (default 4g), for bulk uploads;
* proxy timeouts to `queryTimeout + 30` seconds;
* request/response buffering off, so large uploads and results stream through;
* `X-Forwarded-For` to the client's address (`$remote_addr`, replacing whatever the
  client sent), and `Forwarded` to nothing. The server always trusts nginx for it
  (`--rate-limit-trusted-proxy` for 127.0.0.1 and ::1, or `unix` with `unixSocket`),
  `rateLimits` or not, so failed logins and rate limits count each client rather than
  nginx. Behind another proxy or CDN, set up nginx's realip module so that
  `$remote_addr` is the client.

`loadDir` passes `--load-dir`: `LOAD <file:…>` over HTTP may read from that directory
only (the service gets it read-only; it must not contain `dataDir` or lie under `/tmp`).

With `auth.configFile` the service starts with `--auth-config` and `systemctl reload
sparkles` re-reads it (SIGHUP). Keep the file out of the Nix store (agenix, sops-nix),
owned by the `sparkles` user. Do not also set nginx `basicAuthFile`: nginx would forward
its own `Authorization` header, which Sparkles would then reject. `unixSocket` makes the
server listen on a Unix socket that nginx proxies to, so trusted proxy headers can be
limited to it (`proxy.trusted = ["unix"]`).

Backup repositories: `backup.configFile` passes `--backup-config` (like
`auth.configFile` it stays out of the Nix store, and `systemctl reload sparkles`
re-reads it), `backup.maxTasks` `--backup-max-tasks`, and `backup.fsRoots` lists the
directories of `fs` repositories, which the module creates for the service user and
makes writable (the service sees the rest of the file system read-only):

```nix
services.sparkles.backup = {
  configFile = "/run/secrets/sparkles-backup.toml";  # [repositories.local] path = "/srv/backups/sparkles/local"
  fsRoots = [ "/srv/backups/sparkles" ];             # also [api] fs_roots, for API registrations
  maxTasks = 1;
};
```

The CLI goes on the system path unless `installCli = false`. The server holds a lock on
its databases, so for offline work (`sparkles load`, `compact`) stop the service first,
or use the HTTP API.

## MCP server (LLM agents)

`sparkles mcp` serves databases, or RDF files loaded into memory, to an MCP host
(Claude Desktop, Claude Code, IDE agents, …) over stdin/stdout:

```sh
sparkles mcp --loc /data/books                 # a database directory (name: books)
sparkles mcp --loc books=/data/books --loc films=/data/films
sparkles mcp --data a.ttl b.nt --name demo     # files, in one in-memory dataset
```

Host configuration (`claude_desktop_config.json`, or a project's `.mcp.json` for Claude
Code; `claude mcp add sparkles -- sparkles mcp --loc /data/books` does the same):

```json
{
  "mcpServers": {
    "sparkles": { "command": "sparkles", "args": ["mcp", "--loc", "/data/books"] }
  }
}
```

The tools are read-only: `list_datasets`, `describe_schema`, `sparql_query`,
`explain_query`, `describe_resource`, `list_commits`, `search_text` (BM25 over a
full-text index; `--text` indexes `--data` files) and `similar_entities` (exact search
over stored `spk:vector` embeddings; it never computes them). Schemas are in
[`docs/API.md`](docs/API.md#mcp-server). Results are sized for a model's context:
query rows come back as a compact table with the dataset's prefixes (100 rows / 64 KiB by
default), every truncation is announced with the exact total and how to continue, and
data values are escaped so they cannot pass for table structure or status lines. Every
result names the commit it read; passing it back as `atCommit` keeps a multi-call
exploration on one snapshot (the server holds the last 4 commits read per dataset for 10
minutes).

Every call runs under the query timeout (30 s by default, `--timeout` is the maximum),
a memory budget (`--query-memory-mb`, default 2048) and the intermediate-row cap, at
most `--max-concurrent` (4) at a time. SERVICE is off unless `--allow-service`: a
prompt-injected model could otherwise send data to any URL; allowed, it follows the
[outbound policy](#outbound-requests-service-and-load). `--disable-tool NAME`
removes a tool. A database held by a running `sparkles serve` is refused (the lock);
only stdio is served for now. Logs go to stderr; stdout carries JSON-RPC only.

## Testing

```sh
mise run ci            # formatting, clippy, all workspace tests, svelte-check, UI unit tests, license notices
mise run test:w3c      # W3C SPARQL 1.0 / 1.1 query / 1.1 update / 1.2 suites, with a summary
mise run test:shacl    # W3C SHACL Core and SHACL-SPARQL suites
mise run test:shex     # shexTest: syntax, negative syntax and structure, representation, validation
mise run ui:e2e        # Playwright end-to-end tests against a real server
```

`crates/sparkles/tests/w3c.rs` runs the W3C SPARQL suites vendored in the Apache Jena
checkout (`../../apache/jena` next to this repository, or `SPARKLES_W3C_DIR`); the SHACL
suites come from the same checkout (or `SPARKLES_SHACL_TESTS`). Without the checkout the
suites are skipped. All of them pass (482/482, 328/328, 157/157, 269/269; SHACL 98/98 and
20/20); `crates/sparkles/tests/w3c-known-failures.txt` lists known failures and is empty.
The shexTest suite comes from the same checkout too (`jena-shex`, or `SPARKLES_SHEX_TESTS`
for an upstream shexTest checkout); `crates/sparkles-shex/tests/known-failures.txt` lists
what fails, with reasons.
